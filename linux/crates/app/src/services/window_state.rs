use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::config_io::{atomic_write_json, xdg_config_path};

static FILE_LOCK: Mutex<()> = Mutex::new(());
static SESSION_RESTORE_ATTEMPTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    #[serde(default)]
    pub last_connection_id: Option<Uuid>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 760,
            maximized: false,
            last_connection_id: None,
        }
    }
}

fn load_locked() -> WindowState {
    let Some(path) = xdg_config_path("window.json") else {
        return WindowState::default();
    };
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_locked(state: &WindowState) {
    let Some(path) = xdg_config_path("window.json") else {
        return;
    };
    if let Err(e) = atomic_write_json(&path, state) {
        tracing::warn!(path = %path.display(), error = %e, "window_state: write failed");
    }
}

pub fn load() -> WindowState {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load_locked()
}

pub fn save_geometry(width: i32, height: i32, maximized: bool) {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load_locked();
    state.width = width;
    state.height = height;
    state.maximized = maximized;
    save_locked(&state);
}

pub fn set_last_connection_id(id: Option<Uuid>) {
    let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = load_locked();
    state.last_connection_id = id;
    save_locked(&state);
}

pub fn take_session_restore_turn() -> bool {
    !SESSION_RESTORE_ATTEMPTED.swap(true, Ordering::SeqCst)
}

pub fn connection_id_to_restore(last_connection_id: Option<Uuid>, available: &[Uuid]) -> Option<Uuid> {
    let id = last_connection_id?;
    available.contains(&id).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_last_connection_id_deserializes_as_none() {
        let parsed: WindowState =
            serde_json::from_str(r#"{"width":800,"height":600,"maximized":false}"#).expect("legacy window.json");
        assert_eq!(parsed.last_connection_id, None);
        assert_eq!(parsed.width, 800);
    }

    #[test]
    fn last_connection_id_round_trips() {
        let id = Uuid::new_v4();
        let state = WindowState {
            width: 1,
            height: 2,
            maximized: true,
            last_connection_id: Some(id),
        };
        let parsed: WindowState =
            serde_json::from_slice(&serde_json::to_vec(&state).expect("serialize")).expect("deserialize");
        assert_eq!(parsed.last_connection_id, Some(id));
        assert!(parsed.maximized);
    }

    #[test]
    fn restore_skips_an_unknown_or_absent_connection() {
        let id = Uuid::new_v4();
        assert_eq!(connection_id_to_restore(None, &[id]), None);
        assert_eq!(connection_id_to_restore(Some(id), &[]), None);
        assert_eq!(connection_id_to_restore(Some(Uuid::new_v4()), &[id]), None);
        assert_eq!(connection_id_to_restore(Some(id), &[id]), Some(id));
    }
}
