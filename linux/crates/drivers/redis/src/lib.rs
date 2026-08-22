use std::time::Duration;

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client, RedisError, TlsCertificates, Value as RedisValue};
use secrecy::ExposeSecret;
use tokio::sync::Mutex;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, DriverMaturity, ExecResult, MAX_QUERY_ROWS,
    QueryResult, TableInfo, Value,
};

pub struct RedisDriver;

#[async_trait]
impl DatabaseDriver for RedisDriver {
    fn id(&self) -> &'static str {
        "redis"
    }

    fn display_name(&self) -> &'static str {
        "Redis"
    }

    fn maturity(&self) -> DriverMaturity {
        DriverMaturity::Experimental
    }

    fn default_port(&self) -> u16 {
        6379
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        let scheme = match opts.tls.mode {
            tablepro_core::TlsMode::Disabled => "redis",
            _ => "rediss",
        };
        let password = opts.password.expose_secret();
        let auth = if !opts.username.is_empty() && !password.is_empty() {
            format!("{}:{}@", urlencoding_lite(&opts.username), urlencoding_lite(password))
        } else if !password.is_empty() {
            format!(":{}@", urlencoding_lite(password))
        } else {
            String::new()
        };
        let db_index = opts.database.parse::<u8>().unwrap_or(0);
        let verifies = opts.tls.mode.verifies_cert();
        let suffix = if opts.tls.mode.encrypts() && !verifies {
            "#insecure"
        } else {
            ""
        };
        let url = format!("{scheme}://{auth}{}:{}/{}{suffix}", opts.host, opts.port, db_index);
        let client = match root_certificate(&opts.tls, verifies)? {
            Some(root_cert) => Client::build_with_tls(
                url,
                TlsCertificates {
                    client_tls: None,
                    root_cert: Some(root_cert),
                },
            )
            .map_err(map_redis_error)?,
            None => Client::open(url).map_err(map_redis_error)?,
        };
        let manager = match tokio::time::timeout(CONNECT_TIMEOUT, ConnectionManager::new(client)).await {
            Ok(result) => result.map_err(map_redis_error)?,
            Err(_) => return Err(DriverError::ConnectionRefused),
        };
        Ok(Box::new(RedisConnection {
            conn: Mutex::new(manager),
            db_count: 16,
        }))
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Read the certificate authority the connection names. Only a verifying mode
/// consults it, so an encrypt-only session never fails on a path it ignores.
fn root_certificate(config: &tablepro_core::TlsConfig, verifies: bool) -> Result<Option<Vec<u8>>, DriverError> {
    if !verifies {
        return Ok(None);
    }
    let Some(path) = &config.root_cert else {
        return Ok(None);
    };
    std::fs::read(path).map(Some).map_err(|error| {
        DriverError::Tls(format!(
            "cannot read the certificate authority at {}: {error}",
            path.display()
        ))
    })
}

struct RedisConnection {
    conn: Mutex<ConnectionManager>,
    db_count: usize,
}

fn redis_columns() -> Vec<ColumnInfo> {
    vec![
        ColumnInfo {
            name: "Key".into(),
            data_type: "string".into(),
            nullable: false,
            primary_key: true,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        },
        ColumnInfo {
            name: "Type".into(),
            data_type: "string".into(),
            nullable: false,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        },
        ColumnInfo {
            name: "TTL".into(),
            data_type: "integer".into(),
            nullable: false,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        },
        ColumnInfo {
            name: "Value".into(),
            data_type: "string".into(),
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        },
    ]
}

#[async_trait]
impl Connection for RedisConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let mut conn = self.conn.lock().await;
        let count = match redis::cmd("CONFIG")
            .arg("GET")
            .arg("databases")
            .query_async::<Vec<String>>(&mut *conn)
            .await
        {
            Ok(pairs) => pairs
                .get(1)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(self.db_count),
            Err(_) => self.db_count,
        };
        Ok((0..count)
            .map(|i| TableInfo {
                schema: None,
                name: format!("db{i}"),
            })
            .collect())
    }

    async fn fetch_columns(&self, _schema: Option<&str>, _table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(redis_columns())
    }

    async fn fetch_rows(
        &self,
        _schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let db = parse_db_name(table)?;
        let mut conn = self.conn.lock().await;
        redis::cmd("SELECT")
            .arg(db)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(map_redis_error)?;
        let keys = scan_keys(&mut conn, "*", (offset + limit) as usize).await?;
        let skip = offset as usize;
        let page: Vec<String> = keys.into_iter().skip(skip).take(limit as usize).collect();
        let mut rows = Vec::with_capacity(page.len());
        for key in page {
            rows.push(key_row(&mut conn, &key).await?);
        }
        Ok(QueryResult {
            columns: redis_columns(),
            rows,
            truncated: false,
        })
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                truncated: false,
            });
        }
        let mut conn = self.conn.lock().await;
        let args = split_redis_cli(trimmed);
        if args.is_empty() {
            return Err(DriverError::Query {
                message: "empty Redis command".into(),
                sqlstate: None,
            });
        }
        let mut cmd = redis::cmd(&args[0]);
        for arg in &args[1..] {
            cmd.arg(arg);
        }
        let value: RedisValue = cmd.query_async(&mut *conn).await.map_err(map_redis_error)?;
        Ok(redis_value_to_result(value))
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let result = self.query(sql).await?;
        Ok(ExecResult {
            rows_affected: result.rows.len() as u64,
        })
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        if params.is_empty() {
            return self.execute(sql).await;
        }
        Err(DriverError::Unsupported(
            "Redis execute_params does not support bound parameters".into(),
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
        let mut conn = self.conn.lock().await;
        redis::cmd("PING")
            .query_async::<String>(&mut *conn)
            .await
            .map_err(map_redis_error)?;
        Ok(())
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let mut conn = self.conn.lock().await;
        let info: String = redis::cmd("INFO")
            .arg("server")
            .query_async(&mut *conn)
            .await
            .map_err(map_redis_error)?;
        let version = info
            .lines()
            .find_map(|line| line.strip_prefix("redis_version:").map(str::trim).map(str::to_string));
        Ok(version.map(|v| format!("Redis {v}")))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

async fn scan_keys(conn: &mut ConnectionManager, pattern: &str, limit: usize) -> Result<Vec<String>, DriverError> {
    let mut cursor: u64 = 0;
    let mut keys = Vec::new();
    loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(100)
            .query_async(conn)
            .await
            .map_err(map_redis_error)?;
        keys.extend(batch);
        cursor = next;
        if cursor == 0 || keys.len() >= limit {
            break;
        }
    }
    keys.truncate(limit);
    Ok(keys)
}

async fn key_row(conn: &mut ConnectionManager, key: &str) -> Result<Vec<Value>, DriverError> {
    let key_type: String = conn.key_type(key).await.map_err(map_redis_error)?;
    let ttl: i64 = conn.ttl(key).await.map_err(map_redis_error)?;
    let preview = match key_type.as_str() {
        "string" => {
            let v: String = conn.get(key).await.map_err(map_redis_error)?;
            v
        }
        "hash" => {
            let v: Vec<(String, String)> = conn.hgetall(key).await.map_err(map_redis_error)?;
            format!("{v:?}")
        }
        "list" => {
            let v: Vec<String> = conn.lrange(key, 0, 20).await.map_err(map_redis_error)?;
            format!("{v:?}")
        }
        "set" => {
            let v: Vec<String> = conn.smembers(key).await.map_err(map_redis_error)?;
            format!("{v:?}")
        }
        "zset" => {
            let v: Vec<String> = conn.zrange(key, 0, 20).await.map_err(map_redis_error)?;
            format!("{v:?}")
        }
        other => format!("<{other}>"),
    };
    Ok(vec![
        Value::Text(key.to_string()),
        Value::Text(key_type),
        Value::Int(ttl),
        Value::Text(preview),
    ])
}

fn redis_value_to_result(value: RedisValue) -> QueryResult {
    match value {
        RedisValue::Nil => QueryResult {
            columns: vec![text_col("result")],
            rows: vec![vec![Value::Null]],
            truncated: false,
        },
        RedisValue::Okay => QueryResult {
            columns: vec![text_col("result")],
            rows: vec![vec![Value::Text("OK".into())]],
            truncated: false,
        },
        RedisValue::SimpleString(s) => QueryResult {
            columns: vec![text_col("result")],
            rows: vec![vec![Value::Text(s)]],
            truncated: false,
        },
        RedisValue::Int(i) => QueryResult {
            columns: vec![ColumnInfo {
                name: "result".into(),
                data_type: "integer".into(),
                nullable: false,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            }],
            rows: vec![vec![Value::Int(i)]],
            truncated: false,
        },
        RedisValue::Double(f) => QueryResult {
            columns: vec![ColumnInfo {
                name: "result".into(),
                data_type: "double".into(),
                nullable: false,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            }],
            rows: vec![vec![Value::Float(f)]],
            truncated: false,
        },
        RedisValue::Boolean(b) => QueryResult {
            columns: vec![ColumnInfo {
                name: "result".into(),
                data_type: "boolean".into(),
                nullable: false,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            }],
            rows: vec![vec![Value::Bool(b)]],
            truncated: false,
        },
        RedisValue::BulkString(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            QueryResult {
                columns: vec![text_col("result")],
                rows: vec![vec![Value::Text(text)]],
                truncated: false,
            }
        }
        RedisValue::Array(items) | RedisValue::Set(items) => {
            let mut rows = Vec::new();
            let mut truncated = false;
            for item in items {
                if rows.len() >= MAX_QUERY_ROWS {
                    truncated = true;
                    break;
                }
                rows.push(vec![redis_scalar(item)]);
            }
            QueryResult {
                columns: vec![text_col("value")],
                rows,
                truncated,
            }
        }
        RedisValue::Map(pairs) => {
            let rows = pairs
                .into_iter()
                .take(MAX_QUERY_ROWS)
                .map(|(k, v)| vec![redis_scalar(k), redis_scalar(v)])
                .collect();
            QueryResult {
                columns: vec![text_col("key"), text_col("value")],
                rows,
                truncated: false,
            }
        }
        RedisValue::VerbatimString { text, .. } => QueryResult {
            columns: vec![text_col("result")],
            rows: vec![vec![Value::Text(text)]],
            truncated: false,
        },
        other => QueryResult {
            columns: vec![text_col("result")],
            rows: vec![vec![Value::Text(format!("{other:?}"))]],
            truncated: false,
        },
    }
}

fn text_col(name: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: "string".into(),
        nullable: true,
        primary_key: false,
        is_auto_increment: false,
        default_value: None,
        is_generated: false,
    }
}

