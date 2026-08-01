use std::sync::Arc;

use secrecy::SecretString;
use tablepro_core::{ConnectOptions, Connection, DriverRegistry, TlsConfig, TlsMode};
use tablepro_ssh::{SshConfig, SshTunnel};
use tablepro_storage::{
    SavedConnection, SavedSshAuth, SavedSshConfig, load_password, load_ssh_passphrase, load_ssh_password,
};

use super::database_service::{self, ConnectionMetadata, ReconnectParams};

pub async fn open_saved(
    registry: Arc<DriverRegistry>,
    saved: SavedConnection,
) -> Result<Vec<tablepro_core::TableInfo>, String> {
    let driver = registry
        .get(&saved.driver_id)
        .ok_or_else(|| format!("driver {} not registered", saved.driver_id))?;
    let password = load_password(saved.id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| SecretString::new(String::new().into()));
    let id = saved.id;
    let environment = saved.environment;
    let read_only = saved.read_only;
    let tls_mode = saved.effective_tls_mode();

    let ssh_hops = match &saved.ssh {
        Some(ssh) => Some(resolve_saved_ssh_chain(id, ssh).await?),
        None => None,
    };

    let opts = ConnectOptions {
        host: saved.host,
        port: saved.port,
        database: saved.database,
        username: saved.username,
        password,
        tls: TlsConfig {
            mode: tls_mode,
            ..Default::default()
        },
    };

    let (conn, tunnel) = establish(&*driver, opts.clone(), ssh_hops.clone()).await?;
    let server_version = conn.server_version().await.ok().flatten();
    let tables = conn.list_tables().await.map_err(|e| format!("list_tables: {e}"))?;
    let metadata = ConnectionMetadata {
        id,
        name: saved.name.clone(),
        driver_id: saved.driver_id.clone(),
        environment,
        read_only,
        server_version,
    };
    let params = ReconnectParams {
        driver,
        opts,
        ssh: ssh_hops,
    };
    database_service::instance().add(id, metadata, conn, tunnel, read_only, params);
    Ok(tables)
}

pub async fn establish(
    driver: &dyn tablepro_core::DatabaseDriver,
    mut opts: ConnectOptions,
    ssh: Option<Vec<SshConfig>>,
) -> Result<(Box<dyn Connection>, Option<SshTunnel>), String> {
    let tunnel = if let Some(hops) = ssh {
        if hops.is_empty() {
            return Err("ssh: jump chain is empty".into());
        }
        let remote_host = std::mem::take(&mut opts.host);
        let remote_port = opts.port;
        let tun = SshTunnel::open_chain(&hops, remote_host, remote_port)
            .await
            .map_err(|e| format!("ssh: {e}"))?;
        opts.host = tun.local_host().to_string();
        opts.port = tun.local_port();
        Some(tun)
    } else {
        None
    };
    let raw = driver.connect(opts).await.map_err(|e| format!("connect: {e}"))?;
    Ok((raw, tunnel))
}

async fn resolve_saved_ssh_chain(id: uuid::Uuid, saved: &SavedSshConfig) -> Result<Vec<SshConfig>, String> {
    let hops = saved.flatten_hops();
    let mut out = Vec::with_capacity(hops.len());
    for (index, hop) in hops.into_iter().enumerate() {
        out.push(resolve_saved_ssh_hop(id, hop, index).await?);
    }
    Ok(out)
}

async fn resolve_saved_ssh_hop(id: uuid::Uuid, saved: &SavedSshConfig, hop_index: usize) -> Result<SshConfig, String> {
    let auth = match &saved.auth {
        SavedSshAuth::Password => {
            // Hop 0 uses the legacy keyring slot; deeper hops share the
            // same password only when configured that way in JSON (UI
            // does not yet collect per-hop secrets).
            let pw = if hop_index == 0 {
                load_ssh_password(id)
                    .await
                    .map_err(|e| format!("load ssh password: {e}"))?
                    .ok_or_else(|| "ssh password not in keyring".to_string())?
            } else {
                load_ssh_password(id)
                    .await
                    .map_err(|e| format!("load ssh password for hop {hop_index}: {e}"))?
                    .ok_or_else(|| {
                        format!(
                            "ssh password for jump hop {hop_index} not in keyring \
                             (edit connections.json jump auth to use a private key, or store the hop-0 password)"
                        )
                    })?
            };
            tablepro_ssh::SshAuth::Password { password: pw }
        }
        SavedSshAuth::PrivateKey { path, has_passphrase } => {
            let passphrase = if *has_passphrase {
                load_ssh_passphrase(id)
                    .await
                    .map_err(|e| format!("load ssh passphrase: {e}"))?
            } else {
                None
            };
            tablepro_ssh::SshAuth::PrivateKey {
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

pub fn tls_from_toggle(enabled: bool) -> TlsConfig {
    TlsConfig {
        mode: if enabled {
            TlsMode::VerifyFull
        } else {
            TlsMode::Disabled
        },
        ..Default::default()
    }
}
