use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{ClientOptions, Tls, TlsOptions};
use mongodb::{Client, Database};
use secrecy::ExposeSecret;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, DriverMaturity, ExecResult, MAX_QUERY_ROWS,
    QueryResult, TableInfo, Value,
};

const SAMPLE_DOCS: i64 = 50;

pub struct MongodbDriver;

#[async_trait]
impl DatabaseDriver for MongodbDriver {
    fn id(&self) -> &'static str {
        "mongodb"
    }

    fn display_name(&self) -> &'static str {
        "MongoDB"
    }

    fn maturity(&self) -> DriverMaturity {
        DriverMaturity::Experimental
    }

    fn default_port(&self) -> u16 {
        27017
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        let scheme = "mongodb";
        let password = opts.password.expose_secret();
        let auth = if !opts.username.is_empty() {
            format!("{}:{}@", encode_uri(&opts.username), encode_uri(password))
        } else {
            String::new()
        };
        let db_path = if opts.database.is_empty() {
            String::new()
        } else {
            format!("/{}", encode_uri(&opts.database))
        };
        let uri = format!("{scheme}://{auth}{}:{}{db_path}", opts.host, opts.port);
        let mut client_opts = ClientOptions::parse(&uri).await.map_err(map_mongo_error)?;
        client_opts.app_name = Some("TablePro".into());
        client_opts.tls = Some(tls_for(&opts.tls));
        client_opts.connect_timeout = Some(CONNECT_TIMEOUT);
        client_opts.server_selection_timeout = Some(CONNECT_TIMEOUT);
        let client = Client::with_options(client_opts).map_err(map_mongo_error)?;
        let database_name = if opts.database.is_empty() {
            "test".into()
        } else {
            opts.database
        };
        client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(map_mongo_error)?;
        Ok(Box::new(MongodbConnection { client, database_name }))
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Map the shared TLS modes onto what the rustls-backed MongoDB driver can
/// express. It has no CA-only mode, so `VerifyCa` verifies the hostname too,
/// which is stricter than requested and never weaker.
fn tls_for(config: &tablepro_core::TlsConfig) -> Tls {
    use tablepro_core::TlsMode;
    if config.mode == TlsMode::Disabled {
        return Tls::Disabled;
    }
    let mut options = TlsOptions::default();
    if let Some(path) = &config.root_cert {
        options.ca_file_path = Some(path.clone());
    }
    if !config.mode.verifies_cert() {
        options.allow_invalid_certificates = Some(true);
    }
    Tls::Enabled(options)
}

struct MongodbConnection {
    client: Client,
    database_name: String,
}

impl MongodbConnection {
    fn db(&self) -> Database {
        self.client.database(&self.database_name)
    }
}

#[async_trait]
impl Connection for MongodbConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let names = self.db().list_collection_names().await.map_err(map_mongo_error)?;
        let mut tables: Vec<TableInfo> = names
            .into_iter()
            .map(|name| TableInfo {
                schema: Some(self.database_name.clone()),
                name,
            })
            .collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tables)
    }

    async fn fetch_columns(&self, _schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let coll = self.db().collection::<Document>(table);
        let mut cursor = coll.find(doc! {}).limit(SAMPLE_DOCS).await.map_err(map_mongo_error)?;
        let mut union: BTreeMap<String, String> = BTreeMap::new();
        while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
            for (key, value) in doc {
                union.entry(key).or_insert_with(|| bson_type_name(&value));
            }
        }
        if !union.contains_key("_id") {
            union.insert("_id".into(), "ObjectId".into());
        }
        let mut columns: Vec<ColumnInfo> = Vec::with_capacity(union.len());
        if let Some(ty) = union.remove("_id") {
            columns.push(ColumnInfo {
                name: "_id".into(),
                data_type: ty,
                nullable: false,
                primary_key: true,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            });
        }
        for (name, data_type) in union {
            columns.push(ColumnInfo {
                name,
                data_type,
                nullable: true,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            });
        }
        Ok(columns)
    }

    async fn fetch_rows(
        &self,
        _schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let columns = self.fetch_columns(None, table).await?;
        let coll = self.db().collection::<Document>(table);
        let mut cursor = coll
            .find(doc! {})
            .skip(offset)
            .limit(limit as i64)
            .await
            .map_err(map_mongo_error)?;
        let mut rows = Vec::new();
        while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
            rows.push(document_to_row(&doc, &columns));
        }
        Ok(QueryResult {
            columns,
            rows,
            truncated: false,
        })
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let trimmed = sql.trim();
        if let Some(parsed) = parse_find_shell(trimmed) {
            return self.run_find(parsed).await;
        }
        if let Some(parsed) = parse_aggregate_shell(trimmed) {
            return self.run_aggregate(parsed).await;
        }
        if trimmed.starts_with('{') {
            let filter: Document = mongodb::bson::from_slice(trimmed.as_bytes())
                .or_else(|_| serde_json_to_document(trimmed))
                .map_err(|e| DriverError::Query {
                    message: format!("invalid MongoDB filter JSON: {e}"),
                    sqlstate: None,
                })?;
            let coll_name = self
                .list_tables()
                .await?
                .into_iter()
                .next()
                .map(|t| t.name)
                .ok_or_else(|| DriverError::Query {
                    message: "no collections available for filter query".into(),
                    sqlstate: None,
                })?;
            return self
                .run_find(FindQuery {
                    collection: coll_name,
                    filter,
                    skip: 0,
                    limit: MAX_QUERY_ROWS as i64,
                })
                .await;
        }
        Err(DriverError::Unsupported(format!(
            "MongoDB driver accepts db.collection.find(...) / aggregate(...) or JSON filter; got: {trimmed}"
        )))
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let trimmed = sql.trim();
        if let Some((coll, doc)) = parse_insert_one(trimmed) {
            self.db()
                .collection::<Document>(&coll)
                .insert_one(doc)
                .await
                .map_err(map_mongo_error)?;
            return Ok(ExecResult { rows_affected: 1 });
        }
        if let Some((coll, filter)) = parse_delete_many(trimmed) {
            let result = self
                .db()
                .collection::<Document>(&coll)
                .delete_many(filter)
                .await
                .map_err(map_mongo_error)?;
            return Ok(ExecResult {
                rows_affected: result.deleted_count,
            });
        }
        Err(DriverError::Unsupported(
            "MongoDB execute supports insertOne/deleteMany shell forms only in this MVP".into(),
        ))
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        if params.is_empty() {
            return self.execute(sql).await;
        }
        Err(DriverError::Unsupported(
            "MongoDB execute_params does not support bound parameters".into(),
        ))
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let mut affected = Vec::with_capacity(statements.len());
        for (idx, (sql, params)) in statements.iter().enumerate() {
            match self.execute_params(sql, params).await {
                Ok(res) => affected.push(res.rows_affected),
                Err(e) => {
                    return Err(DriverError::Transaction {
                        statement_index: idx,
                        source: Box::new(e),
                    });
                }
            }
        }
        Ok(affected)
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(map_mongo_error)?;
        Ok(())
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let reply = self
            .client
            .database("admin")
            .run_command(doc! { "buildInfo": 1 })
            .await
            .map_err(map_mongo_error)?;
        let version = reply.get_str("version").ok().map(|v| format!("MongoDB {v}"));
        Ok(version)
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

