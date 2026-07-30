use async_trait::async_trait;
use clickhouse::Client;
use secrecy::ExposeSecret;
use serde_json::Value as JsonValue;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, ExecResult, MAX_QUERY_ROWS,
    QueryResult, TableInfo, Value,
};

pub struct ClickhouseDriver;

#[async_trait]
impl DatabaseDriver for ClickhouseDriver {
    fn id(&self) -> &'static str {
        "clickhouse"
    }

    fn display_name(&self) -> &'static str {
        "ClickHouse"
    }

    fn default_port(&self) -> u16 {
        8123
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        let scheme = match opts.tls.mode {
            tablepro_core::TlsMode::Disabled => "http",
            _ => "https",
        };
        let url = format!("{scheme}://{}:{}", opts.host, opts.port);
        let mut client = Client::default().with_url(url);
        if !opts.username.is_empty() {
            client = client.with_user(&opts.username);
        }
        let password = opts.password.expose_secret();
        if !password.is_empty() {
            client = client.with_password(password);
        }
        if !opts.database.is_empty() {
            client = client.with_database(&opts.database);
        }
        let conn = ClickhouseConnection {
            client,
            database: if opts.database.is_empty() {
                "default".into()
            } else {
                opts.database
            },
        };
        conn.ping().await?;
        Ok(Box::new(conn))
    }
}

struct ClickhouseConnection {
    client: Client,
    database: String,
}

#[async_trait]
impl Connection for ClickhouseConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let sql = format!(
            "SELECT database, name FROM system.tables \
             WHERE database = '{}' AND is_temporary = 0 \
             AND engine NOT LIKE '%View' \
             ORDER BY name",
            escape_literal(&self.database)
        );
        let json = fetch_json(&self.client, &sql).await?;
        Ok(json_rows(&json)
            .into_iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_string();
                let schema = row.get("database").and_then(|v| v.as_str()).map(str::to_string);
                Some(TableInfo { schema, name })
            })
            .collect())
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let db = schema.unwrap_or(self.database.as_str());
        let sql = format!(
            "SELECT name, type, default_kind, default_expression, is_in_primary_key \
             FROM system.columns \
             WHERE database = '{}' AND table = '{}' \
             ORDER BY position",
            escape_literal(db),
            escape_literal(table)
        );
        let json = fetch_json(&self.client, &sql).await?;
        Ok(json_rows(&json)
            .into_iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_string();
                let data_type = row.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let primary_key = row
                    .get("is_in_primary_key")
                    .and_then(|v| v.as_u64().or_else(|| v.as_bool().map(|b| b as u64)))
                    .unwrap_or(0)
                    > 0;
                let default_kind = row.get("default_kind").and_then(|v| v.as_str()).unwrap_or("");
                let default_expression = row
                    .get("default_expression")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let is_generated = matches!(default_kind, "MATERIALIZED" | "ALIAS" | "EPHEMERAL");
                Some(ColumnInfo {
                    name,
                    data_type,
                    nullable: true,
                    primary_key,
                    is_auto_increment: false,
                    default_value: default_expression,
                    is_generated,
                })
            })
            .collect())
    }

    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let sql = format!(
            "SELECT * FROM {} LIMIT {limit} OFFSET {offset}",
            qualified(schema.or(Some(self.database.as_str())), table)
        );
        run_select(&self.client, &sql, limit as usize).await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let trimmed = sql.trim();
        if looks_like_mutation(trimmed) {
            self.execute(trimmed).await?;
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                truncated: false,
            });
        }
        run_select(&self.client, trimmed, MAX_QUERY_ROWS).await
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        self.client
            .query(sql)
            .execute()
            .await
            .map_err(map_ch_error)?;
        Ok(ExecResult { rows_affected: 0 })
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        if params.is_empty() {
            return self.execute(sql).await;
        }
        Err(DriverError::Unsupported(
            "ClickHouse execute_params does not support bound parameters yet".into(),
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
            .query("SELECT 1")
            .execute()
            .await
            .map_err(map_ch_error)?;
        Ok(())
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let json = fetch_json(&self.client, "SELECT version() AS v").await?;
        let version = json_rows(&json)
            .into_iter()
            .next()
            .and_then(|row| row.get("v").and_then(|v| v.as_str()).map(str::to_string));
        Ok(version.map(|v| format!("ClickHouse {v}")))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

async fn run_select(client: &Client, sql: &str, limit: usize) -> Result<QueryResult, DriverError> {
    let json = fetch_json(client, sql).await?;
    parse_json_result(&json, limit)
}

async fn fetch_json(client: &Client, sql: &str) -> Result<JsonValue, DriverError> {
    let mut cursor = client.query(sql).fetch_bytes("JSON").map_err(map_ch_error)?;
    let bytes = cursor.collect().await.map_err(map_ch_error)?;
    serde_json::from_slice(&bytes).map_err(|e| DriverError::Internal(format!("ClickHouse JSON parse: {e}")))
}

fn parse_json_result(json: &JsonValue, limit: usize) -> Result<QueryResult, DriverError> {
    let meta = json
        .get("meta")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    let columns: Vec<ColumnInfo> = meta
        .iter()
        .filter_map(|col| {
            Some(ColumnInfo {
                name: col.get("name")?.as_str()?.to_string(),
                data_type: col.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                nullable: true,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            })
        })
        .collect();
    let data = json.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    let truncated = data.len() > limit;
    let rows: Vec<Vec<Value>> = data
        .into_iter()
        .take(limit)
        .map(|row| {
            columns
                .iter()
                .map(|c| json_to_value(row.get(&c.name).unwrap_or(&JsonValue::Null)))
                .collect()
        })
        .collect();
    Ok(QueryResult {
        columns,
        rows,
        truncated,
    })
}

fn json_rows(json: &JsonValue) -> Vec<serde_json::Map<String, JsonValue>> {
    json.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default()
}

fn json_to_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    Value::Int(u as i64)
                } else {
                    Value::Text(u.to_string())
                }
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Text(n.to_string())
            }
        }
        JsonValue::String(s) => Value::Text(s.clone()),
        other => Value::Json(other.clone()),
    }
}

