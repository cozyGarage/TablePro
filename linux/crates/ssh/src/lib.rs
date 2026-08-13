use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use russh::ChannelMsg;
use russh::client::{self, Config, Handle};
use russh::keys::known_hosts::{check_known_hosts_path, known_host_keys_path, learn_known_hosts_path};
use russh::keys::ssh_key::{HashAlg, PublicKey};
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};

const LOCAL_BIND_HOST: &str = "127.0.0.1";

#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
}

#[derive(Debug, Clone)]
pub enum SshAuth {
    Password {
        password: SecretString,
    },
    PrivateKey {
        path: PathBuf,
        passphrase: Option<SecretString>,
    },
}

#[derive(Debug, Error)]
pub enum SshError {
    #[error("connect: {0}")]
    Connect(String),
    #[error("authentication failed")]
    Auth,
    #[error("SSH jump chain requires at least one hop")]
    EmptyChain,
    #[error("read private key {path}: {source}")]
    Key {
        path: PathBuf,
        #[source]
        source: russh::keys::Error,
    },
    #[error("local bind: {0}")]
    Bind(#[source] std::io::Error),
    #[error(
        "host key for {host}:{port} does not match {known_hosts}: stored fingerprint differs (line {line}). \
         If the server was reinstalled, remove the old line; otherwise this may indicate a man-in-the-middle attack. \
         New fingerprint: {new_fingerprint}"
    )]
    HostKeyMismatch {
        host: String,
        port: u16,
        new_fingerprint: String,
        line: usize,
        known_hosts: PathBuf,
    },
    #[error("known_hosts: {0}")]
    KnownHosts(String),
    #[error("ssh: {0}")]
    Ssh(#[from] russh::Error),
}

/// Local TCP listener that forwards through one or more SSH sessions.
///
/// For a jump chain, intermediate hops stay alive as nested
/// [`SshTunnel`] values so each local forward remains reachable.
pub struct SshTunnel {
    local_port: u16,
    cancel: CancellationToken,
    _task: tokio::task::JoinHandle<()>,
    /// Intermediate hop tunnels (dropped after this forwarder cancels).
    _upstream: Vec<SshTunnel>,
}

impl SshTunnel {
    /// Single-hop convenience: tunnel through `cfg` to `remote_host:remote_port`.
    pub async fn open(cfg: SshConfig, remote_host: String, remote_port: u16) -> Result<Self, SshError> {
        Self::open_chain(std::slice::from_ref(&cfg), remote_host, remote_port).await
    }

    /// Multi-hop tunnel. `hops[0]` is the first bastion (TCP connect
    /// goes there directly); `hops[n-1]` is the last jump before the
    /// database. Nested local forwards: hop0 → hop1:22 → … → remote.
    pub async fn open_chain(hops: &[SshConfig], remote_host: String, remote_port: u16) -> Result<Self, SshError> {
        if hops.is_empty() {
            return Err(SshError::EmptyChain);
        }

        let mut upstream: Vec<SshTunnel> = Vec::new();
        let mut tcp_host = hops[0].host.clone();
        let mut tcp_port = hops[0].port;

        for (i, hop) in hops.iter().enumerate() {
            let is_last = i + 1 == hops.len();
            let (fwd_host, fwd_port) = if is_last {
                (remote_host.as_str(), remote_port)
            } else {
                (hops[i + 1].host.as_str(), hops[i + 1].port)
            };

            let mut tunnel = open_single(hop, &tcp_host, tcp_port, fwd_host.to_string(), fwd_port).await?;

            if is_last {
                tunnel._upstream = upstream;
                return Ok(tunnel);
            }

            tcp_host = tunnel.local_host().to_string();
            tcp_port = tunnel.local_port();
            upstream.push(tunnel);
        }

        unreachable!("open_chain loop always returns on last hop")
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    pub fn local_host(&self) -> &'static str {
        LOCAL_BIND_HOST
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Debug, Clone)]
enum HostKeyOutcome {
    Trusted,
    LearnedNew { fingerprint: String },
    Changed { fingerprint: String, line: usize },
    KnownHostsIo(String),
}