struct FindQuery {
    collection: String,
    filter: Document,
    skip: u64,
    limit: i64,
}

struct AggregateQuery {
    collection: String,
    pipeline: Vec<Document>,
}

impl MongodbConnection {
    async fn run_find(&self, q: FindQuery) -> Result<QueryResult, DriverError> {
        let columns = self.fetch_columns(None, &q.collection).await?;
        let coll = self.db().collection::<Document>(&q.collection);
        let mut cursor = coll
            .find(q.filter)
            .skip(q.skip)
            .limit(q.limit)
            .await
            .map_err(map_mongo_error)?;
        let mut rows = Vec::new();
        let mut truncated = false;
        while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
            if rows.len() >= MAX_QUERY_ROWS {
                truncated = true;
                break;
            }
            rows.push(document_to_row(&doc, &columns));
        }
        Ok(QueryResult {
            columns,
            rows,
            truncated,
        })
    }

    async fn run_aggregate(&self, q: AggregateQuery) -> Result<QueryResult, DriverError> {
        let coll = self.db().collection::<Document>(&q.collection);
        let mut cursor = coll.aggregate(q.pipeline).await.map_err(map_mongo_error)?;
        let mut docs = Vec::new();
        let mut truncated = false;
        while let Some(doc) = cursor.try_next().await.map_err(map_mongo_error)? {
            if docs.len() >= MAX_QUERY_ROWS {
                truncated = true;
                break;
            }
            docs.push(doc);
        }
        let columns = columns_from_docs(&docs);
        let rows = docs.iter().map(|d| document_to_row(d, &columns)).collect();
        Ok(QueryResult {
            columns,
            rows,
            truncated,
        })
    }
}

