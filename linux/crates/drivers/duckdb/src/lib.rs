use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use duckdb::{Connection as DuckConnection, params_from_iter, types::ValueRef};

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, DriverMaturity, ExecResult, MAX_QUERY_ROWS,
    QueryResult, TableInfo, Value,
};

pub struct DuckdbDriver;

#[async_trait]
impl DatabaseDriver for DuckdbDriver {
    fn id(&self) -> &'static str {
        "duckdb"
    }

    fn display_name(&self) -> &'static str {
        "DuckDB"
    }

    fn maturity(&self) -> DriverMaturity {
        DriverMaturity::Experimental
    }

    fn default_port(&self) -> u16 {
        0
    }

    fn is_file_based(&self) -> bool {
        true
    }

    fn ddl_is_transactional(&self) -> bool {
        true
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        let path = opts.database.clone();
        let conn = tokio::task::spawn_blocking(move || open_path(&path))
            .await
            .map_err(|e| DriverError::Internal(format!("duckdb connect join: {e}")))??;
        Ok(Box::new(DuckdbConnection {
            conn: Arc::new(Mutex::new(conn)),
        }))
    }
}

/// A `.duckdb`/`.db` file opens directly. A flat data file (Parquet,
/// CSV, TSV, JSON) isn't a DuckDB database at all -- open an in-memory
/// database instead and expose the file as a view through DuckDB's own
/// file-reading functions, so the rest of the driver (which only knows
/// how to query DuckDB tables/views) needs no special case.
fn open_path(path: &str) -> Result<DuckConnection, DriverError> {
    if path.is_empty() || path == ":memory:" {
        return DuckConnection::open_in_memory().map_err(map_duck_error);
    }
    let Some(reader_fn) = flat_file_reader_fn(path) else {
        return DuckConnection::open(path).map_err(map_duck_error);
    };
    let conn = DuckConnection::open_in_memory().map_err(map_duck_error)?;
    let sql = format!(
        "CREATE VIEW {} AS SELECT * FROM {}('{}')",
        quote_duckdb_ident(&derive_view_name(path)),
        reader_fn,
        escape_duckdb_literal(path)
    );
    conn.execute(&sql, []).map_err(map_duck_error)?;
    Ok(conn)
}

fn flat_file_reader_fn(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".parquet") {
        Some("read_parquet")
    } else if lower.ends_with(".csv") || lower.ends_with(".tsv") {
        Some("read_csv_auto")
    } else if lower.ends_with(".json") {
        Some("read_json_auto")
    } else {
        None
    }
}

/// The file's stem, sanitized into a safe view name -- falls back to
/// "data" for a stem that sanitizes away to nothing (an all-symbol
/// filename, or none at all).
fn derive_view_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_ident)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "data".to_string())
}

fn sanitize_ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn quote_duckdb_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn escape_duckdb_literal(value: &str) -> String {
    value.replace('\'', "''")
}

struct DuckdbConnection {
    conn: Arc<Mutex<DuckConnection>>,
}

