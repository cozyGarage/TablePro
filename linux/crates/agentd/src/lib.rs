//! Shared composition pieces for the headless TablePro agent daemon.
//!
//! Agents reach a database through exactly the transport a saved connection
//! describes, and every handle handed out is wrapped by `PolicyGuard`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tablepro_core::{
    AuthMode, ColumnInfo, Connection, DriverError, DriverRegistry, ExecResult, ForeignKeyInfo, IndexInfo,
    OperationControl, QueryResult, TableInfo, TlsMode, Transaction, Value,
};
use tablepro_mcp::ConnectionProvider;
use tablepro_policy::{AuditState, GuardContext, PolicyConfig, PolicyGuard, Principal};
use tablepro_ssh::SshTunnel;
use tablepro_storage::{SavedConnection, SavedSshConfig, load_connections};
use uuid::Uuid;

const SESSION_PING_TIMEOUT: Duration = Duration::from_secs(5);

struct OpenSession {
    key: SessionKey,
    connection: Arc<dyn Connection>,
}

struct SessionConnection {
    inner: Arc<dyn Connection>,
    _tunnel: Option<SshTunnel>,
}

#[async_trait]
impl Connection for SessionConnection {
    fn supports_server_cancellation(&self) -> bool {
        self.inner.supports_server_cancellation()
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        self.inner.list_tables().await
    }

    async fn list_tables_controlled(&self, control: &OperationControl) -> Result<Vec<TableInfo>, DriverError> {
        self.inner.list_tables_controlled(control).await
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        self.inner.fetch_columns(schema, table).await
    }

    async fn fetch_columns_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        control: &OperationControl,
    ) -> Result<Vec<ColumnInfo>, DriverError> {
        self.inner.fetch_columns_controlled(schema, table, control).await
    }

    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        self.inner.fetch_rows(schema, table, offset, limit).await
    }

    async fn fetch_rows_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        self.inner
            .fetch_rows_controlled(schema, table, offset, limit, control)
            .await
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        self.inner.query(sql).await
    }

    async fn query_controlled(&self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        self.inner.query_controlled(sql, control).await
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        self.inner.query_params(sql, params).await
    }

    async fn query_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        self.inner.query_params_controlled(sql, params, control).await
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        self.inner.execute(sql).await
    }

    async fn execute_controlled(&self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        self.inner.execute_controlled(sql, control).await
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        self.inner.execute_params(sql, params).await
    }

    async fn execute_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        self.inner.execute_params_controlled(sql, params, control).await
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        self.inner.execute_in_transaction(statements).await
    }

    async fn execute_in_transaction_controlled(
        &self,
        statements: &[(String, Vec<Value>)],
        control: &OperationControl,
    ) -> Result<Vec<u64>, DriverError> {
        self.inner.execute_in_transaction_controlled(statements, control).await
    }

    async fn fetch_indexes(&self, schema: Option<&str>, table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        self.inner.fetch_indexes(schema, table).await
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        self.inner.fetch_foreign_keys(schema, table).await
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        self.inner.begin().await
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        self.inner.server_version().await
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.inner.ping().await
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SessionKey {
    driver_id: String,
    host: String,
    port: u16,
    socket_dir: Option<PathBuf>,
    database: String,
    username: String,
    tls_mode: TlsMode,
    tls_root_cert: Option<PathBuf>,
    auth_mode: AuthMode,
    ssh: Option<SavedSshConfig>,
}

impl From<&SavedConnection> for SessionKey {
    fn from(saved: &SavedConnection) -> Self {
        Self {
            driver_id: saved.driver_id.clone(),
            host: saved.host.clone(),
            port: saved.port,
            socket_dir: saved.socket_dir.clone(),
            database: saved.database.clone(),
            username: saved.username.clone(),
            tls_mode: saved.effective_tls_mode(),
            tls_root_cert: saved.tls_root_cert.clone(),
            auth_mode: saved.auth_mode,
            ssh: saved.ssh.clone(),
        }
    }
}

pub struct DaemonProvider {
    registry: Arc<DriverRegistry>,
    policy: Arc<PolicyConfig>,
    audit: Arc<dyn tablepro_policy::AuditSink>,
    audit_state: Arc<AuditState>,
    approval: Arc<dyn tablepro_policy::ApprovalSink>,
    sessions: Mutex<HashMap<Uuid, OpenSession>>,
    session_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

#[async_trait]
impl ConnectionProvider for DaemonProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        load_connections().await.map_err(|e| e.to_string())
    }

    async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String> {
        let saved = load_connections()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id == connection_id)
            .ok_or_else(|| format!("connection {connection_id} not found"))?;

        let raw = self.open_session(&saved).await?;
        let ctx = GuardContext {
            connection_id: saved.id,
            connection_name: saved.name.clone(),
            driver_id: saved.driver_id.clone(),
            environment: saved.environment,
            read_only: saved.read_only,
            principal,
            policy: self.policy.clone(),
            approval: self.approval.clone(),
            audit: self.audit.clone(),
            audit_state: self.audit_state.clone(),
        };
        Ok(Arc::new(PolicyGuard::new(raw, ctx)) as Arc<dyn Connection>)
    }
}