fn columns_from_docs(docs: &[Document]) -> Vec<ColumnInfo> {
    let mut union: BTreeMap<String, String> = BTreeMap::new();
    for doc in docs {
        for (key, value) in doc {
            union.entry(key.clone()).or_insert_with(|| bson_type_name(value));
        }
    }
    let mut columns = Vec::new();
    if let Some(ty) = union.remove("_id") {
        columns.push(ColumnInfo {
            name: "_id".into(),
            data_type: ty,
            nullable: false,
            primary_key: true,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        });
    }
    for (name, data_type) in union {
        columns.push(ColumnInfo {
            name,
            data_type,
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        });
    }
    columns
}

fn document_to_row(doc: &Document, columns: &[ColumnInfo]) -> Vec<Value> {
    columns
        .iter()
        .map(|c| match doc.get(&c.name) {
            Some(b) => bson_to_value(b),
            None => Value::Null,
        })
        .collect()
}

fn bson_to_value(b: &Bson) -> Value {
    match b {
        Bson::Null => Value::Null,
        Bson::Boolean(v) => Value::Bool(*v),
        Bson::Int32(v) => Value::Int(*v as i64),
        Bson::Int64(v) => Value::Int(*v),
        Bson::Double(v) => Value::Float(*v),
        Bson::String(v) => Value::Text(v.clone()),
        Bson::ObjectId(v) => Value::Text(v.to_hex()),
        Bson::DateTime(v) => Value::Text(v.try_to_rfc3339_string().unwrap_or_else(|_| v.to_string())),
        Bson::Binary(bin) => Value::Bytes(bin.bytes.clone()),
        Bson::Decimal128(d) => Value::Text(d.to_string()),
        Bson::Document(d) => Value::Json(document_to_json(d)),
        Bson::Array(a) => Value::Json(serde_json::Value::Array(a.iter().map(bson_to_json).collect())),
        other => Value::Text(other.to_string()),
    }
}

fn bson_to_json(b: &Bson) -> serde_json::Value {
    match b {
        Bson::Null => serde_json::Value::Null,
        Bson::Boolean(v) => serde_json::Value::Bool(*v),
        Bson::Int32(v) => serde_json::json!(*v),
        Bson::Int64(v) => serde_json::json!(*v),
        Bson::Double(v) => serde_json::json!(*v),
        Bson::String(v) => serde_json::Value::String(v.clone()),
        Bson::Document(d) => document_to_json(d),
        Bson::Array(a) => serde_json::Value::Array(a.iter().map(bson_to_json).collect()),
        other => serde_json::Value::String(other.to_string()),
    }
}

fn document_to_json(doc: &Document) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in doc {
        map.insert(k.clone(), bson_to_json(v));
    }
    serde_json::Value::Object(map)
}

fn bson_type_name(b: &Bson) -> String {
    match b {
        Bson::Null => "null",
        Bson::Boolean(_) => "bool",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "date",
        Bson::Binary(_) => "binData",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Decimal128(_) => "decimal",
        _ => "mixed",
    }
    .into()
}

fn encode_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse `db.coll.find({...})` / `db["coll"].find({...}).skip(n).limit(m)`.
fn parse_find_shell(input: &str) -> Option<FindQuery> {
    let input = input.trim().trim_end_matches(';');
    let (collection, rest) = split_collection_call(input, "find")?;
    let (filter_src, after_filter) = extract_balanced(rest.trim_start(), '(', ')')?;
    let filter = if filter_src.trim().is_empty() {
        Document::new()
    } else {
        serde_json_to_document(filter_src).ok()?
    };
    let mut skip = 0u64;
    let mut limit = MAX_QUERY_ROWS as i64;
    let mut remaining = after_filter;
    while let Some(pos) = remaining.find('.') {
        remaining = &remaining[pos + 1..];
        if let Some(rest) = remaining.strip_prefix("skip") {
            let (n, after) = extract_balanced(rest.trim_start(), '(', ')')?;
            skip = n.trim().parse().ok()?;
            remaining = after;
        } else if let Some(rest) = remaining.strip_prefix("limit") {
            let (n, after) = extract_balanced(rest.trim_start(), '(', ')')?;
            limit = n.trim().parse().ok()?;
            remaining = after;
        } else {
            break;
        }
    }
    Some(FindQuery {
        collection,
        filter,
        skip,
        limit,
    })
}