fn redis_scalar(value: RedisValue) -> Value {
    match value {
        RedisValue::Nil => Value::Null,
        RedisValue::Int(i) => Value::Int(i),
        RedisValue::Double(f) => Value::Float(f),
        RedisValue::Boolean(b) => Value::Bool(b),
        RedisValue::SimpleString(s) => Value::Text(s),
        RedisValue::BulkString(b) => Value::Text(String::from_utf8_lossy(&b).into_owned()),
        RedisValue::Okay => Value::Text("OK".into()),
        RedisValue::VerbatimString { text, .. } => Value::Text(text),
        other => Value::Text(format!("{other:?}")),
    }
}

fn parse_db_name(table: &str) -> Result<u8, DriverError> {
    let stripped = table.strip_prefix("db").unwrap_or(table);
    stripped.parse::<u8>().map_err(|_| DriverError::Query {
        message: format!("invalid Redis database name: {table}"),
        sqlstate: None,
    })
}

/// Minimal Redis CLI tokenizer: splits on whitespace, respects double quotes.
pub fn split_redis_cli(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn map_redis_error(err: RedisError) -> DriverError {
    let msg = err.to_string();
    if msg.contains("Connection refused") || err.is_connection_refusal() {
        DriverError::ConnectionRefused
    } else if msg.contains("NOAUTH") || msg.contains("WRONGPASS") || msg.contains("invalid password") {
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
    fn structure_metadata_is_not_declared_without_a_fetch() {
        let d = RedisDriver;
        let source = include_str!("lib.rs");
        assert!(!d.supports_index_metadata());
        assert!(!d.supports_foreign_key_metadata());
        assert!(!source.contains(&["async fn ", "fetch_indexes("].concat()));
        assert!(!source.contains(&["async fn ", "fetch_foreign_keys("].concat()));
    }

    #[test]
    fn driver_metadata() {
        let d = RedisDriver;
        assert_eq!(d.id(), "redis");
        assert_eq!(d.display_name(), "Redis");
        assert_eq!(d.default_port(), 6379);
    }

    #[test]
    fn split_redis_cli_handles_quotes() {
        assert_eq!(split_redis_cli("GET foo"), vec!["GET", "foo"]);
        assert_eq!(
            split_redis_cli(r#"SET key "hello world""#),
            vec!["SET", "key", "hello world"]
        );
        assert_eq!(split_redis_cli(r#"SET k "a\"b""#), vec!["SET", "k", "a\"b"]);
    }

    #[test]
    fn parse_db_name_accepts_db_n() {
        assert_eq!(parse_db_name("db0").unwrap(), 0);
        assert_eq!(parse_db_name("db15").unwrap(), 15);
        assert!(parse_db_name("users").is_err());
    }
}
