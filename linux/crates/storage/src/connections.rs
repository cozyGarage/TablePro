use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StorageError;
use crate::file_access::{lock_path, read_bounded, write_atomically};
use tablepro_core::{AuthMode, Environment, TlsMode};

const CURRENT_VERSION: u32 = 1;
const MAX_CONNECTIONS_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 1_000;
const MAX_CONNECTION_VALUE_BYTES: usize = 64 * 1024;

/// Every JSON key `SavedConnection` owns. A key outside this list came
/// from a newer TablePro (or a hand edit) and is carried through a
/// rewrite untouched, so downgrading does not silently strip a field
/// the newer version depends on. Keys inside the list are always taken
/// from the freshly serialized record — a cleared `Option` must not be
/// resurrected from the copy on disk.
const KNOWN_CONNECTION_FIELDS: &[&str] = &[
    "id",
    "name",
    "driver_id",
    "host",
    "port",
    "socket_dir",
    "database",
    "username",
    "use_tls",
    "tls_mode",
    "tls_root_cert",
    "read_only",
    "auth_mode",
    "environment",
    "ssh",
    "last_opened_at",
];

const KNOWN_FILE_FIELDS: &[&str] = &["version", "connections"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: Uuid,
    pub name: String,
    pub driver_id: String,
    pub host: String,
    pub port: u16,
    /// PostgreSQL local Unix-domain socket directory. Omitted for network
    /// connections and legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_dir: Option<PathBuf>,
    pub database: String,
    pub username: String,
    /// Legacy boolean TLS flag. Prefer `tls_mode`. Kept for round-trip of
    /// files written before Stage 1; new writes always set `tls_mode`.
    #[serde(default)]
    pub use_tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_mode: Option<TlsMode>,
    /// Certificate authority used to verify the server, for engines whose
    /// certificate is not issued by a CA in the system trust store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_root_cert: Option<PathBuf>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SavedSshConfig>,
    /// Last successful open of this connection. Drives the welcome
    /// view's recency-first sort. `None` for connections saved before
    /// this field shipped (legacy files just deserialize into None);
    /// they sort after every connection that has been opened at least
    /// once and fall back to alphabetical against each other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<DateTime<Utc>>,
}