#[async_trait]
impl Connection for DuckdbConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
            let mut stmt = guard
                .prepare(
                    "SELECT table_schema, table_name FROM information_schema.tables \
                     WHERE table_schema NOT IN ('information_schema', 'pg_catalog') \
                     AND table_type = 'BASE TABLE' \
                     ORDER BY table_schema, table_name",
                )
                .map_err(map_duck_error)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(TableInfo {
                        schema: row.get::<_, Option<String>>(0)?,
                        name: row.get::<_, String>(1)?,
                    })
                })
                .map_err(map_duck_error)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_duck_error)?);
            }
            Ok(out)
        })
        .await
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let conn = Arc::clone(&self.conn);
        let schema = schema.map(str::to_string);
        let table = table.to_string();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
            let sql = if let Some(schema) = schema.as_deref() {
                format!(
                    "SELECT column_name, data_type, is_nullable, column_default \
                     FROM information_schema.columns \
                     WHERE table_schema = '{}' AND table_name = '{}' \
                     ORDER BY ordinal_position",
                    escape_literal(schema),
                    escape_literal(&table)
                )
            } else {
                format!(
                    "SELECT column_name, data_type, is_nullable, column_default \
                     FROM information_schema.columns \
                     WHERE table_name = '{}' \
                     ORDER BY ordinal_position",
                    escape_literal(&table)
                )
            };
            let mut stmt = guard.prepare(&sql).map_err(map_duck_error)?;
            let rows = stmt
                .query_map([], |row| {
                    let nullable: String = row.get(2)?;
                    Ok(ColumnInfo {
                        name: row.get(0)?,
                        data_type: row.get(1)?,
                        nullable: nullable.eq_ignore_ascii_case("YES"),
                        primary_key: false,
                        is_auto_increment: false,
                        default_value: row.get(3)?,
                        is_generated: false,
                    })
                })
                .map_err(map_duck_error)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(map_duck_error)?);
            }
            Ok(out)
        })
        .await
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
            qualified(schema, table)
        );
        self.query(&sql).await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        blocking(move || run_query(&conn, &sql, &[], MAX_QUERY_ROWS)).await
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
            let rows_affected = guard.execute(&sql, []).map_err(map_duck_error)? as u64;
            Ok(ExecResult { rows_affected })
        })
        .await
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        if params.is_empty() {
            return self.execute(sql).await;
        }
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        let params = params.to_vec();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
            let bind = values_to_duck_params(&params);
            let rows_affected = guard
                .execute(&sql, params_from_iter(bind.iter()))
                .map_err(map_duck_error)? as u64;
            Ok(ExecResult { rows_affected })
        })
        .await
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let conn = Arc::clone(&self.conn);
        let statements = statements.to_vec();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
            guard.execute_batch("BEGIN").map_err(map_duck_error)?;
            let mut affected = Vec::with_capacity(statements.len());
            for (idx, (sql, params)) in statements.iter().enumerate() {
                let result = if params.is_empty() {
                    guard.execute(sql, [])
                } else {
                    let bind = values_to_duck_params(params);
                    guard.execute(sql, params_from_iter(bind.iter()))
                };
                match result {
                    Ok(n) => affected.push(n as u64),
                    Err(e) => {
                        let _ = guard.execute_batch("ROLLBACK");
                        return Err(DriverError::Transaction {
                            statement_index: idx,
                            source: Box::new(map_duck_error(e)),
                        });
                    }
                }
            }
            guard.execute_batch("COMMIT").map_err(map_duck_error)?;
            Ok(affected)
        })
        .await
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.execute("SELECT 1").await.map(|_| ())
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let result = self.query("SELECT version()").await?;
        let version = result.rows.first().and_then(|r| r.first()).and_then(|v| match v {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        Ok(version.map(|v| format!("DuckDB {v}")))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

fn run_query(
    conn: &Arc<Mutex<DuckConnection>>,
    sql: &str,
    params: &[Value],
    limit: usize,
) -> Result<QueryResult, DriverError> {
    let guard = conn
        .lock()
        .map_err(|_| DriverError::Internal("duckdb lock poisoned".into()))?;
    let mut stmt = guard.prepare(sql).map_err(map_duck_error)?;
    let bind = values_to_duck_params(params);
    let mut rows = if params.is_empty() {
        stmt.query([])
    } else {
        stmt.query(params_from_iter(bind.iter()))
    }
    .map_err(map_duck_error)?;

    // DuckDB requires the statement to be stepped before column metadata
    // is available. Collect all values first (capped), then read names.
    let mut raw_rows: Vec<Vec<Value>> = Vec::new();
    let mut truncated = false;
    let mut column_count = 0usize;
    while let Some(row) = rows.next().map_err(map_duck_error)? {
        if column_count == 0 {
            column_count = row.as_ref().column_count();
        }
        if raw_rows.len() >= limit {
            truncated = true;
            break;
        }
        raw_rows.push(
            (0..column_count)
                .map(|i| duck_value_ref_to_value(row.get_ref_unwrap(i)))
                .collect(),
        );
    }
    if column_count == 0
        && let Some(stmt_ref) = rows.as_ref()
    {
        column_count = stmt_ref.column_count();
    }
    let columns: Vec<ColumnInfo> = if let Some(stmt_ref) = rows.as_ref() {
        (0..column_count)
            .map(|i| ColumnInfo {
                name: stmt_ref
                    .column_name(i)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| format!("col{i}")),
                data_type: format!("{:?}", stmt_ref.column_type(i)),
                nullable: true,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            })
            .collect()
    } else {
        (0..column_count)
            .map(|i| ColumnInfo {
                name: format!("col{i}"),
                data_type: "UNKNOWN".into(),
                nullable: true,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            })
            .collect()
    };

    Ok(QueryResult {
        columns,
        rows: raw_rows,
        truncated,
    })
}

fn duck_value_ref_to_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Boolean(b) => Value::Bool(b),
        ValueRef::TinyInt(i) => Value::Int(i as i64),
        ValueRef::SmallInt(i) => Value::Int(i as i64),
        ValueRef::Int(i) => Value::Int(i as i64),
        ValueRef::BigInt(i) => Value::Int(i),
        ValueRef::HugeInt(i) => Value::Text(i.to_string()),
        ValueRef::UHugeInt(i) => Value::Text(i.to_string()),
        ValueRef::UTinyInt(i) => Value::Int(i as i64),
        ValueRef::USmallInt(i) => Value::Int(i as i64),
        ValueRef::UInt(i) => Value::Int(i as i64),
        ValueRef::UBigInt(i) => {
            if i <= i64::MAX as u64 {
                Value::Int(i as i64)
            } else {
                Value::Text(i.to_string())
            }
        }
        ValueRef::Float(f) => Value::Float(f as f64),
        ValueRef::Double(f) => Value::Float(f),
        ValueRef::Decimal(d) => Value::Text(d.to_string()),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) | ValueRef::Geometry(b) => Value::Bytes(b.to_vec()),
        ValueRef::Date32(d) => Value::Text(format!("date32:{d}")),
        ValueRef::Time64(unit, t) => Value::Text(format!("time64:{unit:?}:{t}")),
        ValueRef::Timestamp(unit, t) => Value::Text(format!("timestamp:{unit:?}:{t}")),
        ValueRef::Interval { months, days, nanos } => Value::Text(format!("interval:{months}m {days}d {nanos}ns")),
        other => Value::Text(format!("{other:?}")),
    }
}

