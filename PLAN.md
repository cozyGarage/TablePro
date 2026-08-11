# TablePro Linux development plan

Last audited: 2026-08-12

This plan is the source of truth for the Linux application. It separates:

1. Implemented code from release-verified behavior
2. Linux safety and reliability from macOS feature parity
3. Features useful to a Platform Engineer, DBA, or Data Engineer from macOS-only product features
4. Repository cleanup from product development

The Linux application is a native Rust/GTK product. It does not share source with the Swift application, and it does not need the Swift plugin ABI, Apple sync services, licensing, or team-account features.

## Current baseline

Audited branch state:

- Phase 0 verification HEAD: `e8c1aa37`
- Tracking branch: `fork/linux`
- Cached comparison with `origin/linux`: 41 commits ahead, 1 commit behind
- Rust workspace: 15 crates and approximately 37,000 lines of Rust
- Stable drivers: PostgreSQL, MySQL, SQLite, SQL Server, ClickHouse
- Experimental drivers: Redis, MongoDB, DuckDB, Oracle
- No Linux account, subscription, receipt, license-key, or entitlement checks

Verified locally on 2026-08-12:

- File-size guard passes
- `cargo fmt --all -- --check` passes
- 387 fast tests and 5 MCP policy integration tests pass; one Secret Service integration test is ignored
- `cargo deny check` passes
- `cargo audit --no-fetch` passes with the allowed unmaintained `paste` warning
- Full-workspace strict Clippy passes on Rust 1.93.0 and Arch stable Rust 1.97.1
- The complete Rust 1.93 preflight passes, including 283 non-GTK unit tests and 5 MCP policy integration tests; one Secret Service test is ignored
- All 27 Docker driver integration tests pass: PostgreSQL 4, MySQL 4, SQL Server 9, and ClickHouse 10

The repository pins Rust 1.93, but an OS-packaged `/usr/bin/cargo` does not honor `rust-toolchain.toml` without rustup. CI must test the MSRV, while local development and a scheduled job should also test the current stable compiler.

## Product contract

TablePro Linux should provide:

- Safe PostgreSQL operation during incidents
- Fast native GTK workflows
- Verified TLS and SSH
- Dependable cancellation, reconnect, and transaction behavior
- Clear production approval and durable audit records
- Local agents through a constrained MCP interface
- Useful DBA and data-engineering workflows
- Predictable native packaging on Arch/Omarchy first

Every locally shipped Linux feature must work without an account, license key, receipt, subscription, or entitlement service.

The product name remains **TablePro Linux** until repository ownership, package names, trademarks, and the final application ID are explicitly decided. “Bookie” is a possible future rename, not an assumption in implementation work.

## Status vocabulary

- **Implemented**: code exists and unit tests cover its core logic.
- **Integrated**: the feature is connected to every production entry point that should use it.
- **Release-verified**: deterministic tests exercise the real driver, UI, or installed package behavior.
- **Complete**: all acceptance criteria for the phase are release-verified.

A checkbox may be checked only when its stated criterion is verified. A stub or partial path is not complete.

---

# Phase 0: Restore a trustworthy development baseline

## Status

Implemented and locally verified on 2026-08-12. The first hosted execution of the new scheduled current-stable job remains external confirmation after this change is pushed; it does not block Phase 2 development.

## Work

- [x] Preserve the fork's policy, MCP, audit, transaction, TLS, and SSH jump-chain architecture while reconciling upstream SQL Server Kerberos support.
- [x] Separate SQL Server's physical dial endpoint from its TLS/Kerberos service identity.
- [x] Keep legacy saved connections compatible.
- [x] Pass Clippy on Rust 1.93 and current stable Rust.
- [x] Document rustup versus distro-toolchain behavior on Arch/Omarchy.
- [x] Add a current-stable scheduled CI check without changing the Rust 1.93 MSRV.
- [x] Record every upstream Linux sync in a short sync log.

## Known transport gap

PostgreSQL through SSH cannot currently use a distinct TCP dial address and TLS server name through sqlx 0.8.6. Do not downgrade `VerifyFull` silently. Resolve this in Phase 3 through a supported sqlx API, a small reviewed sqlx change, or a PostgreSQL connector that accepts a supplied stream.

## Acceptance criteria

- [x] Rust 1.93 remains the declared MSRV in `rust-toolchain.toml`, `Cargo.toml`, and `clippy.toml`.
- [x] Preflight passes with Rust 1.93 on the supported Arch/Omarchy development setup.
- [x] Full-workspace Clippy passes with Arch stable 1.97.1 and is repeated by scheduled CI using an explicit `+stable` selector.
- [x] Direct endpoint, tunneled service identity, legacy serialization, and SQL Server password behavior remain covered by unit and real-driver integration tests.