impl DaemonProvider {
    pub fn new(
        registry: Arc<DriverRegistry>,
        policy: Arc<PolicyConfig>,
        audit: Arc<dyn tablepro_policy::AuditSink>,
        audit_state: Arc<AuditState>,
        approval: Arc<dyn tablepro_policy::ApprovalSink>,
    ) -> Self {
        Self {
            registry,
            policy,
            audit,
            audit_state,
            approval,
            sessions: Mutex::new(HashMap::new()),
            session_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn open_session(&self, saved: &SavedConnection) -> Result<Arc<dyn Connection>, String> {
        let session_lock = self.session_lock(saved.id)?;
        let _open = session_lock.lock_owned().await;
        let key = SessionKey::from(saved);
        let cached = self.cached_connection(saved.id, &key)?;
        if let Some(connection) = cached {
            let healthy = ping_is_healthy(connection.ping(), SESSION_PING_TIMEOUT).await;
            if healthy && self.session_is_current(saved.id, &key, &connection)? {
                return Ok(connection);
            }
            self.remove_session(saved.id, &key, &connection)?;
        }

        let driver = self
            .registry
            .get(&saved.driver_id)
            .ok_or_else(|| format!("driver {} not registered", saved.driver_id))?;
        let ssh = tablepro_transport::saved_ssh_chain(saved)
            .await
            .map_err(|e| e.to_string())?;
        let opts = tablepro_transport::connect_options_for(saved)
            .await
            .map_err(|e| e.to_string())?;
        let (raw, tunnel) = tablepro_transport::establish(driver.as_ref(), opts, ssh)
            .await
            .map_err(|e| e.to_string())?;
        let connection: Arc<dyn Connection> = Arc::new(SessionConnection {
            inner: Arc::from(raw),
            _tunnel: tunnel,
        });

        self.sessions
            .lock()
            .map_err(|_| "session cache unavailable".to_string())?
            .insert(
                saved.id,
                OpenSession {
                    key,
                    connection: connection.clone(),
                },
            );
        Ok(connection)
    }

    fn cached_connection(&self, id: Uuid, key: &SessionKey) -> Result<Option<Arc<dyn Connection>>, String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "session cache unavailable".to_string())?;
        let stale = if sessions.get(&id).is_some_and(|session| session.key != *key) {
            sessions.remove(&id)
        } else {
            None
        };
        let connection = sessions.get(&id).map(|session| session.connection.clone());
        drop(stale);
        Ok(connection)
    }