fn values_to_duck_params(params: &[Value]) -> Vec<duckdb::types::Value> {
    params
        .iter()
        .map(|p| match p {
            Value::Null => duckdb::types::Value::Null,
            Value::Bool(b) => duckdb::types::Value::Boolean(*b),
            Value::Int(i) => duckdb::types::Value::BigInt(*i),
            Value::Float(f) => duckdb::types::Value::Double(*f),
            Value::Text(s) => duckdb::types::Value::Text(s.clone()),
            Value::Bytes(b) => duckdb::types::Value::Blob(b.clone()),
            Value::Date(d) => duckdb::types::Value::Text(d.to_string()),
            Value::Time(t) => duckdb::types::Value::Text(t.to_string()),
            Value::DateTime(dt) => duckdb::types::Value::Text(dt.to_string()),
            Value::TimestampTz(ts) => duckdb::types::Value::Text(ts.to_rfc3339()),
            Value::Decimal(d) => duckdb::types::Value::Text(d.to_string()),
            Value::Uuid(u) => duckdb::types::Value::Text(u.to_string()),
            Value::Json(j) => duckdb::types::Value::Text(j.to_string()),
        })
        .collect()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qualified(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(s), quote_ident(table)),
        _ => quote_ident(table),
    }
}

fn escape_literal(s: &str) -> String {
    s.replace('\'', "''")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DriverError> + Send + 'static,
) -> Result<T, DriverError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DriverError::Internal(format!("duckdb task join: {e}")))?
}

fn map_duck_error(err: duckdb::Error) -> DriverError {
    DriverError::Query {
        message: err.to_string(),
        sqlstate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_metadata_is_not_declared_without_a_fetch() {
        let d = DuckdbDriver;
        let source = include_str!("lib.rs");
        assert!(!d.supports_index_metadata());
        assert!(!d.supports_foreign_key_metadata());
        assert!(!source.contains(&["async fn ", "fetch_indexes("].concat()));
        assert!(!source.contains(&["async fn ", "fetch_foreign_keys("].concat()));
    }
    use tempfile::TempDir;

    #[test]
    fn driver_metadata() {
        let d = DuckdbDriver;
        assert_eq!(d.id(), "duckdb");
        assert_eq!(d.display_name(), "DuckDB");
        assert!(d.is_file_based());
        assert_eq!(d.default_port(), 0);
    }

    #[test]
    fn quote_ident_doubles_quotes() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[tokio::test]
    async fn connect_create_and_list_tables() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.duckdb");
        let driver = DuckdbDriver;
        let conn = driver
            .connect(ConnectOptions {
                database: path.to_string_lossy().into(),
                ..Default::default()
            })
            .await
            .unwrap();
        conn.execute("CREATE TABLE foo (id INTEGER, name VARCHAR)")
            .await
            .unwrap();
        conn.execute("INSERT INTO foo VALUES (1, 'a'), (2, 'b')").await.unwrap();
        let tables = conn.list_tables().await.unwrap();
        assert!(tables.iter().any(|t| t.name == "foo"));
        let cols = conn.fetch_columns(None, "foo").await.unwrap();
        assert_eq!(cols.len(), 2);
        let rows = conn.fetch_rows(None, "foo", 0, 10).await.unwrap();
        assert_eq!(rows.rows.len(), 2);
    }
}
