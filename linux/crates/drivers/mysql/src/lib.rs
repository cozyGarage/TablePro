use std::time::Duration;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use sqlx::mysql::{MySql, MySqlConnectOptions, MySqlPoolOptions, MySqlRow};
use sqlx::pool::PoolConnection;
use sqlx::{Column, Connection as SqlxConnection, Pool, Row, TypeInfo};

use futures::stream::StreamExt;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, ExecResult, ForeignKeyInfo, IndexInfo,
    MAX_QUERY_ROWS, OperationControl, QueryResult, TableInfo, Transport, Value, check_pre_dispatch,
    run_controlled_setup, run_server_cancellable,
};

/// Name of the SSH-forwarded rendezvous socket inside the private directory
/// `SshTunnel::open_chain_socket` creates. Not a real mysqld socket; just a
/// name shared between `forwarded_socket_name` (which tells the transport
/// layer to forward one) and `connect` (which dials it).
const FORWARDED_SOCKET_NAME: &str = "mysqld.sock";

pub struct MysqlDriver;

#[async_trait]
impl DatabaseDriver for MysqlDriver {
    fn id(&self) -> &'static str {
        "mysql"
    }

    fn display_name(&self) -> &'static str {
        "MySQL"
    }

    fn default_port(&self) -> u16 {
        3306
    }

    fn supports_index_metadata(&self) -> bool {
        true
    }

    fn supports_foreign_key_metadata(&self) -> bool {
        true
    }

    fn forwarded_socket_name(&self, _service_port: u16) -> Option<String> {
        Some(FORWARDED_SOCKET_NAME.to_string())
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        let mysql_opts = mysql_connect_options(&opts);
        let cancellation_options = mysql_opts.clone();
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(mysql_opts)
            .await
            .map_err(map_sqlx_error)?;
        let cancellation_pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(cancellation_options)
            .await
            .map_err(map_sqlx_error)?;
        Ok(Box::new(MysqlConnection {
            pool,
            cancellation_pool,
        }))
    }
}

struct MysqlConnection {
    pool: Pool<MySql>,
    cancellation_pool: Pool<MySql>,
}