    fn session_is_current(&self, id: Uuid, key: &SessionKey, connection: &Arc<dyn Connection>) -> Result<bool, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "session cache unavailable".to_string())?;
        Ok(sessions
            .get(&id)
            .is_some_and(|session| session.key == *key && Arc::ptr_eq(&session.connection, connection)))
    }

    fn remove_session(&self, id: Uuid, key: &SessionKey, connection: &Arc<dyn Connection>) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "session cache unavailable".to_string())?;
        if sessions
            .get(&id)
            .is_some_and(|session| session.key == *key && Arc::ptr_eq(&session.connection, connection))
        {
            sessions.remove(&id);
        }
        Ok(())
    }

    fn session_lock(&self, id: Uuid) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
        let mut locks = self
            .session_locks
            .lock()
            .map_err(|_| "session lock cache unavailable".to_string())?;
        Ok(locks
            .entry(id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }
}

async fn ping_is_healthy(
    ping: impl std::future::Future<Output = Result<(), tablepro_core::DriverError>>,
    timeout: Duration,
) -> bool {
    tokio::time::timeout(timeout, ping)
        .await
        .is_ok_and(|result| result.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use drivers_sqlite::SqliteDriver;
    use tablepro_core::{ConnectOptions, DatabaseDriver, DriverError, Environment};
    use tablepro_policy::{DenyApprovalSink, NullAuditSink};

    fn saved_connection() -> SavedConnection {
        SavedConnection {
            id: Uuid::new_v4(),
            name: "Warehouse".into(),
            driver_id: "postgres".into(),
            host: "db.example".into(),
            port: 5432,
            socket_dir: None,
            database: "warehouse".into(),
            username: "reader".into(),
            use_tls: true,
            tls_mode: Some(TlsMode::VerifyFull),
            tls_root_cert: None,
            read_only: true,
            auth_mode: AuthMode::Password,
            environment: Environment::Prod,
            ssh: None,
            last_opened_at: None,
        }
    }

    #[test]
    fn session_key_changes_when_saved_transport_changes() {
        let original = saved_connection();
        let original_key = SessionKey::from(&original);
        let mut changed = original.clone();
        changed.host = "replacement.example".into();

        assert!(original_key != SessionKey::from(&changed));

        changed = original.clone();
        changed.socket_dir = Some(PathBuf::from("/run/postgresql"));

        assert!(original_key != SessionKey::from(&changed));
    }

    #[test]
    fn session_key_ignores_policy_only_changes() {
        let original = saved_connection();
        let original_key = SessionKey::from(&original);
        let mut changed = original.clone();
        changed.read_only = false;
        changed.environment = Environment::Dev;

        assert!(original_key == SessionKey::from(&changed));
    }

    #[tokio::test]
    async fn cached_ping_is_bounded() {
        let ping = std::future::pending::<Result<(), DriverError>>();

        assert!(!ping_is_healthy(ping, Duration::from_millis(1)).await);
    }

    #[tokio::test]
    async fn retired_session_drops_after_final_issued_reference() {
        let saved = saved_connection();
        let key = SessionKey::from(&saved);
        let mut opts = ConnectOptions {
            database: ":memory:".into(),
            ..Default::default()
        };
        opts.tls.mode = TlsMode::Disabled;
        let raw: Arc<dyn Connection> = Arc::from(SqliteDriver.connect(opts).await.expect("open test database"));
        let raw_lifetime = Arc::downgrade(&raw);
        let connection: Arc<dyn Connection> = Arc::new(SessionConnection {
            inner: raw,
            _tunnel: None,
        });
        let provider = DaemonProvider::new(
            Arc::new(DriverRegistry::new()),
            Arc::new(PolicyConfig::default()),
            Arc::new(NullAuditSink),
            Arc::new(AuditState::new()),
            Arc::new(DenyApprovalSink),
        );
        provider.sessions.lock().expect("sessions").insert(
            saved.id,
            OpenSession {
                key: key.clone(),
                connection: connection.clone(),
            },
        );
        let issued = connection.clone();

        provider
            .remove_session(saved.id, &key, &connection)
            .expect("remove cached session");
        drop(connection);

        assert!(raw_lifetime.upgrade().is_some());

        drop(issued);

        assert!(raw_lifetime.upgrade().is_none());
    }
}
