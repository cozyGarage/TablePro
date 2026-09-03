use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, Environment};
use tablepro_policy::{
    AuditState, DenyApprovalSink, GuardContext, NullAuditSink, PolicyConfig, PolicyGuard, Principal, load_policy,
};
use tablepro_ssh::{SshConfig, SshTunnel};
use tablepro_storage::AuditJournal;

use super::connection_monitor;

static SERVICE: OnceLock<DatabaseService> = OnceLock::new();

pub fn instance() -> &'static DatabaseService {
    SERVICE.get_or_init(DatabaseService::new)
}

pub(super) struct EntryInner {
    pub(super) connection: Arc<dyn Connection>,
    pub(super) tunnel: Option<SshTunnel>,
    pub(super) health: ConnectionHealth,
}

#[derive(Debug, Clone)]
pub struct ConnectionMetadata {
    pub id: Uuid,
    pub name: String,
    pub driver_id: String,
    pub environment: Environment,
    pub read_only: bool,
    pub server_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionHealth {
    Healthy,
    Reconnecting { attempt: u32 },
}

pub struct ReconnectParams {
    pub driver: Arc<dyn DatabaseDriver>,
    pub opts: ConnectOptions,
    pub ssh: Option<Vec<SshConfig>>,
}

struct Entry {
    inner: Arc<Mutex<EntryInner>>,
    metadata: ConnectionMetadata,
    read_only: bool,
    environment: Environment,
    cancel: CancellationToken,
    _monitor: tokio::task::JoinHandle<()>,
}

struct AuditRuntime {
    sink: Arc<dyn tablepro_policy::AuditSink>,
    available: bool,
    state: Arc<AuditState>,
}

impl AuditRuntime {
    fn open_default() -> Self {
        match AuditJournal::open_default() {
            Ok(journal) => {
                let recovered = journal.recovery().recovered_unresolved_operations();
                if recovered {
                    tracing::error!(
                        operations = journal.recovery().recovered_operation_ids().len(),
                        "unresolved audit intents recovered; governed writes remain disabled"
                    );
                }
                let state = if recovered {
                    AuditState::with_governed_writes_disabled()
                } else {
                    AuditState::new()
                };
                Self {
                    sink: Arc::new(journal),
                    available: true,
                    state: Arc::new(state),
                }
            }
            Err(error) => {
                tracing::error!(%error, "audit journal unavailable; MCP and governed writes are disabled");
                Self::unavailable()
            }
        }
    }

    fn unavailable() -> Self {
        Self {
            sink: Arc::new(NullAuditSink),
            available: false,
            state: Arc::new(AuditState::with_governed_writes_disabled()),
        }
    }
}

pub struct DatabaseService {
    connections: Mutex<HashMap<Uuid, Entry>>,
    policy: Mutex<Arc<PolicyConfig>>,
    audit: Arc<dyn tablepro_policy::AuditSink>,
    audit_available: bool,
    policy_available: bool,
    audit_state: Arc<AuditState>,
    approval: Mutex<Arc<dyn tablepro_policy::ApprovalSink>>,
}

impl DatabaseService {
    fn new() -> Self {
        let audit = AuditRuntime::open_default();
        let (policy, policy_available) = match load_policy() {
            Ok(policy) => (Arc::new(policy), true),
            Err(error) => {
                tracing::error!(
                    error = %error,
                    "policy file could not be loaded; using defaults and disabling MCP"
                );
                (Arc::new(PolicyConfig::default()), false)
            }
        };
        Self {
            connections: Mutex::new(HashMap::new()),
            policy: Mutex::new(policy),
            audit: audit.sink,
            audit_available: audit.available,
            policy_available,
            audit_state: audit.state,
            approval: Mutex::new(Arc::new(DenyApprovalSink)),
        }
    }

    pub fn audit_available(&self) -> bool {
        self.audit_available
    }

    pub fn policy_available(&self) -> bool {
        self.policy_available
    }

    /// Whether a prior governed operation has an unresolved outcome.  The UI
    /// uses this to stop a connection transition after cancelling running
    /// work: switching databases must not hide a newly ambiguous write.
    pub fn governed_writes_disabled(&self) -> bool {
        self.audit_state.governed_writes_disabled()
    }

