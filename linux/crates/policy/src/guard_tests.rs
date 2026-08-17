use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Notify;

use super::*;
use crate::approval::{ApprovalOutcome, ApprovalRequest, AutoApproveSink, DenyApprovalSink};
use crate::audit::NullAuditSink;

struct CountingTx {
    executes: Arc<AtomicUsize>,
    commits: Arc<AtomicUsize>,
}

#[async_trait]
impl Transaction for CountingTx {
    async fn query(&mut self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn execute(&mut self, _: &str) -> Result<ExecResult, DriverError> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

struct CountingConn {
    executes: Arc<AtomicUsize>,
    commits: Arc<AtomicUsize>,
}

struct BlockingWriteConn {
    dispatched: Arc<Notify>,
    release: Arc<Notify>,
}

struct AmbiguousWriteConn;

struct ControlledWriteConn {
    outcome_unknown: bool,
}

struct TransactionQueryErrorConn;

struct TransactionQueryErrorTx;

struct QueryCountingConn {
    queries: Arc<AtomicUsize>,
}

struct QueryCountingTx {
    queries: Arc<AtomicUsize>,
}

struct AmbiguousCommitTx;

#[async_trait]
impl Transaction for QueryCountingTx {
    async fn query(&mut self, _: &str) -> Result<QueryResult, DriverError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(empty_result())
    }

    async fn execute(&mut self, _: &str) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Transaction for TransactionQueryErrorTx {
    async fn query(&mut self, _: &str) -> Result<QueryResult, DriverError> {
        Err(query_error())
    }

    async fn execute(&mut self, _: &str) -> Result<ExecResult, DriverError> {
        Err(query_error())
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Transaction for AmbiguousCommitTx {
    async fn query(&mut self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn execute(&mut self, _: &str) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        Err(DriverError::Disconnected)
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for CountingConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(QueryResult {
            columns: vec![],
            rows: vec![vec![Value::Int(1)]],
            truncated: false,
        })
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        self.execute("").await
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        self.executes.fetch_add(statements.len(), Ordering::SeqCst);
        Ok(vec![1; statements.len()])
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Ok(Box::new(CountingTx {
            executes: self.executes.clone(),
            commits: self.commits.clone(),
        }))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for QueryCountingConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(QueryResult {
            columns: vec![],
            rows: vec![vec![Value::Int(1)]],
            truncated: false,
        })
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        Ok(vec![1])
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Ok(Box::new(QueryCountingTx {
            queries: self.queries.clone(),
        }))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for TransactionQueryErrorConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Err(query_error())
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        Err(query_error())
    }

    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        Err(query_error())
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        Err(query_error())
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Ok(Box::new(TransactionQueryErrorTx))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for BlockingWriteConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        self.dispatched.notify_one();
        self.release.notified().await;
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_params(&self, sql: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        self.execute(sql).await
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        self.execute("").await.map(|result| vec![result.rows_affected])
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Err(DriverError::Unsupported("transaction".into()))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for ControlledWriteConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_controlled(&self, _: &str, _: &OperationControl) -> Result<ExecResult, DriverError> {
        if self.outcome_unknown {
            return Err(DriverError::OperationOutcomeUnknown {
                source: Box::new(DriverError::TimedOut),
            });
        }
        Err(DriverError::TimedOut)
    }

    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        Ok(ExecResult { rows_affected: 1 })
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        Ok(vec![1])
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Err(DriverError::Unsupported("transaction".into()))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl Connection for AmbiguousWriteConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }

    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(empty_result())
    }

    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        Err(DriverError::Tls("Bearer raw-driver-secret".into()))
    }

    async fn execute_params(&self, sql: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        self.execute(sql).await
    }

    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        Err(DriverError::Tls("Bearer raw-driver-secret".into()))
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Ok(Box::new(AmbiguousCommitTx))
    }

    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

struct SequenceAuditSink {
    failures: Mutex<Vec<AuditRecordPhase>>,
    events: Mutex<Vec<AuditEvent>>,
}

impl SequenceAuditSink {
    fn new(failures: Vec<AuditRecordPhase>) -> Self {
        Self {
            failures: Mutex::new(failures),
            events: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl AuditSink for SequenceAuditSink {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        let mut failures = self.failures.lock().expect("failure lock");
        if failures.first() == Some(&event.phase) {
            failures.remove(0);
            return Err(AuditError::Persistence("injected failure".into()));
        }
        drop(failures);
        self.events.lock().expect("event lock").push(event);
        Ok(())
    }
}

struct CountingApprovalSink {
    calls: Arc<AtomicUsize>,
}

struct SequenceApprovalSink {
    calls: Arc<AtomicUsize>,
    outcomes: Mutex<Vec<ApprovalOutcome>>,
}

#[async_trait]
impl ApprovalSink for SequenceApprovalSink {
    async fn request(&self, _request: ApprovalRequest) -> ApprovalOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut outcomes = self.outcomes.lock().expect("outcome lock");
        if outcomes.is_empty() {
            return ApprovalOutcome::Deny;
        }
        outcomes.remove(0)
    }
}

#[async_trait]
impl ApprovalSink for CountingApprovalSink {
    async fn request(&self, _request: ApprovalRequest) -> ApprovalOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ApprovalOutcome::AllowOnce
    }
}

fn query_error() -> DriverError {
    DriverError::Query {
        message: "injected query error".into(),
        sqlstate: Some("42000".into()),
    }
}

fn empty_result() -> QueryResult {
    QueryResult {
        columns: vec![],
        rows: vec![],
        truncated: false,
    }
}

fn context(
    principal: Principal,
    environment: Environment,
    policy: PolicyConfig,
    approval: Arc<dyn ApprovalSink>,
    audit: Arc<dyn AuditSink>,
    audit_state: Arc<AuditState>,
) -> GuardContext {
    GuardContext {
        connection_id: Uuid::nil(),
        connection_name: "test".into(),
        driver_id: "postgres".into(),
        environment,
        read_only: false,
        principal,
        policy: Arc::new(policy),
        approval,
        audit,
        audit_state,
    }
}

fn connection(executes: Arc<AtomicUsize>, commits: Arc<AtomicUsize>) -> Arc<dyn Connection> {
    Arc::new(CountingConn { executes, commits })
}

fn query_counting_connection(queries: Arc<AtomicUsize>) -> Arc<dyn Connection> {
    Arc::new(QueryCountingConn { queries })
}

fn allowed_agent_policy() -> PolicyConfig {
    let mut policy = PolicyConfig::default();
    let local = policy.environments.entry("local".into()).or_default();
    local.agent_writes = Some(crate::config::WritePolicy::Allow);
    local.agent_allow_multi_statement = Some(true);
    policy
}

#[tokio::test]
async fn categorical_agent_denial_skips_blast_radius_query() {
    let queries = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        query_counting_connection(queries.clone()),
        context(
            Principal::Agent {
                token: "token".into(),
                client: None,
                model: None,
            },
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::new()),
        ),
    );

