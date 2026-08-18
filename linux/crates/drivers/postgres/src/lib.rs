use std::time::Duration;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{Column, Connection as SqlxConnection, Pool, Postgres, Row, TypeInfo};

use futures::stream::StreamExt;

use tablepro_core::{
    ColumnInfo, ConnectOptions, Connection, DatabaseDriver, DriverError, ExecResult, ForeignKeyInfo, IndexInfo,
    MAX_QUERY_ROWS, OperationControl, QueryResult, TableInfo, Transport, Value,
};

pub struct PgDriver;

#[async_trait]
impl DatabaseDriver for PgDriver {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn default_port(&self) -> u16 {
        5432
    }

    fn ddl_is_transactional(&self) -> bool {
        true
    }

    fn forwarded_socket_name(&self, service_port: u16) -> Option<String> {
        Some(format!(".s.PGSQL.{service_port}"))
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        use tablepro_core::TlsMode;
        let mut pg_opts = match opts.transport() {
            Transport::Tcp { host, port } => PgConnectOptions::new().host(host).port(port),
            Transport::Socket {
                directory,
                identity_host,
                identity_port,
            } => PgConnectOptions::new()
                .host(identity_host)
                .port(identity_port)
                .socket(directory),
        };
        pg_opts = pg_opts
            .database(&opts.database)
            .username(&opts.username)
            .password(opts.password.expose_secret())
            .ssl_mode(match opts.tls.mode {
                TlsMode::Disabled => sqlx::postgres::PgSslMode::Disable,
                TlsMode::Prefer => sqlx::postgres::PgSslMode::Prefer,
                TlsMode::Require => sqlx::postgres::PgSslMode::Require,
                TlsMode::VerifyCa => sqlx::postgres::PgSslMode::VerifyCa,
                TlsMode::VerifyFull => sqlx::postgres::PgSslMode::VerifyFull,
            });
        if let Some(path) = &opts.tls.root_cert {
            pg_opts = pg_opts.ssl_root_cert(path);
        }
        let cancellation_options = pg_opts.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(pg_opts)
            .await
            .map_err(map_sqlx_error)?;
        let cancellation_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(cancellation_options)
            .await
            .map_err(map_sqlx_error)?;
        Ok(Box::new(PgConnection {
            pool,
            cancellation_pool,
        }))
    }
}

struct PgConnection {
    pool: Pool<Postgres>,
    cancellation_pool: Pool<Postgres>,
}

#[derive(Clone, Copy)]
enum Interruption {
    Cancelled,
    TimedOut,
}

const CONTROL_SETUP_TIMEOUT: Duration = Duration::from_secs(2);
const CANCELLATION_GRACE: Duration = Duration::from_secs(5);
const CANCELLATION_DISPATCH_TIMEOUT: Duration = Duration::from_secs(2);

