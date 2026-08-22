//! `establish` refuses several combinations before it ever dials. Each
//! one exists because the alternative silently weakens a guarantee the
//! user asked for, so the refusals are as much a part of the contract
//! as the successful path.

use std::path::PathBuf;

use tablepro_transport::TransportError;

use async_trait::async_trait;
use tablepro_core::{AuthMode, ConnectOptions, Connection, DatabaseDriver, DriverError, TlsConfig, TlsMode};
use tablepro_ssh::{SshAuth, SshConfig};

struct FakeDriver {
    integrated_auth: bool,
    local_socket: bool,
}

impl FakeDriver {
    fn plain() -> Self {
        Self {
            integrated_auth: false,
            local_socket: false,
        }
    }

    fn with_local_socket() -> Self {
        Self {
            integrated_auth: false,
            local_socket: true,
        }
    }
}

#[async_trait]
impl DatabaseDriver for FakeDriver {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn display_name(&self) -> &'static str {
        "Fake"
    }

    fn default_port(&self) -> u16 {
        5432
    }

    fn supports_integrated_auth(&self) -> bool {
        self.integrated_auth
    }

    fn supports_local_socket(&self) -> bool {
        self.local_socket
    }

    async fn connect(&self, _opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        panic!("a refused connection must never reach the driver");
    }
}

fn options() -> ConnectOptions {
    ConnectOptions {
        host: "db.example".into(),
        port: 5432,
        database: "app".into(),
        username: "postgres".into(),
        tls: TlsConfig::disabled(),
        ..Default::default()
    }
}

fn socket_options() -> ConnectOptions {
    ConnectOptions {
        local_socket_dir: Some(PathBuf::from("/run/postgresql")),
        ..options()
    }
}

fn hop() -> SshConfig {
    SshConfig {
        host: "bastion.example".into(),
        port: 22,
        username: "jump".into(),
        auth: SshAuth::Password {
            password: secrecy::SecretString::new("unused".to_string().into()),
        },
    }
}

/// `establish` returns an opened connection on success, and neither a
/// boxed `Connection` nor an `SshTunnel` is `Debug`, so the refusal has
/// to be unwrapped by hand rather than with `expect_err`.
async fn refusal(
    driver: &dyn DatabaseDriver,
    opts: ConnectOptions,
    ssh: Option<Vec<SshConfig>>,
    what: &str,
) -> TransportError {
    match tablepro_transport::establish(driver, opts, ssh).await {
        Ok(_) => panic!("{what}"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn integrated_authentication_is_refused_by_a_driver_that_cannot_do_it() {
    let error = refusal(
        &FakeDriver::plain(),
        ConnectOptions {
            auth_mode: AuthMode::Kerberos,
            ..options()
        },
        None,
        "a driver without integrated auth must refuse it",
    )
    .await;
    assert!(
        format!("{error}").contains("Fake"),
        "the message must name the driver: {error}"
    );
}

#[tokio::test]
async fn a_local_socket_is_refused_by_a_driver_that_cannot_do_it() {
    let error = refusal(
        &FakeDriver::plain(),
        socket_options(),
        None,
        "a driver without local-socket support must refuse one",
    )
    .await;
    assert!(matches!(error, TransportError::LocalSocketUnsupported(_)), "{error:?}");
}

#[tokio::test]
async fn a_local_socket_cannot_be_combined_with_ssh() {
    let error = refusal(
        &FakeDriver::with_local_socket(),
        socket_options(),
        Some(vec![hop()]),
        "a forwarded socket and a local socket are different transports",
    )
    .await;
    assert!(matches!(error, TransportError::LocalSocketWithSsh), "{error:?}");
}

#[tokio::test]
async fn a_local_socket_cannot_be_combined_with_tls() {
    let error = refusal(
        &FakeDriver::with_local_socket(),
        ConnectOptions {
            tls: TlsConfig {
                mode: TlsMode::VerifyFull,
                ..TlsConfig::disabled()
            },
            ..socket_options()
        },
        None,
        "a Unix socket has no certificate to verify, so asking for one must not be ignored",
    )
    .await;
    assert!(matches!(error, TransportError::LocalSocketWithTls), "{error:?}");
}

#[tokio::test]
async fn a_relative_socket_directory_is_refused() {
    let error = refusal(
        &FakeDriver::with_local_socket(),
        ConnectOptions {
            local_socket_dir: Some(PathBuf::from("relative/path")),
            ..options()
        },
        None,
        "a relative socket path depends on the process working directory",
    )
    .await;
    assert!(matches!(error, TransportError::InvalidLocalSocket(_)), "{error:?}");
}

#[tokio::test]
async fn a_zero_socket_port_is_refused() {
    let error = refusal(
        &FakeDriver::with_local_socket(),
        ConnectOptions {
            port: 0,
            ..socket_options()
        },
        None,
        "the socket file name is derived from the port",
    )
    .await;
    assert!(matches!(error, TransportError::InvalidLocalSocket(_)), "{error:?}");
}

#[tokio::test]
async fn an_empty_jump_chain_is_refused_rather_than_treated_as_a_direct_connection() {
    let error = refusal(
        &FakeDriver::plain(),
        options(),
        Some(Vec::new()),
        "an empty chain must not silently become a direct dial",
    )
    .await;
    assert!(format!("{error}").contains("chain"), "{error}");
}
