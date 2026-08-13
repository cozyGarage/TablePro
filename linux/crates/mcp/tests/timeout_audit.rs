use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tablepro_core::{ColumnInfo, Connection, DriverError, Environment, ExecResult, QueryResult, TableInfo, Value};
use tablepro_mcp::{ConnectionProvider, McpBridge, TokenPermissions, TokenStore};
use tablepro_policy::{
    AuditError, AuditEvent, AuditRecordPhase, AuditSink, AuditState, AutoApproveSink, GuardContext, PolicyConfig,
    PolicyGuard, Principal, WritePolicy,
};
use tablepro_storage::SavedConnection;
use uuid::Uuid;

struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

#[async_trait]
impl AuditSink for RecordingAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        self.events.lock().expect("event lock").push(event);
        Ok(())
    }
}

struct HangingConnection;

#[async_trait]
impl Connection for HangingConnection {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(Vec::new())
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(Vec::new())
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        })
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        std::future::pending().await
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        std::future::pending().await
    }

    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        std::future::pending().await
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        std::future::pending().await
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

struct GuardedProvider {
    connection_id: Uuid,
    policy: Arc<PolicyConfig>,
    audit: Arc<RecordingAuditSink>,
    audit_state: Arc<AuditState>,
}

#[async_trait]
impl ConnectionProvider for GuardedProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        Ok(Vec::new())
    }

    async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String> {
        if connection_id != self.connection_id {
            return Err("connection not found".into());
        }
        let context = GuardContext {
            connection_id,
            connection_name: "timeout-test".into(),
            driver_id: "postgres".into(),
            environment: Environment::Local,
            read_only: false,
            principal,
            policy: self.policy.clone(),
            approval: Arc::new(AutoApproveSink),
            audit: self.audit.clone(),
            audit_state: self.audit_state.clone(),
        };
        Ok(Arc::new(PolicyGuard::new(Arc::new(HangingConnection), context)))
    }
}

#[tokio::test]
async fn timed_out_mutation_leaves_intent_and_blocks_later_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let connection_id = Uuid::new_v4();
    let tokens = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let (_metadata, plaintext) = tokens
        .issue(
            "timeout-test".into(),
            TokenPermissions::ReadWrite,
            vec![connection_id],
            None,
        )
        .unwrap();
    let token = tokens.authenticate(&plaintext).unwrap();
    let audit = Arc::new(RecordingAuditSink {
        events: Mutex::new(Vec::new()),
    });
    let audit_state = Arc::new(AuditState::new());
    let mut policy = PolicyConfig::default();
    policy.environments.entry("local".into()).or_default().agent_writes = Some(WritePolicy::Allow);
    let provider = Arc::new(GuardedProvider {
        connection_id,
        policy: Arc::new(policy),
        audit: audit.clone(),
        audit_state: audit_state.clone(),
    });
    let mut bridge = McpBridge::new(provider, tokens);
    bridge.query_timeout_secs = 0;

    let first = bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (1)", false)
        .await
        .expect_err("write must time out");
    assert!(first.contains("timed out"));
    assert!(audit_state.governed_writes_disabled());

    let second = bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (2)", false)
        .await
        .expect_err("later write must be blocked");
    assert!(second.contains("governed writes are disabled"));

    let events = audit.events.lock().expect("event lock");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, AuditRecordPhase::Intent);
}