    guard
        .execute("UPDATE jobs SET status = 'done' WHERE id = 1")
        .await
        .expect_err("agent write must be denied");

    assert_eq!(queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn read_only_denial_skips_blast_radius_query() {
    let queries = Arc::new(AtomicUsize::new(0));
    let mut guard_context = context(
        Principal::human_gui(),
        Environment::Prod,
        PolicyConfig::default(),
        Arc::new(AutoApproveSink),
        Arc::new(SequenceAuditSink::new(vec![])),
        Arc::new(AuditState::new()),
    );
    guard_context.read_only = true;
    let guard = PolicyGuard::new(query_counting_connection(queries.clone()), guard_context);

    guard
        .execute("UPDATE jobs SET status = 'done' WHERE id = 1")
        .await
        .expect_err("read-only write must be denied");

    assert_eq!(queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disabled_audit_state_cannot_be_bypassed_by_approval() {
    let queries = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        query_counting_connection(queries.clone()),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(CountingApprovalSink {
                calls: approvals.clone(),
            }),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::with_governed_writes_disabled()),
        ),
    );

    guard
        .execute("UPDATE jobs SET status = 'done' WHERE id = 1")
        .await
        .expect_err("disabled audit state must deny the write");

    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert_eq!(queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn agent_transaction_read_intent_failure_prevents_driver_query() {
    let queries = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        query_counting_connection(queries.clone()),
        context(
            Principal::Agent {
                token: "token".into(),
                client: None,
                model: None,
            },
            Environment::Local,
            allowed_agent_policy(),
            Arc::new(AutoApproveSink),
            Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Intent])),
            Arc::new(AuditState::new()),
        ),
    );
    let mut transaction = guard.begin().await.expect("begin");

    transaction
        .query("SELECT 1")
        .await
        .expect_err("read intent failure must surface");

    assert_eq!(queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn required_intent_failure_prevents_driver_execution() {
    let executes = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Intent]));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit,
            Arc::new(AuditState::new()),
        ),
    );

    let error = guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("intent must fail");

    assert!(error.to_string().contains("audit intent could not be persisted"));
    assert_eq!(executes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn post_execution_audit_failure_poisons_shared_state() {
    let executes = Arc::new(AtomicUsize::new(0));
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Outcome]));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let sibling = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit,
            state,
        ),
    );

    let first = guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("outcome must fail");
    let second = sibling
        .execute("INSERT INTO jobs(id) VALUES (2)")
        .await
        .expect_err("shared state must remain poisoned");

    assert!(first.to_string().contains("operation may have succeeded"));
    assert!(second.to_string().contains("governed writes are disabled"));
    assert_eq!(executes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropped_write_future_poisons_shared_state() {
    let state = Arc::new(AuditState::new());
    let dispatched = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let guard = PolicyGuard::new(
        Arc::new(BlockingWriteConn {
            dispatched: dispatched.clone(),
            release,
        }),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(SequenceAuditSink::new(vec![])),
            state.clone(),
        ),
    );

    let task = tokio::spawn(async move { guard.execute("INSERT INTO jobs(id) VALUES (1)").await });
    dispatched.notified().await;
    task.abort();
    task.await.expect_err("write task must be cancelled");

    assert!(state.governed_writes_disabled());
}