---

# Phase 1: Close authorization and approval bypasses

## Status

Implemented and locally verified on 2026-08-11. The code-level bypasses are closed; dismissed-dialog and real GTK flow verification remain in Phase 4 before this phase is release-complete.

## 1.1 Route approval by principal and runtime

The application now installs a principal-aware approval router during GUI startup. Its default is deny, GTK approval requires an active application window, and the headless daemon offers only deny or interactive TTY approval.

Implemented rules:

- Human GUI operations always use GTK approval.
- In-app MCP operations may request GTK approval only while an active application window exists.
- In-app agent approval without an active window is denied.
- `tablepro-agentd` defaults to deny and must not offer automatic approval in production builds.
- Automatic approval remains available only to tests.
- Closing or dismissing an approval dialog denies the request.

Primary files:

- `linux/crates/app/src/services/database_service.rs`
- `linux/crates/app/src/services/mcp_service.rs`
- `linux/crates/app/src/services/gtk_approval.rs`
- New: `linux/crates/app/src/services/approval_router.rs`
- `linux/crates/agentd/src/main.rs`
- `linux/crates/policy/src/approval.rs`

## 1.2 Enforce read-only before parse fallback

Read-only enforcement now runs before unparseable-statement approval fallback. A read-only connection denies any statement that cannot be proven read-only.

## 1.3 Classify administrative side effects

`StatementClass::Administrative` covers MySQL `KILL` and PostgreSQL administrative functions, including calls nested outside the SELECT projection. A statement such as `SELECT pg_terminate_backend(pid)` is not treated as a harmless read.

- Agents with read-only access cannot terminate sessions.
- Activity termination accepts a parsed numeric session identifier, never arbitrary interpolated text.
- Administrative actions follow environment approval and audit policy.

Primary files:

- `linux/crates/policy/src/classify.rs`
- `linux/crates/policy/src/rules.rs`
- `linux/crates/policy/src/guard.rs`
- `linux/crates/core/src/activity.rs`
- `linux/crates/app/src/ui/activity_dialog.rs`

## 1.4 Isolate MCP query history

`search_query_history` is removed from the MCP tool list until it:

- Filters to explicitly allowlisted connection IDs
- Redacts SQL literals
- Uses the caller's rate limit
- Emits an audit event
- Returns nothing for an empty connection allowlist

## 1.5 Merge partial policy configuration safely

Environment and connection overrides deserialize as optional fields and merge onto the selected environment's secure defaults. A partial production configuration retains `agent_writes = deny` unless the user explicitly changes it, and omitted masking rules retain the default sensitive-field patterns.

## Acceptance criteria

- [x] No production GUI or daemon path constructs `AutoApproveSink`.
- [x] GUI startup without MCP installs GTK approval routing.
- [x] Read-only connections deny unparseable SQL and data-changing CTEs.
- [x] Agents cannot call administrative functions using read scope.
- [x] Session IDs parse as positive integers before SQL is built.
- [x] An empty MCP allowlist exposes no connection data or history.
- [x] Partial production policy retains secure defaults.
- [x] One batch requests at most one approval.
- [ ] Real GTK tests prove dismissal denies and approval applies to exactly one operation (Phase 4).

---

# Phase 2: Make audit enforcement fail closed

## Status

Not started. Release blocker.

## Required behavior

| Caller and operation | Audit unavailable |
|---|---|
| Human Local/Dev read | Allow with a persistent warning |
| Human Local/Dev write | Follow an explicit best-effort local policy |
| Human Staging/Prod read | Allow only with a visible warning initially |
| Human Staging/Prod mutation, DDL, or admin | Deny |
| In-app MCP operation | Deny |
| Headless agent operation | Refuse to start or deny |
| Transaction commit | Deny before commit if intent cannot be recorded |

Using a stricter rule that denies every Staging/Prod operation when audit is unavailable is acceptable. Falling back silently to `NullAuditSink` is not.

## Durable operation records

Every state-changing operation needs:

1. A durable intent before contacting the database
2. An outcome after completion

Intent records include operation and batch IDs, principal, connection, environment, operation class, redacted SQL, SQL hash, target objects, approval result, and preview state.

Outcome records include success, denial, cancellation, timeout or unknown state, rows affected, duration, transaction outcome, and error category. A missing outcome means unknown, not “nothing happened.”