#[async_trait]
impl Connection for MysqlConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let rows = sqlx::query(
            "SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR)
             FROM information_schema.tables
             WHERE table_schema = DATABASE()
             ORDER BY table_name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|r| TableInfo {
                schema: Some(r.get::<String, _>(0)),
                name: r.get::<String, _>(1),
            })
            .collect())
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        // `column_type` is canonical: it carries the precision /
        // length the user typed (`tinyint(1)`, `varchar(255)`,
        // `decimal(10,2)`, `enum('a','b')`). `data_type` strips all
        // that — returns `tinyint` for both `tinyint(1)` and
        // `tinyint(4)`, which collapses MySQL's idiomatic boolean
        // type into a generic int and breaks the bool-detection
        // heuristic in `classify_type`. Prefer column_type for the
        // displayed `data_type`.
        let rows = sqlx::query(
            "SELECT CAST(column_name AS CHAR), CAST(column_type AS CHAR),
                    CAST(is_nullable AS CHAR), CAST(column_key AS CHAR),
                    CAST(extra AS CHAR), CAST(column_default AS CHAR),
                    CAST(generation_expression AS CHAR)
             FROM information_schema.columns
             WHERE table_schema = COALESCE(?, DATABASE()) AND table_name = ?
             ORDER BY ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let extra = r.try_get::<String, _>(4).unwrap_or_default().to_ascii_lowercase();
                // information_schema.column_default uses NULL for "no
                // default", but some sqlx + MySQL combinations surface
                // it as an empty string. Treat empty as absent so the
                // build_insert_from_draft "omit when default present"
                // heuristic doesn't trigger on phantom defaults.
                let default_value: Option<String> = r
                    .try_get::<Option<String>, _>(5)
                    .unwrap_or(None)
                    .filter(|s| !s.is_empty());
                let generation_expr: Option<String> = r.try_get::<Option<String>, _>(6).unwrap_or(None);
                ColumnInfo {
                    name: r.get::<String, _>(0),
                    data_type: r.get::<String, _>(1),
                    nullable: r.get::<String, _>(2) == "YES",
                    primary_key: r.get::<String, _>(3) == "PRI",
                    is_auto_increment: extra.contains("auto_increment"),
                    default_value,
                    // Two false-positives to guard against:
                    //   1. MySQL 8.0.13+ marks expression-default columns
                    //      (e.g. DEFAULT CURRENT_TIMESTAMP) with extra =
                    //      "DEFAULT_GENERATED" — contains "generated" but
                    //      not a generated column. Match the explicit
                    //      keywords instead.
                    //   2. information_schema.generation_expression returns
                    //      an *empty string* for non-generated columns,
                    //      not NULL — so `generation_expr.is_some()` is
                    //      true even for plain columns. Check non-empty.
                    is_generated: generation_expr.as_deref().is_some_and(|s| !s.is_empty())
                        || extra.contains("virtual generated")
                        || extra.contains("stored generated"),
                }
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
            qualified(schema, table)
        );
        stream_into_result(&self.pool, &sql, limit as usize).await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        stream_into_result(&self.pool, sql, MAX_QUERY_ROWS).await
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        params_into_result(&self.pool, sql, params, MAX_QUERY_ROWS).await
    }

    async fn query_controlled(&self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        self.query_params_controlled(sql, &[], control).await
    }

    async fn query_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let connection_id = connection_id_controlled(&mut connection, control).await?;
        let result = run_server_cancellable(
            params_into_result(&mut *connection, sql, params, MAX_QUERY_ROWS),
            request_cancellation(&self.cancellation_pool, connection_id),
            confirms_cancellation,
            control,
        )
        .await;
        release_connection(connection, &result).await;
        result
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let res = sqlx::query(sql).execute(&self.pool).await.map_err(map_sqlx_error)?;
        Ok(ExecResult {
            rows_affected: res.rows_affected(),
        })
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let q = bind_mysql_params(sqlx::query(sql), params);
        let res = q.execute(&self.pool).await.map_err(map_sqlx_error)?;
        Ok(ExecResult {
            rows_affected: res.rows_affected(),
        })
    }

    async fn execute_controlled(&self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        self.execute_params_controlled(sql, &[], control).await
    }

    async fn execute_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let connection_id = connection_id_controlled(&mut connection, control).await?;
        let result = run_server_cancellable(
            execute_on(&mut *connection, sql, params),
            request_cancellation(&self.cancellation_pool, connection_id),
            confirms_cancellation,
            control,
        )
        .await;
        release_connection(connection, &result).await;
        result
    }

    fn supports_server_cancellation(&self) -> bool {
        true
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let mut affected = Vec::with_capacity(statements.len());
        for (idx, (sql, params)) in statements.iter().enumerate() {
            let q = bind_mysql_params(sqlx::query(sql), params);
            match q.execute(&mut *tx).await {
                Ok(res) => affected.push(res.rows_affected()),
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(DriverError::Transaction {
                        statement_index: idx,
                        source: Box::new(map_sqlx_error(e)),
                    });
                }
            }
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(affected)
    }

    async fn fetch_indexes(&self, schema: Option<&str>, table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        // information_schema.statistics returns one row per (index,
        // column). Group rows by index_name in Rust because sqlx can't
        // GROUP_CONCAT-then-split natively for ordered column lists.
        // PRIMARY is the literal index name MySQL uses for the PK.
        let rows = sqlx::query(
            "SELECT
                CAST(index_name AS CHAR) AS index_name,
                non_unique,
                CAST(column_name AS CHAR) AS column_name
            FROM information_schema.statistics
            WHERE table_schema = COALESCE(?, DATABASE())
              AND table_name = ?
            ORDER BY index_name, seq_in_index",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        let mut by_name: std::collections::BTreeMap<String, IndexInfo> = std::collections::BTreeMap::new();
        for r in rows {
            let name: String = r.get(0);
            let non_unique: i64 = r.try_get(1).unwrap_or(0);
            let column: String = r.get(2);
            let entry = by_name.entry(name.clone()).or_insert_with(|| IndexInfo {
                name: name.clone(),
                columns: Vec::new(),
                unique: non_unique == 0,
                primary: name == "PRIMARY",
            });
            entry.columns.push(column);
        }
        Ok(by_name.into_values().collect())
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        // key_column_usage gives us the FK column ↔ referenced column
        // pairs (one row per (constraint, ordinal)); referential_constraints
        // adds the ON DELETE / ON UPDATE rules. Group by constraint_name
        // in Rust to assemble the column lists.
        let rows = sqlx::query(
            "SELECT
                CAST(kcu.constraint_name AS CHAR),
                CAST(kcu.column_name AS CHAR),
                CAST(kcu.referenced_table_name AS CHAR),
                CAST(kcu.referenced_table_schema AS CHAR),
                CAST(kcu.referenced_column_name AS CHAR),
                CAST(rc.delete_rule AS CHAR),
                CAST(rc.update_rule AS CHAR)
            FROM information_schema.key_column_usage kcu
            JOIN information_schema.referential_constraints rc
                ON rc.constraint_name = kcu.constraint_name
                AND rc.constraint_schema = kcu.constraint_schema
            WHERE kcu.table_schema = COALESCE(?, DATABASE())
              AND kcu.table_name = ?
              AND kcu.referenced_table_name IS NOT NULL
            ORDER BY kcu.constraint_name, kcu.ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        let mut by_name: std::collections::BTreeMap<String, ForeignKeyInfo> = std::collections::BTreeMap::new();
        for r in rows {
            let name: String = r.get(0);
            let column: String = r.get(1);
            let ref_table: String = r.get(2);
            let ref_schema: Option<String> = r.try_get(3).unwrap_or(None);
            let ref_column: String = r.get(4);
            let delete_rule: Option<String> = r.try_get(5).ok();
            let update_rule: Option<String> = r.try_get(6).ok();
            let entry = by_name.entry(name.clone()).or_insert_with(|| ForeignKeyInfo {
                name: name.clone(),
                columns: Vec::new(),
                ref_schema,
                ref_table,
                ref_columns: Vec::new(),
                on_delete: delete_rule.filter(|s| !s.is_empty() && s != "NO ACTION"),
                on_update: update_rule.filter(|s| !s.is_empty() && s != "NO ACTION"),
            });
            entry.columns.push(column);
            entry.ref_columns.push(ref_column);
        }
        Ok(by_name.into_values().collect())
    }

    async fn ping(&self) -> Result<(), DriverError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn begin(&self) -> Result<Box<dyn tablepro_core::Transaction>, DriverError> {
        let tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        Ok(Box::new(MysqlTransaction { tx: Some(tx) }))
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let row = sqlx::query("SELECT VERSION()")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        let v: String = row.try_get(0).unwrap_or_default();
        Ok(Some(v))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        self.pool.close().await;
        Ok(())
    }
}

struct MysqlTransaction {
    tx: Option<sqlx::Transaction<'static, MySql>>,
}

#[async_trait]
impl tablepro_core::Transaction for MysqlTransaction {
    async fn query(&mut self, sql: &str) -> Result<QueryResult, DriverError> {
        let tx = self
            .tx
            .as_mut()
            .ok_or_else(|| DriverError::Internal("transaction closed".into()))?;
        let rows = sqlx::query(sql).fetch_all(&mut **tx).await.map_err(map_sqlx_error)?;
        if rows.is_empty() {
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                truncated: false,
            });
        }
        let columns: Vec<ColumnInfo> = rows[0]
            .columns()
            .iter()
            .map(|c| ColumnInfo {
                name: c.name().to_string(),
                data_type: c.type_info().name().to_string(),
                nullable: true,
                primary_key: false,
                is_auto_increment: false,
                default_value: None,
                is_generated: false,
            })
            .collect();
        let data: Vec<Vec<Value>> = rows
            .iter()
            .map(|r| (0..columns.len()).map(|i| extract_value(r, i)).collect())
            .collect();
        Ok(QueryResult {
            columns,
            rows: data,
            truncated: false,
        })
    }

    async fn execute(&mut self, sql: &str) -> Result<ExecResult, DriverError> {
        let tx = self
            .tx
            .as_mut()
            .ok_or_else(|| DriverError::Internal("transaction closed".into()))?;
        let res = sqlx::query(sql).execute(&mut **tx).await.map_err(map_sqlx_error)?;
        Ok(ExecResult {
            rows_affected: res.rows_affected(),
        })
    }

    async fn commit(mut self: Box<Self>) -> Result<(), DriverError> {
        let tx = self
            .tx
            .take()
            .ok_or_else(|| DriverError::Internal("transaction closed".into()))?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), DriverError> {
        let tx = self
            .tx
            .take()
            .ok_or_else(|| DriverError::Internal("transaction closed".into()))?;
        tx.rollback().await.map_err(map_sqlx_error)
    }
}