#[async_trait]
impl Connection for PgConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let rows = sqlx::query(
            "SELECT schemaname, tablename
             FROM pg_tables
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY schemaname, tablename",
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
        // Source schema metadata from pg_catalog rather than
        // information_schema:
        //   - pg_attribute.attgenerated ('s' for STORED, '' otherwise)
        //     is the canonical generated-column flag. The
        //     information_schema.is_generated text column is brittle
        //     across PG versions.
        //   - pg_attribute.attidentity ('a' / 'd' for ALWAYS / BY
        //     DEFAULT identity, '' otherwise) authoritatively flags
        //     identity columns.
        //   - format_type() returns the user-facing type name including
        //     length / precision (e.g. "character varying(255)") which
        //     matches what the user wrote in CREATE TABLE.
        //   - pg_get_expr() returns the default expression text.
        let rows = sqlx::query(
            "SELECT
                a.attname,
                pg_catalog.format_type(a.atttypid, a.atttypmod) AS data_type,
                NOT a.attnotnull AS nullable,
                EXISTS (
                    SELECT 1 FROM pg_catalog.pg_constraint c
                    WHERE c.conrelid = a.attrelid
                      AND c.contype = 'p'
                      AND a.attnum = ANY(c.conkey)
                ) AS is_pk,
                pg_catalog.pg_get_expr(d.adbin, d.adrelid) AS default_value,
                a.attidentity <> '' AS is_identity,
                a.attgenerated <> '' AS is_generated
             FROM pg_catalog.pg_attribute a
             JOIN pg_catalog.pg_class t ON a.attrelid = t.oid
             JOIN pg_catalog.pg_namespace n ON t.relnamespace = n.oid
             LEFT JOIN pg_catalog.pg_attrdef d
                 ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE n.nspname = COALESCE($2, current_schema())
               AND t.relname = $1
               AND a.attnum > 0
               AND NOT a.attisdropped
             ORDER BY a.attnum",
        )
        .bind(table)
        .bind(schema)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let raw_default: Option<String> = r.try_get::<Option<String>, _>(4).unwrap_or(None);
                let is_identity = r.try_get::<bool, _>(5).unwrap_or(false);
                let is_generated = r.try_get::<bool, _>(6).unwrap_or(false);
                // SERIAL / BIGSERIAL columns aren't IDENTITY in PG's
                // catalog terms but have a `nextval(...)` default; treat
                // them as auto-increment for the inline-insert UI.
                let is_serial = raw_default
                    .as_deref()
                    .map(|d| d.starts_with("nextval("))
                    .unwrap_or(false);
                // For identity / serial columns the default expression
                // is internal sequence machinery — suppress so the UI
                // doesn't leak implementation details. Otherwise
                // normalise the expression for display.
                let default_value = if is_identity || is_serial {
                    None
                } else {
                    raw_default.map(normalize_pg_default)
                };
                ColumnInfo {
                    name: r.get::<String, _>(0),
                    data_type: r.get::<String, _>(1),
                    nullable: r.get::<bool, _>(2),
                    primary_key: r.get::<bool, _>(3),
                    is_auto_increment: is_identity || is_serial,
                    default_value,
                    is_generated,
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
            "SELECT * FROM {} OFFSET {offset} LIMIT {limit}",
            qualified(schema, table)
        );
        stream_into_result(&self.pool, &sql, limit as usize).await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        stream_into_result(&self.pool, sql, MAX_QUERY_ROWS).await
    }

    async fn query_controlled(&self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let backend_pid = backend_pid_controlled(&mut connection, control).await?;
        let (connection, result) =
            controlled_query(connection, &self.cancellation_pool, backend_pid, sql, &[], control).await;
        drop(connection);
        result
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let query = bind_pg_params(sqlx::query(sql), params);
        let mut stream = query.fetch(&self.pool);
        collect_query_rows(&mut stream, MAX_QUERY_ROWS).await
    }

    async fn query_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let backend_pid = backend_pid_controlled(&mut connection, control).await?;
        let (connection, result) =
            controlled_query(connection, &self.cancellation_pool, backend_pid, sql, params, control).await;
        drop(connection);
        result
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let res = sqlx::query(sql).execute(&self.pool).await.map_err(map_sqlx_error)?;
        Ok(ExecResult {
            rows_affected: res.rows_affected(),
        })
    }

    async fn execute_controlled(&self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let backend_pid = backend_pid_controlled(&mut connection, control).await?;
        let (connection, result) =
            controlled_execute(connection, &self.cancellation_pool, backend_pid, sql, &[], control).await;
        drop(connection);
        result
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let query = bind_pg_params(sqlx::query(sql), params);
        let result = query.execute(&self.pool).await.map_err(map_sqlx_error)?;
        Ok(ExecResult {
            rows_affected: result.rows_affected(),
        })
    }

    async fn execute_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        let mut connection = acquire_controlled(&self.pool, control).await?;
        let backend_pid = backend_pid_controlled(&mut connection, control).await?;
        let (connection, result) =
            controlled_execute(connection, &self.cancellation_pool, backend_pid, sql, params, control).await;
        drop(connection);
        result
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let mut affected = Vec::with_capacity(statements.len());
        for (idx, (sql, params)) in statements.iter().enumerate() {
            let q = bind_pg_params(sqlx::query(sql), params);
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
        // pg_index + pg_class + pg_attribute join. `array_agg ORDER BY
        // ordinality` keeps the column order deterministic; pg_index
        // stores `indkey` as an int2vector positional reference so we
        // unnest with `WITH ORDINALITY` to capture position.
        let rows = sqlx::query(
            "SELECT
                i.relname AS index_name,
                ix.indisunique,
                ix.indisprimary,
                array_agg(a.attname ORDER BY k.ordinality) AS columns
            FROM pg_catalog.pg_class t
            JOIN pg_catalog.pg_namespace n ON t.relnamespace = n.oid
            JOIN pg_catalog.pg_index ix ON ix.indrelid = t.oid
            JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid
            JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ordinality) ON true
            JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
            WHERE n.nspname = COALESCE($2, current_schema())
              AND t.relname = $1
              AND a.attnum > 0
            GROUP BY i.relname, ix.indisunique, ix.indisprimary
            ORDER BY i.relname",
        )
        .bind(table)
        .bind(schema)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|r| IndexInfo {
                name: r.get::<String, _>(0),
                unique: r.get::<bool, _>(1),
                primary: r.get::<bool, _>(2),
                columns: r.get::<Vec<String>, _>(3),
            })
            .collect())
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        // pg_constraint with contype = 'f'. confkey arrays are parallel
        // to conkey via ordinality; the LATERAL join pairs them so the
        // FK column ↔ referenced column mapping survives composite
        // FKs. confdeltype / confupdtype are single chars normalised
        // to canonical SQL keyword strings.
        let rows = sqlx::query(
            "SELECT
                c.conname AS fk_name,
                array_agg(a.attname ORDER BY kf.ordinality) AS columns,
                fn_class.relname AS ref_table,
                fn_ns.nspname AS ref_schema,
                array_agg(fa.attname ORDER BY kf.ordinality) AS ref_columns,
                c.confdeltype::text AS on_delete_code,
                c.confupdtype::text AS on_update_code
            FROM pg_catalog.pg_constraint c
            JOIN pg_catalog.pg_class t ON t.oid = c.conrelid
            JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
            JOIN pg_catalog.pg_class fn_class ON fn_class.oid = c.confrelid
            JOIN pg_catalog.pg_namespace fn_ns ON fn_ns.oid = fn_class.relnamespace
            JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS kf(attnum, ordinality) ON true
            JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = kf.attnum
            JOIN LATERAL unnest(c.confkey) WITH ORDINALITY AS kfr(attnum, ordinality)
                ON kfr.ordinality = kf.ordinality
            JOIN pg_catalog.pg_attribute fa ON fa.attrelid = c.confrelid AND fa.attnum = kfr.attnum
            WHERE c.contype = 'f'
              AND n.nspname = COALESCE($2, current_schema())
              AND t.relname = $1
            GROUP BY c.conname, fn_class.relname, fn_ns.nspname, c.confdeltype, c.confupdtype
            ORDER BY c.conname",
        )
        .bind(table)
        .bind(schema)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows
            .into_iter()
            .map(|r| ForeignKeyInfo {
                name: r.get::<String, _>(0),
                columns: r.get::<Vec<String>, _>(1),
                ref_table: r.get::<String, _>(2),
                ref_schema: r.try_get::<Option<String>, _>(3).unwrap_or(None),
                ref_columns: r.get::<Vec<String>, _>(4),
                on_delete: pg_action_char_to_keyword(r.try_get::<String, _>(5).ok().as_deref().unwrap_or("a")),
                on_update: pg_action_char_to_keyword(r.try_get::<String, _>(6).ok().as_deref().unwrap_or("a")),
            })
            .collect())
    }

    async fn ping(&self) -> Result<(), DriverError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn begin(&self) -> Result<Box<dyn tablepro_core::Transaction>, DriverError> {
        let mut connection = tokio::time::timeout(CONTROL_SETUP_TIMEOUT, self.pool.acquire())
            .await
            .map_err(|_| DriverError::TimedOut)?
            .map_err(map_sqlx_error)?;
        let backend_pid = tokio::time::timeout(CONTROL_SETUP_TIMEOUT, backend_pid(&mut connection))
            .await
            .map_err(|_| DriverError::TimedOut)??;
        sqlx::query("BEGIN")
            .execute(&mut *connection)
            .await
            .map_err(map_sqlx_error)?;
        Ok(Box::new(PgTransaction {
            connection: Some(connection),
            cancellation_pool: self.cancellation_pool.clone(),
            backend_pid,
        }))
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let row = sqlx::query("SHOW server_version")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        let v: String = row.try_get(0).unwrap_or_default();
        Ok(Some(format!("PostgreSQL {v}")))
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        self.pool.close().await;
        self.cancellation_pool.close().await;
        Ok(())
    }
}