## Journal requirements

- `AuditSink::record` returns a result.
- Journal creation uses mode `0600`.
- Opening verifies the existing hash chain and writability.
- Corrupt journals are rejected visibly.
- Writers are serialized in-process and locked across GUI/daemon processes.
- Required intent and commit records call `sync_data`.

Tests to add:

- `linux/crates/storage/tests/audit_journal_concurrency.rs`
- `linux/crates/storage/tests/audit_journal_recovery.rs`
- `linux/crates/mcp/tests/timeout_audit.rs`

## Acceptance criteria

- Audit initialization failure cannot become `NullAuditSink` in production.
- Production mutations are denied before driver execution when intent cannot be persisted.
- Agentd does not serve MCP without its required journal.
- One thousand concurrent appends produce one valid chain.
- Two processes cannot fork the sequence.
- A failure after database execution returns “operation may have succeeded” and disables further governed writes.

---

# Phase 3: Verify PostgreSQL safety and reliability

## Status

Not started. Release blocker for the primary persona.

## Driver contract

Add a cancellable-operation contract to the driver boundary. PostgreSQL must retain a server cancellation handle, request cancellation on timeout/user action, confirm the query leaves `pg_stat_activity`, and discard an untrustworthy protocol connection.

## Deterministic fixture

Add one Docker Compose fixture under `linux/tests/fixtures/postgres-release/` containing:

- PostgreSQL 16 with TLS and a hostname-specific certificate
- SSH bastion with deterministic keys
- An SSH-only database network path
- Toxiproxy or equivalent for reconnect tests
- Seed data and lock-test helpers

Required scenarios:

- Direct `VerifyFull`, wrong hostname, and unknown CA
- `VerifyFull` through SSH using the original database hostname
- Read-only data-changing CTE denial
- Server-confirmed timeout and cancellation
- Batch and interactive rollback
- Activity and blocking-lock queries
- Direct and SSH reconnect

## Acceptance criteria

- No TLS hostname downgrade occurs through SSH.
- Cancelled/timed-out queries leave the server and receive terminal audit outcomes.
- Reconnect replaces the connection and tunnel and later queries succeed.
- The fixture runs as a required release-candidate gate.

---

# Phase 4: Add targeted GTK safety tests

## Status

Not started.

Use SQLite, temporary XDG directories, `dbus-run-session`, Xvfb and AT-SPI automation. Cover only the critical flows:

1. Dismissed production approval leaves the database unchanged.
2. “Approve once” performs exactly one mutation and asks again next time.
3. An unavailable audit journal cannot be bypassed through approval.

Promote the test to a required PR check after 30 retry-free scheduled runs.

---

# Phase 5: Restore documentation and parity tracking

## Status

In progress.

## Documentation cleanup

- Rewrite `linux/docs/production-audit.md` from current code and test evidence.
- Make `linux/ROADMAP.md` a concise status view of this plan rather than a competing historical roadmap.
- Correct test counts, CI jobs, drivers, XDG locations, TLS limitations, audit behavior, cancellation behavior, and packaging maturity.
- Add user-facing changes to `linux/CHANGELOG.md` under `[Unreleased]`.

## Swift parity matrix

Maintain a feature matrix with these states: Swift reference, Linux implemented, Linux integrated, Linux release-verified, intentionally not ported.

### Already useful on Linux

- Saved connections and secret storage
- Multi-tab workspace and restoration
- SQL editor and multi-result execution
- Browse/edit/filter/sort/pagination
- Table/column/index/foreign-key editing
- Query history
- CSV and JSON export
- Activity and EXPLAIN
- SSH, TLS, reconnect and Kerberos foundations
- MCP and headless agent foundations

### Priority A: daily DBA and data-engineering workflows

- Schema-aware SQL autocomplete
- Named query parameters
- Saved SQL favorites and quick switcher
- SQL file open/save and external-change detection
- Views, materialized views, triggers, routines, sequences, and extensions
- PostgreSQL/MySQL users, roles, and privileges
- PostgreSQL backup/restore
- CSV, JSON, and SQL import
- SQL dump export and true large-result streaming
- Connection URL import, groups, tags, and favorites
- Reusable SSH profiles and custom CA/client certificates

### Priority B: platform transports and data systems

- SSH agent, keyboard-interactive, none-auth, and remote Unix sockets
- SOCKS5, Cloud SQL Proxy, and Cloudflare Tunnel when required
- Redshift and CockroachDB PostgreSQL-compatible profiles
- Snowflake, BigQuery, and Trino
- Cassandra/ScyllaDB, Elasticsearch, DynamoDB, and etcd based on actual use