async fn stream_into_result<'e, E>(executor: E, sql: &str, limit: usize) -> Result<QueryResult, DriverError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let mut stream = sqlx::query(sql).fetch(executor);
    let mut collected: Vec<MySqlRow> = Vec::new();
    let mut truncated = false;
    while let Some(row_result) = stream.next().await {
        let row = row_result.map_err(map_sqlx_error)?;
        if collected.len() >= limit {
            truncated = true;
            break;
        }
        collected.push(row);
    }
    Ok(rows_into_result(&collected, truncated))
}

fn rows_into_result(collected: &[MySqlRow], truncated: bool) -> QueryResult {
    let Some(first) = collected.first() else {
        return QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated,
        };
    };
    let columns: Vec<ColumnInfo> = first
        .columns()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            data_type: c.type_info().name().to_string(),
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        })
        .collect();
    let rows: Vec<Vec<Value>> = collected
        .iter()
        .map(|r| (0..columns.len()).map(|i| extract_value(r, i)).collect())
        .collect();
    QueryResult {
        columns,
        rows,
        truncated,
    }
}

fn extract_value(row: &MySqlRow, idx: usize) -> Value {
    let type_name = row.columns()[idx].type_info().name().to_ascii_uppercase();
    match type_name.as_str() {
        "TINYINT" | "SMALLINT" | "INT" | "MEDIUMINT" | "BIGINT" => {
            row.try_get::<i64, _>(idx).map(Value::Int).unwrap_or(Value::Null)
        }
        "FLOAT" | "DOUBLE" => row.try_get::<f64, _>(idx).map(Value::Float).unwrap_or(Value::Null),
        "DECIMAL" | "NUMERIC" => row
            .try_get::<rust_decimal::Decimal, _>(idx)
            .map(Value::Decimal)
            .unwrap_or(Value::Null),
        "BOOLEAN" => row.try_get::<bool, _>(idx).map(Value::Bool).unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .map(Value::Date)
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .map(Value::Time)
            .unwrap_or(Value::Null),
        "DATETIME" => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .map(Value::DateTime)
            .unwrap_or(Value::Null),
        "TIMESTAMP" => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
            .map(Value::TimestampTz)
            .unwrap_or(Value::Null),
        "JSON" => row
            .try_get::<serde_json::Value, _>(idx)
            .map(Value::Json)
            .unwrap_or(Value::Null),
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "VARBINARY" | "BINARY" => {
            row.try_get::<Vec<u8>, _>(idx).map(Value::Bytes).unwrap_or(Value::Null)
        }
        _ => row.try_get::<String, _>(idx).map(Value::Text).unwrap_or(Value::Null),
    }
}