struct ClientHandler {
    target_host: String,
    target_port: u16,
    known_hosts_path: PathBuf,
    outcome: Arc<Mutex<Option<HostKeyOutcome>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let outcome = verify_or_learn(
            &self.target_host,
            self.target_port,
            key,
            &self.known_hosts_path,
            &fingerprint,
        );
        let allow = matches!(outcome, HostKeyOutcome::Trusted | HostKeyOutcome::LearnedNew { .. });
        if let Ok(mut slot) = self.outcome.lock() {
            *slot = Some(outcome);
        }
        Ok(allow)
    }
}

fn verify_or_learn(host: &str, port: u16, key: &PublicKey, known_hosts: &Path, fingerprint: &str) -> HostKeyOutcome {
    match check_known_hosts_path(host, port, key, known_hosts) {
        Ok(true) => HostKeyOutcome::Trusted,
        Ok(false) => match known_host_keys_path(host, port, known_hosts) {
            Ok(existing) if !existing.is_empty() => HostKeyOutcome::Changed {
                fingerprint: fingerprint.to_string(),
                line: existing[0].0,
            },
            Ok(_) => match ensure_parent_dir(known_hosts).and_then(|_| {
                learn_known_hosts_path(host, port, key, known_hosts).map_err(|e| std::io::Error::other(format!("{e}")))
            }) {
                Ok(()) => HostKeyOutcome::LearnedNew {
                    fingerprint: fingerprint.to_string(),
                },
                Err(e) => HostKeyOutcome::KnownHostsIo(e.to_string()),
            },
            Err(e) => HostKeyOutcome::KnownHostsIo(e.to_string()),
        },
        Err(russh::keys::Error::KeyChanged { line }) => HostKeyOutcome::Changed {
            fingerprint: fingerprint.to_string(),
            line,
        },
        Err(e) => HostKeyOutcome::KnownHostsIo(e.to_string()),
    }
}

fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn default_known_hosts_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("tablepro").join("known_hosts"))
}

/// Open one hop: TCP to `(tcp_host, tcp_port)`, verify host key as
/// `cfg.host:cfg.port`, forward local listeners to `fwd_host:fwd_port`.
async fn open_single(
    cfg: &SshConfig,
    tcp_host: &str,
    tcp_port: u16,
    fwd_host: String,
    fwd_port: u16,
) -> Result<SshTunnel, SshError> {
    let session = Arc::new(connect_and_auth(cfg, tcp_host, tcp_port).await?);
    let listener = TcpListener::bind((LOCAL_BIND_HOST, 0)).await.map_err(SshError::Bind)?;
    let local_port = listener.local_addr().map_err(SshError::Bind)?.port();

    tracing::info!(
        ssh_host = %cfg.host,
        ssh_port = cfg.port,
        tcp_host,
        tcp_port,
        local_port,
        remote_host = %fwd_host,
        remote_port = fwd_port,
        "ssh tunnel listening"
    );

    let cancel = CancellationToken::new();
    let task = tokio::spawn(forwarder_loop(listener, session, fwd_host, fwd_port, cancel.clone()));

    Ok(SshTunnel {
        local_port,
        cancel,
        _task: task,
        _upstream: Vec::new(),
    })
}

