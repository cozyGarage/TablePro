//! Shared composition pieces for the headless TablePro agent daemon.
//!
//! Agents reach a database through exactly the transport a saved connection
//! describes, and every handle handed out is wrapped by `PolicyGuard`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tablepro_core::{Connection, DriverRegistry};
use tablepro_mcp::ConnectionProvider;
use tablepro_policy::{AuditState, GuardContext, PolicyConfig, PolicyGuard, Principal};
use tablepro_ssh::SshTunnel;
use tablepro_storage::{SavedConnection, load_connections};
use tokio::sync::Mutex;
use uuid::Uuid;

struct OpenSession {
    connection: Arc<dyn Connection>,
    _tunnel: Option<SshTunnel>,
}

pub struct DaemonProvider {
    registry: Arc<DriverRegistry>,
    policy: Arc<PolicyConfig>,
    audit: Arc<dyn tablepro_policy::AuditSink>,
    audit_state: Arc<AuditState>,
    approval: Arc<dyn tablepro_policy::ApprovalSink>,
    sessions: Mutex<HashMap<Uuid, OpenSession>>,
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
        }
    }

    async fn open_session(&self, saved: &SavedConnection) -> Result<Arc<dyn Connection>, String> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(&saved.id)
            && session.connection.ping().await.is_ok()
        {
            return Ok(session.connection.clone());
        }
        sessions.remove(&saved.id);

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

        let connection: Arc<dyn Connection> = Arc::from(raw);
        sessions.insert(
            saved.id,
            OpenSession {
                connection: connection.clone(),
                _tunnel: tunnel,
            },
        );
        Ok(connection)
    }
}