async fn params_into_result<'e, E>(
    executor: E,
    sql: &str,
    params: &[Value],
    limit: usize,
) -> Result<QueryResult, DriverError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    if params.is_empty() {
        return stream_into_result(executor, sql, limit).await;
    }
    let query = bind_mysql_params(sqlx::query(sql), params);
    let mut stream = query.fetch(executor);
    let mut collected: Vec<MySqlRow> = Vec::new();
    let mut truncated = false;
    while let Some(row_result) = stream.next().await {
        let row = row_result.map_err(map_sqlx_error)?;
        if collected.len() >= limit {
            truncated = true;
            break;
        }
        collected.push(row);
    }
    Ok(rows_into_result(&collected, truncated))
}

async fn execute_on<'e, E>(executor: E, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError>
where
    E: sqlx::Executor<'e, Database = MySql>,
{
    let query = bind_mysql_params(sqlx::query(sql), params);
    let result = query.execute(executor).await.map_err(map_sqlx_error)?;
    Ok(ExecResult {
        rows_affected: result.rows_affected(),
    })
}

async fn acquire_controlled(
    pool: &Pool<MySql>,
    control: &OperationControl,
) -> Result<PoolConnection<MySql>, DriverError> {
    check_pre_dispatch(control)?;
    run_controlled_setup(pool.acquire(), control)
        .await?
        .map_err(map_sqlx_error)
}

