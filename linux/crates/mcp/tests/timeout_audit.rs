use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use tablepro_core::{
    ColumnInfo, Connection, DriverError, Environment, ExecResult, OperationControl, QueryResult, TableInfo, Value,
    check_pre_dispatch,
};
use tablepro_mcp::{ConnectionProvider, McpBridge, TokenPermissions, TokenStore};
use tablepro_policy::{
    AuditError, AuditEvent, AuditRecordPhase, AuditSink, AuditState, AuditTerminalStatus, AutoApproveSink,
    GuardContext, PolicyConfig, PolicyGuard, Principal, WritePolicy,
};
use tablepro_storage::SavedConnection;
use uuid::Uuid;

struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
    unavailable: bool,
}

impl RecordingAuditSink {
    fn recording() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            unavailable: false,
        }
    }

    fn unavailable() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            unavailable: true,
        }
    }
}

#[async_trait]
impl AuditSink for RecordingAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        if self.unavailable {
            return Err(AuditError::Unavailable("audit sink is offline in this test".into()));
        }
        self.events.lock().expect("event lock").push(event);
        Ok(())
    }
}

enum ControlledWriteBehavior {
    ConfirmedTimeoutThenSuccess,
    UnknownInterruption,
}

struct ControlledConnection {
    behavior: ControlledWriteBehavior,
    attempts: AtomicUsize,
}

#[async_trait]
impl Connection for ControlledConnection {
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

    async fn query_controlled(&self, _: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        check_pre_dispatch(control)?;
        Ok(QueryResult {
            columns: Vec::new(),
            rows: vec![vec![Value::Int(3)]],
            truncated: false,
        })
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        std::future::pending().await
    }

    async fn execute_controlled(&self, _: &str, _: &OperationControl) -> Result<ExecResult, DriverError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            ControlledWriteBehavior::ConfirmedTimeoutThenSuccess if attempt == 0 => Err(DriverError::TimedOut),
            ControlledWriteBehavior::ConfirmedTimeoutThenSuccess => Ok(ExecResult { rows_affected: 1 }),
            ControlledWriteBehavior::UnknownInterruption => Err(DriverError::OperationOutcomeUnknown {
                source: Box::new(DriverError::TimedOut),
            }),
        }
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
    connection: Arc<ControlledConnection>,
}

#[async_trait]
impl ConnectionProvider for GuardedProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        Ok(vec![SavedConnection {
            id: self.connection_id,
            name: "timeout-test".into(),
            driver_id: "postgres".into(),
            host: "localhost".into(),
            port: 5432,
            socket_dir: None,
            database: "timeout".into(),
            username: "timeout".into(),
            use_tls: false,
            tls_mode: None,
            tls_root_cert: None,
            read_only: false,
            auth_mode: Default::default(),
            environment: Environment::Local,
            ssh: None,
            last_opened_at: None,
        }])
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
        Ok(Arc::new(PolicyGuard::new(self.connection.clone(), context)))
    }
}

#[tokio::test]
async fn confirmed_timeout_records_terminal_event_and_allows_later_write() {
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
    let audit = Arc::new(RecordingAuditSink::recording());
    let audit_state = Arc::new(AuditState::new());
    let mut policy = PolicyConfig::default();
    policy.environments.entry("local".into()).or_default().agent_writes = Some(WritePolicy::Allow);
    let provider = Arc::new(GuardedProvider {
        connection_id,
        policy: Arc::new(policy),
        audit: audit.clone(),
        audit_state: audit_state.clone(),
        connection: Arc::new(ControlledConnection {
            behavior: ControlledWriteBehavior::ConfirmedTimeoutThenSuccess,
            attempts: AtomicUsize::new(0),
        }),
    });
    let mut bridge = McpBridge::new(provider, tokens);
    bridge.query_timeout_secs = 0;

    let first = bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (1)", false)
        .await
        .expect_err("write must time out");
    assert!(first.contains("timed out"));
    assert!(!audit_state.governed_writes_disabled());

    bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (2)", false)
        .await
        .expect("later write must be allowed");

    let events = audit.events.lock().expect("event lock");
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].phase, AuditRecordPhase::Intent);
    assert_eq!(events[1].phase, AuditRecordPhase::Outcome);
    assert_eq!(events[1].terminal_status, AuditTerminalStatus::TimedOut);
    assert_eq!(events[2].phase, AuditRecordPhase::Intent);
    assert_eq!(events[3].phase, AuditRecordPhase::Outcome);
    assert_eq!(events[3].terminal_status, AuditTerminalStatus::Succeeded);
}

