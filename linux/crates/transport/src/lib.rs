use secrecy::SecretString;
use tablepro_core::{AuthMode, ConnectOptions, Connection, DatabaseDriver, DriverError, TlsConfig, TlsMode};
use tablepro_ssh::{SshAuth, SshConfig, SshTunnel};
use tablepro_storage::{
    SavedConnection, SavedSshAuth, SavedSshConfig, load_password, load_ssh_passphrase, load_ssh_password,
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("ssh: {0}")]
    Ssh(String),
    #[error("{0}")]
    Secret(String),
    #[error("integrated authentication is not supported by the {0} driver")]
    IntegratedAuthUnsupported(String),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// Choose the process-wide rustls crypto provider.
///
/// The static drivers pull in both `ring` (MySQL, SQL Server, MongoDB) and
/// `aws-lc-rs` (ClickHouse), which leaves rustls unable to pick a default on
/// its own. Any library that builds a `ClientConfig` without naming a provider
/// then panics at connect time. Every composition root, including test
/// binaries that link more than one driver, must call this before connecting.
/// Calling it more than once is harmless: the first call wins.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub fn tls_config(mode: TlsMode) -> TlsConfig {
    TlsConfig {
        mode,
        ..Default::default()
    }
}

pub async fn connect_options_for(saved: &SavedConnection) -> Result<ConnectOptions, TransportError> {
    let password = match saved.auth_mode {
        AuthMode::Password => load_password(saved.id)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| SecretString::new(String::new().into())),
        AuthMode::Kerberos => SecretString::new(String::new().into()),
    };
    Ok(ConnectOptions {
        host: saved.host.clone(),
        port: saved.port,
        database: saved.database.clone(),
        username: saved.username.clone(),
        password,
        tls: TlsConfig {
            mode: saved.effective_tls_mode(),
            root_cert: saved.tls_root_cert.clone(),
            ..Default::default()
        },
        auth_mode: saved.auth_mode,
        service_endpoint: None,
        forwarded_socket_dir: None,
    })
}

pub async fn saved_ssh_chain(saved: &SavedConnection) -> Result<Option<Vec<SshConfig>>, TransportError> {
    let Some(ssh) = &saved.ssh else {
        return Ok(None);
    };
    resolve_saved_ssh_chain(saved.id, ssh).await.map(Some)
}

pub async fn establish(
    driver: &dyn DatabaseDriver,
    mut opts: ConnectOptions,
    ssh: Option<Vec<SshConfig>>,
) -> Result<(Box<dyn Connection>, Option<SshTunnel>), TransportError> {
    check_auth_mode(opts.auth_mode, driver.supports_integrated_auth(), driver.display_name())?;
    opts.forwarded_socket_dir = None;

    let tunnel = if let Some(hops) = ssh {
        if hops.is_empty() {
            return Err(TransportError::Ssh("jump chain is empty".into()));
        }
        let remote = (std::mem::take(&mut opts.host), opts.port);
        let socket_name = forwarded_socket_name(driver, opts.tls.mode, remote.1);
        let tun = match &socket_name {
            Some(name) => SshTunnel::open_chain_socket(&hops, remote.0.clone(), remote.1, name).await,
            None => SshTunnel::open_chain(&hops, remote.0.clone(), remote.1).await,
        }
        .map_err(|e| TransportError::Ssh(e.to_string()))?;
        match tun.socket_dir() {
            Some(directory) => forward_through_socket(&mut opts, remote, directory.to_path_buf()),
            None => redirect_through_tunnel(&mut opts, remote, (tun.local_host().to_string(), tun.local_port())),
        }
        Some(tun)
    } else {
        None
    };
    let raw = driver.connect(opts).await?;
    Ok((raw, tunnel))
}