    pub fn set_approval_sink(&self, sink: Arc<dyn tablepro_policy::ApprovalSink>) {
        *self.approval.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    pub fn reload_policy(&self) -> Result<(), String> {
        let next = load_policy()?;
        *self.policy.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        tracing::info!("policy reloaded");
        Ok(())
    }

    /// Whether some window already owns the saved connection `id`. Callers
    /// must check this before activating: two windows opening the same
    /// saved connection would otherwise silently overwrite each other's
    /// entry, so a caller that steals a live entry can leave the other
    /// window's connection handle answering for a session it no longer
    /// owns.
    pub fn is_active(&self, id: Uuid) -> bool {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id)
    }

    /// Register a fully validated connection and give it focus. Callers must
    /// prepare the connection before this method: activation itself does no
    /// I/O and is the final step of a connection switch. Activation is
    /// additive. A caller that means to replace a connection closes the
    /// previous one itself, so ownership is never dropped as a side effect
    /// of opening something else.
    ///
    /// Returns `false` without registering anything when `id` is already
    /// active. `connection` and `tunnel` are dropped in that case -- this
    /// consumes them either way, so refusal alone is enough to close them.
    #[must_use]
    pub fn activate(
        &self,
        id: Uuid,
        metadata: ConnectionMetadata,
        connection: Box<dyn Connection>,
        tunnel: Option<SshTunnel>,
        read_only: bool,
        params: ReconnectParams,
    ) -> bool {
        let mut connections = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if connections.contains_key(&id) {
            return false;
        }
        let arc: Arc<dyn Connection> = Arc::from(connection);
        let environment = metadata.environment;
        let inner = Arc::new(Mutex::new(EntryInner {
            connection: arc,
            tunnel,
            health: ConnectionHealth::Healthy,
        }));
        let cancel = CancellationToken::new();
        let monitor = tokio::spawn(connection_monitor::run(inner.clone(), params, cancel.clone()));
        let entry = Entry {
            inner,
            metadata,
            read_only,
            environment,
            cancel,
            _monitor: monitor,
        };
        connections.insert(id, entry);
        true
    }

    pub fn metadata(&self, id: Uuid) -> Option<ConnectionMetadata> {
        let entries = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(&id).map(|e| e.metadata.clone())
    }

    pub fn all_connections(&self) -> Vec<ConnectionMetadata> {
        let entries = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = entries.values().map(|e| e.metadata.clone()).collect();
        out.sort_by_key(|connection| connection.name.to_lowercase());
        out
    }

    /// Policy-gated handle. Raw connections are not exposed; the returned
    /// `Arc<dyn Connection>` is always a [`PolicyGuard`].
    pub fn handle(&self, id: Uuid, principal: Principal) -> Option<Arc<dyn Connection>> {
        let entries = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&id)?;
        let inner = entry.inner.lock().unwrap_or_else(|e| e.into_inner());
        let policy = self.policy.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let approval = self.approval.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let ctx = GuardContext {
            connection_id: entry.metadata.id,
            connection_name: entry.metadata.name.clone(),
            driver_id: entry.metadata.driver_id.clone(),
            environment: entry.environment,
            read_only: entry.read_only,
            principal,
            policy,
            approval,
            audit: self.audit.clone(),
            audit_state: self.audit_state.clone(),
        };
        Some(Arc::new(PolicyGuard::new(inner.connection.clone(), ctx)) as Arc<dyn Connection>)
    }

    /// Alias for [`handle`] with the human GUI principal.
    pub fn get(&self, id: Uuid) -> Option<Arc<dyn Connection>> {
        self.handle(id, Principal::human_gui())
    }

