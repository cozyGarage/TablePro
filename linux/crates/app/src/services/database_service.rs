use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, Environment};
use tablepro_policy::{
    AutoApproveSink, GuardContext, NullAuditSink, PolicyConfig, PolicyGuard, Principal, load_policy,
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
    pub read_only: bool,
    pub environment: Environment,
}

struct Entry {
    inner: Arc<Mutex<EntryInner>>,
    metadata: ConnectionMetadata,
    read_only: bool,
    environment: Environment,
    cancel: CancellationToken,
    _monitor: tokio::task::JoinHandle<()>,
}

pub struct DatabaseService {
    connections: Mutex<HashMap<Uuid, Entry>>,
    active: Mutex<Option<Uuid>>,
    policy: Mutex<Arc<PolicyConfig>>,
    audit: Arc<dyn tablepro_policy::AuditSink>,
    approval: Mutex<Arc<dyn tablepro_policy::ApprovalSink>>,
}

impl DatabaseService {
    fn new() -> Self {
        let audit: Arc<dyn tablepro_policy::AuditSink> = match AuditJournal::open_default() {
            Ok(j) => Arc::new(j),
            Err(e) => {
                tracing::warn!("audit journal unavailable: {e}");
                Arc::new(NullAuditSink)
            }
        };
        Self {
            connections: Mutex::new(HashMap::new()),
            active: Mutex::new(None),
            policy: Mutex::new(Arc::new(load_policy())),
            audit,
            approval: Mutex::new(Arc::new(AutoApproveSink)),
        }
    }

    pub fn set_approval_sink(&self, sink: Arc<dyn tablepro_policy::ApprovalSink>) {
        *self.approval.lock().expect("database_service lock") = sink;
    }

    pub fn reload_policy(&self) {
        *self.policy.lock().expect("database_service lock") = Arc::new(load_policy());
    }

    pub fn add(
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
        self.connections
            .lock()
            .expect("database_service lock")
            .insert(id, entry);
        *self.active.lock().expect("database_service lock") = Some(id);
    }

    pub fn active_metadata(&self) -> Option<ConnectionMetadata> {
        let id = self.active_id()?;
        let entries = self.connections.lock().expect("database_service lock");
        entries.get(&id).map(|e| e.metadata.clone())
    }

    pub fn all_connections(&self) -> Vec<ConnectionMetadata> {
        let entries = self.connections.lock().expect("database_service lock");
        let mut out: Vec<_> = entries.values().map(|e| e.metadata.clone()).collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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
        };
        Some(Arc::new(PolicyGuard::new(inner.connection.clone(), ctx)) as Arc<dyn Connection>)
    }

    pub fn active_handle(&self, principal: Principal) -> Option<Arc<dyn Connection>> {
        let id = self.active_id()?;
        self.handle(id, principal)
    }

    /// Human GUI convenience: active connection under the GUI principal.
    pub fn active(&self) -> Option<Arc<dyn Connection>> {
        self.active_handle(Principal::human_gui())
    }

    /// Alias for [`handle`] with the human GUI principal.
    pub fn get(&self, id: Uuid) -> Option<Arc<dyn Connection>> {
        self.handle(id, Principal::human_gui())
    }

    pub fn active_id(&self) -> Option<Uuid> {
        *self.active.lock().expect("database_service lock")
    }

    pub fn active_health(&self) -> Option<ConnectionHealth> {
        let id = self.active_id()?;
        let entries = self.connections.lock().expect("database_service lock");
        let entry = entries.get(&id)?;
        let inner = entry.inner.lock().expect("entry inner lock");
        Some(inner.health.clone())
    }

    pub fn is_active_read_only(&self) -> bool {
        let id = match self.active_id() {
            Some(id) => id,
            None => return false,
        };
        self.connections
            .lock()
            .expect("database_service lock")
            .get(&id)
            .map(|e| e.read_only)
            .unwrap_or(false)
    }

    pub fn active_environment(&self) -> Option<Environment> {
        let id = self.active_id()?;
        self.connections
            .lock()
            .expect("database_service lock")
            .get(&id)
            .map(|e| e.environment)
    }

    pub fn remove(&self, id: Uuid) {
        let mut entries = self.connections.lock().expect("database_service lock");
        if let Some(entry) = entries.remove(&id) {
            entry.cancel.cancel();
        }
        let mut active = self.active.lock().expect("database_service lock");
        if *active == Some(id) {
            *active = None;
        }
    }

    pub fn clear_all(&self) {
        let mut entries = self.connections.lock().expect("database_service lock");
        for (_, entry) in entries.drain() {
            entry.cancel.cancel();
        }
        *self.active.lock().expect("database_service lock") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_is_singleton() {
        let a = instance() as *const _;
        let b = instance() as *const _;
        assert_eq!(a, b);
    }
}