fn looks_like_mutation(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    upper.starts_with("INSERT")
        || upper.starts_with("ALTER")
        || upper.starts_with("CREATE")
        || upper.starts_with("DROP")
        || upper.starts_with("TRUNCATE")
        || upper.starts_with("OPTIMIZE")
        || upper.starts_with("RENAME")
        || upper.starts_with("DELETE")
        || upper.starts_with("UPDATE")
        || upper.starts_with("ATTACH")
        || upper.starts_with("DETACH")
        || upper.starts_with("SYSTEM")
}

fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn qualified(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(s), quote_ident(table)),
        _ => quote_ident(table),
    }
}

fn escape_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn map_ch_error(err: clickhouse::error::Error) -> DriverError {
    let msg = err.to_string();
    if msg.contains("Connection refused") || msg.contains("connect") {
        return DriverError::ConnectionRefused;
    }
    if msg.contains("Authentication") || msg.contains("password") {
        return DriverError::AuthFailed;
    }
    DriverError::Query {
        message: msg,
        sqlstate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_metadata() {
        let d = ClickhouseDriver;
        assert_eq!(d.id(), "clickhouse");
        assert_eq!(d.display_name(), "ClickHouse");
        assert_eq!(d.default_port(), 8123);
        assert!(!d.is_file_based());
    }

    #[test]
    fn quote_ident_escapes_backticks() {
        assert_eq!(quote_ident("users"), "`users`");
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn json_to_value_maps_scalars() {
        assert_eq!(json_to_value(&JsonValue::Null), Value::Null);
        assert_eq!(json_to_value(&JsonValue::Bool(true)), Value::Bool(true));
        assert_eq!(json_to_value(&JsonValue::from(42)), Value::Int(42));
        assert_eq!(json_to_value(&JsonValue::from("hi")), Value::Text("hi".into()));
    }

    #[test]
    fn parse_json_result_reads_meta_and_data() {
        let json: JsonValue = serde_json::json!({
            "meta": [{"name": "id", "type": "UInt64"}, {"name": "name", "type": "String"}],
            "data": [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}],
            "rows": 2
        });
        let result = parse_json_result(&json, 10).unwrap();
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][0], Value::Int(1));
        assert_eq!(result.rows[0][1], Value::Text("a".into()));
    }
}
