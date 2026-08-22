use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StorageError;
use tablepro_core::{AuthMode, Environment, TlsMode};

const CURRENT_VERSION: u32 = 1;

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
    save_to(&connections_path()?, connections).await
}

pub async fn delete_connection(id: Uuid) -> Result<(), StorageError> {
    let mut existing = load_connections().await.unwrap_or_default();
    existing.retain(|c| c.id != id);
    save_connections(&existing).await
}

/// Stamp `last_opened_at = now()` on the matching connection. Called
/// once per successful open so the welcome view can sort recency-first.
/// No-op when `id` isn't in the file (e.g. an unsaved connection
/// opened from the dialog without ticking "Save"); the missing-id case
/// is silent because there is nothing to update.
pub async fn touch_last_opened(id: Uuid) -> Result<(), StorageError> {
    let mut existing = load_connections().await.unwrap_or_default();
    let mut hit = false;
    for c in existing.iter_mut() {
        if c.id == id {
            c.last_opened_at = Some(Utc::now());
            hit = true;
            break;
        }
    }
    if !hit {
        return Ok(());
    }
    save_connections(&existing).await
}

pub(crate) async fn load_from(path: &Path) -> Result<Vec<SavedConnection>, StorageError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = tokio::fs::read(path).await?;
    let file: ConnectionsFile = serde_json::from_slice(&bytes)?;
    if file.version != CURRENT_VERSION {
        return Err(StorageError::Schema(format!(
            "connections.json version {} not supported (expected {})",
            file.version, CURRENT_VERSION,
        )));
    }
    Ok(file.connections)
}

pub(crate) async fn save_to(path: &Path, connections: &[SavedConnection]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let stored = read_raw_file(path).await;
    let file = ConnectionsFile {
        version: CURRENT_VERSION,
        connections: connections.to_vec(),
    };
    let mut document = serde_json::to_value(&file)?;
    if let Some(stored) = stored.as_ref() {
        carry_unknown_fields(&mut document, stored);
    }
    let json = serde_json::to_vec_pretty(&document)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_atomically(&path, &json))
        .await
        .map_err(|error| StorageError::Schema(format!("saved connection task failed: {error}")))?
}

/// The file exactly as it sits on disk, or `None` when it is absent or
/// not a JSON object. A malformed file is not an error here: the caller
/// is about to overwrite it and simply has nothing to preserve.
async fn read_raw_file(path: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let bytes = tokio::fs::read(path).await.ok()?;
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(object)) => Some(object),
        _ => None,
    }
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

fn write_atomically(path: &Path, json: &[u8]) -> Result<(), StorageError> {
    let tmp = path.with_extension("json.tmp");
    let mut handle = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    handle.write_all(json)?;
    handle.sync_all()?;
    drop(handle);
    std::fs::rename(&tmp, path)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<(), StorageError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)?.sync_all()?;
    Ok(())
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
    async fn a_failed_save_leaves_the_previous_file_intact() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        let original = vec![sample_connection()];
        save_to(&path, &original).await.unwrap();

        let tmp = path.with_extension("json.tmp");
        std::fs::create_dir(&tmp).unwrap();
        let failure = save_to(&path, &[]).await;
        assert!(
            failure.is_err(),
            "a save that cannot write its temporary file must fail"
        );
        std::fs::remove_dir(&tmp).unwrap();

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
    async fn an_unreadable_previous_file_does_not_block_a_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let original = vec![sample_connection()];
        save_to(&path, &original).await.unwrap();
        assert_eq!(load_from(&path).await.unwrap(), original);
    }
}