### Deferred product features

- ER diagram
- Advanced JSON/PHP/binary cell viewers
- CSV inspector/editor
- Embedded terminal
- Vim mode and multi-cursor
- Built-in AI chat

### Intentionally not ported

- iCloud Sync and Handoff
- Apple-specific Keychain/CloudKit behavior
- Swift plugin ABI and registry runtime
- macOS licensing, team seats, and entitlement checks
- Sparkle updates

---

# Phase 6: Port persona-priority features

## Status

Not started. Begin only after Phases 1–4 are release-safe.

Deliver in small vertical slices:

1. SQL autocomplete, parameters, favorites, and quick switcher
2. PostgreSQL object browser and administration
3. Import/export and backup/restore
4. Connection organization and reusable transport profiles
5. Large-result streaming and optional Parquet export

Each slice needs driver capability declarations, unit tests, one real-driver integration path, GTK error states, documentation, and changelog entries.

---

# Phase 7: Expand drivers by actual need

## Status

Not started.

Do not pursue “25 drivers” as a vanity metric. Prioritize:

1. PostgreSQL-compatible Redshift and CockroachDB profiles
2. Trino
3. Snowflake
4. BigQuery
5. Other engines only when a real workflow and integration fixture exist

A driver becomes Stable only when connect, browse, native query dialect, common writes, type mapping, cancellation/reconnect behavior where applicable, and CI integration are verified.

---

# Phase 8: Finalize identity and package for Arch/Omarchy

## Status

Blocked on the product/repository naming decision and safety phases.

Before publishing:

- Confirm permanent repository owner and product name.
- Check package registries and trademarks.
- Choose the final reverse-DNS application ID.
- Preserve existing XDG directories, connection UUIDs, secrets, tokens, history, workspace, and audit data across any rename.

AUR is the first distribution target. The package must use a versioned source archive and real checksum, install desktop/AppStream/icon/translation/license files, pass `makepkg --cleanbuild`, `namcap`, `desktop-file-validate`, and `appstreamcli validate`, and launch under a fresh D-Bus session.

Do not enable an agent daemon automatically. If packaged, it requires explicit user configuration, a valid policy, and a working audit journal.

Flatpak follows after the identity is final and offline Cargo sources, portals, Secret Service, SSH, Kerberos, export paths, and updates are verified.

---

# Phase 9: Remove macOS material only after extraction

## Status

Deferred and gated.

The Rust workspace has no compile-time dependency on Swift. The macOS tree is nevertheless the current parity reference and upstream-sync anchor.

## Keep until parity extraction is complete

- Swift feature source and tests used to define behavior
- Root changelog and feature documentation
- Driver-specific SQL quirks and regression knowledge
- Reusable icons/assets and legal files
- Linux GitHub workflows

## Candidates for removal in a standalone Linux repository

- `TablePro/`, `TableProMobile/`, Swift tests, and Xcode projects
- `Plugins/`, `Packages/`, `LocalPackages/`, and `CloudKit/`
- macOS/iOS workflows and macOS build/release scripts
- `appcast.xml`, `.swiftlint.yml`, and `.swiftformat`
- macOS marketing documentation after useful specifications are migrated

## Cleanup acceptance criteria

- The permanent repository strategy is documented.
- Every desired Swift feature is present in the parity matrix.
- Useful tests, SQL behavior, assets, and legal material are migrated.
- Linux CI, packaging, documentation, issue links, and release gates have no removed-path references.
- Upstream synchronization has an explicit replacement process.

---

# First trusted Linux release gate

Run from `linux/`:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings
cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins
cargo deny check
cargo audit

cargo test --test integration -p tablepro-driver-postgres -- --include-ignored --test-threads=1
./scripts/test-postgres-release.sh
./scripts/test-gtk-safety.sh

cargo build --release --locked -p tablepro-app -p tablepro-agentd
```

The release is trusted only when:

- Production writes cannot bypass approval.
- Read-only blocks data-changing CTEs, administrative functions, and unparseable statements.
- Agent production writes are denied.
- Production mutations require durable audit intent.
- PostgreSQL cancellation is confirmed server-side.
- `VerifyFull` works through SSH using the original hostname.
- Reconnect restores a usable connection and tunnel.
- The installed Arch package launches and upgrades without losing user data.
- Every Linux feature remains available without a commercial gate.

SwiftLint is not a Linux release gate. It remains part of macOS CI only while the repository is a monorepo.