async fn connect_and_auth(cfg: &SshConfig, tcp_host: &str, tcp_port: u16) -> Result<Handle<ClientHandler>, SshError> {
    let known_hosts_path = default_known_hosts_path()
        .ok_or_else(|| SshError::KnownHosts("neither XDG_CONFIG_HOME nor HOME is set".into()))?;
    let outcome = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        target_host: cfg.host.clone(),
        target_port: cfg.port,
        known_hosts_path: known_hosts_path.clone(),
        outcome: outcome.clone(),
    };

    let config = Arc::new(Config {
        nodelay: true,
        ..Default::default()
    });
    let mut session = match client::connect(config, (tcp_host, tcp_port), handler).await {
        Ok(s) => s,
        Err(e) => return Err(map_connect_error(e, &cfg.host, cfg.port, &known_hosts_path, &outcome)),
    };

    match outcome.lock().ok().and_then(|s| s.clone()) {
        Some(HostKeyOutcome::LearnedNew { fingerprint }) => tracing::info!(
            host = %cfg.host,
            port = cfg.port,
            fingerprint = %fingerprint,
            "ssh: learned new host key (TOFU)",
        ),
        Some(HostKeyOutcome::Trusted) => tracing::debug!(host = %cfg.host, "ssh: host key matches known_hosts"),
        Some(HostKeyOutcome::KnownHostsIo(e)) => return Err(SshError::KnownHosts(e)),
        Some(HostKeyOutcome::Changed { .. }) => unreachable!("connect should fail on key mismatch"),
        None => {}
    }

    let auth = match &cfg.auth {
        SshAuth::Password { password } => {
            session
                .authenticate_password(&cfg.username, password.expose_secret())
                .await?
        }
        SshAuth::PrivateKey { path, passphrase } => {
            let pp = passphrase.as_ref().map(|s| s.expose_secret().to_string());
            let key = load_secret_key(path, pp.as_deref()).map_err(|e| SshError::Key {
                path: path.clone(),
                source: e,
            })?;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            session
                .authenticate_publickey(&cfg.username, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        }
    };

    if !auth.success() {
        return Err(SshError::Auth);
    }
    Ok(session)
}

fn map_connect_error(
    err: russh::Error,
    host: &str,
    port: u16,
    known_hosts: &Path,
    outcome: &Arc<Mutex<Option<HostKeyOutcome>>>,
) -> SshError {
    // Recover a poisoned mutex so a panic during host-key verification
    // never silently downgrades a HostKeyMismatch to a generic Connect error.
    let captured = outcome.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    match captured {
        Some(HostKeyOutcome::Changed { fingerprint, line }) => SshError::HostKeyMismatch {
            host: host.to_string(),
            port,
            new_fingerprint: fingerprint,
            line,
            known_hosts: known_hosts.to_path_buf(),
        },
        Some(HostKeyOutcome::KnownHostsIo(e)) => SshError::KnownHosts(e),
        _ => SshError::Connect(err.to_string()),
    }
}

async fn forwarder_loop(
    listener: TcpListener,
    session: Arc<Handle<ClientHandler>>,
    remote_host: String,
    remote_port: u16,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("ssh tunnel cancelled");
                return;
            }
            accept = listener.accept() => match accept {
                Ok((socket, peer)) => {
                    let session = session.clone();
                    let remote_host = remote_host.clone();
                    let cancel = cancel.clone();
                    tokio::spawn(async move {
                        if let Err(e) = forward_one(session, socket, peer, remote_host, remote_port, cancel).await {
                            tracing::warn!(error = %e, "ssh forward failed");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ssh listener accept failed");
                    return;
                }
            },
        }
    }
}