impl SavedConnection {
    pub fn effective_tls_mode(&self) -> TlsMode {
        self.tls_mode.unwrap_or(if self.use_tls {
            TlsMode::VerifyFull
        } else {
            TlsMode::Disabled
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SavedSshAuth,
    /// Next hop toward the database (ProxyJump-style chain).
    /// `None` means this is the last jump before the DB host.
    /// Omitted in legacy files via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<Box<SavedSshConfig>>,
}

impl SavedSshConfig {
    /// Flatten nested `jump` links into hop order: first bastion first.
    pub fn flatten_hops(&self) -> Vec<&SavedSshConfig> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(hop) = cur {
            out.push(hop);
            cur = hop.jump.as_deref();
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedSshAuth {
    Password,
    PrivateKey {
        path: PathBuf,
        #[serde(default)]
        has_passphrase: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectionsFile {
    version: u32,
    connections: Vec<SavedConnection>,
}

pub async fn load_connections() -> Result<Vec<SavedConnection>, StorageError> {
    load_from(&connections_path()?).await
}

pub async fn save_connections(connections: &[SavedConnection]) -> Result<(), StorageError> {
    upsert_to(&connections_path()?, connections).await
}

pub async fn delete_connection(id: Uuid) -> Result<(), StorageError> {
    delete_from(&connections_path()?, id).await
}

async fn delete_from(path: &Path, id: Uuid) -> Result<(), StorageError> {
    let _guard = lock_path(path).await?;
    let mut existing = load_from(path).await?;
    existing.retain(|connection| connection.id != id);
    save_to_unlocked(path, &existing).await
}

/// Stamp `last_opened_at = now()` on the matching connection. Called
/// once per successful open so the welcome view can sort recency-first.
/// No-op when `id` isn't in the file (e.g. an unsaved connection
/// opened from the dialog without ticking "Save"); the missing-id case
/// is silent because there is nothing to update.
pub async fn touch_last_opened(id: Uuid) -> Result<(), StorageError> {
    touch_last_opened_from(&connections_path()?, id).await
}

async fn touch_last_opened_from(path: &Path, id: Uuid) -> Result<(), StorageError> {
    let _guard = lock_path(path).await?;
    let mut existing = load_from(path).await?;
    let Some(connection) = existing.iter_mut().find(|connection| connection.id == id) else {
        return Ok(());
    };
    connection.last_opened_at = Some(Utc::now());
    save_to_unlocked(path, &existing).await
}

pub(crate) async fn load_from(path: &Path) -> Result<Vec<SavedConnection>, StorageError> {
    let Some(bytes) = read_bounded(path, MAX_CONNECTIONS_FILE_BYTES).await? else {
        return Ok(Vec::new());
    };
    let file: ConnectionsFile = serde_json::from_slice(&bytes)?;
    validate_file(&file)?;
    Ok(file.connections)
}

#[cfg(test)]
pub(crate) async fn save_to(path: &Path, connections: &[SavedConnection]) -> Result<(), StorageError> {
    let _guard = lock_path(path).await?;
    save_to_unlocked(path, connections).await
}

async fn upsert_to(path: &Path, connections: &[SavedConnection]) -> Result<(), StorageError> {
    validate_connections(connections)?;
    let _guard = lock_path(path).await?;
    let mut existing = load_from(path).await?;
    for connection in connections {
        existing.retain(|saved| saved.id != connection.id);
        existing.push(connection.clone());
    }
    save_to_unlocked(path, &existing).await
}

async fn save_to_unlocked(path: &Path, connections: &[SavedConnection]) -> Result<(), StorageError> {
    validate_connections(connections)?;
    let stored = read_raw_file(path).await?;
    let file = ConnectionsFile {
        version: CURRENT_VERSION,
        connections: connections.to_vec(),
    };
    let mut document = serde_json::to_value(&file)?;
    if let Some(stored) = stored.as_ref() {
        carry_unknown_fields(&mut document, stored);
    }
    let json = serde_json::to_vec_pretty(&document)?;
    if json.len() > MAX_CONNECTIONS_FILE_BYTES {
        return Err(StorageError::TooLarge {
            got: json.len(),
            limit: MAX_CONNECTIONS_FILE_BYTES,
        });
    }
    write_atomically(path, &json).await
}

async fn read_raw_file(path: &Path) -> Result<Option<serde_json::Map<String, serde_json::Value>>, StorageError> {
    let Some(bytes) = read_bounded(path, MAX_CONNECTIONS_FILE_BYTES).await? else {
        return Ok(None);
    };
    let stored: serde_json::Value = serde_json::from_slice(&bytes)?;
    let object = stored
        .as_object()
        .cloned()
        .ok_or_else(|| StorageError::Schema("connections.json must contain an object".into()))?;
    let file: ConnectionsFile = serde_json::from_value(serde_json::Value::Object(object.clone()))?;
    validate_file(&file)?;
    Ok(Some(object))
}

fn validate_file(file: &ConnectionsFile) -> Result<(), StorageError> {
    if file.version != CURRENT_VERSION {
        return Err(StorageError::Schema(format!(
            "connections.json version {} not supported (expected {})",
            file.version, CURRENT_VERSION,
        )));
    }
    validate_connections(&file.connections)
}

fn validate_connections(connections: &[SavedConnection]) -> Result<(), StorageError> {
    if connections.len() > MAX_CONNECTIONS {
        return Err(StorageError::Schema(format!(
            "connections are limited to {MAX_CONNECTIONS} entries"
        )));
    }
    for connection in connections {
        validate_connection(connection)?;
    }
    Ok(())
}

fn validate_connection(connection: &SavedConnection) -> Result<(), StorageError> {
    for value in [
        connection.name.as_str(),
        connection.driver_id.as_str(),
        connection.host.as_str(),
        connection.database.as_str(),
        connection.username.as_str(),
    ] {
        validate_value_size(value.len())?;
    }
    for path in [connection.socket_dir.as_deref(), connection.tls_root_cert.as_deref()]
        .into_iter()
        .flatten()
    {
        validate_value_size(path.as_os_str().as_encoded_bytes().len())?;
    }
    let mut ssh = connection.ssh.as_ref();
    while let Some(hop) = ssh {
        validate_value_size(hop.host.len())?;
        validate_value_size(hop.username.len())?;
        if let SavedSshAuth::PrivateKey { path, .. } = &hop.auth {
            validate_value_size(path.as_os_str().as_encoded_bytes().len())?;
        }
        ssh = hop.jump.as_deref();
    }
    Ok(())
}

fn validate_value_size(size: usize) -> Result<(), StorageError> {
    if size > MAX_CONNECTION_VALUE_BYTES {
        return Err(StorageError::TooLarge {
            got: size,
            limit: MAX_CONNECTION_VALUE_BYTES,
        });
    }
    Ok(())
}

fn carry_unknown_fields(document: &mut serde_json::Value, stored: &serde_json::Map<String, serde_json::Value>) {
    let Some(fresh) = document.as_object_mut() else {
        return;
    };
    for (key, value) in stored {
        if KNOWN_FILE_FIELDS.contains(&key.as_str()) {
            continue;
        }
        fresh.entry(key.clone()).or_insert_with(|| value.clone());
    }
    let stored_records = stored.get("connections").and_then(|value| value.as_array());
    let Some(stored_records) = stored_records else {
        return;
    };
    let Some(fresh_records) = fresh.get_mut("connections").and_then(|value| value.as_array_mut()) else {
        return;
    };
    for record in fresh_records {
        let Some(id) = record.get("id").and_then(|value| value.as_str()).map(str::to_owned) else {
            continue;
        };
        let Some(previous) = stored_records
            .iter()
            .filter_map(|candidate| candidate.as_object())
            .find(|candidate| candidate.get("id").and_then(|value| value.as_str()) == Some(id.as_str()))
        else {
            continue;
        };
        let Some(target) = record.as_object_mut() else {
            continue;
        };
        for (key, value) in previous {
            if KNOWN_CONNECTION_FIELDS.contains(&key.as_str()) {
                continue;
            }
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
}

fn connections_path() -> Result<PathBuf, StorageError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .ok_or_else(|| StorageError::Schema("neither XDG_CONFIG_HOME nor HOME is set".into()))?;
    Ok(base.join("tablepro").join("connections.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    fn sample_connection() -> SavedConnection {
        SavedConnection {
            id: Uuid::new_v4(),
            name: "Local Postgres".into(),
            driver_id: "postgres".into(),
            host: "localhost".into(),
            port: 5432,
            socket_dir: None,
            database: "postgres".into(),
            username: "postgres".into(),
            use_tls: false,
            tls_mode: Some(TlsMode::Disabled),
            tls_root_cert: None,
            read_only: false,
            auth_mode: AuthMode::Password,
            environment: Environment::Local,
            ssh: None,
            last_opened_at: None,
        }
    }

    #[tokio::test]
    async fn load_returns_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let result = load_from(&path).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let original = vec![sample_connection()];
        save_to(&path, &original).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(original, loaded);
    }

    #[tokio::test]
    async fn postgres_socket_directory_round_trips_without_a_version_bump() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        connection.socket_dir = Some(PathBuf::from("/run/postgresql"));
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"version\": 1"));
        assert_eq!(load_from(&path).await.unwrap(), vec![connection]);
    }

    #[tokio::test]
    async fn save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/dir/connections.json");
        save_to(&path, &[]).await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_saved_file_is_readable_only_by_its_owner() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        save_to(&path, &[sample_connection()]).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn rewriting_an_existing_file_keeps_it_private() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        save_to(&path, &[]).await.unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_to(&path, &[sample_connection()]).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn a_shared_temporary_path_does_not_block_a_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let shared_tmp = path.with_extension("json.tmp");
        std::fs::create_dir(&shared_tmp).unwrap();
        let original = vec![sample_connection()];

        save_to(&path, &original).await.unwrap();

        assert_eq!(load_from(&path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn a_truncated_file_is_refused_instead_of_loading_as_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        save_to(&path, &[sample_connection()]).await.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load_from(&path).await.is_err());
    }

    #[tokio::test]
    async fn load_rejects_a_file_over_the_size_limit() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(u64::try_from(MAX_CONNECTIONS_FILE_BYTES + 1).unwrap())
            .unwrap();

        let error = load_from(&path).await.unwrap_err();

        assert!(matches!(
            error,
            StorageError::TooLarge {
                limit: MAX_CONNECTIONS_FILE_BYTES,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn load_rejects_excessive_connection_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let file = ConnectionsFile {
            version: CURRENT_VERSION,
            connections: vec![sample_connection(); MAX_CONNECTIONS + 1],
        };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let error = load_from(&path).await.unwrap_err();

        assert!(matches!(error, StorageError::Schema(message) if message.contains("limited")));
    }

    #[tokio::test]
    async fn oversized_serialization_does_not_replace_the_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let prefix = br#"{"version":1,"connections":[],"future":""#;
        let suffix = br#""}"#;
        let filler_len = MAX_CONNECTIONS_FILE_BYTES - prefix.len() - suffix.len() - 1;
        let mut original = Vec::with_capacity(MAX_CONNECTIONS_FILE_BYTES - 1);
        original.extend_from_slice(prefix);
        original.resize(original.len() + filler_len, b'x');
        original.extend_from_slice(suffix);
        std::fs::write(&path, &original).unwrap();

        let error = save_to(&path, &[sample_connection()]).await.unwrap_err();

        assert!(matches!(
            error,
            StorageError::TooLarge {
                limit: MAX_CONNECTIONS_FILE_BYTES,
                ..
            }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn load_rejects_oversized_connection_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        connection.name = "x".repeat(MAX_CONNECTION_VALUE_BYTES + 1);
        let file = ConnectionsFile {
            version: CURRENT_VERSION,
            connections: vec![connection],
        };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let error = load_from(&path).await.unwrap_err();

        assert!(matches!(
            error,
            StorageError::TooLarge {
                limit: MAX_CONNECTION_VALUE_BYTES,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn local_sqlite_and_duckdb_records_do_not_store_password_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connections = Vec::new();
        for driver_id in ["sqlite", "duckdb"] {
            let mut connection = sample_connection();
            connection.driver_id = driver_id.into();
            connection.host.clear();
            connection.port = 0;
            connection.database = format!("/tmp/{driver_id}.db");
            connection.username.clear();
            connections.push(connection);
        }

        save_to(&path, &connections).await.unwrap();

        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for record in document["connections"].as_array().unwrap() {
            assert!(record.get("password").is_none());
        }
        assert_eq!(load_from(&path).await.unwrap(), connections);
    }

    #[tokio::test]
    async fn a_certificate_authority_path_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        connection.tls_mode = Some(TlsMode::VerifyFull);
        connection.tls_root_cert = Some(PathBuf::from("/etc/tablepro/corp-ca.crt"));
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(loaded[0].tls_root_cert, connection.tls_root_cert);
    }

    #[tokio::test]
    async fn a_file_without_a_certificate_authority_still_loads() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let id = Uuid::new_v4();
        let legacy = format!(
            r#"{{"version":1,"connections":[{{
                "id":"{id}","name":"Old","driver_id":"postgres",
                "host":"db.example","port":5432,"database":"postgres",
                "username":"postgres","use_tls":true}}]}}"#
        );
        tokio::fs::write(&path, legacy).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert!(loaded[0].tls_root_cert.is_none());
        assert_eq!(loaded[0].effective_tls_mode(), TlsMode::VerifyFull);
    }

    #[tokio::test]
    async fn load_rejects_unknown_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        tokio::fs::write(&path, r#"{"version":999,"connections":[]}"#)
            .await
            .unwrap();
        let err = load_from(&path).await.unwrap_err();
        assert!(matches!(err, StorageError::Schema(_)));
    }

    #[tokio::test]
    async fn load_accepts_legacy_files_without_ssh_field() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let id = Uuid::new_v4();
        let legacy = format!(
            r#"{{"version":1,"connections":[{{
                "id":"{id}","name":"Old","driver_id":"postgres",
                "host":"localhost","port":5432,"database":"postgres",
                "username":"postgres","use_tls":false}}]}}"#
        );
        tokio::fs::write(&path, legacy).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].socket_dir.is_none());
        assert!(loaded[0].ssh.is_none());
        assert_eq!(loaded[0].environment, Environment::Local);
        assert_eq!(loaded[0].effective_tls_mode(), TlsMode::Disabled);
    }

    #[tokio::test]
    async fn legacy_use_tls_true_maps_to_verify_full() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let id = Uuid::new_v4();
        let legacy = format!(
            r#"{{"version":1,"connections":[{{
                "id":"{id}","name":"Old","driver_id":"postgres",
                "host":"db.example","port":5432,"database":"postgres",
                "username":"postgres","use_tls":true}}]}}"#
        );
        tokio::fs::write(&path, legacy).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(loaded[0].effective_tls_mode(), TlsMode::VerifyFull);
    }

    #[tokio::test]
    async fn ssh_config_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut conn = sample_connection();
        conn.ssh = Some(SavedSshConfig {
            host: "bastion.example.com".into(),
            port: 22,
            username: "deploy".into(),
            auth: SavedSshAuth::PrivateKey {
                path: PathBuf::from("/home/u/.ssh/id_ed25519"),
                has_passphrase: true,
            },
            jump: None,
        });
        save_to(&path, &[conn.clone()]).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(loaded, vec![conn]);
    }

    #[tokio::test]
    async fn ssh_jump_chain_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut conn = sample_connection();
        conn.ssh = Some(SavedSshConfig {
            host: "edge.example.com".into(),
            port: 22,
            username: "edge".into(),
            auth: SavedSshAuth::Password,
            jump: Some(Box::new(SavedSshConfig {
                host: "bastion.example.com".into(),
                port: 22,
                username: "deploy".into(),
                auth: SavedSshAuth::PrivateKey {
                    path: PathBuf::from("/home/u/.ssh/id_ed25519"),
                    has_passphrase: false,
                },
                jump: None,
            })),
        });
        save_to(&path, &[conn.clone()]).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert_eq!(loaded, vec![conn.clone()]);
        let hops = loaded[0].ssh.as_ref().unwrap().flatten_hops();
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].host, "edge.example.com");
        assert_eq!(hops[1].host, "bastion.example.com");
    }

    #[tokio::test]
    async fn auth_mode_defaults_to_password_for_legacy_connections() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let id = Uuid::new_v4();
        let legacy = format!(
            r#"{{"version":1,"connections":[{{
                "id":"{id}","name":"Old","driver_id":"mssql",
                "host":"localhost","port":1433,"database":"master",
                "username":"sa","use_tls":false}}]}}"#
        );
        tokio::fs::write(&path, legacy).await.unwrap();

        let loaded = load_from(&path).await.unwrap();

        assert_eq!(loaded[0].auth_mode, AuthMode::Password);
    }

    #[tokio::test]
    async fn kerberos_auth_mode_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        connection.driver_id = "mssql".into();
        connection.auth_mode = AuthMode::Kerberos;

        save_to(&path, &[connection.clone()]).await.unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let loaded = load_from(&path).await.unwrap();

        assert_eq!(json["connections"][0]["auth_mode"], "kerberos");
        assert_eq!(loaded, vec![connection]);
    }

    #[tokio::test]
    async fn load_accepts_legacy_ssh_without_jump() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let id = Uuid::new_v4();
        let legacy = format!(
            r#"{{"version":1,"connections":[{{
                "id":"{id}","name":"Old","driver_id":"postgres",
                "host":"localhost","port":5432,"database":"postgres",
                "username":"postgres","use_tls":false,
                "ssh":{{"host":"bastion","port":22,"username":"u","auth":{{"kind":"password"}}}}
            }}]}}"#
        );
        tokio::fs::write(&path, legacy).await.unwrap();
        let loaded = load_from(&path).await.unwrap();
        assert!(loaded[0].ssh.as_ref().unwrap().jump.is_none());
    }

    #[tokio::test]
    async fn the_known_field_list_covers_every_serialized_key() {
        let mut connection = sample_connection();
        connection.socket_dir = Some(PathBuf::from("/run/postgresql"));
        connection.tls_root_cert = Some(PathBuf::from("/etc/ca.crt"));
        connection.last_opened_at = Some(Utc::now());
        connection.ssh = Some(SavedSshConfig {
            host: "bastion".into(),
            port: 22,
            username: "u".into(),
            auth: SavedSshAuth::Password,
            jump: None,
        });
        let value = serde_json::to_value(&connection).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        for key in &keys {
            assert!(
                KNOWN_CONNECTION_FIELDS.contains(&key.as_str()),
                "{key} is serialized but missing from KNOWN_CONNECTION_FIELDS"
            );
        }
        for known in KNOWN_CONNECTION_FIELDS {
            assert!(
                keys.iter().any(|key| key.as_str() == *known),
                "{known} is listed but no longer serialized"
            );
        }
    }

    #[tokio::test]
    async fn a_field_written_by_a_newer_version_survives_a_rewrite() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();

        let mut document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["connections"][0]["colour_tag"] = serde_json::json!("teal");
        document["future_section"] = serde_json::json!({ "keep": true });
        std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        connection.name = "Renamed".into();
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();

        let rewritten: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(rewritten["connections"][0]["colour_tag"], "teal");
        assert_eq!(rewritten["future_section"]["keep"], true);
        assert_eq!(rewritten["connections"][0]["name"], "Renamed");
        assert_eq!(load_from(&path).await.unwrap()[0].name, "Renamed");
    }

    #[tokio::test]
    async fn clearing_an_optional_field_is_not_undone_by_field_preservation() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let mut connection = sample_connection();
        connection.tls_root_cert = Some(PathBuf::from("/etc/tablepro/corp-ca.crt"));
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();

        connection.tls_root_cert = None;
        save_to(&path, std::slice::from_ref(&connection)).await.unwrap();

        let rewritten: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(rewritten["connections"][0].get("tls_root_cert").is_none());
        assert!(load_from(&path).await.unwrap()[0].tls_root_cert.is_none());
    }

    #[tokio::test]
    async fn a_file_written_by_this_version_still_loads_unchanged() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let original = vec![sample_connection()];
        save_to(&path, &original).await.unwrap();
        save_to(&path, &original).await.unwrap();
        assert_eq!(load_from(&path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn a_malformed_previous_file_is_preserved_and_blocks_a_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let malformed = b"{ not json";
        std::fs::write(&path, malformed).unwrap();

        assert!(save_to(&path, &[sample_connection()]).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), malformed);
    }

    #[tokio::test]
    async fn concurrent_deletions_do_not_restore_another_deleted_connection() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let first = sample_connection();
        let second = sample_connection();
        save_to(&path, &[first.clone(), second.clone()]).await.unwrap();
        let first_path = path.clone();
        let second_path = path.clone();

        let (first_result, second_result) =
            tokio::join!(delete_from(&first_path, first.id), delete_from(&second_path, second.id),);

        first_result.unwrap();
        second_result.unwrap();
        assert!(load_from(&path).await.unwrap().is_empty());
    }
}
