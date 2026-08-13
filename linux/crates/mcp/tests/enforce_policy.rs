use std::sync::Arc;

use async_trait::async_trait;
use tablepro_core::{
    ColumnInfo, Connection, DriverError, ExecResult, ForeignKeyInfo, IndexInfo, QueryResult, TableInfo, Value,
};
use tablepro_policy::Principal;
use tablepro_storage::SavedConnection;
use uuid::Uuid;

use tablepro_mcp::{ConnectionProvider, McpBridge, TokenPermissions, TokenStore};

/// Integration-style test: every tool path that touches a connection must
/// go through the provider (which in production wraps PolicyGuard) and
/// leave a journalled policy decision when PolicyGuard is used.
#[tokio::test]
async fn tool_path_requires_provider_and_token() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let conn_id = Uuid::nil();
    let (_meta, plain) = store
        .issue("test".into(), TokenPermissions::ReadWrite, vec![conn_id], None)
        .unwrap();

    let provider = Arc::new(RecordingProvider::default());
    let bridge = McpBridge::new(provider.clone(), store);
    let token = bridge.authenticate(&plain).unwrap();

    let result = bridge.execute_query(&token, conn_id, "SELECT 1").await;
    assert!(
        provider.connection_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "provider.connection must be called; result={result:?}"
    );
    assert!(result.is_ok(), "stub query should succeed: {result:?}");

    // Without a token, authenticate fails — no further connection touch.
    assert!(bridge.authenticate("bad-token").is_err());
}

#[tokio::test]
async fn write_without_tools_write_scope_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let conn_id = Uuid::nil();
    let (_meta, plain) = store
        .issue("ro".into(), TokenPermissions::ReadOnly, vec![conn_id], None)
        .unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let bridge = McpBridge::new(provider, store);
    let token = bridge.authenticate(&plain).unwrap();
    let err = bridge
        .execute_write(&token, conn_id, "DELETE FROM t", false)
        .await
        .unwrap_err();
    assert!(err.contains("scope") || err.contains("tools"), "{err}");
}

#[tokio::test]
async fn empty_allowlist_denies_connection_access() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let (_meta, plain) = store
        .issue("open".into(), TokenPermissions::ReadWrite, vec![], None)
        .unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let bridge = McpBridge::new(provider.clone(), store);
    let token = bridge.authenticate(&plain).unwrap();
    let err = bridge.execute_query(&token, Uuid::nil(), "SELECT 1").await.unwrap_err();
    assert!(err.contains("allowlist"), "empty allowlist must fail closed: {err}");
    assert_eq!(provider.connection_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn read_only_token_cannot_call_administrative_function() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let connection_id = Uuid::nil();
    let (_metadata, plaintext) = store
        .issue(
            "read-only".into(),
            TokenPermissions::ReadOnly,
            vec![connection_id],
            None,
        )
        .unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let bridge = McpBridge::new(provider.clone(), store);
    let token = bridge.authenticate(&plaintext).unwrap();

    let error = bridge
        .execute_query(&token, connection_id, "SELECT pg_terminate_backend(42)")
        .await
        .unwrap_err();

    assert!(
        error.contains("scope"),
        "administrative query must require write scope: {error}"
    );
    assert_eq!(provider.connection_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn guarded_tool_path_journals_policy_decision() {
    use tablepro_core::Environment;
    use tablepro_policy::{AuditSink, AuditState, AutoApproveSink, GuardContext, PolicyConfig, PolicyGuard, Principal};
    use tablepro_storage::AuditJournal;

    let dir = tempfile::TempDir::new().unwrap();
    let journal_path = dir.path().join("audit.jsonl");
    let journal = Arc::new(AuditJournal::open(journal_path.clone()));
    let store = Arc::new(TokenStore::open(dir.path().join("tokens.json")).unwrap());
    let conn_id = Uuid::new_v4();
    let (_meta, plain) = store
        .issue("test".into(), TokenPermissions::ReadWrite, vec![conn_id], None)
        .unwrap();

    struct GuardedProvider {
        journal: Arc<AuditJournal>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ConnectionProvider for GuardedProvider {
        async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
            Ok(vec![])
        }
        async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let raw: Arc<dyn Connection> = Arc::new(StubConn);
            let ctx = GuardContext {
                connection_id,
                connection_name: "test".into(),
                driver_id: "sqlite".into(),
                environment: Environment::Local,
                read_only: false,
                principal,
                policy: Arc::new(PolicyConfig::default()),
                approval: Arc::new(AutoApproveSink),
                audit: self.journal.clone() as Arc<dyn AuditSink>,
                audit_state: Arc::new(AuditState::new()),
            };
            Ok(Arc::new(PolicyGuard::new(raw, ctx)) as Arc<dyn Connection>)
        }
    }

    let provider = Arc::new(GuardedProvider {
        journal: journal.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let bridge = McpBridge::new(provider.clone(), store);
    let token = bridge.authenticate(&plain).unwrap();
    let _ = bridge
        .execute_query(&token, conn_id, "SELECT 1")
        .await
        .expect("select through guard");
    assert!(provider.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    let text = std::fs::read_to_string(&journal_path).unwrap_or_default();
    assert!(
        !text.trim().is_empty(),
        "expected at least one journalled policy decision, got empty journal"
    );
}

#[derive(Default)]
struct RecordingProvider {
    connection_calls: std::sync::atomic::AtomicUsize,
}

struct StubConn;

#[async_trait]
impl Connection for StubConn {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        Ok(vec![])
    }
    async fn fetch_columns(&self, _: Option<&str>, _: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        Ok(vec![])
    }
    async fn fetch_rows(&self, _: Option<&str>, _: &str, _: u64, _: u64) -> Result<QueryResult, DriverError> {
        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            truncated: false,
        })
    }
    async fn query(&self, _: &str) -> Result<QueryResult, DriverError> {
        Ok(QueryResult {
            columns: vec![],
            rows: vec![vec![Value::Int(1)]],
            truncated: false,
        })
    }
    async fn execute(&self, _: &str) -> Result<ExecResult, DriverError> {
        Err(DriverError::PolicyDenied("journalled deny".into()))
    }
    async fn execute_params(&self, _: &str, _: &[Value]) -> Result<ExecResult, DriverError> {
        Err(DriverError::PolicyDenied("journalled deny".into()))
    }
    async fn execute_in_transaction(&self, _: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        Err(DriverError::PolicyDenied("journalled deny".into()))
    }
    async fn fetch_indexes(&self, _: Option<&str>, _: &str) -> Result<Vec<IndexInfo>, DriverError> {
        Ok(vec![])
    }
    async fn fetch_foreign_keys(&self, _: Option<&str>, _: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        Ok(vec![])
    }
    async fn ping(&self) -> Result<(), DriverError> {
        Ok(())
    }
    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl ConnectionProvider for RecordingProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        Ok(vec![])
    }
    async fn connection(&self, _connection_id: Uuid, _principal: Principal) -> Result<Arc<dyn Connection>, String> {
        self.connection_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(StubConn))
    }
}