async fn connection_id_controlled(
    connection: &mut PoolConnection<MySql>,
    control: &OperationControl,
) -> Result<u64, DriverError> {
    run_controlled_setup(connection_id(connection), control).await?
}

async fn connection_id(connection: &mut PoolConnection<MySql>) -> Result<u64, DriverError> {
    sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_sqlx_error)
}

/// MySQL has no request-scoped cancel channel, so the statement is
/// stopped by `KILL QUERY` from a second session. That aborts the
/// running statement and leaves the session alive, which is what makes
/// the connection reusable afterwards.
async fn request_cancellation(pool: &Pool<MySql>, connection_id: u64) -> Result<(), DriverError> {
    sqlx::query(&format!("KILL QUERY {connection_id}"))
        .execute(pool)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

/// `ER_QUERY_INTERRUPTED` is the only outcome that proves the server
/// aborted the statement rather than the client giving up on it.
fn confirms_cancellation(error: &DriverError) -> bool {
    matches!(
        error,
        DriverError::Query {
            sqlstate: Some(sqlstate),
            ..
        } if sqlstate == "70100"
    )
}

async fn release_connection<T>(connection: PoolConnection<MySql>, result: &Result<T, DriverError>) {
    if matches!(result, Err(DriverError::OperationOutcomeUnknown { .. })) {
        let _ = connection.detach().close_hard().await;
    }
}

fn bind_mysql_params<'q>(
    mut q: sqlx::query::Query<'q, MySql, sqlx::mysql::MySqlArguments>,
    params: &'q [Value],
) -> sqlx::query::Query<'q, MySql, sqlx::mysql::MySqlArguments> {
    for p in params {
        q = match p {
            Value::Null => q.bind(Option::<&str>::None),
            Value::Bool(b) => q.bind(*b),
            Value::Int(i) => q.bind(*i),
            Value::Float(f) => q.bind(*f),
            Value::Text(s) => q.bind(s.clone()),
            Value::Bytes(b) => q.bind(b.clone()),
            Value::Date(d) => q.bind(*d),
            Value::Time(t) => q.bind(*t),
            Value::DateTime(dt) => q.bind(*dt),
            Value::TimestampTz(ts) => q.bind(*ts),
            Value::Decimal(d) => q.bind(*d),
            Value::Uuid(u) => q.bind(u.to_string()),
            Value::Json(j) => q.bind(j.clone()),
        };
    }
    q
}