struct PgTransaction {
    connection: Option<PoolConnection<Postgres>>,
    cancellation_pool: Pool<Postgres>,
    backend_pid: i32,
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_mut() {
            connection.close_on_drop();
        }
    }
}

#[async_trait]
impl tablepro_core::Transaction for PgTransaction {
    async fn query(&mut self, sql: &str) -> Result<QueryResult, DriverError> {
        let connection = self.connection_mut()?;
        query_connection(connection, sql, &[]).await
    }

    async fn query_controlled(&mut self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        self.query_params_controlled(sql, &[], control).await
    }

    async fn query_params(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let connection = self.connection_mut()?;
        query_connection(connection, sql, params).await
    }

    async fn query_params_controlled(
        &mut self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        check_pre_dispatch(control)?;
        let connection = self.take_connection()?;
        let (connection, result) = controlled_query(
            connection,
            &self.cancellation_pool,
            self.backend_pid,
            sql,
            params,
            control,
        )
        .await;
        self.connection = connection;
        result
    }

    async fn execute(&mut self, sql: &str) -> Result<ExecResult, DriverError> {
        let connection = self.connection_mut()?;
        execute_connection(connection, sql, &[]).await
    }

    async fn execute_controlled(&mut self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        self.execute_params_controlled(sql, &[], control).await
    }

