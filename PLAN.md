# TablePro Linux development plan

Last audited: 2026-08-17

This plan is the source of truth for the Linux application. It separates:

1. Implemented code from release-verified behavior
2. Safety and reliability from feature breadth
3. Features useful to a Platform Engineer, DBA, or Data Engineer from deferred product work
4. Repository maintenance from product development

The application is a Linux-only native Rust and GTK product. Database drivers are static workspace crates. Every shipped feature remains available without an account, license key, subscription, paid tier, or remote entitlement check.

## Current baseline

Audited branch state:

- Safety baseline HEAD before repository extraction: `17a108b1`
- Tracking branch: `fork/linux`
- Rust workspace: 16 crates, 134 Rust source files, and 42,539 lines of Rust
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

Complete locally on 2026-08-13. Rust 1.93 CI is green, current stable Clippy passes locally and has a scheduled job, upstream Linux changes are logged, and the shared security fixes through `origin/main` at `c849d75f` are reconciled. The first hosted current-stable execution remains external confirmation after push.

## Work

- [x] Preserve the fork's policy, MCP, audit, transaction, TLS, and SSH jump-chain architecture while reconciling upstream SQL Server Kerberos support.
- [x] Separate SQL Server's physical dial endpoint from its TLS/Kerberos service identity.
- [x] Keep legacy saved connections compatible.
- [x] Pass Clippy on Rust 1.93 and current stable Rust.
- [x] Document rustup versus distro-toolchain behavior on Arch/Omarchy.
- [x] Add a current-stable scheduled CI check without changing the Rust 1.93 MSRV.
- [x] Record upstream reconciliations from 2026-08-10 onward in a short sync log.
- [x] Reconcile browser-origin, SSH host-key algorithm, and GitHub Action pinning fixes from upstream security work.

## Known transport gap

PostgreSQL through SSH cannot currently use a distinct TCP dial address and TLS server name through sqlx 0.8.6. Do not downgrade `VerifyFull` silently. Resolve this in Phase 3 through a supported sqlx API, a small reviewed sqlx change, or a PostgreSQL connector that accepts a supplied stream.

## Acceptance criteria

- [x] Rust 1.93 remains the declared MSRV in `rust-toolchain.toml`, `Cargo.toml`, and `clippy.toml`.
- [x] Preflight passes with Rust 1.93 on the supported Arch/Omarchy development setup.
- [x] Full-workspace Clippy passes with Arch stable 1.97.1, and scheduled CI uses an explicit `+stable` selector.
- [x] Direct endpoint, tunneled service identity, and legacy serialization are unit-tested; SQL Server password behavior is real-driver tested.
- [ ] Verified SQL Server TLS and real Kerberos/SPN negotiation need a deterministic release fixture in Phase 3. Phase 0 verifies configuration and endpoint construction, not external KDC behavior.

---

# Phase 1: Close authorization and approval bypasses

## Status

Implemented on 2026-08-11 and security-reviewed on 2026-08-13. The review closed mixed-script risk aggregation, PostgreSQL side-effecting function classification, agent token allowlist issuance, and activity API validation. Dismissed-dialog and real GTK flow verification remain in Phase 4 before this phase is release-complete.

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

`StatementClass::Administrative` covers MySQL `KILL` and PostgreSQL administrative or side-effecting functions, including calls nested outside the SELECT projection. Calls such as `pg_terminate_backend`, `pg_promote`, `nextval`, `setval`, advisory locks, large-object mutation, and server configuration changes are not treated as harmless reads.

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
- [x] Mixed transaction batches preserve DDL and unscoped DML restrictions in either statement order.
- [x] Agent token issuance requires at least one existing saved connection.
- [x] One batch requests at most one approval.
- [ ] Real GTK tests prove dismissal denies and approval applies to exactly one operation (Phase 4).

---

# Phase 2: Make audit enforcement fail closed

## Status

Implemented and locally verified on 2026-08-13. Release verification remains tied to the PostgreSQL cancellation fixture in Phase 3 and the GTK safety flows in Phase 4.

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

