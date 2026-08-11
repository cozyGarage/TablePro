# Production-readiness audit

**Date**: 2026-08-11

**Branch**: `linux`

**State**: capable personal database client; not ready for trusted production writes or unattended agents

This document records current implementation and verification evidence. The repository-level [`PLAN.md`](../../PLAN.md) owns sequencing and acceptance criteria; [`ROADMAP.md`](../ROADMAP.md) is the concise status view.

## Evidence collected

At audit time:

- The Rust workspace contains 15 crates and approximately 37,000 lines of Rust.
- 387 fast local tests and 5 MCP policy integration tests passed; one Secret Service integration test was ignored.
- The GTK application contributed 104 passing unit tests.
- File-size and formatting gates passed.
- `cargo deny check` and `cargo audit --no-fetch` passed. The unmaintained `paste` advisory is an explicitly allowed warning.
- Arch's Rust 1.97 exposed a new Clippy lint in the MongoDB parser even though the project declares Rust 1.93. This demonstrates the need to test both the MSRV and current stable compiler.
- Real driver integration tests and GTK end-to-end tests were not run as part of this audit.

## What exists

### Application workflows

- Native GTK4/libadwaita application using Relm4
- Saved connections in XDG JSON storage and secrets in Secret Service/libsecret
- Multi-window, multi-tab browse, SQL editor, and structure workspace
- Workspace restoration, connection recency, preferences, and persisted column widths
- Virtualized data grid with sorting, parameterized filters, pagination, inline edit, staged inserts/updates/deletes, undo/redo, save, and discard
- SQL editor with GtkSourceView highlighting, formatting, run-at-cursor, multiple statements, multiple result tabs, history recording, timeout, and cancel UI
- Table structure editing for columns, indexes, and foreign keys
- Query history backed by SQLite FTS5 with filtering, pinning, deletion, and export
- CSV table export that pages through rows, plus JSON export
- Server activity SQL, lock/replication views, session termination UI, and EXPLAIN display

### Drivers

Stable labels:

- PostgreSQL
- MySQL
- SQLite
- SQL Server, including password and current-ticket Kerberos authentication
- ClickHouse

Experimental labels:

- Redis
- MongoDB
- DuckDB, behind the `duckdb` Cargo feature
- Oracle, behind the `odpi` Cargo/native-client feature

“Stable” currently means the common connect/browse/query/write path is implemented. It does not yet mean the full release matrix has been exercised for TLS, cancellation, reconnect, transactions, large results, and packaging.

### Safety and automation foundations

- SQL classification through `sqlparser`
- Environment-aware policy rules
- Read-only enforcement before approval fallback, including unparseable SQL and data-changing CTEs
- Administrative classification for PostgreSQL backend-control functions and MySQL `KILL`
- Principal-aware guarded connection handles
- Blast-radius estimation and agent result masking
- Principal-aware GTK approval routing that denies when no application window is active
- Hash-chained JSONL audit journal
- Interactive transactions for PostgreSQL, MySQL, and SQLite
- MCP over stdio and loopback HTTP with scoped tokens, connection allowlists, and rate limiting
- Headless `tablepro-agentd`
- SSH tunnels and nested jump chains
- Five TLS modes with `VerifyFull` as the secure network default

### Engineering and distribution foundations

- Layered Rust workspace with static driver registration
- Typed errors and user-facing error translation
- File-size ratchet, rustfmt, Clippy, unit tests, cargo-deny, and cargo-audit checks
- Docker integration suites for PostgreSQL, MySQL, SQL Server, and ClickHouse in CI
- Debian, AUR, and Flatpak scaffolding
- gettext infrastructure and an accessibility checklist

## Phase 1 corrections verified in code and local tests