    async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let connection = self.connection_mut()?;
        execute_connection(connection, sql, params).await
    }

    async fn execute_params_controlled(
        &mut self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        check_pre_dispatch(control)?;
        let connection = self.take_connection()?;
        let (connection, result) = controlled_execute(
            connection,
            &self.cancellation_pool,
            self.backend_pid,
            sql,
            params,
            control,
        )
        .await;
        self.connection = connection;
        result
    }

    async fn commit(mut self: Box<Self>) -> Result<(), DriverError> {
        let connection = self.connection_mut()?;
        sqlx::query("COMMIT")
            .execute(&mut **connection)
            .await
            .map_err(map_sqlx_error)?;
        let _ = self.take_connection()?;
        Ok(())
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), DriverError> {
        let connection = self.connection_mut()?;
        sqlx::query("ROLLBACK")
            .execute(&mut **connection)
            .await
            .map_err(map_sqlx_error)?;
        let _ = self.take_connection()?;
        Ok(())
    }
}

impl PgTransaction {
    fn connection_mut(&mut self) -> Result<&mut PoolConnection<Postgres>, DriverError> {
        self.connection
            .as_mut()
            .ok_or_else(|| DriverError::Internal("transaction is unusable".into()))
    }

    fn take_connection(&mut self) -> Result<PoolConnection<Postgres>, DriverError> {
        self.connection
            .take()
            .ok_or_else(|| DriverError::Internal("transaction is unusable".into()))
    }
}

async fn acquire_controlled(
    pool: &Pool<Postgres>,
    control: &OperationControl,
) -> Result<PoolConnection<Postgres>, DriverError> {
    run_controlled_setup(pool.acquire(), control)
        .await?
        .map_err(map_sqlx_error)
}

async fn backend_pid_controlled(
    connection: &mut PoolConnection<Postgres>,
    control: &OperationControl,
) -> Result<i32, DriverError> {
    run_controlled_setup(backend_pid(connection), control).await?
}