/// Dial through the SSH-forwarded socket when there is one, keeping the real
/// service hostname as the TLS identity sqlx verifies against; otherwise
/// dial and verify the same TCP host, exactly as an untunnelled connection
/// already did.
fn mysql_connect_options(opts: &ConnectOptions) -> MySqlConnectOptions {
    use tablepro_core::TlsMode;
    let mut mysql_opts = match opts.transport() {
        Transport::Tcp { host, port } => MySqlConnectOptions::new().host(host).port(port),
        Transport::Socket {
            directory,
            identity_host,
            identity_port,
            ..
        } => MySqlConnectOptions::new()
            .host(identity_host)
            .port(identity_port)
            .socket(directory.join(FORWARDED_SOCKET_NAME)),
    };
    mysql_opts = mysql_opts
        .database(&opts.database)
        .username(&opts.username)
        .password(opts.password.expose_secret())
        .ssl_mode(match opts.tls.mode {
            TlsMode::Disabled => sqlx::mysql::MySqlSslMode::Disabled,
            TlsMode::Prefer => sqlx::mysql::MySqlSslMode::Preferred,
            TlsMode::Require => sqlx::mysql::MySqlSslMode::Required,
            TlsMode::VerifyCa => sqlx::mysql::MySqlSslMode::VerifyCa,
            TlsMode::VerifyFull => sqlx::mysql::MySqlSslMode::VerifyIdentity,
        });
    if let Some(path) = &opts.tls.root_cert {
        mysql_opts = mysql_opts.ssl_ca(path);
    }
    mysql_opts
}

fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn qualified(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) => format!("{}.{}", quote_ident(s), quote_ident(table)),
        None => quote_ident(table),
    }
}

fn map_sqlx_error(err: sqlx::Error) -> DriverError {
    use sqlx::Error::*;
    match err {
        Database(e) => DriverError::Query {
            message: e.message().to_string(),
            sqlstate: e.code().map(|c| c.to_string()),
        },
        Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => DriverError::ConnectionRefused,
        Tls(e) => DriverError::Tls(e.to_string()),
        PoolClosed | PoolTimedOut => DriverError::Disconnected,
        other => DriverError::Internal(format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_metadata_declarations_match_the_connection_impl() {
        let d = MysqlDriver;
        let source = include_str!("lib.rs");
        assert!(d.supports_index_metadata());
        assert!(d.supports_foreign_key_metadata());
        assert!(source.contains(&["async fn ", "fetch_indexes("].concat()));
        assert!(source.contains(&["async fn ", "fetch_foreign_keys("].concat()));
    }

    #[test]
    fn driver_metadata() {
        let d = MysqlDriver;
        assert_eq!(d.id(), "mysql");
        assert_eq!(d.display_name(), "MySQL");
        assert_eq!(d.default_port(), 3306);
    }

    #[test]
    fn map_io_refused_returns_connection_refused() {
        let err = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(map_sqlx_error(err), DriverError::ConnectionRefused));
    }

    #[test]
    fn quote_ident_doubles_embedded_backticks() {
        assert_eq!(quote_ident("users"), "`users`");
        assert_eq!(quote_ident("My Table"), "`My Table`");
        assert_eq!(quote_ident("evil`; DROP TABLE x; --"), "`evil``; DROP TABLE x; --`");
    }

    #[test]
    fn a_direct_connection_dials_and_verifies_the_same_host() {
        let opts = ConnectOptions {
            host: "db.corp.example".into(),
            port: 3306,
            ..Default::default()
        };
        let mysql_opts = mysql_connect_options(&opts);
        assert_eq!(mysql_opts.get_host(), "db.corp.example");
        assert_eq!(mysql_opts.get_socket(), None);
    }

    /// H1: a tunnelled connection must dial the forwarded local socket but
    /// keep verifying TLS against the real service hostname, not 127.0.0.1
    /// -- exactly the pattern PostgreSQL's driver already uses.
    #[test]
    fn a_tunnelled_connection_dials_the_forwarded_socket_and_verifies_the_service_hostname() {
        let directory = std::path::PathBuf::from("/tmp/tablepro-ssh-abc123");
        let opts = ConnectOptions {
            host: "127.0.0.1".into(),
            port: 54321,
            service_endpoint: Some(("db.corp.example".into(), 3306)),
            forwarded_socket_dir: Some(directory.clone()),
            ..Default::default()
        };
        let mysql_opts = mysql_connect_options(&opts);
        assert_eq!(mysql_opts.get_host(), "db.corp.example");
        assert_eq!(mysql_opts.get_port(), 3306);
        assert_eq!(mysql_opts.get_socket(), Some(&directory.join(FORWARDED_SOCKET_NAME)));
    }
}
