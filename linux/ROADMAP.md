# TablePro Linux roadmap

Last audited: 2026-08-13

The repository-level [`PLAN.md`](../PLAN.md) is the source of truth for sequencing, detailed acceptance criteria, and the Swift parity backlog. This file is the concise Linux status view.

## Current state

TablePro Linux is a substantial GTK4/libadwaita database client, not a prototype. Its core daily-driver workflows are implemented, but production approval, audit enforcement, PostgreSQL cancellation/TLS verification, and release-level testing still block a trusted release.

Status terms:

- **Implemented**: code and core unit tests exist.
- **Integrated**: all intended production entry points use it.
- **Release-verified**: deterministic real-driver, UI, or installed-package tests prove it.
- **Complete**: all phase acceptance criteria are release-verified.

## Verified inventory

| Area | Status | Notes |
|---|---|---|
| PostgreSQL, MySQL, SQLite, SQL Server, ClickHouse | Implemented | Stable driver label; release matrix is incomplete |
| Redis, MongoDB, DuckDB, Oracle | Implemented | Experimental; DuckDB/Oracle require Cargo/native features |
| Browse/edit/filter/sort/pagination | Implemented | Keyset helper exists; large-result behavior needs release tests |
| SQL editor and multiple result tabs | Implemented | Client timeout/cancel UI exists; server cancellation is unverified |
| Structure editor | Implemented | Tables, columns, indexes, and foreign keys |
| Saved connections and libsecret | Implemented | Keyring failure UX needs hardening |
| SSH and jump chains | Implemented | Jump chains are JSON-only in the current GTK form |
| TLS modes | Partially integrated | PostgreSQL `VerifyFull` through SSH remains unresolved |
| Query history | Implemented | MCP access must be isolated before being re-exposed |
| CSV/JSON export | Implemented | CSV streams table pages; Parquet is unsupported |
| Activity and EXPLAIN | Implemented | Administrative classification and numeric session-ID validation are covered |
| Policy, MCP, and agentd | Integrated | Approval and audit failures deny governed operations; release fixtures remain open |
| Audit journal | Integrated | Durable intent/outcome records, recovery, private mode, and cross-process locking are locally verified |
| AUR, Debian, Flatpak | Scaffolded | Not release-verified or ready for a public stable package |
| i18n and accessibility | Infrastructure | English strings/checklist exist; end-user verification is incomplete |

## Active phases

### 0 — Development baseline

- [x] Reconcile upstream SQL Server Kerberos and service identity without losing fork safety work
- [x] Preserve legacy connection serialization
- [x] Pass Clippy on Rust 1.93 and current stable Rust
- [x] Document the Arch/rustup toolchain setup
- [x] Add scheduled current-stable CI
- [x] Reject untrusted browser origins on the MCP loopback endpoint
- [x] Detect SSH host-key changes across key algorithms
- [x] Pin Linux GitHub Actions to immutable commits
- [x] Log upstream reconciliations from 2026-08-10 onward

Phase 0 is complete locally. Rust 1.93 CI is green, current stable Clippy passes locally, and the first hosted current-stable schedule is expected after push. Real SQL Server TLS and Kerberos negotiation remain release-fixture work in Phase 3.

### 1 — Authorization and approval

- [x] Replace production automatic approval with principal-aware routing
- [x] Enforce read-only before unparseable-statement fallback
- [x] Classify administrative and PostgreSQL side-effecting functions
- [x] Preserve DDL and unscoped DML restrictions across mixed transaction batches
- [x] Validate numeric session identifiers before building SQL
- [x] Require explicit saved-connection allowlists when agentd issues tokens
- [x] Remove or isolate MCP query-history search
- [x] Merge partial policies onto secure environment defaults

Phase 1 is implemented, security-reviewed, and locally verified. Phase 4 still owns release-level GTK proof for dialog dismissal and approve-once behavior.

### 2 — Fail-closed audit

- [x] Disable governed writes and in-app MCP when audit initialization fails
- [x] Record durable intent before mutations and transaction completion
- [x] Record explicit, sanitized outcomes after operations
- [x] Verify journal mode, writability, legacy migration, recovery, and cross-process locking
- [x] Refuse agent service when audit storage is unavailable or unresolved writes exist
- [x] Persist unresolved write state across restarts and concurrent processes

Phase 2 is implemented, security-reviewed, and locally verified. Phase 3 still owns PostgreSQL server cancellation and real-driver terminal outcome proof.

### 3 — PostgreSQL release safety

- [ ] Verify direct and SSH `VerifyFull`
- [ ] Implement server-side cancellation
- [ ] Verify rollback, activity, locks, reconnect, and SSH reconnect
- [ ] Add deterministic PostgreSQL release fixture

### 4 — GTK safety tests

- [ ] Dismissed approval denies
- [ ] Approve-once approves exactly one operation
- [ ] Audit failure cannot be approved around

### 5 — Documentation and Swift parity

- [x] Replace the historical roadmap with evidence-based status
- [ ] Keep the production audit synchronized with code and tests
- [ ] Maintain the Swift-to-Linux parity matrix
- [ ] Distinguish implementation from release verification in every claim

### 6 — DBA and data-engineering depth

- [ ] SQL autocomplete, parameters, favorites, and quick switcher
- [ ] PostgreSQL objects, users/roles, and administration
- [ ] Import/export and backup/restore
- [ ] Connection organization and reusable transport profiles
- [ ] True result streaming and optional Parquet export

### 7 — Driver expansion

Prioritize real workflows: Redshift/CockroachDB profiles, Trino, Snowflake, then BigQuery. Other Swift drivers remain demand-driven.

### 8 — Identity and packaging

Finalize the product name, repository, and application ID before publishing. AUR/Omarchy is first; Flatpak follows after sandbox and update behavior are verified.

### 9 — Repository cleanup

Keep the macOS tree as the parity reference until useful behavior, tests, documentation, and assets are extracted. Remove it only after a standalone Linux repository is explicitly chosen and Linux paths no longer depend on it.

## Explicit non-ports

- iCloud Sync and Handoff
- Apple Keychain and CloudKit behavior
- Swift plugin ABI and registry loader
- macOS licensing, team seats, and entitlements
- Sparkle updates

## Next implementation target

Begin Phase 3 with the deterministic PostgreSQL TLS, SSH, cancellation, rollback, lock, and reconnect fixture. Production-write and agent-write workflows are not release-trusted until Phases 1–4 pass their real-driver and GTK tests.