async fn run_controlled_setup<T, F>(operation: F, control: &OperationControl) -> Result<T, DriverError>
where
    F: std::future::Future<Output = T>,
{
    check_pre_dispatch(control)?;
    let setup_deadline = control
        .deadline()
        .map(|deadline| deadline.min(tokio::time::Instant::now() + CONTROL_SETUP_TIMEOUT))
        .unwrap_or_else(|| tokio::time::Instant::now() + CONTROL_SETUP_TIMEOUT);
    tokio::select! {
        biased;
        _ = control.cancellation_token().cancelled() => Err(DriverError::Cancelled),
        _ = tokio::time::sleep_until(setup_deadline) => Err(DriverError::TimedOut),
        result = operation => Ok(result),
    }
}

fn check_pre_dispatch(control: &OperationControl) -> Result<(), DriverError> {
    if control.cancellation_token().is_cancelled() {
        return Err(DriverError::Cancelled);
    }
    if control
        .deadline()
        .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
    {
        return Err(DriverError::TimedOut);
    }
    Ok(())
}

async fn backend_pid(connection: &mut PoolConnection<Postgres>) -> Result<i32, DriverError> {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_sqlx_error)
}

async fn controlled_query(
    mut connection: PoolConnection<Postgres>,
    cancellation_pool: &Pool<Postgres>,
    backend_pid: i32,
    sql: &str,
    params: &[Value],
    control: &OperationControl,
) -> (Option<PoolConnection<Postgres>>, Result<QueryResult, DriverError>) {
    if let Err(error) = check_pre_dispatch(control) {
        return (Some(connection), Err(error));
    }
    let mut operation = Box::pin(query_connection(&mut connection, sql, params));
    let interruption = match tokio::select! {
        biased;
        result = &mut operation => Ok(result),
        _ = control.cancellation_token().cancelled() => Err(Interruption::Cancelled),
        _ = wait_for_deadline(control.deadline()) => Err(Interruption::TimedOut),
    } {
        Ok(result) => {
            drop(operation);
            return (Some(connection), result);
        }
        Err(interruption) => interruption,
    };
    let result = finish_interrupted_operation(&mut operation, cancellation_pool, backend_pid, interruption).await;
    drop(operation);
    finish_connection(connection, result).await
}

async fn controlled_execute(
    mut connection: PoolConnection<Postgres>,
    cancellation_pool: &Pool<Postgres>,
    backend_pid: i32,
    sql: &str,
    params: &[Value],
    control: &OperationControl,
) -> (Option<PoolConnection<Postgres>>, Result<ExecResult, DriverError>) {
    if let Err(error) = check_pre_dispatch(control) {
        return (Some(connection), Err(error));
    }
    let mut operation = Box::pin(execute_connection(&mut connection, sql, params));
    let interruption = match tokio::select! {
        biased;
        result = &mut operation => Ok(result),
        _ = control.cancellation_token().cancelled() => Err(Interruption::Cancelled),
        _ = wait_for_deadline(control.deadline()) => Err(Interruption::TimedOut),
    } {
        Ok(result) => {
            drop(operation);
            return (Some(connection), result);
        }
        Err(interruption) => interruption,
    };
    let result = finish_interrupted_operation(&mut operation, cancellation_pool, backend_pid, interruption).await;
    drop(operation);
    finish_connection(connection, result).await
}

async fn finish_interrupted_operation<T, F>(
    operation: &mut std::pin::Pin<Box<F>>,
    cancellation_pool: &Pool<Postgres>,
    backend_pid: i32,
    interruption: Interruption,
) -> Result<T, DriverError>
where
    F: std::future::Future<Output = Result<T, DriverError>>,
{
    let mut dispatch = Box::pin(tokio::time::timeout(
        CANCELLATION_DISPATCH_TIMEOUT,
        request_cancellation(cancellation_pool, backend_pid),
    ));
    tokio::select! {
        result = &mut *operation => return classify_interrupted_result(result, interruption),
        _ = &mut dispatch => {}
    }
    match tokio::time::timeout(CANCELLATION_GRACE, &mut *operation).await {
        Ok(result) => classify_interrupted_result(result, interruption),
        Err(_) => Err(unknown_interruption(interruption)),
    }
}

