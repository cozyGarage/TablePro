use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use secrecy::ExposeSecret;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DriverError, ExecResult, MAX_QUERY_ROWS, QueryResult, TableInfo, Value,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn connect(opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
    let connect_string = if opts.database.contains('=') || opts.database.starts_with('(') {
        opts.database.clone()
    } else {
        format!("{}:{}/{}", opts.host, opts.port, opts.database)
    };
    let username = opts.username.clone();
    let password = opts.password.expose_secret().to_string();
    let connecting = tokio::task::spawn_blocking(move || {
        oracle::Connector::new(&username, &password, &connect_string)
            .connect()
            .map_err(map_oracle_error)
    });
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(joined) => joined.map_err(|e| DriverError::Internal(format!("oracle connect join: {e}")))??,
        Err(_) => return Err(DriverError::ConnectionRefused),
    };
    Ok(Box::new(OracleConnection {
        conn: Arc::new(Mutex::new(conn)),
    }))
}

struct OracleConnection {
    conn: Arc<Mutex<oracle::Connection>>,
}

#[async_trait]
impl Connection for OracleConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let result = self
            .query(
                "SELECT owner, table_name FROM all_tables \
                 WHERE owner NOT IN ('SYS','SYSTEM','XDB','CTXSYS','MDSYS','OLAPSYS','OUTLN','WMSYS') \
                 ORDER BY owner, table_name",
            )
            .await?;
        Ok(result
            .rows
            .into_iter()
            .filter_map(|row| {
                let name = match row.get(1)? {
                    Value::Text(s) => s.clone(),
                    _ => return None,
                };
                let schema = match row.first()? {
                    Value::Text(s) => Some(s.clone()),
                    _ => None,
                };
                Some(TableInfo { schema, name })
            })
            .collect())
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let owner = schema.unwrap_or("USER").to_uppercase();
        let table = table.to_uppercase();
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("oracle lock poisoned".into()))?;
            let rows = if owner == "USER" {
                guard.query_as::<(String, String, String, Option<String>)>(
                    "SELECT column_name, data_type, nullable, data_default \
                     FROM user_tab_columns WHERE table_name = :1 ORDER BY column_id",
                    &[&table],
                )
            } else {
                guard.query_as::<(String, String, String, Option<String>)>(
                    "SELECT column_name, data_type, nullable, data_default \
                     FROM all_tab_columns WHERE owner = :1 AND table_name = :2 ORDER BY column_id",
                    &[&owner, &table],
                )
            }
            .map_err(map_oracle_error)?;
            let mut out = Vec::new();
            for row in rows {
                let (name, data_type, nullable, default_value) = row.map_err(map_oracle_error)?;
                out.push(ColumnInfo {
                    name,
                    data_type,
                    nullable: nullable == "Y",
                    primary_key: false,
                    is_auto_increment: false,
                    default_value,
                    is_generated: false,
                });
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
        let qualified = match schema {
            Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(s), quote_ident(table)),
            _ => quote_ident(table),
        };
        let sql = format!("SELECT * FROM {qualified} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY");
        self.query(&sql).await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("oracle lock poisoned".into()))?;
            run_query(&guard, &sql)
        })
        .await
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let conn = Arc::clone(&self.conn);
        let sql = sql.to_string();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("oracle lock poisoned".into()))?;
            let rows_affected = guard.execute(&sql, &[]).map_err(map_oracle_error)?.row_count() as u64;
            guard.commit().map_err(map_oracle_error)?;
            Ok(ExecResult { rows_affected })
        })
        .await
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        if params.is_empty() {
            return self.execute(sql).await;
        }
        Err(DriverError::Unsupported(
            "Oracle execute_params binding is not implemented in this MVP".into(),
        ))
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let conn = Arc::clone(&self.conn);
        let statements = statements.to_vec();
        blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| DriverError::Internal("oracle lock poisoned".into()))?;
            let mut affected = Vec::with_capacity(statements.len());
            for (idx, (sql, params)) in statements.iter().enumerate() {
                if !params.is_empty() {
                    let _ = guard.rollback();
                    return Err(DriverError::Transaction {
                        statement_index: idx,
                        source: Box::new(DriverError::Unsupported(
                            "Oracle execute_in_transaction does not support bound parameters yet".into(),
                        )),
                    });
                }
                match guard.execute(sql, &[]) {
                    Ok(stmt) => affected.push(stmt.row_count() as u64),
                    Err(e) => {
                        let _ = guard.rollback();
                        return Err(DriverError::Transaction {
                            statement_index: idx,
                            source: Box::new(map_oracle_error(e)),
                        });
                    }
                }
            }
            guard.commit().map_err(map_oracle_error)?;
            Ok(affected)
        })
        .await
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.query("SELECT 1 FROM dual").await.map(|_| ())
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let result = self
            .query("SELECT banner FROM v$version WHERE banner LIKE 'Oracle%'")
            .await?;
        Ok(result.rows.first().and_then(|r| r.first()).and_then(|v| match v {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        }))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

fn run_query(conn: &oracle::Connection, sql: &str) -> Result<QueryResult, DriverError> {
    let rows = conn.query(sql, &[]).map_err(map_oracle_error)?;
    let column_info = rows.column_info().to_vec();
    let columns: Vec<ColumnInfo> = column_info
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            data_type: format!("{:?}", c.oracle_type()),
            nullable: c.nullable(),
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        })
        .collect();
    let mut data = Vec::new();
    let mut truncated = false;
    for row_result in rows {
        if data.len() >= MAX_QUERY_ROWS {
            truncated = true;
            break;
        }
        let row = row_result.map_err(map_oracle_error)?;
        let mut values = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            let sql_value: oracle::SqlValue = row.get(i).map_err(map_oracle_error)?;
            values.push(oracle_sql_value_to_value(&sql_value));
        }
        data.push(values);
    }
    Ok(QueryResult {
        columns,
        rows: data,
        truncated,
    })
}

fn oracle_sql_value_to_value(v: &oracle::SqlValue) -> Value {
    if v.is_null().unwrap_or(true) {
        return Value::Null;
    }
    if let Ok(i) = v.get::<i64>() {
        return Value::Int(i);
    }
    if let Ok(f) = v.get::<f64>() {
        return Value::Float(f);
    }
    if let Ok(s) = v.get::<String>() {
        return Value::Text(s);
    }
    Value::Text(format!("{v}"))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DriverError> + Send + 'static,
) -> Result<T, DriverError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DriverError::Internal(format!("oracle task join: {e}")))?
}

fn map_oracle_error(err: oracle::Error) -> DriverError {
    let msg = err.to_string();
    if msg.contains("ORA-01017") || msg.contains("invalid username/password") {
        DriverError::AuthFailed
    } else if msg.contains("could not connect") || msg.contains("DPI-1047") {
        DriverError::ConnectionRefused
    } else {
        DriverError::Query {
            message: msg,
            sqlstate: None,
        }
    }
}
