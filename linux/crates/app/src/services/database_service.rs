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
    audit_state: Arc<AuditState>,
    approval: Mutex<Arc<dyn tablepro_policy::ApprovalSink>>,
}

impl DatabaseService {
    fn new() -> Self {
        let audit = AuditRuntime::open_default();
        Self {
            connections: Mutex::new(HashMap::new()),
            policy: Mutex::new(Arc::new(load_policy())),
            audit: audit.sink,
            audit_available: audit.available,
            audit_state: audit.state,
            approval: Mutex::new(Arc::new(DenyApprovalSink)),
        }
    }

    pub fn audit_available(&self) -> bool {
        self.audit_available
    }

    /// Whether a prior governed operation has an unresolved outcome.  The UI
    /// uses this to stop a connection transition after cancelling running
    /// work: switching databases must not hide a newly ambiguous write.
    pub fn governed_writes_disabled(&self) -> bool {
        self.audit_state.governed_writes_disabled()
    }

    pub fn set_approval_sink(&self, sink: Arc<dyn tablepro_policy::ApprovalSink>) {
        *self.approval.lock().expect("database_service lock") = sink;
    }

    pub fn reload_policy(&self) {
        let next = Arc::new(load_policy());
        *self.policy.lock().expect("database_service lock") = next;
        tracing::info!("policy reloaded");
    }

    /// Register a fully validated connection and give it focus. Callers must
    /// prepare the connection before this method: activation itself is
    /// deliberately infallible and is the final step of a connection switch.
    /// Activation is additive. A caller that means to replace a connection
    /// closes the previous one itself, so ownership is never dropped as a
    /// side effect of opening something else.
    pub fn activate(
        &self,
        id: Uuid,
        metadata: ConnectionMetadata,
        connection: Box<dyn Connection>,
        tunnel: Option<SshTunnel>,
        read_only: bool,
        params: ReconnectParams,
    ) {
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
        let mut connections = self.connections.lock().expect("database_service lock");
        connections.insert(id, entry);
    }

    pub fn metadata(&self, id: Uuid) -> Option<ConnectionMetadata> {
        let entries = self.connections.lock().expect("database_service lock");
        entries.get(&id).map(|e| e.metadata.clone())
    }

    pub fn all_connections(&self) -> Vec<ConnectionMetadata> {
        let entries = self.connections.lock().expect("database_service lock");
        let mut out: Vec<_> = entries.values().map(|e| e.metadata.clone()).collect();
        out.sort_by_key(|connection| connection.name.to_lowercase());
        out
    }

    /// Policy-gated handle. Raw connections are not exposed; the returned
    /// `Arc<dyn Connection>` is always a [`PolicyGuard`].
    pub fn handle(&self, id: Uuid, principal: Principal) -> Option<Arc<dyn Connection>> {
        let entries = self.connections.lock().expect("database_service lock");
        let entry = entries.get(&id)?;
        let inner = entry.inner.lock().expect("entry inner lock");
        let policy = self.policy.lock().expect("database_service lock").clone();
        let approval = self.approval.lock().expect("database_service lock").clone();
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
        let entries = self.connections.lock().expect("database_service lock");
        let entry = entries.get(&id)?;
        let inner = entry.inner.lock().expect("entry inner lock");
        Some(inner.health.clone())
    }

    pub fn close(&self, id: Uuid) {
        let mut entries = self.connections.lock().expect("database_service lock");
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
        service.activate(
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
