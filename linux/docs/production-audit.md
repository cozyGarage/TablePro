# Production-readiness audit

**Date**: 2026-08-11

**Branch**: `linux`

**State**: capable personal database client; not ready for trusted production writes or unattended agents

This document records current implementation and verification evidence. The repository-level [`PLAN.md`](../../PLAN.md) owns sequencing and acceptance criteria; [`ROADMAP.md`](../ROADMAP.md) is the concise status view.

## Evidence collected

At audit time:

- The Rust workspace contains 15 crates and approximately 37,000 lines of Rust.
- 376 local tests passed; one Secret Service integration test was ignored.
- The GTK application contributed 103 passing unit tests.
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
- Read-only enforcement at the policy layer for classified writes, including data-changing CTEs
- Principal-aware guarded connection handles
- Blast-radius estimation and agent result masking
- GTK approval dialog
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

## Release-blocking findings

### 1. Approval can default to automatic

`DatabaseService` starts with `AutoApproveSink`. GTK approval is installed later as a side effect of MCP startup, and the same mutable sink serves human and agent principals. A production action can therefore reach an automatic approval path before the correct UI sink is installed.

Required correction: route approval by principal and runtime. Production GUI and daemon code must never construct automatic approval.

### 2. Audit failure silently becomes no audit

Both the GTK service and headless daemon catch `AuditJournal::open_default()` failure and replace it with `NullAuditSink`. Production mutations and agent operations can continue without durable evidence.

Required correction: record durable intent before mutations, make audit errors observable, fail closed for governed operations, and verify journal writability, permissions, recovery, concurrency, and cross-process locking.

### 3. Read-only precedence is incomplete

The policy evaluates unparseable SQL before it checks the connection's read-only setting. Human policy may therefore approve SQL that cannot be proven read-only.

Required correction: read-only denies every statement that cannot be proven read-only before approval is considered.

### 4. Administrative SQL is classified as a read

Calls such as `SELECT pg_terminate_backend(pid)` have administrative side effects despite their SELECT syntax. Current session IDs are accepted as strings and interpolated into driver-specific SQL.

Required correction: add an administrative operation class and accept only parsed numeric identifiers.

### 5. MCP query history bypasses connection isolation

`search_query_history` reads the shared history database without applying the connection allowlist, SQL-literal redaction, policy, masking, or an audit event.

Required correction: remove the tool until it is connection-isolated and governed.

### 6. Partial policy files can weaken production defaults

Serde field defaults are generic `EnvPolicy::default()` values. A partial `[environments.prod]` section can receive `agent_writes = approve` instead of inheriting secure production defaults.

Required correction: deserialize optional overrides and merge them field by field over the selected environment's defaults.

### 7. PostgreSQL `VerifyFull` through SSH is unresolved

The core model separates the physical dial endpoint from the database service identity. SQL Server consumes this for TLS and Kerberos, but sqlx 0.8.6 does not expose separate PostgreSQL dial and TLS-server-name inputs.

Required correction: use a supported sqlx API, a reviewed sqlx patch, or a connector that verifies the original hostname over the tunneled stream. Never downgrade verification silently.

### 8. Cancellation is not proven on the server

The editor can race a cancellation token or timeout against the query future. Dropping a future does not prove PostgreSQL stopped the operation, and it can prevent a terminal audit outcome.

Required correction: add a cancellable driver contract, send server cancellation, confirm the query leaves `pg_stat_activity`, discard an untrustworthy connection, and record cancelled, timed-out, or unknown outcomes.

### 9. Release tests are missing

There is no deterministic PostgreSQL TLS/SSH/reconnect fixture and no GTK end-to-end safety test. Unit coverage is strong, but the most important safety properties cross driver, policy, storage, and UI boundaries.

Required correction: add the focused release fixture and three GTK approval/audit flows defined in `PLAN.md`.

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