fn redirect_through_tunnel(opts: &mut ConnectOptions, service_endpoint: (String, u16), dial_endpoint: (String, u16)) {
    opts.service_endpoint = Some(service_endpoint);
    opts.host = dial_endpoint.0;
    opts.port = dial_endpoint.1;
    opts.forwarded_socket_dir = None;
}

fn forward_through_socket(opts: &mut ConnectOptions, service_endpoint: (String, u16), directory: std::path::PathBuf) {
    opts.host = service_endpoint.0.clone();
    opts.port = service_endpoint.1;
    opts.service_endpoint = Some(service_endpoint);
    opts.forwarded_socket_dir = Some(directory);
}

fn forwarded_socket_name(driver: &dyn DatabaseDriver, tls_mode: TlsMode, service_port: u16) -> Option<String> {
    if !tls_mode.verifies_cert() {
        return None;
    }
    driver.forwarded_socket_name(service_port)
}

fn check_auth_mode(mode: AuthMode, supports_integrated_auth: bool, driver_name: &str) -> Result<(), TransportError> {
    if mode == AuthMode::Kerberos && !supports_integrated_auth {
        return Err(TransportError::IntegratedAuthUnsupported(driver_name.to_string()));
    }
    Ok(())
}

async fn resolve_saved_ssh_chain(id: Uuid, saved: &SavedSshConfig) -> Result<Vec<SshConfig>, TransportError> {
    let hops = saved.flatten_hops();
    let mut out = Vec::with_capacity(hops.len());
    for (index, hop) in hops.into_iter().enumerate() {
        out.push(resolve_saved_ssh_hop(id, hop, index).await?);
    }
    Ok(out)
}

