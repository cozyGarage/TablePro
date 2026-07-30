# Production-readiness audit

**Date**: 2026-07-30
**Branch**: `linux`
**State**: daily-driver capable client; not yet a governed data plane

This document replaces the 2026-04-26 audit. That snapshot described a
demo with three drivers and ~31 tests. The tree has moved far past that.
This audit records what is actually shipped, what is still wrong, and
why the roadmap pivoted to a policy chokepoint plus agent access.

---

## What we have today

### Functional

- Four drivers: PostgreSQL (sqlx), MySQL (sqlx), SQLite (sqlx), MSSQL (tiberius)
- Connect dialog with engine picker, TLS toggle, read-only switch, SSH section
- Saved connections (JSON + libsecret) with delete, reconnect, last-opened
- Multi-tab workspace (`AdwTabView`): browse, SQL editor, structure/DDL
- Virtualized `GtkColumnView` browse with inline cell edit, drafts, undo
- Filter strip with parameterized `query_params`; column-header sort
- Pagination (offset/limit), CSV/JSON export, context menus
- Structure tab: columns, indexes, FKs; create/edit/drop table
- SQL editor: GtkSourceView 5, run/cancel, timeout, multi-statement result
  sub-tabs, format, run-at-cursor, line comment, history recording
- SSH tunnel (russh), single-hop, TOFU known_hosts
- Auto-reconnect (30s ping, exponential backoff)
- Query history (SQLite FTS5), preferences dialog, column-width persistence,
  workspace-state persistence, single-instance flock

### Engineering

- Relm4 SimpleComponent / Component / FactoryComponent architecture
- `DatabaseService` singleton owning multi-connection map + health monitors
- Typed errors with user-friendly message layer
- ~294 unit / lib tests; Postgres + MySQL integration tests in CI (ignored
  locally, run with `--include-ignored` in CI)
- gettext-rs + `tr!` infrastructure (`po/` exists; LINGUAS empty)
- Flatpak manifest, metainfo, desktop file, scalable SVG icon
- Linux CI: fmt, clippy `-D warnings`, build, lib tests; integration job

### Scale

~28k lines of Rust across ~65 `.rs` files under `crates/`.

---

## What "production-ready" means now

For a personal DBA / DE / SRE daily driver who also wants safe AI-agent
access:

1. Install from Flathub (or run from source) on Fedora / Ubuntu / Arch
2. Connect to everyday Postgres / MySQL / SQLite / MSSQL with verified TLS
   and optional SSH
3. Browse and edit real types (dates, decimals, JSON, UUIDs) correctly
4. Run queries against large tables without OOM
5. Treat **environment** (Local / Dev / Staging / Prod) as enforced policy,
   not a cosmetic tag
6. Let Cursor / Claude Code talk to the DB only through a gated MCP
   surface with approval, row caps, and an audit journal
7. Recover from network blips; cancel long queries
8. Accessibility and i18n infrastructure in place

Today we meet (3) partially, (4) partially (10k materialization cap), (5)
not at all, (6) not at all, (7) partially, (8) partially.

---

## Critical gaps (ordered)

### 1. Read-only is bypassable

`ReadOnlyConnection` blocks `execute*` but passes `query` /
`query_params` through. On PostgreSQL a data-modifying CTE is a write
delivered as a query. String / regex checks miss this. Fix: AST
classification via `sqlparser`, fail closed on parse errors, absorb
read-only into a policy rule.

### 2. TLS does not authenticate the server

`PgSslMode::Require`, `MySqlSslMode::Required`, and MSSQL
`EncryptionLevel::Required` + `trust_cert()` encrypt without verifying
the certificate chain or hostname. MITM-able. Fix: `TlsConfig` with
`VerifyFull` default and cert-fingerprint TOFU fallback.

### 3. No policy chokepoint

`DatabaseService::get` / `active` return `Arc<dyn Connection>` to any
caller. There is no principal (human vs agent), no statement-level rules,
no approval sink, no blast-radius precheck, no column masking for agents.

### 4. No agent surface

Zero MCP, zero chat, zero headless daemon. The macOS app has a mature MCP
server; Linux has none. The roadmap now sequences MCP → agentd → optional
chat on top of Stage 1 policy.

### 5. No audit journal

Query history records what you ran. It does not answer "what did an agent
do at 03:00" with principal, decision, rule, and outcome. Need a separate
append-only hash-chained journal.

### 6. Interactive transactions missing

`execute_in_transaction` is all-or-nothing. Agent write preview needs
`begin` → execute → report rows → commit / rollback.

### 7. Result scaling

`MAX_QUERY_ROWS = 10_000`; full materialization; OFFSET pagination at high
offsets is a linear rescan. Streaming and keyset pagination are Stage 5.

### 8. TLS / cert UI and jump hosts

No custom CA / client-cert UI; SSH is single-hop only.

### 9. Distribution and a11y

Metainfo lists only three engines (MSSQL missing). No Flathub screenshots.
Orca / keyboard-only untested. LINGUAS empty.

### 10. Observability and supply chain

No `cargo-deny` / `cargo-audit` in CI. MSSQL integration tests not in CI.

---

## What we are not chasing next

ER diagrams, vim mode, multi-cursor, snippet systems, AppImage/.deb/.rpm
packaging for a personal audience, and alphabetical driver expansion ahead
of ClickHouse → Redis → DuckDB → MongoDB → Oracle.

---

## Critical path (matches ROADMAP stages)

1. Stage 0: docs + CI truth
2. Stage 1: policy crate, TLS fix, journal, transactions, chokepoint
3. Stage 2: MCP in the GTK app
4. Stage 3: headless agentd
5. Stage 5: activity / EXPLAIN / scale / drivers (reorderable)
6. Stage 6: Flathub + a11y + i18n finish
7. Stage 4: built-in chat (optional, last)

Stages 1–2 are ~8 focused weeks and deliver the entire "safely work with
an agent" outcome. Protect them from scope creep.

---

## Effort tiers

| Tier | Covers | Status |
|---|---|---|
| Working client | 4 drivers, multi-tab, edit, SSH, history | **current** |
| Governed personal | Policy + TLS + journal + MCP | Stage 1–2 |
| Headless SRE | + agentd | Stage 3 |
| DBA depth | + activity, EXPLAIN, streaming, more drivers | Stage 5 |
| Shipped daily driver | + Flathub, a11y, i18n | Stage 6 |

This audit is a snapshot at 2026-07-30. Re-run when Stage 1 lands.