#[tokio::test]
async fn batch_query_error_records_unknown_and_poisons_state() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(TransactionQueryErrorConn),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let statements = vec![("INSERT INTO jobs(id) VALUES (1)".into(), vec![])];

    guard
        .execute_in_transaction(&statements)
        .await
        .expect_err("batch query error must surface");

    assert!(state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("batch outcome");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::Unknown);
    assert_eq!(outcome.transaction_outcome, AuditTransactionOutcome::Unknown);
    assert_eq!(outcome.error_category, Some(AuditErrorCategory::Query));
}

#[tokio::test]
async fn interactive_transaction_execute_query_error_records_unknown_and_poisons_state() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(TransactionQueryErrorConn),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let mut transaction = guard.begin().await.expect("begin");

    transaction
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("transaction query error must surface");

    assert!(state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("transaction statement outcome");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::Unknown);
    assert_eq!(outcome.transaction_outcome, AuditTransactionOutcome::Unknown);
    assert_eq!(outcome.error_category, Some(AuditErrorCategory::Query));
}

#[tokio::test]
async fn confirmed_controlled_timeout_records_timeout_without_poisoning_state() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(ControlledWriteConn { outcome_unknown: false }),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let control = OperationControl::new(Default::default(), None);

    let error = guard
        .execute_controlled("INSERT INTO jobs(id) VALUES (1)", &control)
        .await
        .expect_err("confirmed timeout must surface");

    assert!(matches!(error, DriverError::TimedOut));
    assert!(!state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("outcome event");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::TimedOut);
    assert_eq!(outcome.error_category, Some(AuditErrorCategory::Timeout));
}

#[tokio::test]
async fn controlled_unknown_outcome_records_unknown_and_poisons_state() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(ControlledWriteConn { outcome_unknown: true }),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let control = OperationControl::new(Default::default(), None);

    let error = guard
        .execute_controlled("INSERT INTO jobs(id) VALUES (1)", &control)
        .await
        .expect_err("unknown outcome must surface");

    assert!(matches!(error, DriverError::OperationOutcomeUnknown { .. }));
    assert!(state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("outcome event");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::Unknown);
    assert_eq!(outcome.error_category, Some(AuditErrorCategory::Unknown));
}

#[tokio::test]
async fn ambiguous_driver_error_records_unknown_and_poisons_state() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(AmbiguousWriteConn),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );

    guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("TLS error must surface");

    assert!(state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("outcome event");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::Unknown);
    assert_eq!(outcome.error_category, Some(AuditErrorCategory::Tls));
}

#[tokio::test]
async fn audit_events_sanitize_driver_details_and_agent_token() {
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(AmbiguousWriteConn),
        context(
            Principal::Agent {
                token: "Bearer raw-agent-token".into(),
                client: Some("review-test".into()),
                model: None,
            },
            Environment::Local,
            allowed_agent_policy(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            Arc::new(AuditState::new()),
        ),
    );

    guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("TLS error must surface");

    let events = audit.events.lock().expect("event lock");
    let serialized = serde_json::to_string(&*events).expect("serialize events");
    assert!(!serialized.contains("raw-agent-token"));
    assert!(!serialized.contains("raw-driver-secret"));
    assert!(serialized.contains("sha256:"));
    assert!(serialized.contains("tls_error"));
}

#[tokio::test]
async fn ambiguous_commit_records_unknown_transaction_outcome() {
    let state = Arc::new(AuditState::new());
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        Arc::new(AmbiguousWriteConn),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            state.clone(),
        ),
    );
    let transaction = guard.begin().await.expect("begin");

    transaction.commit().await.expect_err("commit must be ambiguous");

    assert!(state.governed_writes_disabled());
    let events = audit.events.lock().expect("event lock");
    let outcome = events.last().expect("commit outcome");
    assert_eq!(outcome.terminal_status, AuditTerminalStatus::Unknown);
    assert_eq!(outcome.transaction_outcome, AuditTransactionOutcome::Unknown);
}

