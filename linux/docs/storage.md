# Storage

TablePro uses XDG files for local state, SQLite for query history, a JSONL audit journal, and Secret Service for credentials. The code is split between `tablepro-storage` and focused application services.

## Locations

| Data | Format | Default path |
|---|---|---|
| Saved connections | Versioned JSON | `$XDG_CONFIG_HOME/tablepro/connections.json` |
| Preferences | JSON | `$XDG_CONFIG_HOME/tablepro/preferences.json` |
| Window state | JSON | `$XDG_CONFIG_HOME/tablepro/window.json` |
| Workspace tabs | JSON | `$XDG_CONFIG_HOME/tablepro/workspace_state.json` |
| Column widths | JSON | `$XDG_CONFIG_HOME/tablepro/column_widths.json` |
| Table filters | JSON | `$XDG_CONFIG_HOME/tablepro/filter_settings.json` |
| Query history | SQLite with FTS5 | `$XDG_CONFIG_HOME/tablepro/history.db` |
| Policy | TOML | `$XDG_CONFIG_HOME/tablepro/policy.toml` |
| Audit journal | Hash-chained JSONL | `$XDG_DATA_HOME/tablepro/audit.jsonl` |
| Database passwords, SSH passwords, SSH key passphrases, MCP secrets | Secret Service through `oo7` | Desktop keyring |

If `XDG_CONFIG_HOME` is unset, config storage falls back to `~/.config/tablepro/`. If `XDG_DATA_HOME` is unset, the audit journal falls back to `~/.local/share/tablepro/`.

The current history database is under XDG config because that is what `query_history::db_path()` implements. Do not document or migrate it to XDG data without a code change and a tested migration.

## Saved connections

`tablepro-storage` exposes `load_connections`, `save_connections`, `delete_connection`, and `touch_last_opened`. The file has a top-level version and a connection array. Loading rejects unsupported versions.

Connection records contain host, port, database, username, TLS mode, read-only state, authentication mode, environment, optional SSH configuration, and optional last-opened time. Passwords and SSH secrets are not part of the JSON record.

`save_connections` writes a temporary file beside `connections.json` and renames it over the destination. The application JSON services use the same write-then-rename shape with per-process temporary names. These writes do not claim crash durability beyond what the current code provides.

## Preferences and UI state

Application services own the JSON files for preferences, window state, workspace state, column widths, and table filters. They read from the XDG config directory and use defaults when a file is absent or cannot be decoded.

Window state stores size, maximize flag, and the last connection id. Closing a window keeps that id so the next launch can reopen the same connection. An explicit disconnect or deleting that saved connection clears it. Geometry writes do not drop the last connection id.

Workspace state is keyed by connection UUID and limited before it is written. Unknown tab variants deserialize as `Unknown` and are dropped during restore. This lets an older build ignore a newer tab kind without rejecting the whole workspace file.

## Query history

`tablepro_storage::query_history` owns a single SQLite pool. `init()` creates `history.db`, enables WAL mode, creates the `history` table, and creates an FTS5 virtual table with triggers that follow inserts, deletes, and query text updates.

History records include query text, driver and connection identity, execution time, duration, affected rows, success, cancellation, pin state, and an optional error. Search supports text, connection, outcome, time, and limit filters. Unpinned entries can be pruned by age.

A query larger than 1 MiB is rejected by the history recorder.

History export from the GUI writes through a temporary sibling file and a rename, the same way current-page CSV and JSON export do.

## Policy

The GUI loads `$XDG_CONFIG_HOME/tablepro/policy.toml` at startup. A missing file uses the built-in defaults. A file that exists but cannot be read or parsed is refused: the GUI uses defaults, disables MCP, and a later reload that fails keeps the policy already in memory. An explicit empty `mask_patterns` list is treated as the default sensitive-field patterns. `tablepro-agentd` still requires a readable `--policy` path and does not start without one.

## Audit journal

`AuditJournal::open_default()` opens `$XDG_DATA_HOME/tablepro/audit.jsonl`. Initialization creates the parent directory, sets the journal mode to `0600`, verifies the hash chain, recovers safe trailing fragments, and reports unresolved operation IDs.

Each record contains a sequence number, previous hash, current hash, and audit event. Mutation intents and transaction commit records use the durable append path. File locks serialize initialization and appends across processes.

The GTK application disables governed writes and MCP startup when the required audit journal cannot be opened. `tablepro-agentd` also refuses startup when the journal is unavailable or unresolved operations are recovered.

## Secrets

The secrets module uses `oo7::Keyring`, which talks to the Linux Secret Service API. Items use the schema `com.tablepro.linux.Password` and attributes for `connection-id` and `kind`.

The implemented kinds are:

- Database password
- SSH password
- SSH private-key passphrase
- MCP token secret

Loaded values are returned as `secrecy::SecretString`. If Secret Service cannot be opened during a load, the storage layer logs a warning and returns no secret. It does not copy the value into a JSON file.

## Change rules

When changing storage behavior:

1. Keep secrets out of JSON and logs.
2. Preserve existing file locations unless the change includes a tested migration.
3. Keep saved connection versions explicit and reject versions the code cannot read.
4. Test missing files, malformed or unsupported data, round trips, and migration behavior.
5. Keep audit failures fail-closed for governed writes. A policy file that cannot be read must not become an empty or silently defaulted policy for MCP.
6. Update this document from the implementation, not from planned APIs.

## Query favorites

`$XDG_CONFIG_HOME/tablepro/favorites.json` holds saved queries as `{version, favorites[]}`, written through a temporary file and a rename. Each entry keeps an id, name, statement, optional driver and connection ids, a creation time, and a last-used time. Saving a name that already exists replaces that entry's statement and keeps its id. The file is capped at 500 entries, and a name or statement that is only whitespace is rejected.
