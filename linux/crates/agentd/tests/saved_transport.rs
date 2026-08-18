use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tablepro_agentd::DaemonProvider;
use tablepro_core::{
    AuthMode, ConnectOptions, Connection, DatabaseDriver, DriverError, DriverRegistry, Environment, TlsMode,
};
use tablepro_mcp::ConnectionProvider;
use tablepro_policy::{AuditState, DenyApprovalSink, PolicyConfig, Principal};
use tablepro_storage::{AuditJournal, SavedConnection, SavedSshAuth, SavedSshConfig, save_connections};
use uuid::Uuid;

struct CountingDriver {
    dials: Arc<AtomicUsize>,
}

#[async_trait]
impl DatabaseDriver for CountingDriver {
    fn id(&self) -> &'static str {
        "postgres"
    }
    fn display_name(&self) -> &'static str {
        "Counting"
    }
    fn default_port(&self) -> u16 {
        5432
    }
    async fn connect(&self, _opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        Err(DriverError::Unsupported("counting driver never connects".into()))
    }
}

fn saved_with_bastion(host: &str, bastion_port: u16) -> SavedConnection {
    SavedConnection {
        id: Uuid::new_v4(),
        name: "Tunnelled warehouse".into(),
        driver_id: "postgres".into(),
        host: host.into(),
        port: 5432,
        database: "warehouse".into(),
        username: "reader".into(),
        use_tls: false,
        tls_mode: Some(TlsMode::VerifyFull),
        read_only: true,
        auth_mode: AuthMode::Password,
        environment: Environment::Prod,
        ssh: Some(SavedSshConfig {
            host: "127.0.0.1".into(),
            port: bastion_port,
            username: "jump".into(),
            auth: SavedSshAuth::PrivateKey {
                path: PathBuf::from("/nonexistent/id_ed25519"),
                has_passphrase: false,
            },
            jump: None,
        }),
        last_opened_at: None,
    }
}

fn provider(dials: Arc<AtomicUsize>, journal: &tempfile::TempDir) -> DaemonProvider {
    let mut registry = DriverRegistry::new();
    registry.register(Arc::new(CountingDriver { dials }));
    let audit =
        AuditJournal::open_validated(journal.path().join("audit.jsonl")).expect("open a temporary audit journal");
    DaemonProvider::new(
        Arc::new(registry),
        Arc::new(PolicyConfig::default()),
        Arc::new(audit),
        Arc::new(AuditState::new()),
        Arc::new(DenyApprovalSink),
    )
}

fn agent() -> Principal {
    Principal::Agent {
        token: "token".into(),
        client: None,
        model: None,
    }
}

#[tokio::test]
async fn a_saved_ssh_hop_is_used_instead_of_dialling_the_database_directly() {
    let config = tempfile::TempDir::new().expect("temporary config directory");
    let journal = tempfile::TempDir::new().expect("temporary journal directory");
    // SAFETY: the storage crate resolves saved connections from this variable
    // and the test process owns it for the duration of this test binary.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", config.path()) };

    let saved = saved_with_bastion("127.0.0.1", 1);
    let connection_id = saved.id;
    save_connections(&[saved]).await.expect("save the fixture connection");

    let dials = Arc::new(AtomicUsize::new(0));
    let provider = provider(dials.clone(), &journal);

    let error = provider
        .connection(connection_id, agent())
        .await
        .err()
        .expect("an unreachable bastion must fail the connection");

    assert!(
        error.starts_with("ssh:"),
        "the failure must come from the tunnel, not the database: {error}"
    );
    assert_eq!(
        dials.load(Ordering::SeqCst),
        0,
        "a connection configured to reach the database through a bastion must never dial it directly"
    );
}