#[tokio::test]
async fn commit_intent_failure_prevents_inner_commit() {
    let commits = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Intent]));
    let guard = PolicyGuard::new(
        connection(Arc::new(AtomicUsize::new(0)), commits.clone()),
        context(
            Principal::human_gui(),
            Environment::Local,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit,
            Arc::new(AuditState::new()),
        ),
    );
    let transaction = guard.begin().await.expect("begin");

    let error = transaction.commit().await.expect_err("commit intent must fail");

    assert!(
        error
            .to_string()
            .contains("commit denied because audit intent could not be persisted")
    );
    assert_eq!(commits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rollback_uses_distinct_operation_class() {
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        connection(Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            Arc::new(AuditState::new()),
        ),
    );
    let transaction = guard.begin().await.expect("begin");

    transaction.rollback().await.expect("rollback");

    let events = audit.events.lock().expect("event lock");
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.operation_class == AuditOperationClass::TransactionRollback)
    );
    assert_eq!(events[0].phase, AuditRecordPhase::Intent);
    assert_eq!(events[1].transaction_outcome, AuditTransactionOutcome::RolledBack);
}

#[tokio::test]
async fn agent_read_fails_when_outcome_cannot_be_recorded() {
    let audit = Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Outcome]));
    let guard = PolicyGuard::new(
        connection(Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::Agent {
                token: "token".into(),
                client: None,
                model: None,
            },
            Environment::Local,
            allowed_agent_policy(),
            Arc::new(DenyApprovalSink),
            audit,
            Arc::new(AuditState::new()),
        ),
    );

    let error = guard
        .query("SELECT 1")
        .await
        .expect_err("agent audit failure must surface");

    assert!(
        error
            .to_string()
            .contains("audit recording failed after read execution")
    );
}

#[tokio::test]
async fn batch_records_correlated_redacted_intent_and_outcome() {
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        connection(Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            Arc::new(AuditState::new()),
        ),
    );
    let statements = vec![("INSERT INTO jobs(secret) VALUES ('raw-secret')".into(), vec![])];

    guard.execute_in_transaction(&statements).await.expect("execute batch");

    let events = audit.events.lock().expect("event lock");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].operation_id, events[1].operation_id);
    assert_eq!(events[0].batch_id, events[1].batch_id);
    assert_eq!(events[0].redacted_sql, "[REDACTED]");
    assert!(!events[0].redacted_sql.contains("raw-secret"));
    assert_eq!(events[0].sql_hash.len(), 64);
}

#[tokio::test]
async fn approve_once_authorizes_only_the_current_operation() {
    let executes = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(SequenceApprovalSink {
                calls: approvals.clone(),
                outcomes: Mutex::new(vec![ApprovalOutcome::AllowOnce, ApprovalOutcome::Deny]),
            }),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::new()),
        ),
    );

    guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect("first operation should be approved");
    guard
        .execute("INSERT INTO jobs(id) VALUES (2)")
        .await
        .expect_err("second operation must request approval again");

    assert_eq!(approvals.load(Ordering::SeqCst), 2);
    assert_eq!(executes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transaction_batch_requests_one_approval() {
    let executes = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(CountingApprovalSink {
                calls: approvals.clone(),
            }),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::new()),
        ),
    );
    let statements = vec![
        ("INSERT INTO jobs(id) VALUES (1)".into(), vec![]),
        ("INSERT INTO jobs(id) VALUES (2)".into(), vec![]),
    ];

    let rows = guard.execute_in_transaction(&statements).await.expect("approved batch");

    assert_eq!(rows, vec![1, 1]);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(executes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn local_unaudited_write_requires_explicit_opt_in() {
    let denied_executes = Arc::new(AtomicUsize::new(0));
    let denied = PolicyGuard::new(
        connection(denied_executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Local,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(NullAuditSink),
            Arc::new(AuditState::new()),
        ),
    );
    denied
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("default local policy must require audit");
    assert_eq!(denied_executes.load(Ordering::SeqCst), 0);

    let allowed_executes = Arc::new(AtomicUsize::new(0));
    let mut policy = PolicyConfig::default();
    policy
        .environments
        .entry("local".into())
        .or_default()
        .human_allow_unaudited_writes = Some(true);
    let allowed = PolicyGuard::new(
        connection(allowed_executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Local,
            policy,
            Arc::new(AutoApproveSink),
            Arc::new(NullAuditSink),
            Arc::new(AuditState::new()),
        ),
    );
    let error = allowed
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("failed outcome must report uncertain execution");
    assert!(error.to_string().contains("operation may have succeeded"));
    assert_eq!(allowed_executes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn null_sink_denies_production_write() {
    let executes = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(NullAuditSink),
            Arc::new(AuditState::new()),
        ),
    );

    guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("null sink must deny");

    assert_eq!(executes.load(Ordering::SeqCst), 0);
}
