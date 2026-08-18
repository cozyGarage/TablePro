use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StorageError;

const CURRENT_VERSION: u32 = 1;
const MAX_FAVORITES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: Uuid,
    pub name: String,
    pub sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

impl SavedQuery {
    pub fn new(name: String, sql: String, driver_id: Option<String>, connection_id: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            sql,
            driver_id,
            connection_id,
            created_at: Utc::now(),
            last_used_at: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FavoritesFile {
    version: u32,
    #[serde(default)]
    favorites: Vec<SavedQuery>,
}

pub async fn load_favorites() -> Result<Vec<SavedQuery>, StorageError> {
    load_from(&favorites_path()?).await
}

pub async fn save_favorite(favorite: SavedQuery) -> Result<Vec<SavedQuery>, StorageError> {
    let path = favorites_path()?;
    let mut existing = load_from(&path).await.unwrap_or_default();
    upsert(&mut existing, favorite)?;
    save_to(&path, &existing).await?;
    Ok(existing)
}

pub async fn delete_favorite(id: Uuid) -> Result<Vec<SavedQuery>, StorageError> {
    let path = favorites_path()?;
    let mut existing = load_from(&path).await.unwrap_or_default();
    existing.retain(|favorite| favorite.id != id);
    save_to(&path, &existing).await?;
    Ok(existing)
}

pub async fn touch_favorite(id: Uuid) -> Result<(), StorageError> {
    let path = favorites_path()?;
    let mut existing = load_from(&path).await.unwrap_or_default();
    let Some(favorite) = existing.iter_mut().find(|favorite| favorite.id == id) else {
        return Ok(());
    };
    favorite.last_used_at = Some(Utc::now());
    save_to(&path, &existing).await
}

pub(crate) fn upsert(existing: &mut Vec<SavedQuery>, favorite: SavedQuery) -> Result<(), StorageError> {
    if favorite.name.trim().is_empty() {
        return Err(StorageError::Schema("a favorite needs a name".into()));
    }
    if favorite.sql.trim().is_empty() {
        return Err(StorageError::Schema("a favorite needs a statement".into()));
    }
    if let Some(slot) = existing.iter_mut().find(|candidate| candidate.id == favorite.id) {
        *slot = favorite;
        return Ok(());
    }
    if let Some(slot) = existing
        .iter_mut()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(favorite.name.trim()))
    {
        let id = slot.id;
        *slot = SavedQuery { id, ..favorite };
        return Ok(());
    }
    if existing.len() >= MAX_FAVORITES {
        return Err(StorageError::Schema(format!(
            "favorites are limited to {MAX_FAVORITES} entries"
        )));
    }
    existing.push(favorite);
    Ok(())
}

pub fn rank_favorites(favorites: &[SavedQuery]) -> Vec<SavedQuery> {
    let mut ordered = favorites.to_vec();
    ordered.sort_by(|left, right| {
        right
            .last_used_at
            .cmp(&left.last_used_at)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    ordered
}

pub fn matches_filter(favorite: &SavedQuery, filter: &str) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    favorite.name.to_lowercase().contains(&needle) || favorite.sql.to_lowercase().contains(&needle)
}

pub(crate) async fn load_from(path: &Path) -> Result<Vec<SavedQuery>, StorageError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = tokio::fs::read(path).await?;
    let file: FavoritesFile = serde_json::from_slice(&bytes)?;
    if file.version != CURRENT_VERSION {
        return Err(StorageError::Schema(format!(
            "favorites.json version {} not supported (expected {})",
            file.version, CURRENT_VERSION,
        )));
    }
    Ok(file.favorites)
}

pub(crate) async fn save_to(path: &Path, favorites: &[SavedQuery]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let file = FavoritesFile {
        version: CURRENT_VERSION,
        favorites: favorites.to_vec(),
    };
    let json = serde_json::to_vec_pretty(&file)?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, &json).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn favorites_path() -> Result<PathBuf, StorageError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                let mut path = PathBuf::from(home);
                path.push(".config");
                path
            })
        })
        .ok_or_else(|| StorageError::Schema("neither XDG_CONFIG_HOME nor HOME is set".into()))?;
    Ok(base.join("tablepro").join("favorites.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn favorite(name: &str, sql: &str) -> SavedQuery {
        SavedQuery::new(name.to_string(), sql.to_string(), Some("postgres".into()), None)
    }

    #[test]
    fn a_favorite_needs_a_name_and_a_statement() {
        let mut existing = Vec::new();
        assert!(upsert(&mut existing, favorite("  ", "SELECT 1")).is_err());
        assert!(upsert(&mut existing, favorite("name", "   ")).is_err());
        assert!(existing.is_empty());
    }

    #[test]
    fn saving_the_same_name_replaces_the_statement_and_keeps_the_id() {
        let mut existing = Vec::new();
        upsert(&mut existing, favorite("Daily", "SELECT 1")).expect("first save");
        let id = existing[0].id;

        upsert(&mut existing, favorite("daily", "SELECT 2")).expect("second save");

        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].id, id);
        assert_eq!(existing[0].sql, "SELECT 2");
    }

    #[test]
    fn saving_the_same_id_updates_in_place() {
        let mut existing = Vec::new();
        upsert(&mut existing, favorite("Daily", "SELECT 1")).expect("first save");
        let mut edited = existing[0].clone();
        edited.name = "Renamed".into();

        upsert(&mut existing, edited).expect("edit");

        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].name, "Renamed");
    }

    #[test]
    fn favorites_are_capped() {
        let mut existing: Vec<SavedQuery> = (0..MAX_FAVORITES)
            .map(|index| favorite(&format!("q{index}"), "SELECT 1"))
            .collect();
        assert!(upsert(&mut existing, favorite("one more", "SELECT 1")).is_err());
        assert_eq!(existing.len(), MAX_FAVORITES);
    }

    #[test]
    fn ranking_puts_recently_used_first_then_sorts_by_name() {
        let mut recent = favorite("zeta", "SELECT 1");
        recent.last_used_at = Some(Utc::now());
        let older = favorite("alpha", "SELECT 2");
        let middle = favorite("beta", "SELECT 3");

        let ranked = rank_favorites(&[older, middle, recent]);

        assert_eq!(ranked[0].name, "zeta");
        assert_eq!(ranked[1].name, "alpha");
        assert_eq!(ranked[2].name, "beta");
    }

    #[test]
    fn filtering_matches_name_and_statement_case_insensitively() {
        let entry = favorite("Daily revenue", "SELECT sum(total) FROM orders");
        assert!(matches_filter(&entry, ""));
        assert!(matches_filter(&entry, "daily"));
        assert!(matches_filter(&entry, "ORDERS"));
        assert!(!matches_filter(&entry, "customers"));
    }

    #[tokio::test]
    async fn favorites_round_trip_through_a_file() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("favorites.json");

        let entries = vec![favorite("Daily", "SELECT 1")];
        save_to(&path, &entries).await.expect("save");
        let loaded = load_from(&path).await.expect("load");

        assert_eq!(loaded, entries);
    }

    #[tokio::test]
    async fn a_missing_file_loads_as_empty() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let loaded = load_from(&dir.path().join("favorites.json")).await.expect("load");
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn an_unsupported_version_is_rejected() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("favorites.json");
        tokio::fs::write(&path, br#"{"version":99,"favorites":[]}"#)
            .await
            .expect("write");

        assert!(load_from(&path).await.is_err());
    }
}
