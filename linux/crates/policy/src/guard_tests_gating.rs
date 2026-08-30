use sha2::Digest;

use super::*;

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
async fn blast_radius_count_runs_through_the_controlled_query_path() {
    let queries = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        query_counting_connection(queries.clone()),
        context(
            Principal::human_gui(),
            Environment::Local,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::new()),
        ),
    );

    guard
        .execute("UPDATE jobs SET status = 'done' WHERE id = 1")
        .await
        .expect("local write");

    assert_eq!(queries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_expired_deadline_skips_the_blast_radius_count() {
    let queries = Arc::new(AtomicUsize::new(0));
    let guard = PolicyGuard::new(
        query_counting_connection(queries.clone()),
        context(
            Principal::human_gui(),
            Environment::Local,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            Arc::new(SequenceAuditSink::new(vec![])),
            Arc::new(AuditState::new()),
        ),
    );
    let control = OperationControl::with_timeout(std::time::Duration::ZERO);

    let error = guard
        .execute_controlled("UPDATE jobs SET status = 'done' WHERE id = 1", &control)
        .await
        .expect_err("expired control must refuse the write");

    assert!(
        matches!(error, DriverError::TimedOut) || error.to_string().contains("timed out"),
        "{error}"
    );
    assert_eq!(queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn list_views_records_a_governed_read_outcome() {
    let audit = Arc::new(SequenceAuditSink::new(vec![]));
    let guard = PolicyGuard::new(
        connection(Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))),
        context(
            Principal::human_gui(),
            Environment::Local,
            PolicyConfig::default(),
            Arc::new(AutoApproveSink),
            audit.clone(),
            Arc::new(AuditState::new()),
        ),
    );

    let views = guard.list_views().await.expect("list views");
    assert!(views.is_empty());

    let events = audit.events.lock().expect("event lock");
    let expected_hash = hex::encode(sha2::Sha256::digest(b"LIST VIEWS"));
    assert!(
        events
            .iter()
            .any(|event| event.decision_rule == "metadata_read" && event.sql_hash == expected_hash),
        "list_views must write a metadata audit record"
    );
}