async fn finish_connection<T>(
    connection: PoolConnection<Postgres>,
    result: Result<T, DriverError>,
) -> (Option<PoolConnection<Postgres>>, Result<T, DriverError>) {
    if is_unconfirmed_interruption(&result) {
        let raw_connection = connection.detach();
        let _ = raw_connection.close_hard().await;
        return (None, result);
    }
    (Some(connection), result)
}

fn is_unconfirmed_interruption<T>(result: &Result<T, DriverError>) -> bool {
    matches!(result, Err(DriverError::OperationOutcomeUnknown { .. }))
}

async fn request_cancellation(pool: &Pool<Postgres>, backend_pid: i32) -> Result<(), DriverError> {
    let cancelled: bool = sqlx::query_scalar("SELECT pg_cancel_backend($1)")
        .bind(backend_pid)
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_error)?;
    if cancelled {
        Ok(())
    } else {
        Err(DriverError::Internal(
            "PostgreSQL rejected the cancellation request".into(),
        ))
    }
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn classify_interrupted_result<T>(
    result: Result<T, DriverError>,
    interruption: Interruption,
) -> Result<T, DriverError> {
    match result {
        Ok(value) => Ok(value),
        Err(DriverError::Query {
            sqlstate: Some(sqlstate),
            ..
        }) if sqlstate == "57014" => Err(interruption_error(interruption)),
        Err(error) => Err(DriverError::OperationOutcomeUnknown {
            source: Box::new(error),
        }),
    }
}

fn interruption_error(interruption: Interruption) -> DriverError {
    match interruption {
        Interruption::Cancelled => DriverError::Cancelled,
        Interruption::TimedOut => DriverError::TimedOut,
    }
}

fn unknown_interruption(interruption: Interruption) -> DriverError {
    DriverError::OperationOutcomeUnknown {
        source: Box::new(interruption_error(interruption)),
    }
}

async fn query_connection(
    connection: &mut PoolConnection<Postgres>,
    sql: &str,
    params: &[Value],
) -> Result<QueryResult, DriverError> {
    let query = bind_pg_params(sqlx::query(sql), params);
    let mut stream = query.fetch(&mut **connection);
    collect_query_rows(&mut stream, MAX_QUERY_ROWS).await
}

async fn execute_connection(
    connection: &mut PoolConnection<Postgres>,
    sql: &str,
    params: &[Value],
) -> Result<ExecResult, DriverError> {
    let query = bind_pg_params(sqlx::query(sql), params);
    let result = query.execute(&mut **connection).await.map_err(map_sqlx_error)?;
    Ok(ExecResult {
        rows_affected: result.rows_affected(),
    })
}

async fn stream_into_result(pool: &Pool<Postgres>, sql: &str, limit: usize) -> Result<QueryResult, DriverError> {
    let mut stream = sqlx::query(sql).fetch(pool);
    collect_query_rows(&mut stream, limit).await
}

async fn collect_query_rows<'a, S>(stream: &mut S, limit: usize) -> Result<QueryResult, DriverError>
where
    S: futures::Stream<Item = Result<PgRow, sqlx::Error>> + Unpin + 'a,
{
    let mut collected = Vec::new();
    let mut truncated = false;
    while let Some(row_result) = stream.next().await {
        let row = row_result.map_err(map_sqlx_error)?;
        if collected.len() >= limit {
            truncated = true;
            break;
        }
        collected.push(row);
    }
    if collected.is_empty() {
        return Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated,
        });
    }
    let columns = collected[0]
        .columns()
        .iter()
        .map(|column| ColumnInfo {
            name: column.name().to_string(),
            data_type: column.type_info().name().to_string(),
            nullable: true,
            primary_key: false,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        })
        .collect::<Vec<_>>();
    let rows = collected
        .iter()
        .map(|row| (0..columns.len()).map(|index| extract_value(row, index)).collect())
        .collect();
    Ok(QueryResult {
        columns,
        rows,
        truncated,
    })
}