- `DatabaseService` starts with deny and the GUI installs its approval router during startup, independently of MCP startup.
- Production GUI and daemon code do not construct `AutoApproveSink`; agentd exposes deny and interactive TTY modes only.
- GTK approval without an active application window denies, as does closing or dismissing the dialog.
- Read-only enforcement runs before unparseable-statement handling.
- PostgreSQL backend-control functions and MySQL `KILL` are administrative writes. Token-aware detection covers predicates and wrapper expressions without treating comments or string literals as calls.
- Session termination accepts only positive numeric identifiers before driver-specific SQL is built.
- MCP no longer advertises or dispatches `search_query_history`.
- Partial environment and connection policy sections overlay secure environment defaults; omitted mask patterns retain sensitive-field defaults.
- Transactional batches authorize once per batch rather than once per statement.

These corrections are implemented and unit/integration tested. They are not yet release-verified through the real GTK dismissal and approve-once flows; Phase 4 owns those tests.

## Remaining release-blocking findings

### 1. Audit failure silently becomes no audit

Both the GTK service and headless daemon catch `AuditJournal::open_default()` failure and replace it with `NullAuditSink`. Production mutations and agent operations can continue without durable evidence.

Required correction: record durable intent before mutations, make audit errors observable, fail closed for governed operations, and verify journal writability, permissions, recovery, concurrency, and cross-process locking.

### 2. PostgreSQL `VerifyFull` through SSH is unresolved

The core model separates the physical dial endpoint from the database service identity. SQL Server consumes this for TLS and Kerberos, but sqlx 0.8.6 does not expose separate PostgreSQL dial and TLS-server-name inputs.

Required correction: use a supported sqlx API, a reviewed sqlx patch, or a connector that verifies the original hostname over the tunneled stream. Never downgrade verification silently.

### 3. Cancellation is not proven on the server

The editor can race a cancellation token or timeout against the query future. Dropping a future does not prove PostgreSQL stopped the operation, and it can prevent a terminal audit outcome.

Required correction: add a cancellable driver contract, send server cancellation, confirm the query leaves `pg_stat_activity`, discard an untrustworthy connection, and record cancelled, timed-out, or unknown outcomes.

### 4. Release tests are missing

There is no deterministic PostgreSQL TLS/SSH/reconnect fixture and no GTK end-to-end safety test. Unit coverage is strong, but the most important safety properties cross driver, policy, storage, and UI boundaries.

Required correction: add the focused release fixture and three GTK approval/audit flows defined in `PLAN.md`. Those tests must promote the Phase 1 authorization work from locally verified to release-verified.

## Partial or overstated features

- Parquet export is a stub that returns Unsupported; only CSV streaming is implemented.
- Arbitrary query results are still materialized, with a high row cap; they are not true result streaming.
- TLS fingerprint fields exist in the model but are not a complete storage/UI/driver workflow.
- SSH jump chains exist in saved JSON but are not fully editable in the GTK connection form.
- Packaging files build artifacts, but installation, launch, update, keyring, SSH, and Kerberos behavior are not release-verified.
- English gettext infrastructure exists; complete translations do not.
- Accessibility labels/checklists exist; full keyboard and screen-reader validation does not.

## Swift parity position

The Linux application matches the Swift application's core connection, query, browse, edit, structure, history, SSH, and explain workflows only partially. High-value gaps for Platform/DBA/Data Engineering work include:

- Schema-aware autocomplete, named parameters, SQL favorites, and quick switching
- SQL file workflows
- Views, routines, triggers, sequences, extensions, users, roles, and privileges
- Backup/restore and CSV/JSON/SQL import
- Connection URL import, organization, reusable SSH profiles, and certificate configuration
- True large-result streaming
- Redshift/CockroachDB profiles, Trino, Snowflake, and BigQuery

Apple sync, Handoff, Swift plugins, licensing, team accounts, and Sparkle are intentional non-ports.

## Production readiness decision

Safe for current development and personal testing:

- Local/dev browsing and querying against non-critical databases
- SQLite-based UI work
- Driver development with disposable integration databases
- Read-only exploration when the user independently limits database credentials

Not approved as trusted behavior yet:

- Production mutations
- Unattended MCP or agent writes
- Relying on the audit journal as durable evidence
- Assuming cancel stopped a server query
- PostgreSQL `VerifyFull` through SSH
- Public stable package distribution

Re-run this audit after Phases 1–4 of `PLAN.md` are release-verified.