fn parse_aggregate_shell(input: &str) -> Option<AggregateQuery> {
    let input = input.trim().trim_end_matches(';');
    let (collection, rest) = split_collection_call(input, "aggregate")?;
    let (pipeline_src, _) = extract_balanced(rest.trim_start(), '(', ')')?;
    let value: serde_json::Value = serde_json::from_str(pipeline_src).ok()?;
    let arr = value.as_array()?;
    let mut pipeline = Vec::new();
    for item in arr {
        pipeline.push(serde_json_to_document(&item.to_string()).ok()?);
    }
    Some(AggregateQuery { collection, pipeline })
}

fn parse_insert_one(input: &str) -> Option<(String, Document)> {
    let (collection, rest) = split_collection_call(input.trim().trim_end_matches(';'), "insertOne")?;
    let (doc_src, _) = extract_balanced(rest.trim_start(), '(', ')')?;
    Some((collection, serde_json_to_document(doc_src).ok()?))
}

fn parse_delete_many(input: &str) -> Option<(String, Document)> {
    let (collection, rest) = split_collection_call(input.trim().trim_end_matches(';'), "deleteMany")?;
    let (doc_src, _) = extract_balanced(rest.trim_start(), '(', ')')?;
    Some((collection, serde_json_to_document(doc_src).ok()?))
}

fn split_collection_call<'a>(input: &'a str, method: &str) -> Option<(String, &'a str)> {
    let rest = input.strip_prefix("db")?;
    let (collection, after) = if let Some(rest) = rest.strip_prefix('.') {
        let end = rest.find('.')?;
        let name = &rest[..end];
        if !is_simple_ident(name) {
            return None;
        }
        (name.to_string(), &rest[end..])
    } else if let Some(rest) = rest.strip_prefix("[\"") {
        let end = rest.find("\"]")?;
        let name = rest[..end].to_string();
        let after = &rest[end + 2..];
        (name, after)
    } else {
        let rest = rest.strip_prefix("['")?;
        let end = rest.find("']")?;
        let name = rest[..end].to_string();
        let after = &rest[end + 2..];
        (name, after)
    };
    let after = after.strip_prefix('.')?;
    let after = after.strip_prefix(method)?;
    Some((collection, after))
}

fn extract_balanced(input: &str, open: char, close: char) -> Option<(&str, &str)> {
    let input = input.trim_start();
    if !input.starts_with(open) {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = None::<char>;
    let mut escaped = false;
    for (i, c) in input.char_indices() {
        if let Some(q) = in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some((&input[1..i], &input[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn serde_json_to_document(src: &str) -> Result<Document, String> {
    let value: serde_json::Value = serde_json::from_str(src).map_err(|e| format!("JSON parse error: {e}"))?;
    mongodb::bson::to_document(&value).map_err(|e| format!("BSON convert error: {e}"))
}

fn map_mongo_error(err: mongodb::error::Error) -> DriverError {
    let msg = err.to_string();
    if msg.contains("Connection refused") || msg.contains("connection") {
        DriverError::ConnectionRefused
    } else if msg.contains("Authentication failed") || msg.contains("auth") {
        DriverError::AuthFailed
    } else {
        DriverError::Query {
            message: msg,
            sqlstate: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_metadata() {
        let d = MongodbDriver;
        assert_eq!(d.id(), "mongodb");
        assert_eq!(d.display_name(), "MongoDB");
        assert_eq!(d.default_port(), 27017);
    }

    #[test]
    fn parse_find_shell_basic() {
        let q = parse_find_shell(r#"db.users.find({"age": {"$gt": 18}}).limit(10)"#).unwrap();
        assert_eq!(q.collection, "users");
        assert_eq!(q.limit, 10);
        assert_eq!(q.filter.get_document("age").unwrap().get_i64("$gt").unwrap(), 18);
    }

    #[test]
    fn parse_find_shell_bracket_name() {
        let q = parse_find_shell(r#"db["my-coll"].find({})"#).unwrap();
        assert_eq!(q.collection, "my-coll");
    }

    #[test]
    fn parse_aggregate_shell_pipeline() {
        let q = parse_aggregate_shell(r#"db.orders.aggregate([{"$match": {"status": "a"}}])"#).unwrap();
        assert_eq!(q.collection, "orders");
        assert_eq!(q.pipeline.len(), 1);
    }

    #[test]
    fn bson_type_name_object_id() {
        assert_eq!(bson_type_name(&Bson::Null), "null");
        assert_eq!(bson_type_name(&Bson::String("x".into())), "string");
    }
}