fn extract_value(row: &PgRow, idx: usize) -> Value {
    let type_name = row.columns()[idx].type_info().name().to_ascii_uppercase();
    match type_name.as_str() {
        "BOOL" => row.try_get::<bool, _>(idx).map(Value::Bool).unwrap_or(Value::Null),
        "INT2" => row
            .try_get::<i16, _>(idx)
            .map(|v| Value::Int(v as i64))
            .unwrap_or(Value::Null),
        "INT4" => row
            .try_get::<i32, _>(idx)
            .map(|v| Value::Int(v as i64))
            .unwrap_or(Value::Null),
        "INT8" => row.try_get::<i64, _>(idx).map(Value::Int).unwrap_or(Value::Null),
        "FLOAT4" => row
            .try_get::<f32, _>(idx)
            .map(|v| Value::Float(v as f64))
            .unwrap_or(Value::Null),
        "FLOAT8" => row.try_get::<f64, _>(idx).map(Value::Float).unwrap_or(Value::Null),
        "NUMERIC" => row
            .try_get::<rust_decimal::Decimal, _>(idx)
            .map(Value::Decimal)
            .unwrap_or(Value::Null),
        "DATE" => row
            .try_get::<chrono::NaiveDate, _>(idx)
            .map(Value::Date)
            .unwrap_or(Value::Null),
        "TIME" => row
            .try_get::<chrono::NaiveTime, _>(idx)
            .map(Value::Time)
            .unwrap_or(Value::Null),
        "TIMESTAMP" => row
            .try_get::<chrono::NaiveDateTime, _>(idx)
            .map(Value::DateTime)
            .unwrap_or(Value::Null),
        "TIMESTAMPTZ" => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
            .map(Value::TimestampTz)
            .unwrap_or(Value::Null),
        "UUID" => row
            .try_get::<uuid::Uuid, _>(idx)
            .map(Value::Uuid)
            .unwrap_or(Value::Null),
        "JSON" | "JSONB" => row
            .try_get::<serde_json::Value, _>(idx)
            .map(Value::Json)
            .unwrap_or(Value::Null),
        "BYTEA" => row.try_get::<Vec<u8>, _>(idx).map(Value::Bytes).unwrap_or(Value::Null),
        _ => row.try_get::<String, _>(idx).map(Value::Text).unwrap_or(Value::Null),
    }
}

/// Bind a positional parameter list to a sqlx Postgres query in the
/// same order as the `params` slice. Centralised here so
/// `execute_params` and `execute_in_transaction` produce identical
/// bindings without duplicating the variant match.
fn bind_pg_params<'q>(
    mut q: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    params: &'q [Value],
) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
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
            Value::Uuid(u) => q.bind(*u),
            Value::Json(j) => q.bind(j.clone()),
        };
    }
    q
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Normalize the `default_value` text returned by `pg_get_expr`.
/// PG appends an explicit type cast to typed literal defaults
/// (`'hi'::text`, `42::integer`, `'2024-01-01'::date`); strip the
/// trailing `::TYPE` cast for display so the value reads as the user
/// would type it. Then, if the result is a single-quoted string
/// literal, strip the outer quotes (matching the SQLite driver's
/// behaviour) so default values look the same across all engines.
/// Function-call defaults like `now()` and complex expressions are
/// returned unchanged.
fn normalize_pg_default(raw: String) -> String {
    let stripped = strip_pg_type_cast(&raw).unwrap_or(raw.as_str()).to_string();
    strip_outer_single_quotes(&stripped)
}

fn strip_pg_type_cast(raw: &str) -> Option<&str> {
    let idx = raw.rfind("::")?;
    let suffix = &raw[idx + 2..];
    if suffix.is_empty() {
        return None;
    }
    let is_type_name = suffix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '(' || c == ')' || c == ',' || c == '_');
    if is_type_name { Some(&raw[..idx]) } else { None }
}

fn strip_outer_single_quotes(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        // PG escapes embedded apostrophes by doubling, same as SQLite.
        return raw[1..raw.len() - 1].replace("''", "'");
    }
    raw.to_string()
}

fn qualified(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) => format!("{}.{}", quote_ident(s), quote_ident(table)),
        None => quote_ident(table),
    }
}