async fn resolve_saved_ssh_hop(
    id: Uuid,
    saved: &SavedSshConfig,
    hop_index: usize,
) -> Result<SshConfig, TransportError> {
    let auth = match &saved.auth {
        SavedSshAuth::Password => {
            let pw = if hop_index == 0 {
                load_ssh_password(id)
                    .await
                    .map_err(|e| TransportError::Secret(format!("load ssh password: {e}")))?
                    .ok_or_else(|| TransportError::Secret("ssh password not in keyring".into()))?
            } else {
                load_ssh_password(id)
                    .await
                    .map_err(|e| TransportError::Secret(format!("load ssh password for hop {hop_index}: {e}")))?
                    .ok_or_else(|| {
                        TransportError::Secret(format!(
                            "ssh password for jump hop {hop_index} not in keyring \
                             (edit connections.json jump auth to use a private key, or store the hop-0 password)"
                        ))
                    })?
            };
            SshAuth::Password { password: pw }
        }
        SavedSshAuth::PrivateKey { path, has_passphrase } => {
            let passphrase = if *has_passphrase {
                load_ssh_passphrase(id)
                    .await
                    .map_err(|e| TransportError::Secret(format!("load ssh passphrase: {e}")))?
            } else {
                None
            };
            SshAuth::PrivateKey {
                path: path.clone(),
                passphrase,
            }
        }
    };
    Ok(SshConfig {
        host: saved.host.clone(),
        port: saved.port,
        username: saved.username.clone(),
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_uses_local_dial_endpoint_and_keeps_service_identity() {
        let mut opts = ConnectOptions {
            host: "sql.corp.example".into(),
            port: 1433,
            ..Default::default()
        };

        redirect_through_tunnel(
            &mut opts,
            ("sql.corp.example".into(), 1433),
            ("127.0.0.1".into(), 54321),
        );

        assert_eq!(opts.host, "127.0.0.1");
        assert_eq!(opts.port, 54321);
        assert_eq!(opts.service_address(), ("sql.corp.example", 1433));
    }

    struct SocketDriver;

    #[async_trait::async_trait]
    impl DatabaseDriver for SocketDriver {
        fn id(&self) -> &'static str {
            "socket"
        }
        fn display_name(&self) -> &'static str {
            "Socket"
        }
        fn default_port(&self) -> u16 {
            5432
        }
        fn forwarded_socket_name(&self, service_port: u16) -> Option<String> {
            Some(format!(".s.PGSQL.{service_port}"))
        }
        async fn connect(&self, _opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
            Err(DriverError::Unsupported("test driver".into()))
        }
    }

    struct TcpOnlyDriver;

    #[async_trait::async_trait]
    impl DatabaseDriver for TcpOnlyDriver {
        fn id(&self) -> &'static str {
            "tcp-only"
        }
        fn display_name(&self) -> &'static str {
            "TCP only"
        }
        fn default_port(&self) -> u16 {
            3306
        }
        async fn connect(&self, _opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
            Err(DriverError::Unsupported("test driver".into()))
        }
    }

    #[test]
    fn socket_forwarding_applies_only_when_the_mode_verifies_certificates() {
        assert_eq!(
            forwarded_socket_name(&SocketDriver, TlsMode::VerifyFull, 5432).as_deref(),
            Some(".s.PGSQL.5432")
        );
        assert_eq!(
            forwarded_socket_name(&SocketDriver, TlsMode::VerifyCa, 5432).as_deref(),
            Some(".s.PGSQL.5432")
        );
        assert!(forwarded_socket_name(&SocketDriver, TlsMode::Require, 5432).is_none());
        assert!(forwarded_socket_name(&SocketDriver, TlsMode::Disabled, 5432).is_none());
        assert!(forwarded_socket_name(&TcpOnlyDriver, TlsMode::VerifyFull, 3306).is_none());
    }

    #[test]
    fn socket_forwarding_keeps_the_service_hostname_for_tls() {
        let mut opts = ConnectOptions {
            host: String::new(),
            port: 5432,
            tls: tls_config(TlsMode::VerifyFull),
            ..Default::default()
        };

        forward_through_socket(
            &mut opts,
            ("db.corp.example".into(), 5432),
            std::path::PathBuf::from("/run/user/1000/tablepro-ssh-abc"),
        );

        assert_eq!(opts.service_address(), ("db.corp.example", 5432));
        assert_eq!(
            opts.transport(),
            tablepro_core::Transport::Socket {
                directory: std::path::Path::new("/run/user/1000/tablepro-ssh-abc"),
                identity_host: "db.corp.example",
                identity_port: 5432,
            }
        );
    }

    #[test]
    fn tcp_forwarding_clears_any_earlier_socket_directory() {
        let mut opts = ConnectOptions {
            host: "db.corp.example".into(),
            port: 5432,
            forwarded_socket_dir: Some(std::path::PathBuf::from("/run/user/1000/stale")),
            ..Default::default()
        };

        redirect_through_tunnel(&mut opts, ("db.corp.example".into(), 5432), ("127.0.0.1".into(), 54321));

        assert!(opts.forwarded_socket_dir.is_none());
        assert_eq!(
            opts.transport(),
            tablepro_core::Transport::Tcp {
                host: "127.0.0.1",
                port: 54321
            }
        );
    }

    #[tokio::test]
    async fn establish_never_reuses_a_socket_directory_from_an_earlier_attempt() {
        let opts = ConnectOptions {
            host: "db.corp.example".into(),
            port: 5432,
            forwarded_socket_dir: Some(std::path::PathBuf::from("/run/user/1000/removed")),
            ..Default::default()
        };

        let error = establish(&SocketDriver, opts, None)
            .await
            .err()
            .expect("test driver refuses to connect");

        assert!(error.to_string().contains("test driver"), "unexpected error: {error}");
    }

    #[test]
    fn kerberos_requires_driver_support() {
        assert!(check_auth_mode(AuthMode::Kerberos, false, "PostgreSQL").is_err());
        assert!(check_auth_mode(AuthMode::Kerberos, true, "SQL Server").is_ok());
        assert!(check_auth_mode(AuthMode::Password, false, "PostgreSQL").is_ok());
    }
}