#[tokio::test]
async fn unknown_interruption_records_unknown_outcome_and_blocks_later_writes() {
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
    let audit = Arc::new(RecordingAuditSink::recording());
    let audit_state = Arc::new(AuditState::new());
    let mut policy = PolicyConfig::default();
    policy.environments.entry("local".into()).or_default().agent_writes = Some(WritePolicy::Allow);
    let connection = Arc::new(ControlledConnection {
        behavior: ControlledWriteBehavior::UnknownInterruption,
        attempts: AtomicUsize::new(0),
    });
    let provider = Arc::new(GuardedProvider {
        connection_id,
        policy: Arc::new(policy),
        audit: audit.clone(),
        audit_state: audit_state.clone(),
        connection: connection.clone(),
    });
    let mut bridge = McpBridge::new(provider, tokens);
    bridge.query_timeout_secs = 0;

    let first = bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (1)", false)
        .await
        .expect_err("write outcome must be unknown");
    assert!(first.contains("outcome is unknown"));
    assert!(audit_state.governed_writes_disabled());

    let second = bridge
        .execute_write(&token, connection_id, "INSERT INTO jobs(id) VALUES (2)", false)
        .await
        .expect_err("later write must be blocked");
    assert!(second.contains("governed writes are disabled"));
    assert_eq!(connection.attempts.load(Ordering::SeqCst), 1);

    let events = audit.events.lock().expect("event lock");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].phase, AuditRecordPhase::Intent);
    assert_eq!(events[1].phase, AuditRecordPhase::Outcome);
    assert_eq!(events[1].terminal_status, AuditTerminalStatus::Unknown);
}

struct MetadataHarness {
    bridge: McpBridge,
    token: tablepro_mcp::McpToken,
    connection_id: Uuid,
    audit: Arc<RecordingAuditSink>,
    audit_state: Arc<AuditState>,
    _token_dir: tempfile::TempDir,
}

fn metadata_harness(timeout_secs: u64, audit: Arc<RecordingAuditSink>) -> MetadataHarness {
    let dir = tempfile::TempDir::new().expect("temporary token directory");
    let connection_id = Uuid::new_v4();
    let tokens = Arc::new(TokenStore::open(dir.path().join("tokens.json")).expect("token store"));
    let (_metadata, plaintext) = tokens
        .issue(
            "metadata-test".into(),
            TokenPermissions::ReadOnly,
            vec![connection_id],
            None,
        )
        .expect("issue a read-only token");
    let token = tokens.authenticate(&plaintext).expect("authenticate");
    let audit_state = Arc::new(AuditState::new());
    let provider = Arc::new(GuardedProvider {
        connection_id,
        policy: Arc::new(PolicyConfig::default()),
        audit: audit.clone(),
        audit_state: audit_state.clone(),
        connection: Arc::new(ControlledConnection {
            behavior: ControlledWriteBehavior::ConfirmedTimeoutThenSuccess,
            attempts: AtomicUsize::new(0),
        }),
    });
    let mut bridge = McpBridge::new(provider, tokens);
    bridge.query_timeout_secs = timeout_secs;
    MetadataHarness {
        bridge,
        token,
        connection_id,
        audit,
        audit_state,
        _token_dir: dir,
    }
}

