use super::*;

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
async fn controlled_read_unknown_outcome_does_not_poison_governed_writes() {
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
        .query_controlled("SELECT pg_sleep(30)", &control)
        .await
        .expect_err("unknown read outcome must surface");

    assert!(matches!(error, DriverError::OperationOutcomeUnknown { .. }));
    assert!(!state.governed_writes_disabled());
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

#[tokio::test]
async fn human_policy_deny_fails_closed_when_outcome_cannot_be_recorded() {
    let executes = Arc::new(AtomicUsize::new(0));
    let mut ctx = context(
        Principal::human_gui(),
        Environment::Local,
        PolicyConfig::default(),
        Arc::new(AutoApproveSink),
        Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Outcome])),
        Arc::new(AuditState::new()),
    );
    ctx.read_only = true;
    let guard = PolicyGuard::new(connection(executes.clone(), Arc::new(AtomicUsize::new(0))), ctx);

    let error = guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("unaudited deny must fail closed");

    assert!(error.to_string().contains("audit recording failed"));
    assert_eq!(executes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn human_approval_deny_fails_closed_when_outcome_cannot_be_recorded() {
    let executes = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        connection(executes.clone(), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Prod,
            PolicyConfig::default(),
            Arc::new(DenyApprovalSink),
            Arc::new(SequenceAuditSink::new(vec![AuditRecordPhase::Outcome])),
            Arc::new(AuditState::new()),
        ),
    );

    let error = guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("unaudited approval deny must fail closed");

    assert!(error.to_string().contains("audit recording failed"));
    assert_eq!(executes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn human_policy_deny_keeps_the_policy_message_when_audit_succeeds() {
    let executes = Arc::new(AtomicUsize::new(0));
    let mut ctx = context(
        Principal::human_gui(),
        Environment::Local,
        PolicyConfig::default(),
        Arc::new(AutoApproveSink),
        Arc::new(SequenceAuditSink::new(vec![])),
        Arc::new(AuditState::new()),
    );
    ctx.read_only = true;
    let guard = PolicyGuard::new(connection(executes.clone(), Arc::new(AtomicUsize::new(0))), ctx);

    let error = guard
        .execute("INSERT INTO jobs(id) VALUES (1)")
        .await
        .expect_err("read-only write must be denied");

    assert!(error.to_string().contains("read-only"));
    assert!(!error.to_string().contains("audit recording failed"));
    assert_eq!(executes.load(Ordering::SeqCst), 0);
}