/// Map `pg_constraint.confdeltype` / `confupdtype` single-char codes
/// to canonical SQL action keywords. Returns `None` for the default
/// "no action" so the FK builder can omit the redundant ON clause.
fn pg_action_char_to_keyword(code: &str) -> Option<String> {
    match code {
        "r" => Some("RESTRICT".into()),
        "c" => Some("CASCADE".into()),
        "n" => Some("SET NULL".into()),
        "d" => Some("SET DEFAULT".into()),
        // 'a' = NO ACTION is the default; surface as None so the
        // generated DDL stays clean.
        _ => None,
    }
}

fn is_certificate_failure(err: &std::io::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = source {
        let text = current.to_string().to_ascii_lowercase();
        if text.contains("certificate") || text.contains("tls handshake") {
            return true;
        }
        source = current.source();
    }
    false
}

fn map_sqlx_error(err: sqlx::Error) -> DriverError {
    use sqlx::Error::*;
    match err {
        Database(e) => DriverError::Query {
            message: e.message().to_string(),
            sqlstate: e.code().map(|c| c.to_string()),
        },
        Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => DriverError::ConnectionRefused,
        Io(e) if is_certificate_failure(&e) => DriverError::Tls(e.to_string()),
        Tls(e) => DriverError::Tls(e.to_string()),
        PoolClosed | PoolTimedOut => DriverError::Disconnected,
        other => DriverError::Internal(format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_certificate_failure_returns_tls_error() {
        let err = sqlx::Error::Io(std::io::Error::other(
            "invalid peer certificate: certificate not valid for name \"127.0.0.1\"",
        ));
        let mapped = map_sqlx_error(err);
        assert!(matches!(mapped, DriverError::Tls(detail) if detail.contains("certificate")));
    }

    #[test]
    fn map_plain_io_failure_stays_internal() {
        let err = sqlx::Error::Io(std::io::Error::other("connection reset by peer"));
        assert!(matches!(map_sqlx_error(err), DriverError::Internal(_)));
    }

    #[test]
    fn map_io_refused_returns_connection_refused() {
        let err = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(map_sqlx_error(err), DriverError::ConnectionRefused));
    }

    #[test]
    fn driver_metadata() {
        let d = PgDriver;
        assert_eq!(d.id(), "postgres");
        assert_eq!(d.default_port(), 5432);
    }

    #[test]
    fn quote_ident_doubles_embedded_quotes() {
        assert_eq!(quote_ident("users"), "\"users\"");
        assert_eq!(quote_ident("My Table"), "\"My Table\"");
        assert_eq!(
            quote_ident("evil\"; DROP TABLE x; --"),
            "\"evil\"\"; DROP TABLE x; --\""
        );
    }

    #[test]
    fn normalize_pg_default_strips_type_cast_and_quotes() {
        assert_eq!(normalize_pg_default("'hi'::text".into()), "hi");
        assert_eq!(normalize_pg_default("42::integer".into()), "42");
        assert_eq!(normalize_pg_default("'2024-01-01'::date".into()), "2024-01-01");
        assert_eq!(
            normalize_pg_default("'2024-01-01 12:00:00'::timestamp without time zone".into()),
            "2024-01-01 12:00:00"
        );
        assert_eq!(normalize_pg_default("'it''s'::text".into()), "it's");
    }

    #[test]
    fn normalize_pg_default_leaves_function_calls_alone() {
        // now() has no cast — return as-is.
        assert_eq!(normalize_pg_default("now()".into()), "now()");
        assert_eq!(normalize_pg_default("CURRENT_TIMESTAMP".into()), "CURRENT_TIMESTAMP");
        // Already-unquoted expression: untouched.
        assert_eq!(normalize_pg_default("gen_random_uuid()".into()), "gen_random_uuid()");
    }

    #[test]
    fn normalize_pg_default_handles_nested_casts() {
        // (a::int + b)::numeric → strip outer ::numeric, leave inner alone.
        assert_eq!(normalize_pg_default("(a::int + b)::numeric".into()), "(a::int + b)");
    }

    #[test]
    fn normalize_pg_default_unquoted_string_passthrough() {
        // Already-unquoted (e.g. legacy MySQL-style) — no double-strip.
        assert_eq!(normalize_pg_default("hello".into()), "hello");
        assert_eq!(normalize_pg_default("'unbalanced".into()), "'unbalanced");
    }
}