    pub fn health(&self, id: Uuid) -> Option<ConnectionHealth> {
        let entries = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&id)?;
        let inner = entry.inner.lock().unwrap_or_else(|e| e.into_inner());
        Some(inner.health.clone())
    }

    pub fn close(&self, id: Uuid) {
        let mut entries = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.remove(&id) {
            entry.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_core::DatabaseDriver;

    #[test]
    fn instance_is_singleton() {
        let a = instance() as *const _;
        let b = instance() as *const _;
        assert_eq!(a, b);
    }

    #[test]
    fn unavailable_audit_runtime_disables_governed_writes() {
        let audit = AuditRuntime::unavailable();

        assert!(!audit.available);
        assert!(audit.state.governed_writes_disabled());
    }

    async fn open_memory_connection(service: &DatabaseService, name: &str) -> Uuid {
        let driver: Arc<dyn DatabaseDriver> = Arc::new(drivers_sqlite::SqliteDriver);
        let id = Uuid::new_v4();
        let options = ConnectOptions {
            database: ":memory:".into(),
            ..Default::default()
        };
        let connection = driver.connect(options.clone()).await.expect("sqlite connection");
        let activated = service.activate(
            id,
            ConnectionMetadata {
                id,
                name: name.into(),
                driver_id: "sqlite".into(),
                environment: Environment::Local,
                read_only: false,
                server_version: None,
            },
            connection,
            None,
            false,
            ReconnectParams {
                driver,
                opts: options,
                ssh: None,
            },
        );
        assert!(activated, "the id is freshly generated, so activation must succeed");
        id
    }

    #[tokio::test]
    async fn activation_is_additive_and_keeps_earlier_connections() {
        let service = DatabaseService::new();

        let first = open_memory_connection(&service, "first").await;
        assert_eq!(service.all_connections().len(), 1);

        let second = open_memory_connection(&service, "second").await;
        assert_eq!(service.all_connections().len(), 2);
        assert!(service.handle(first, Principal::human_gui()).is_some());
        assert!(service.handle(second, Principal::human_gui()).is_some());
    }

    /// H3: two windows opening the same saved connection must not let the
    /// second one silently steal the first's entry. Before this fix,
    /// `connections.insert(id, entry)` always overwrote, so window A's
    /// connection handle would silently start answering for window B's
    /// session, and A's original entry (and its reconnect-monitor task)
    /// leaked with no cancellation.
    #[tokio::test]
    async fn a_second_window_cannot_steal_an_already_active_connection() {
        let service = DatabaseService::new();
        let id = Uuid::new_v4();
        let driver: Arc<dyn DatabaseDriver> = Arc::new(drivers_sqlite::SqliteDriver);
        let options = ConnectOptions {
            database: ":memory:".into(),
            ..Default::default()
        };
        let first_metadata = ConnectionMetadata {
            id,
            name: "window A".into(),
            driver_id: "sqlite".into(),
            environment: Environment::Local,
            read_only: false,
            server_version: None,
        };
        let first_conn = driver.connect(options.clone()).await.expect("sqlite connection");
        assert!(service.activate(
            id,
            first_metadata,
            first_conn,
            None,
            false,
            ReconnectParams {
                driver: driver.clone(),
                opts: options.clone(),
                ssh: None,
            },
        ));

        let second_metadata = ConnectionMetadata {
            id,
            name: "window B".into(),
            driver_id: "sqlite".into(),
            environment: Environment::Local,
            read_only: false,
            server_version: None,
        };
        let second_conn = driver.connect(options.clone()).await.expect("sqlite connection");
        let activated = service.activate(
            id,
            second_metadata,
            second_conn,
            None,
            false,
            ReconnectParams {
                driver,
                opts: options,
                ssh: None,
            },
        );

        assert!(
            !activated,
            "a second window must not overwrite the first window's entry"
        );
        assert_eq!(
            service.metadata(id).map(|m| m.name),
            Some("window A".into()),
            "window A's entry must survive the refused activation untouched"
        );
        assert!(service.is_active(id));
    }

    #[tokio::test]
    async fn closing_one_connection_leaves_every_other_connection_usable() {
        let service = DatabaseService::new();
        let first = open_memory_connection(&service, "first").await;
        let second = open_memory_connection(&service, "second").await;

        service.close(second);

        assert_eq!(service.all_connections().len(), 1);
        assert!(service.handle(second, Principal::human_gui()).is_none());
        let survivor = service.handle(first, Principal::human_gui()).expect("first survives");
        survivor.ping().await.expect("the surviving connection still answers");
    }

    #[tokio::test]
    async fn each_connection_keeps_its_own_metadata() {
        let service = DatabaseService::new();
        let first = open_memory_connection(&service, "first").await;
        let second = open_memory_connection(&service, "second").await;

        assert_eq!(service.metadata(first).map(|m| m.name), Some("first".to_string()));
        assert_eq!(service.metadata(second).map(|m| m.name), Some("second".to_string()));

        service.close(first);

        assert!(service.metadata(first).is_none());
        assert_eq!(service.metadata(second).map(|m| m.name), Some("second".to_string()));
    }
}
