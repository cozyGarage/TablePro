use std::path::PathBuf;

use tablepro_core::{AuthMode, Environment, TlsMode, Transport};
use tablepro_ssh::SshAuth;
use tablepro_storage::{SavedConnection, SavedSshAuth, SavedSshConfig};
use tablepro_transport::{connect_options_for, saved_ssh_chain};
use uuid::Uuid;

fn saved(tls_mode: Option<TlsMode>, use_tls: bool) -> SavedConnection {
    SavedConnection {
        id: Uuid::new_v4(),
        name: "Warehouse".into(),
        driver_id: "postgres".into(),
        host: "db.corp.example".into(),
        port: 5432,
        socket_dir: None,
        database: "warehouse".into(),
        username: "reader".into(),
        use_tls,
        tls_mode,
        tls_root_cert: None,
        read_only: false,
        auth_mode: AuthMode::Password,
        environment: Environment::Prod,
        ssh: None,
        last_opened_at: None,
    }
}

fn hop(host: &str, port: u16, username: &str, jump: Option<SavedSshConfig>) -> SavedSshConfig {
    SavedSshConfig {
        host: host.into(),
        port,
        username: username.into(),
        auth: SavedSshAuth::PrivateKey {
            path: PathBuf::from("/home/user/.ssh/id_ed25519"),
            has_passphrase: false,
        },
        jump: jump.map(Box::new),
    }
}

#[tokio::test]
async fn a_direct_connection_carries_no_tunnel_state() {
    let connection = saved(Some(TlsMode::VerifyFull), false);
    let opts = connect_options_for(&connection).await.expect("build connect options");

    assert_eq!(opts.host, "db.corp.example");
    assert_eq!(opts.port, 5432);
    assert_eq!(opts.tls.mode, TlsMode::VerifyFull);
    assert!(opts.service_endpoint.is_none());
    assert!(opts.forwarded_socket_dir.is_none());
    assert_eq!(
        opts.transport(),
        Transport::Tcp {
            host: "db.corp.example",
            port: 5432
        }
    );
    assert!(saved_ssh_chain(&connection).await.expect("no ssh chain").is_none());
}

#[tokio::test]
async fn a_saved_certificate_authority_reaches_the_driver() {
    let mut connection = saved(Some(TlsMode::VerifyFull), false);
    connection.tls_root_cert = Some(PathBuf::from("/etc/tablepro/corp-ca.crt"));

    let opts = connect_options_for(&connection).await.expect("build connect options");

    assert_eq!(opts.tls.mode, TlsMode::VerifyFull);
    assert_eq!(
        opts.tls.root_cert.as_deref(),
        Some(std::path::Path::new("/etc/tablepro/corp-ca.crt"))
    );
}

#[tokio::test]
async fn a_saved_local_socket_reaches_gui_and_agent_connection_options() {
    let mut connection = saved(Some(TlsMode::Disabled), false);
    connection.socket_dir = Some(PathBuf::from("/run/postgresql"));

    let opts = connect_options_for(&connection).await.expect("build connect options");

    assert_eq!(opts.local_socket_dir, connection.socket_dir);
    assert!(matches!(
        opts.transport(),
        Transport::Socket {
            origin: tablepro_core::SocketOrigin::Local,
            ..
        }
    ));
}

#[tokio::test]
async fn a_connection_without_a_certificate_authority_uses_the_system_trust_store() {
    let opts = connect_options_for(&saved(Some(TlsMode::VerifyFull), false))
        .await
        .expect("build connect options");
    assert!(opts.tls.root_cert.is_none());
}

#[tokio::test]
async fn a_legacy_tls_flag_still_means_full_verification() {
    let opts = connect_options_for(&saved(None, true))
        .await
        .expect("build connect options");
    assert_eq!(opts.tls.mode, TlsMode::VerifyFull);
}

#[tokio::test]
async fn a_jump_chain_resolves_in_hop_order() {
    let mut connection = saved(Some(TlsMode::VerifyFull), false);
    connection.ssh = Some(hop(
        "bastion.corp.example",
        22,
        "jump",
        Some(hop("inner.corp.example", 2222, "inner", None)),
    ));

    let chain = saved_ssh_chain(&connection)
        .await
        .expect("resolve chain")
        .expect("an ssh chain is configured");

    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].host, "bastion.corp.example");
    assert_eq!(chain[0].port, 22);
    assert_eq!(chain[0].username, "jump");
    assert_eq!(chain[1].host, "inner.corp.example");
    assert_eq!(chain[1].port, 2222);
    assert!(matches!(chain[0].auth, SshAuth::PrivateKey { .. }));
}

#[tokio::test]
async fn a_tunnelled_connection_still_starts_from_the_database_hostname() {
    let mut connection = saved(Some(TlsMode::VerifyFull), false);
    connection.ssh = Some(hop("bastion.corp.example", 22, "jump", None));

    let opts = connect_options_for(&connection).await.expect("build connect options");

    assert_eq!(
        opts.host, "db.corp.example",
        "the tunnel is applied by establish, not by option assembly"
    );
    assert!(opts.forwarded_socket_dir.is_none());
}