async fn forward_one(
    session: Arc<Handle<ClientHandler>>,
    mut socket: TcpStream,
    peer: std::net::SocketAddr,
    remote_host: String,
    remote_port: u16,
    cancel: CancellationToken,
) -> Result<(), russh::Error> {
    let mut channel = session
        .channel_open_direct_tcpip(
            remote_host,
            u32::from(remote_port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await?;

    let mut buf = vec![0u8; 65536];
    let mut local_eof = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = channel.eof().await;
                return Ok(());
            }
            r = socket.read(&mut buf), if !local_eof => {
                match r {
                    Ok(0) => {
                        local_eof = true;
                        let _ = channel.eof().await;
                    }
                    Ok(n) => {
                        if channel.data(&buf[..n]).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(_) => return Ok(()),
                }
            }
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if socket.write_all(&data).await.is_err() {
                        return Ok(());
                    }
                }
                Some(ChannelMsg::ExtendedData { .. }) => {}
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    let _ = socket.shutdown().await;
                    return Ok(());
                }
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_auth_password_redacts_in_debug() {
        let auth = SshAuth::Password {
            password: SecretString::new("topsecret".to_string().into()),
        };
        let dbg = format!("{auth:?}");
        assert!(!dbg.contains("topsecret"), "password leaked in Debug: {dbg}");
    }

    #[test]
    fn empty_chain_is_rejected() {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(SshTunnel::open_chain(&[], "db".into(), 5432));
        assert!(matches!(result, Err(SshError::EmptyChain)));
    }

    #[test]
    fn open_is_single_hop_wrapper_shape() {
        let cfg = SshConfig {
            host: "bastion.example".into(),
            port: 22,
            username: "deploy".into(),
            auth: SshAuth::Password {
                password: SecretString::new("x".to_string().into()),
            },
        };
        let hops = std::slice::from_ref(&cfg);
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].host, "bastion.example");
    }

    const KEY_A_BASE64: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIGAdbe+Xv3hfmzwpfcGVeMHE/jfo5bmR1IgIpfuP4ypR";
    const KEY_B_BASE64: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIFvC8V+mh5lxNlOLorBehIwTS2R/nvw2ghab6N1SlSk6";
    const RSA_KEY_BASE64: &str = "AAAAB3NzaC1yc2EAAAADAQABAAABAQDNTWi6IachyABYXmaOyLJ2TyBGTwkzBdub6FHS7xEB4XyqU9ZkAaFajaYlhT+3zmoFgDHhnNxULyJtOK3nHpcRaouO9XT8IfUqmmcVbkJrgsoS/gUyHKWqqbzpr70uZXoqM+tjbBAvPt7S6kts1QsJgi+AvPod+1lpfbe9O6Az+hREcPqHIn0BznhMzU/d6DirOCTE81fWoACZm0y6hrhE4MDU17JpTMe9E5bbNKLUh65d2BXBo0MolCClns/UA5trjRboNz+28HRMnyPqIzqMCE7J9HLmpCdTD2+aEGnD2RbsVwYKPRokiyJVOHtQGZwMsEqTgnrG7T61sM1ZCG+D";

    fn parse_key(base64: &str) -> PublicKey {
        russh::keys::parse_public_key_base64(base64).expect("valid base64 public key")
    }

    #[test]
    fn verify_or_learn_creates_known_hosts_on_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("known_hosts");
        let key = parse_key(KEY_A_BASE64);
        let outcome = verify_or_learn("bastion.example.com", 22, &key, &path, "fp");
        assert!(matches!(outcome, HostKeyOutcome::LearnedNew { .. }));
        assert!(path.exists());
    }

    #[test]
    fn verify_or_learn_trusts_recorded_key_on_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = parse_key(KEY_A_BASE64);
        let _ = verify_or_learn("bastion.example.com", 22, &key, &path, "fp");
        let outcome = verify_or_learn("bastion.example.com", 22, &key, &path, "fp");
        assert!(matches!(outcome, HostKeyOutcome::Trusted));
    }

    #[test]
    fn verify_or_learn_detects_key_type_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let ed25519 = parse_key(KEY_A_BASE64);
        let rsa = parse_key(RSA_KEY_BASE64);
        let _ = verify_or_learn("bastion.example.com", 22, &ed25519, &path, "ed25519");
        let outcome = verify_or_learn("bastion.example.com", 22, &rsa, &path, "rsa");
        assert!(matches!(outcome, HostKeyOutcome::Changed { fingerprint, .. } if fingerprint == "rsa"));
    }

    #[test]
    fn verify_or_learn_detects_key_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key_a = parse_key(KEY_A_BASE64);
        let key_b = parse_key(KEY_B_BASE64);
        let _ = verify_or_learn("bastion.example.com", 22, &key_a, &path, "fp_a");
        let outcome = verify_or_learn("bastion.example.com", 22, &key_b, &path, "fp_b");
        match outcome {
            HostKeyOutcome::Changed { fingerprint, .. } => assert_eq!(fingerprint, "fp_b"),
            other => panic!("expected Changed, got {other:?}"),
        }
    }
}
