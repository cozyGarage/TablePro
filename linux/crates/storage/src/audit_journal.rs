use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tablepro_policy::{AuditEvent, AuditSink, Principal};
use async_trait::async_trait;

use crate::error::StorageError;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    seq: u64,
    prev_hash: String,
    hash: String,
    event: AuditEvent,
}

pub struct AuditJournal {
    path: PathBuf,
}

impl AuditJournal {
    pub fn open_default() -> Result<Self, StorageError> {
        Ok(Self {
            path: journal_path()?,
        })
    }

    pub fn open(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn append(&self, event: AuditEvent) -> Result<(), StorageError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (seq, prev_hash) = self.tail_meta().await?;
        let seq = seq + 1;
        let payload = serde_json::to_string(&event)?;
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(seq.to_string().as_bytes());
        hasher.update(payload.as_bytes());
        let hash = hex::encode(hasher.finalize());
        let record = JournalRecord {
            seq,
            prev_hash,
            hash,
            event,
        };
        let line = serde_json::to_string(&record)?;
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        Ok(())
    }

    pub async fn verify_chain(&self) -> Result<u64, StorageError> {
        if !self.path.exists() {
            return Ok(0);
        }
        let text = tokio::fs::read_to_string(&self.path).await?;
        let mut prev = GENESIS_HASH.to_string();
        let mut count = 0u64;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: JournalRecord = serde_json::from_str(line)?;
            if record.prev_hash != prev {
                return Err(StorageError::Schema(format!(
                    "audit journal chain break at seq {}",
                    record.seq
                )));
            }
            let payload = serde_json::to_string(&record.event)?;
            let mut hasher = Sha256::new();
            hasher.update(prev.as_bytes());
            hasher.update(record.seq.to_string().as_bytes());
            hasher.update(payload.as_bytes());
            let expected = hex::encode(hasher.finalize());
            if expected != record.hash {
                return Err(StorageError::Schema(format!(
                    "audit journal hash mismatch at seq {}",
                    record.seq
                )));
            }
            prev = record.hash;
            count += 1;
        }
        Ok(count)
    }

    pub async fn recent(&self, limit: usize) -> Result<Vec<AuditEvent>, StorageError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let text = tokio::fs::read_to_string(&self.path).await?;
        let mut events = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: JournalRecord = serde_json::from_str(line)?;
            events.push(record.event);
        }
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
        Ok(events)
    }

    async fn tail_meta(&self) -> Result<(u64, String), StorageError> {
        if !self.path.exists() {
            return Ok((0, GENESIS_HASH.to_string()));
        }
        let text = tokio::fs::read_to_string(&self.path).await?;
        let mut last = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: JournalRecord = serde_json::from_str(line)?;
            last = Some((record.seq, record.hash));
        }
        Ok(last.unwrap_or((0, GENESIS_HASH.to_string())))
    }
}

const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn journal_path() -> Result<PathBuf, StorageError> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local");
                p.push("share");
                p
            })
        })
        .ok_or_else(|| StorageError::Schema("neither XDG_DATA_HOME nor HOME is set".into()))?;
    Ok(base.join("tablepro").join("audit.jsonl"))
}

#[async_trait]
impl AuditSink for AuditJournal {
    async fn record(&self, event: AuditEvent) {
        if let Err(e) = self.append(event).await {
            tracing::warn!("audit journal append failed: {e}");
        }
    }
}

/// Convenience constructor used by tests.
pub fn sample_event(principal: Principal) -> AuditEvent {
    use tablepro_core::Environment;
    use uuid::Uuid;
    AuditEvent {
        timestamp: Utc::now(),
        principal,
        connection_id: Uuid::nil(),
        connection_name: "test".into(),
        environment: Environment::Local,
        driver_id: "postgres".into(),
        sql: "SELECT 1".into(),
        decision_rule: "read_allow".into(),
        decision_kind: "allow".into(),
        rows_affected: Some(1),
        duration_ms: 1,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_policy::Principal;
    use tempfile::TempDir;

    #[tokio::test]
    async fn append_and_verify() {
        let dir = TempDir::new().unwrap();
        let journal = AuditJournal::open(dir.path().join("audit.jsonl"));
        journal
            .append(sample_event(Principal::human_gui()))
            .await
            .unwrap();
        journal
            .append(sample_event(Principal::Agent {
                token: "abc".into(),
                client: Some("cursor".into()),
                model: None,
            }))
            .await
            .unwrap();
        assert_eq!(journal.verify_chain().await.unwrap(), 2);
        assert_eq!(journal.recent(10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn detects_tamper() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit.jsonl");
        let journal = AuditJournal::open(path.clone());
        journal
            .append(sample_event(Principal::human_gui()))
            .await
            .unwrap();
        let mut text = tokio::fs::read_to_string(&path).await.unwrap();
        text = text.replace("SELECT 1", "SELECT 2");
        tokio::fs::write(&path, text).await.unwrap();
        assert!(journal.verify_chain().await.is_err());
    }
}
