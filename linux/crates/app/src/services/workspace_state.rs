use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::config_io::{atomic_write_json, xdg_config_path};

/// Serialises every read-modify-write of `workspace_state.json`. Each
/// `save_connection` call loads the current state, mutates one entry,
/// and rewrites the whole file; without this lock, two close events
/// firing in quick succession can race with overlapping load → save
/// pairs and silently drop one of the writes. The lock is held for
/// the duration of the load + serialise + atomic-rename sequence —
/// short enough that contention is negligible, long enough to make
/// the sequence atomic from any other thread's perspective.
static FILE_LOCK: Mutex<()> = Mutex::new(());
static MEMORY_LOCK: Mutex<()> = Mutex::new(());
static CACHE: OnceLock<Mutex<HashMap<Uuid, Option<ConnectionWorkspaceState>>>> = OnceLock::new();
static WRITER: OnceLock<WorkspaceWriter> = OnceLock::new();

struct WorkspaceWriter {
    wake: mpsc::SyncSender<()>,
    queue: Arc<Mutex<WorkspaceQueue>>,
}

#[derive(Default)]
struct WorkspaceQueue {
    next_sequence: u64,
    persisted_sequence: u64,
    pending: HashMap<Uuid, PendingWorkspace>,
    flushes: Vec<(u64, mpsc::Sender<Result<(), WorkspaceFlushError>>)>,
}

struct PendingWorkspace {
    sequence: u64,
    state: ConnectionWorkspaceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFlushError {
    WriteFailed,
    WriterUnavailable,
}

impl WorkspaceQueue {
    fn save(&mut self, id: Uuid, state: ConnectionWorkspaceState) {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.pending.insert(
            id,
            PendingWorkspace {
                sequence: self.next_sequence,
                state,
            },
        );
    }

    fn flush(&mut self, sender: mpsc::Sender<Result<(), WorkspaceFlushError>>) {
        if self.persisted_sequence >= self.next_sequence {
            let _ = sender.send(Ok(()));
        } else {
            self.flushes.push((self.next_sequence, sender));
        }
    }

    fn take_pending(&mut self) -> Vec<(Uuid, PendingWorkspace)> {
        self.pending.drain().collect()
    }

    fn restore_pending(&mut self, pending: Vec<(Uuid, PendingWorkspace)>) {
        for (id, entry) in pending {
            if self
                .pending
                .get(&id)
                .is_none_or(|current| current.sequence < entry.sequence)
            {
                self.pending.insert(id, entry);
            }
        }
    }

    fn complete_attempt(&mut self, sequence: u64, result: Result<(), WorkspaceFlushError>) {
        if result.is_ok() {
            self.persisted_sequence = self.persisted_sequence.max(sequence);
        }
        let mut waiting = Vec::new();
        self.flushes.retain(|(target, sender)| {
            if *target <= sequence {
                waiting.push(sender.clone());
                false
            } else {
                true
            }
        });
        for sender in waiting {
            let _ = sender.send(result);
        }
    }

