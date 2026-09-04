use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::config_io::{atomic_write_json, xdg_config_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub default_page_size: u64,
    pub confirm_destructive: bool,
    pub editor_font_size: u32,
    #[serde(default = "default_history_retention_days")]
    pub history_retention_days: u32,
    /// Wall-clock seconds before the editor's Run cancels a query
    /// the driver hasn't returned from. `0` disables the timeout.
    /// Defaults to 60s — long enough for typical OLTP work and
    /// catalog browsing, short enough that a runaway DDL or
    /// cross-join doesn't pin the GTK main thread waiting on
    /// shutdown.
    #[serde(default = "default_query_timeout_secs")]
    pub query_timeout_secs: u32,
}

fn default_history_retention_days() -> u32 {
    30
}

fn default_query_timeout_secs() -> u32 {
    60
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            default_page_size: 1_000,
            confirm_destructive: true,
            editor_font_size: 12,
            history_retention_days: default_history_retention_days(),
            query_timeout_secs: default_query_timeout_secs(),
        }
    }
}

/// Mirrors the cache in `filter_settings.rs` / `column_widths.rs`. Without
/// it, every caller -- including `operation_control::configured_timeout_secs`,
/// read on the GTK thread before each query dispatch -- did a synchronous
/// file read on the main thread for a value that only ever changes from the
/// Preferences dialog.
static CACHE: Mutex<Option<Preferences>> = Mutex::new(None);

pub fn load() -> Preferences {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return load_from_disk(),
    };
    guard.get_or_insert_with(load_from_disk).clone()
}

pub fn save(prefs: &Preferences) {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some(prefs.clone());
    }
    let Some(path) = xdg_config_path("preferences.json") else {
        return;
    };
    if let Err(e) = atomic_write_json(&path, prefs) {
        tracing::warn!(path = %path.display(), error = %e, "preferences: write failed");
    }
}

fn load_from_disk() -> Preferences {
    let Some(path) = xdg_config_path("preferences.json") else {
        return Preferences::default();
    };
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeds the cache directly rather than calling `save`, so the test
    /// never touches the real XDG config file: `load` must return exactly
    /// what's cached instead of re-reading (or falling back to a default
    /// because it can't read) disk.
    #[test]
    fn load_reads_the_cache_instead_of_disk_once_populated() {
        let sentinel = Preferences {
            default_page_size: 424_242,
            ..Preferences::default()
        };
        *CACHE.lock().unwrap() = Some(sentinel.clone());
        assert_eq!(load().default_page_size, sentinel.default_page_size);
    }
}