#[tokio::test]
async fn a_read_scoped_agent_may_read_table_metadata_and_a_page() {
    let harness = metadata_harness(30, Arc::new(RecordingAuditSink::recording()));

    let schema = harness
        .bridge
        .table_schema(&harness.token, harness.connection_id, None, "jobs".into())
        .await
        .expect("read scope is enough for table metadata");
    assert!(schema.columns.is_empty());

    let page = harness
        .bridge
        .browse_table(&harness.token, harness.connection_id, None, "jobs".into(), 0, 10)
        .await
        .expect("read scope is enough for a page");
    assert!(page.rows.is_empty());

    let count = harness
        .bridge
        .count_rows(
            &harness.token,
            harness.connection_id,
            Some("public".into()),
            "jobs".into(),
        )
        .await
        .expect("read scope is enough for an exact count");
    assert_eq!(count, 3);

    let events = harness.audit.events.lock().expect("event lock");
    let terminal = events
        .iter()
        .filter(|event| event.phase == AuditRecordPhase::Outcome)
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 5, "every governed metadata read is audited: {events:?}");
    assert!(
        terminal
            .iter()
            .all(|event| event.terminal_status == AuditTerminalStatus::Succeeded),
        "{terminal:?}"
    );
}

#[tokio::test]
async fn a_timed_out_metadata_read_is_refused_without_a_half_open_audit_operation() {
    let harness = metadata_harness(0, Arc::new(RecordingAuditSink::recording()));

    let schema_error = harness
        .bridge
        .table_schema(&harness.token, harness.connection_id, None, "jobs".into())
        .await
        .expect_err("an expired deadline must refuse the metadata read");
    assert!(schema_error.contains("timed out"), "{schema_error}");

    let page_error = harness
        .bridge
        .browse_table(&harness.token, harness.connection_id, None, "jobs".into(), 0, 10)
        .await
        .expect_err("an expired deadline must refuse the page");
    assert!(page_error.contains("timed out"), "{page_error}");

    let count_error = harness
        .bridge
        .count_rows(&harness.token, harness.connection_id, None, "jobs".into())
        .await
        .expect_err("an expired deadline must refuse the count");
    assert!(count_error.contains("timed out"), "{count_error}");

    let events = harness.audit.events.lock().expect("event lock");
    let intents = events.iter().filter(|e| e.phase == AuditRecordPhase::Intent).count();
    let outcomes = events
        .iter()
        .filter(|e| e.phase == AuditRecordPhase::Outcome)
        .collect::<Vec<_>>();
    assert_eq!(intents, outcomes.len(), "no audit operation may stay open: {events:?}");
    assert!(
        outcomes
            .iter()
            .all(|e| e.terminal_status == AuditTerminalStatus::TimedOut),
        "{outcomes:?}"
    );
    drop(events);
    assert!(!harness.audit_state.governed_writes_disabled());
}

#[tokio::test]
async fn a_metadata_read_is_denied_when_audit_intent_cannot_be_persisted() {
    let harness = metadata_harness(30, Arc::new(RecordingAuditSink::unavailable()));

    for (tool, error) in [
        (
            "table_schema",
            harness
                .bridge
                .table_schema(&harness.token, harness.connection_id, None, "jobs".into())
                .await
                .err(),
        ),
        (
            "browse_table",
            harness
                .bridge
                .browse_table(&harness.token, harness.connection_id, None, "jobs".into(), 0, 10)
                .await
                .err(),
        ),
        (
            "count_rows",
            harness
                .bridge
                .count_rows(&harness.token, harness.connection_id, None, "jobs".into())
                .await
                .err(),
        ),
    ] {
        let error = error.unwrap_or_else(|| panic!("{tool} must be denied when audit is unavailable"));
        assert!(error.contains("denied"), "{tool}: {error}");
    }
}