    fn fail_flushes(&mut self, error: WorkspaceFlushError) {
        for (_, sender) in self.flushes.drain(..) {
            let _ = sender.send(Err(error));
        }
    }
}

const MAX_TABS_PER_CONNECTION: usize = 32;
const MAX_TABLE_NAME_BYTES: usize = 256;
const MAX_SCHEMA_NAME_BYTES: usize = 256;
const MAX_QUERY_BYTES: usize = 256 * 1024;
const FILE_NAME: &str = "workspace_state.json";

const PAGE_SIZE_OPTIONS: &[u64] = &[100, 500, 1_000, 5_000, 10_000];
const DEFAULT_PAGE_SIZE: u64 = 1_000;

/// Unified workspace persistence: one tab strip per connection containing
/// both Browse and Editor tabs in user-chosen display order. Replaces the
/// previous split between browse_state.json and editor.json.
///
/// Per-connection_id because tabs are written against a specific schema —
/// pulling them across connections silently switches their semantic
/// meaning. Selection state is intentionally omitted (ephemeral).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceState {
    #[serde(default)]
    pub connections: HashMap<String, ConnectionWorkspaceState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectionWorkspaceState {
    pub tabs: Vec<WorkspaceTabRecord>,
    #[serde(default)]
    pub active_idx: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceTabRecord {
    Browse {
        schema: Option<String>,
        table: String,
        #[serde(default)]
        offset: u64,
        #[serde(default = "default_page_size")]
        page_size: u64,
        #[serde(default)]
        sort_col: Option<usize>,
        #[serde(default)]
        sort_asc: Option<bool>,
    },
    Editor {
        #[serde(default)]
        query: String,
    },
    /// Persisted Structure tab (Edit mode only — `New` mode tabs are
    /// drafts for tables that don't exist yet, so they don't survive
    /// a disconnect).
    Structure { schema: Option<String>, table: String },
    /// Persisted Table tab (canonical M-1 form). Carries the
    /// user-visible mode (data vs structure) so the tab restores to
    /// the same lens. `offset` / `sort_col` / `sort_asc` describe the
    /// Browse side's last view; the Structure side rehydrates from
    /// driver introspection on first switch.
    Table {
        schema: Option<String>,
        table: String,
        #[serde(default)]
        mode: PersistedTableMode,
        #[serde(default)]
        offset: u64,
        #[serde(default = "default_page_size")]
        page_size: u64,
        #[serde(default)]
        sort_col: Option<usize>,
        #[serde(default)]
        sort_asc: Option<bool>,
    },
    /// Forward-compat: an older binary reading a workspace_state.json
    /// written by a newer binary lands tabs of unrecognised kinds in
    /// this variant. `clamp_connection` and `restore_workspace_tabs`
    /// drop them silently rather than failing the entire load.
    #[serde(other)]
    Unknown,
}

fn default_page_size() -> u64 {
    DEFAULT_PAGE_SIZE
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersistedTableMode {
    #[default]
    Data,
    Structure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RestoredWorkspaceTab {
    Editor {
        query: String,
    },
    Table {
        schema: Option<String>,
        table: String,
        offset: u64,
        page_size: u64,
        sort: Option<(usize, bool)>,
    },
    Structure {
        schema: Option<String>,
        table: String,
    },
}

pub(crate) fn restored_workspace_tab(record: &WorkspaceTabRecord) -> Option<RestoredWorkspaceTab> {
    match record {
        WorkspaceTabRecord::Editor { query } => Some(RestoredWorkspaceTab::Editor { query: query.clone() }),
        WorkspaceTabRecord::Table {
            schema,
            table,
            mode: PersistedTableMode::Data,
            offset,
            page_size,
            sort_col,
            sort_asc,
        } => Some(RestoredWorkspaceTab::Table {
            schema: schema.clone(),
            table: table.clone(),
            offset: *offset,
            page_size: *page_size,
            sort: match (sort_col, sort_asc) {
                (Some(column), Some(ascending)) => Some((*column, *ascending)),
                _ => None,
            },
        }),
        WorkspaceTabRecord::Table {
            schema,
            table,
            mode: PersistedTableMode::Structure,
            ..
        }
        | WorkspaceTabRecord::Structure { schema, table } => {
            if table.is_empty() {
                None
            } else {
                Some(RestoredWorkspaceTab::Structure {
                    schema: schema.clone(),
                    table: table.clone(),
                })
            }
        }
        WorkspaceTabRecord::Browse { .. } | WorkspaceTabRecord::Unknown => None,
    }
}

fn load_locked() -> WorkspaceState {
    let Some(path) = xdg_config_path(FILE_NAME) else {
        return WorkspaceState::default();
    };
    let mut state: WorkspaceState = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    clamp(&mut state);
    state
}

fn save_locked(state: &WorkspaceState) -> Result<(), WorkspaceFlushError> {
    let Some(path) = xdg_config_path(FILE_NAME) else {
        tracing::warn!("workspace_state: no config path; skipping save");
        return Err(WorkspaceFlushError::WriteFailed);
    };
    let mut snapshot = state.clone();
    clamp(&mut snapshot);
    atomic_write_json(&path, &snapshot).map_err(|error| {
        tracing::warn!(path = %path.display(), error = %error, "workspace_state: write failed");
        WorkspaceFlushError::WriteFailed
    })
}

pub fn prefetch_connections(ids: &[Uuid]) {
    prefetch_connections_coordinated(
        &MEMORY_LOCK,
        cache(),
        ids,
        || {
            let receiver = flush();
            if !matches!(receiver.recv(), Ok(Ok(()))) {
                tracing::warn!("workspace_state: preload flush failed");
            }
        },
        || {
            let _guard = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            load_locked()
        },
    );
}

pub fn load_connection(id: Uuid) -> Option<ConnectionWorkspaceState> {
    cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&id)
        .cloned()
        .flatten()
}

pub fn save_connection(id: Uuid, mut conn_state: ConnectionWorkspaceState) {
    clamp_connection(&mut conn_state);
    let writer = writer();
    save_connection_coordinated(&MEMORY_LOCK, cache(), &writer.queue, id, conn_state);
    wake_writer(writer);
}

fn prefetch_connections_coordinated(
    memory_lock: &Mutex<()>,
    cache: &Mutex<HashMap<Uuid, Option<ConnectionWorkspaceState>>>,
    ids: &[Uuid],
    flush: impl FnOnce(),
    load: impl FnOnce() -> WorkspaceState,
) {
    let _memory_guard = memory_lock.lock().unwrap_or_else(|error| error.into_inner());
    flush();
    let state = load();
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    for id in ids {
        cache.insert(*id, state.connections.get(&id.to_string()).cloned());
    }
}

fn save_connection_coordinated(
    memory_lock: &Mutex<()>,
    cache: &Mutex<HashMap<Uuid, Option<ConnectionWorkspaceState>>>,
    queue: &Mutex<WorkspaceQueue>,
    id: Uuid,
    state: ConnectionWorkspaceState,
) {
    let _memory_guard = memory_lock.lock().unwrap_or_else(|error| error.into_inner());
    cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(id, Some(state.clone()));
    queue.lock().unwrap_or_else(|error| error.into_inner()).save(id, state);
}

pub fn flush() -> mpsc::Receiver<Result<(), WorkspaceFlushError>> {
    let (sender, receiver) = mpsc::channel();
    let writer = writer();
    writer
        .queue
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .flush(sender);
    if !wake_writer(writer) {
        writer
            .queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .fail_flushes(WorkspaceFlushError::WriterUnavailable);
    }
    receiver
}

fn cache() -> &'static Mutex<HashMap<Uuid, Option<ConnectionWorkspaceState>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn writer() -> &'static WorkspaceWriter {
    WRITER.get_or_init(|| {
        let (wake, receiver) = mpsc::sync_channel(1);
        let queue = Arc::new(Mutex::new(WorkspaceQueue::default()));
        let writer_queue = Arc::clone(&queue);
        let _writer = std::thread::spawn(move || run_writer(receiver, writer_queue));
        WorkspaceWriter { wake, queue }
    })
}

fn wake_writer(writer: &WorkspaceWriter) -> bool {
    if let Err(mpsc::TrySendError::Disconnected(())) = writer.wake.try_send(()) {
        tracing::warn!("workspace_state: writer unavailable");
        return false;
    }
    true
}

fn run_writer(receiver: mpsc::Receiver<()>, queue: Arc<Mutex<WorkspaceQueue>>) {
    while receiver.recv().is_ok() {
        let pending = queue.lock().unwrap_or_else(|error| error.into_inner()).take_pending();
        if pending.is_empty() {
            continue;
        }
        let attempted_sequence = pending.iter().map(|(_, entry)| entry.sequence).max().unwrap_or(0);
        let result = {
            let _guard = FILE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let mut state = load_locked();
            for (id, entry) in &pending {
                state.connections.insert(id.to_string(), entry.state.clone());
            }
            save_locked(&state)
        };
        let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
        if result.is_err() {
            queue.restore_pending(pending);
        }
        queue.complete_attempt(attempted_sequence, result);
    }
}

fn clamp(state: &mut WorkspaceState) {
    for conn in state.connections.values_mut() {
        clamp_connection(conn);
    }
}

fn clamp_connection(conn: &mut ConnectionWorkspaceState) {
    // Drop forward-compat Unknown variants up front so they never
    // contribute to the tab count or the active_idx selection.
    conn.tabs.retain(|t| !matches!(t, WorkspaceTabRecord::Unknown));
    if conn.tabs.len() > MAX_TABS_PER_CONNECTION {
        conn.tabs.truncate(MAX_TABS_PER_CONNECTION);
    }
    // Migrate legacy Browse / Structure records to Table so the rest
    // of the load path (and clamp logic) only deals with one variant.
    // Browse → Table(Data); Structure → Table(Structure).
    for tab in &mut conn.tabs {
        let migrated = match std::mem::replace(tab, WorkspaceTabRecord::Unknown) {
            WorkspaceTabRecord::Browse {
                schema,
                table,
                offset,
                page_size,
                sort_col,
                sort_asc,
            } => WorkspaceTabRecord::Table {
                schema,
                table,
                mode: PersistedTableMode::Data,
                offset,
                page_size,
                sort_col,
                sort_asc,
            },
            WorkspaceTabRecord::Structure { schema, table } => WorkspaceTabRecord::Table {
                schema,
                table,
                mode: PersistedTableMode::Structure,
                offset: 0,
                page_size: DEFAULT_PAGE_SIZE,
                sort_col: None,
                sort_asc: None,
            },
            other => other,
        };
        *tab = migrated;
    }
    for tab in &mut conn.tabs {
        match tab {
            WorkspaceTabRecord::Editor { query } => {
                if query.len() > MAX_QUERY_BYTES {
                    let boundary = floor_char_boundary(query, MAX_QUERY_BYTES);
                    query.truncate(boundary);
                }
            }
            WorkspaceTabRecord::Table {
                schema,
                table,
                page_size,
                ..
            } => {
                if table.len() > MAX_TABLE_NAME_BYTES {
                    let boundary = floor_char_boundary(table, MAX_TABLE_NAME_BYTES);
                    table.truncate(boundary);
                }
                if let Some(s) = schema.as_mut()
                    && s.len() > MAX_SCHEMA_NAME_BYTES
                {
                    let boundary = floor_char_boundary(s, MAX_SCHEMA_NAME_BYTES);
                    s.truncate(boundary);
                }
                if !PAGE_SIZE_OPTIONS.contains(page_size) {
                    *page_size = DEFAULT_PAGE_SIZE;
                }
            }
            WorkspaceTabRecord::Browse { .. } | WorkspaceTabRecord::Structure { .. } => {
                // Unreachable: legacy variants were converted above.
            }
            WorkspaceTabRecord::Unknown => {
                // Unreachable: stripped by the retain() above.
            }
        }
    }
    if (conn.active_idx as usize) >= conn.tabs.len() {
        conn.active_idx = 0;
    }
}

pub fn bounded_query(query: &str) -> String {
    let boundary = floor_char_boundary(query, MAX_QUERY_BYTES);
    query[..boundary].to_owned()
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut b = idx;
    while b > 0 && !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browse(table: &str) -> WorkspaceTabRecord {
        WorkspaceTabRecord::Browse {
            schema: None,
            table: table.into(),
            offset: 0,
            page_size: DEFAULT_PAGE_SIZE,
            sort_col: None,
            sort_asc: None,
        }
    }

    fn editor(query: &str) -> WorkspaceTabRecord {
        WorkspaceTabRecord::Editor { query: query.into() }
    }

    fn connection(query: &str) -> ConnectionWorkspaceState {
        ConnectionWorkspaceState {
            tabs: vec![editor(query)],
            active_idx: 0,
        }
    }

    #[test]
    fn workspace_queue_coalesces_each_connection_to_its_newest_state() {
        let mut queue = WorkspaceQueue::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        queue.save(first, connection("SELECT 1"));
        queue.save(second, connection("SELECT 2"));
        queue.save(first, connection("SELECT 3"));

        let pending = queue.take_pending();
        assert_eq!(pending.len(), 2);
        let first_state = pending.iter().find(|(id, _)| *id == first).unwrap();
        assert_eq!(first_state.1.sequence, 3);
        assert!(matches!(&first_state.1.state.tabs[0], WorkspaceTabRecord::Editor { query } if query == "SELECT 3"));
    }

    #[test]
    fn workspace_queue_flush_waits_for_all_prior_window_saves() {
        let mut queue = WorkspaceQueue::default();
        queue.save(Uuid::new_v4(), connection("SELECT 1"));
        queue.save(Uuid::new_v4(), connection("SELECT 2"));
        let (sender, receiver) = mpsc::channel();
        queue.flush(sender);

        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
        queue.complete_attempt(1, Ok(()));
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
        queue.complete_attempt(2, Ok(()));
        assert_eq!(receiver.try_recv(), Ok(Ok(())));
    }

    #[test]
    fn workspace_queue_failed_write_is_not_acknowledged_as_persisted() {
        let mut queue = WorkspaceQueue::default();
        let id = Uuid::new_v4();
        queue.save(id, connection("SELECT 1"));
        let pending = queue.take_pending();
        let (sender, receiver) = mpsc::channel();
        queue.flush(sender);

        queue.restore_pending(pending);
        queue.complete_attempt(1, Err(WorkspaceFlushError::WriteFailed));

        assert_eq!(queue.persisted_sequence, 0);
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(receiver.try_recv(), Ok(Err(WorkspaceFlushError::WriteFailed)));
    }

    #[test]
    fn workspace_queue_failed_write_requires_a_successful_retry() {
        let mut queue = WorkspaceQueue::default();
        let id = Uuid::new_v4();
        queue.save(id, connection("SELECT 1"));
        let failed = queue.take_pending();
        let (failed_sender, failed_receiver) = mpsc::channel();
        queue.flush(failed_sender);

        queue.restore_pending(failed);
        queue.complete_attempt(1, Err(WorkspaceFlushError::WriteFailed));
        assert_eq!(failed_receiver.try_recv(), Ok(Err(WorkspaceFlushError::WriteFailed)));

        let (retry_sender, retry_receiver) = mpsc::channel();
        queue.flush(retry_sender);
        assert!(matches!(retry_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let retried = queue.take_pending();
        assert_eq!(retried.len(), 1);
        queue.complete_attempt(1, Ok(()));

        assert_eq!(queue.persisted_sequence, 1);
        assert_eq!(retry_receiver.try_recv(), Ok(Ok(())));
    }

    #[test]
    fn workspace_queue_preserves_saves_queued_during_an_older_write() {
        let mut queue = WorkspaceQueue::default();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        queue.save(first, connection("SELECT 1"));
        let first_batch = queue.take_pending();

        queue.save(second, connection("SELECT 2"));
        queue.save(first, connection("SELECT 3"));
        let (sender, receiver) = mpsc::channel();
        queue.flush(sender);

        queue.complete_attempt(first_batch[0].1.sequence, Ok(()));
        assert!(matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)));

        let second_batch = queue.take_pending();
        assert_eq!(second_batch.len(), 2);
        let attempted_sequence = second_batch.iter().map(|(_, entry)| entry.sequence).max().unwrap();
        queue.complete_attempt(attempted_sequence, Ok(()));

        assert_eq!(queue.persisted_sequence, 3);
        assert_eq!(receiver.try_recv(), Ok(Ok(())));
    }

    #[test]
    fn failed_older_write_does_not_replace_newer_pending_state() {
        let mut queue = WorkspaceQueue::default();
        let id = Uuid::new_v4();
        queue.save(id, connection("SELECT 1"));
        let failed = queue.take_pending();
        queue.save(id, connection("SELECT 2"));

        queue.restore_pending(failed);

        let pending = queue.take_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.sequence, 2);
        assert!(matches!(&pending[0].1.state.tabs[0], WorkspaceTabRecord::Editor { query } if query == "SELECT 2"));
    }

    #[test]
    fn prefetch_cannot_replace_a_save_started_while_snapshot_is_loading() {
        let memory_lock = Arc::new(Mutex::new(()));
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let queue = Arc::new(Mutex::new(WorkspaceQueue::default()));
        let id = Uuid::new_v4();
        let mut disk_state = WorkspaceState::default();
        disk_state.connections.insert(id.to_string(), connection("SELECT old"));
        let (load_started_sender, load_started_receiver) = mpsc::sync_channel(0);
        let (continue_load_sender, continue_load_receiver) = mpsc::sync_channel(0);

        let prefetch_memory_lock = Arc::clone(&memory_lock);
        let prefetch_cache = Arc::clone(&cache);
        let prefetch = std::thread::spawn(move || {
            prefetch_connections_coordinated(
                &prefetch_memory_lock,
                &prefetch_cache,
                &[id],
                || {},
                || {
                    load_started_sender.send(()).unwrap();
                    continue_load_receiver.recv().unwrap();
                    disk_state
                },
            );
        });
        load_started_receiver.recv().unwrap();

        let save_memory_lock = Arc::clone(&memory_lock);
        let save_cache = Arc::clone(&cache);
        let save_queue = Arc::clone(&queue);
        let save = std::thread::spawn(move || {
            save_connection_coordinated(
                &save_memory_lock,
                &save_cache,
                &save_queue,
                id,
                connection("SELECT new"),
            );
        });

        continue_load_sender.send(()).unwrap();
        prefetch.join().unwrap();
        save.join().unwrap();

        let cached = cache.lock().unwrap().get(&id).cloned().flatten().unwrap();
        assert!(matches!(&cached.tabs[0], WorkspaceTabRecord::Editor { query } if query == "SELECT new"));
        let pending = queue.lock().unwrap().take_pending();
        assert_eq!(pending.len(), 1);
        assert!(matches!(&pending[0].1.state.tabs[0], WorkspaceTabRecord::Editor { query } if query == "SELECT new"));
    }

    #[test]
    fn bounded_query_copies_only_the_utf8_safe_limit() {
        let mut query = "a".repeat(MAX_QUERY_BYTES - 1);
        query.push('é');
        query.push_str(&"b".repeat(MAX_QUERY_BYTES));

        let bounded = bounded_query(&query);

        assert_eq!(bounded.len(), MAX_QUERY_BYTES - 1);
        assert!(bounded.is_char_boundary(bounded.len()));
    }

    #[test]
    fn clamp_truncates_tabs_beyond_limit() {
        let mut conn = ConnectionWorkspaceState {
            tabs: (0..40).map(|i| browse(&format!("t{i}"))).collect(),
            active_idx: 35,
        };
        clamp_connection(&mut conn);
        assert_eq!(conn.tabs.len(), MAX_TABS_PER_CONNECTION);
        assert_eq!(conn.active_idx, 0);
    }

    #[test]
    fn clamp_handles_mixed_browse_and_editor_tabs() {
        let mut conn = ConnectionWorkspaceState {
            tabs: vec![browse("users"), editor("SELECT 1"), browse("orders")],
            active_idx: 1,
        };
        clamp_connection(&mut conn);
        assert_eq!(conn.tabs.len(), 3);
        assert_eq!(conn.active_idx, 1);
        assert!(matches!(conn.tabs[1], WorkspaceTabRecord::Editor { .. }));
    }

    #[test]
    fn clamp_replaces_foreign_browse_page_size() {
        let mut conn = ConnectionWorkspaceState {
            tabs: vec![WorkspaceTabRecord::Browse {
                schema: None,
                table: "t".into(),
                offset: 0,
                page_size: 999_999,
                sort_col: None,
                sort_asc: None,
            }],
            active_idx: 0,
        };
        clamp_connection(&mut conn);
        // Legacy Browse migrates to Table(Data) and the foreign page
        // size is replaced with the default in the same pass.
        match &conn.tabs[0] {
            WorkspaceTabRecord::Table { mode, page_size, .. } => {
                assert_eq!(*mode, PersistedTableMode::Data);
                assert_eq!(*page_size, DEFAULT_PAGE_SIZE);
            }
            _ => panic!("expected Table after migration"),
        }
    }

    #[test]
    fn clamp_truncates_long_query_at_char_boundary() {
        let mut q = "a".repeat(MAX_QUERY_BYTES - 1);
        q.push('é');
        let mut conn = ConnectionWorkspaceState {
            tabs: vec![editor(&q)],
            active_idx: 0,
        };
        clamp_connection(&mut conn);
        match &conn.tabs[0] {
            WorkspaceTabRecord::Editor { query } => {
                assert!(query.is_char_boundary(query.len()));
                assert!(query.len() <= MAX_QUERY_BYTES);
            }
            _ => panic!("expected Editor"),
        }
    }

    #[test]
    fn round_trip_preserves_mixed_tabs() {
        let mut state = WorkspaceState::default();
        let id = Uuid::new_v4();
        state.connections.insert(
            id.to_string(),
            ConnectionWorkspaceState {
                tabs: vec![
                    browse("users"),
                    editor("SELECT * FROM orders"),
                    WorkspaceTabRecord::Browse {
                        schema: Some("public".into()),
                        table: "products".into(),
                        offset: 5000,
                        page_size: 5_000,
                        sort_col: Some(2),
                        sort_asc: Some(false),
                    },
                ],
                active_idx: 1,
            },
        );
        let bytes = serde_json::to_vec(&state).unwrap();
        let parsed: WorkspaceState = serde_json::from_slice(&bytes).unwrap();
        let conn = parsed.connections.get(&id.to_string()).unwrap();
        assert_eq!(conn.tabs.len(), 3);
        assert_eq!(conn.active_idx, 1);
        match &conn.tabs[2] {
            WorkspaceTabRecord::Browse { sort_col, sort_asc, .. } => {
                assert_eq!(*sort_col, Some(2));
                assert_eq!(*sort_asc, Some(false));
            }
            _ => panic!("expected Browse"),
        }
    }

    #[test]
    fn a_structure_record_restores_as_a_structure_tab() {
        let record = WorkspaceTabRecord::Structure {
            schema: Some("public".into()),
            table: "users".into(),
        };
        assert_eq!(
            restored_workspace_tab(&record),
            Some(RestoredWorkspaceTab::Structure {
                schema: Some("public".into()),
                table: "users".into(),
            })
        );
    }

    #[test]
    fn a_table_record_in_structure_mode_restores_as_a_structure_tab() {
        let record = WorkspaceTabRecord::Table {
            schema: Some("public".into()),
            table: "users".into(),
            mode: PersistedTableMode::Structure,
            offset: 100,
            page_size: DEFAULT_PAGE_SIZE,
            sort_col: Some(1),
            sort_asc: Some(true),
        };
        assert_eq!(
            restored_workspace_tab(&record),
            Some(RestoredWorkspaceTab::Structure {
                schema: Some("public".into()),
                table: "users".into(),
            })
        );
    }

    #[test]
    fn a_table_record_in_data_mode_restores_browse_state() {
        let record = WorkspaceTabRecord::Table {
            schema: None,
            table: "jobs".into(),
            mode: PersistedTableMode::Data,
            offset: 50,
            page_size: 100,
            sort_col: Some(2),
            sort_asc: Some(false),
        };
        assert_eq!(
            restored_workspace_tab(&record),
            Some(RestoredWorkspaceTab::Table {
                schema: None,
                table: "jobs".into(),
                offset: 50,
                page_size: 100,
                sort: Some((2, false)),
            })
        );
    }

    #[test]
    fn an_empty_structure_table_is_not_restored() {
        let record = WorkspaceTabRecord::Structure {
            schema: None,
            table: String::new(),
        };
        assert_eq!(restored_workspace_tab(&record), None);
    }

    #[test]
    fn legacy_record_loads_with_serde_defaults() {
        // Forward-compat: missing optional fields fall back to defaults.
        let json = r#"{"connections":{"abc":{"tabs":[{"kind":"browse","schema":null,"table":"t"},{"kind":"editor"}],"active_idx":0}}}"#;
        let parsed: WorkspaceState = serde_json::from_str(json).unwrap();
        let tabs = &parsed.connections["abc"].tabs;
        match &tabs[0] {
            WorkspaceTabRecord::Browse { offset, page_size, .. } => {
                assert_eq!(*offset, 0);
                assert_eq!(*page_size, DEFAULT_PAGE_SIZE);
            }
            _ => panic!("expected Browse"),
        }
        match &tabs[1] {
            WorkspaceTabRecord::Editor { query } => assert_eq!(query, ""),
            _ => panic!("expected Editor"),
        }
    }
}