- [x] Audit initialization failure cannot silently become an available `NullAuditSink`; governed writes are disabled and in-app MCP does not start.
- [x] Production mutations are denied before driver execution when intent cannot be persisted.
- [x] Agentd does not serve MCP without its required journal or with unresolved write outcomes.
- [x] One thousand concurrent appends produce one valid chain.
- [x] Two processes cannot fork the sequence.
- [x] A failure after database execution returns “operation may have succeeded” and disables further governed writes.
- [x] Timed-out or dropped mutation futures leave durable intent and block later governed writes.
- [x] Unresolved writes remain blocked across process restarts and concurrent GUI/daemon processes.
- [x] Verified Phase 1 journals rotate intact before the new event schema starts.
- [x] Phase 3 proves server cancellation and terminal cancellation outcomes against PostgreSQL.

---

# Phase 3: Verify PostgreSQL safety and reliability

## Status

In progress. The cancellable driver contract, PostgreSQL server cancellation, terminal audit outcomes, parameterized operations, interactive rollback, and post-cancel pool reuse are implemented and verified against PostgreSQL 16. The deterministic TLS, SSH, lock, and reconnect fixture remains a release blocker.

## Driver contract

The driver boundary now accepts cancellation tokens and deadlines. PostgreSQL holds the exact physical session for each controlled operation, requests server cancellation through a dedicated pool, accepts only SQLSTATE `57014` as confirmation, and hard-closes sessions whose outcomes cannot be confirmed. GTK and MCP wait for terminal driver results instead of dropping operation futures.

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
- [x] Server-confirmed timeout and cancellation
- [x] Interactive rollback after cancellation
- [ ] Batch rollback in the release fixture
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

# Phase 5: Restore documentation and capability tracking

## Status

In progress.

## Documentation cleanup

- [x] Rewrite the repository `README.md`, `CONTRIBUTING.md`, and `CLAUDE.md` for the Linux Rust and GTK application.
- [x] Rewrite `linux/docs/production-audit.md` from current code and test evidence.
- [x] Make `linux/ROADMAP.md` a concise status view of this plan rather than a competing historical roadmap.
- [x] Correct workspace size, CI jobs, drivers, XDG locations, TLS limitations, audit behavior, cancellation behavior, and packaging maturity.
- [x] Remove retired product documentation and automation that described source no longer present in this repository.
- [x] Retain `external-audit.md` as advisory planning research while keeping this plan and current test evidence authoritative.
- [ ] Keep user-facing changes in `linux/CHANGELOG.md` under `[Unreleased]`.

## Linux capability backlog

Track capabilities as planned, implemented, integrated, release-verified, deferred, or excluded. The backlog is based on Linux user needs and test evidence, not another product's source tree.

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

### Product exclusions

- Runtime-loaded database driver plugins
- Remote account, licensing, subscription, or entitlement services
- Cross-platform UI abstractions and embedded browser interfaces
- Organization account administration
- Automatic background activation of the agent daemon

---

# Phase 6: Deliver persona-priority features

## Status

Not started. Begin only after Phases 1–4 are release-safe.

Deliver in small vertical slices. The first post-safety slice stays editor productivity because it has the highest daily value:

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

Measure release startup time, resident memory, binary size, and query-grid behavior before changing Cargo release profiles. Review shutdown and audit durability before selecting `panic = "abort"`. Keep only settings with measured benefit.

---

# Phase 9: Complete the Linux-only repository extraction

## Status

Complete on 2026-08-17.

The Cargo workspace remains under `linux/` to avoid unrelated build and packaging path churn. The repository contains only the Linux application source, Linux automation, Linux documentation, and shared legal or community files.

Removed material included retired application source, tests, project files, runtime driver bundles, platform release tooling, obsolete documentation, stale marketing assets, and workflows that could no longer succeed.

## Cleanup acceptance criteria

- [x] The repository scope and branch strategy are documented.
- [x] Root contributor and agent guidance describes Rust, GTK, Relm4, static drivers, policy, audit, and Linux packaging.
- [x] Linux CI and packaging do not depend on removed source trees.
- [x] Issue templates request Linux environment and database details.
- [x] The root license and Linux changelog remain authoritative.
- [x] External planning research is labeled advisory and cannot override current code or test evidence.
- [x] Optional upstream research is behavior review and manual implementation, never source-tree merging.
- [x] Cargo metadata resolves the complete workspace after extraction.

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
