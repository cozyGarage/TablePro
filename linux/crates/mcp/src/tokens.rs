use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::TokenPermissions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToken {
    pub id: Uuid,
    pub name: String,
    /// SHA-256 hex of the plaintext token. Plaintext is shown once at
    /// issuance and never stored.
    pub token_hash: String,
    pub permissions: TokenPermissions,
    #[serde(default)]
    pub connection_allowlist: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub revoked: bool,
}

pub struct TokenStore {
    path: PathBuf,
    tokens: Mutex<HashMap<Uuid, McpToken>>,
}

impl TokenStore {
    pub fn open_default() -> Result<Self, String> {
        Self::open(tokens_path()?)
    }

    pub fn open(path: PathBuf) -> Result<Self, String> {
        let tokens = if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let list: Vec<McpToken> = serde_json::from_str(&text).unwrap_or_default();
            list.into_iter().map(|t| (t.id, t)).collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            tokens: Mutex::new(tokens),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn issue(
        &self,
        name: String,
        permissions: TokenPermissions,
        connection_allowlist: Vec<Uuid>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(McpToken, String), String> {
        let plaintext = generate_token();
        let token = McpToken {
            id: Uuid::new_v4(),
            name,
            token_hash: hash_token(&plaintext),
            permissions,
            connection_allowlist,
            created_at: Utc::now(),
            expires_at,
            revoked: false,
        };
        {
            let mut map = self.tokens.lock().map_err(|e| e.to_string())?;
            map.insert(token.id, token.clone());
        }
        self.persist()?;
        Ok((token, plaintext))
    }

    pub fn authenticate(&self, plaintext: &str) -> Result<McpToken, String> {
        let hash = hash_token(plaintext);
        let map = self.tokens.lock().map_err(|e| e.to_string())?;
        let token = map
            .values()
            .find(|t| t.token_hash == hash && !t.revoked)
            .cloned()
            .ok_or_else(|| "invalid token".to_string())?;
        if let Some(exp) = token.expires_at
            && exp < Utc::now()
        {
            return Err("token expired".into());
        }
        Ok(token)
    }

    pub fn revoke(&self, id: Uuid) -> Result<(), String> {
        {
            let mut map = self.tokens.lock().map_err(|e| e.to_string())?;
            if let Some(t) = map.get_mut(&id) {
                t.revoked = true;
            }
        }
        self.persist()
    }

    pub fn list(&self) -> Vec<McpToken> {
        self.tokens
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    fn persist(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let list = self.list();
        let json = serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

pub fn generate_token() -> String {
    let raw = Uuid::new_v4().to_string() + &Uuid::new_v4().to_string();
    format!("tp_{}", hex::encode(Sha256::digest(raw.as_bytes()))[..40].to_string())
}

fn hash_token(plaintext: &str) -> String {
    hex::encode(Sha256::digest(plaintext.as_bytes()))
}

fn tokens_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    Ok(base.join("tablepro").join("mcp-tokens.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn issue_and_authenticate() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::open(dir.path().join("tokens.json")).unwrap();
        let (_meta, plain) = store
            .issue("cursor".into(), TokenPermissions::ReadWrite, vec![], None)
            .unwrap();
        let authed = store.authenticate(&plain).unwrap();
        assert_eq!(authed.name, "cursor");
        assert!(store.authenticate("bogus").is_err());
    }

    #[test]
    fn revoke_blocks() {
        let dir = TempDir::new().unwrap();
        let store = TokenStore::open(dir.path().join("tokens.json")).unwrap();
        let (meta, plain) = store
            .issue("x".into(), TokenPermissions::ReadOnly, vec![], None)
            .unwrap();
        store.revoke(meta.id).unwrap();
        assert!(store.authenticate(&plain).is_err());
    }
}
