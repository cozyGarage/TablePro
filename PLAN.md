# TablePro Linux development plan

Last audited: 2026-08-21

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
- Rust workspace: 19 members and approximately 49,000 lines of Rust
- Stable drivers: PostgreSQL, MySQL, SQLite, SQL Server, ClickHouse
- Experimental drivers: Redis, MongoDB, DuckDB; Oracle is excluded because its optional build is broken and unverified
- No Linux account, subscription, receipt, license-key, or entitlement checks

Verified locally on 2026-08-21 against Arch stable Rust 1.97.1:

- File-size guard passes
- `cargo fmt --all -- --check` passes
- Full-workspace strict Clippy passes with `-D warnings`
- The unit tier passes: 575 tests, one ignored Secret Service test
- The sandbox tier passes: 431 tests, two ignored
- The installed GTK tier passes: 12 of 12 scenarios, retry-free
- `cargo deny check` reports advisories, bans, licenses, and sources ok
- All 45 Docker driver integration tests passed on 2026-08-22: PostgreSQL 9, MySQL 7, SQL Server 12, ClickHouse 12, and SQLite 5 without Docker
- The PostgreSQL release fixture passes 44 tests

Verified on hosted CI on 2026-08-21 at commit `c8f91f06`, the first fully green
run: preflight and sandbox, fast checks, driver integration, the driver TLS
fixture, the PostgreSQL release fixture, and the installed GTK safety smoke all
pass. Before this the GTK gate had never been green, because the Secret Service
step called `secret-tool` and the job installed `libsecret-1-dev` without
`libsecret-tools`. The Phase 4 soak ledger starts from this run.

The ledger then recorded one failure, at `74d037c3a`, in
`current_page_csv_export_is_pk_ordered`: the export read back only 24 of 100
rows. The cause was in the product, not the test. A current-page export wrote
row by row straight to the destination file, so any reader that opened it after
the header had landed saw a truncated export; the local machine simply won the
race. Exports now write to a sibling temporary file and rename over the
destination, and `tablepro_core::export` carries a test asserting the
destination cannot be opened while the export is still being written. Two runs
on 2026-08-22, at `712efeb02` and `9ecd184c8`, are fully green across all six
jobs.

Note for the ledger: `712efeb02` still carried the non-atomic export and passed
anyway, which is what makes this class of defect worth a deterministic unit test
rather than soak attempts.

**The ledger is reset to 0 of 30, and none of the runs above count toward it.**
The gate asks for consecutive retry-free attempts at one commit, but the daily
soak checked out `linux` by name, and every `build-linux.yml` job fell back to
the same branch name on a schedule. A scheduled attempt therefore measured
whatever the branch tip happened to be, and the three green runs recorded above
are three different trees rather than three attempts at one candidate. Counting
them together was wrong.

Both workflows now take an explicit `ref` input, every job prints the commit it
resolved, and the soak writes the requested ref and resolved commit into its run
summary. A ledger entry is valid only for a resolved commit, so accumulating the
30 attempts requires a frozen candidate on a `release/*` branch rather than the
moving branch tip.

Run `cargo deny check` from `linux/`. It does not accept `--manifest-path`.

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

## Transport gap closed

PostgreSQL through SSH could not use a distinct TCP dial address and TLS server name, because sqlx derives both from one host field. Resolved on 2026-08-18 without patching sqlx: the last SSH hop binds a Unix socket in a private directory, and the driver passes that directory as the socket transport while the host field carries the service identity used for certificate checks. A TCP-forwarded connection still refuses to verify the local dial address.

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

Release-verified locally on 2026-08-18. The cancellable driver contract, PostgreSQL server cancellation, terminal audit outcomes, parameterized operations, interactive rollback, and post-cancel pool reuse are verified against PostgreSQL 16. The deterministic TLS, SSH, lock, and reconnect fixture passes, including `VerifyFull` through SSH using the original database hostname. The hosted `postgres-release` job is the remaining external confirmation.

## Driver contract

The driver boundary now accepts cancellation tokens and deadlines. PostgreSQL holds the exact physical session for each controlled operation, requests server cancellation through a dedicated pool, accepts only SQLSTATE `57014` as confirmation, and hard-closes sessions whose outcomes cannot be confirmed. GTK and MCP wait for terminal driver results instead of dropping operation futures.

## Deterministic fixture

The Docker Compose fixture lives in `linux/tests/fixtures/postgres-release/` and contains:

- [x] PostgreSQL 16 with TLS and a hostname-specific certificate
- [x] SSH bastion with deterministic keys
- [x] A database with no published port, reachable through the bastion or the proxied path
- [x] Toxiproxy for reconnect tests
- [x] Seed data and lock-test helpers

`linux/scripts/test-postgres-release.sh` generates materials, starts the fixture, and runs `tablepro-release-tests`. The Linux workflow runs it in the `postgres-release` job.

Required scenarios:

- [x] Direct `VerifyFull`, wrong hostname, and unknown CA
- [x] `VerifyFull` through SSH using the original database hostname
- [x] A TCP-forwarded `VerifyFull` fails rather than verifying the local dial address
- [x] Read-only data-changing CTE denial
- [x] Server-confirmed timeout and cancellation
- [x] Interactive rollback after cancellation
- [x] Batch rollback in the release fixture
- [x] Activity and blocking-lock queries
- [x] Direct and SSH reconnect

## Acceptance criteria

- [x] No TLS hostname downgrade occurs through SSH. Certificate hostname and authority failures now surface as TLS errors instead of internal driver errors.
- [x] Cancelled/timed-out queries leave the server and receive terminal audit outcomes.
- [x] Reconnect replaces the connection and tunnel and later queries succeed.
- [x] The fixture runs as a release-candidate gate in CI.
- [x] A tunnelled `VerifyFull` session connects using the original database hostname. The tunnel's last hop binds a private Unix socket, and the PostgreSQL driver dials that socket while verifying the certificate against the saved hostname.

## Follow-up transport work

MySQL, SQL Server, and ClickHouse still forward TCP for every TLS mode. MySQL has the same sqlx limitation PostgreSQL had, so a verifying MySQL connection through SSH cannot yet check the original hostname. Closing that needs the socket transport in the MySQL driver and a MySQL release fixture with TLS and a bastion. Do not enable it without real-driver verification.

---

# Phase 4: Add targeted GTK safety tests

## Status

Implemented and locally release-verified. The installed GTK smoke is a required independent PR job; a separate daily workflow runs five retry-free attempts toward the RC soak ledger.

A local soak on 2026-08-18 found one failure in 14 runs where a keyboard fallback could send a stray Return into the approval dialog. That fallback has been removed: named AT-SPI actions drive controls, and synthetic keys are limited to shortcuts under test. The suite now also proves successful and failed saved-connection switching against separate database files.

Foundation:

- [x] The dialog close response and every unexpected response map to denial through the same constants used by the production dialog.
- [x] `AllowOnce` authorizes one policy operation and is not cached for the next operation.
- [x] Audit initialization failure creates disabled governed-write state, and an approving sink cannot bypass that state.

Use SQLite, temporary XDG directories, `dbus-run-session`, Xvfb, and AT-SPI automation for the installed flows:

1. [x] Dismissed production approval leaves the database unchanged.
2. [x] `Approve once` performs exactly one mutation and asks again next time.
3. [x] An unavailable audit journal cannot be bypassed through approval.
4. [x] A connection switch with pending row edits offers Stay, Discard, and Save, and neither Stay nor Discard writes to the old database.
5. [x] A browse tab reopened after a switch reads the new connection, and the previous page indicator does not survive the switch.
6. [x] Two windows hold two connections at once, and each window's write reaches only its own database.

Unix-socket endpoint behavior is form logic, so it is covered in the unit tier
rather than the GTK tier: the endpoint choice appears only for a driver that
supports a socket, selecting it hides every network row, a relative socket
directory is invalid, and the resolved socket follows the directory and port.

Keep the PR smoke required. Promote an RC only after 30 consecutive retry-free attempts across at least six distinct daily/manual soak runs. Adding a scenario restarts that ledger.

---

# Phase 5: Restore documentation and capability tracking

## Status

In progress. The documentation audit is current as of 2026-08-18. The capability backlog below is the remaining tracking work.

## Documentation cleanup

- [x] Rewrite the repository `README.md`, `CONTRIBUTING.md`, and `CLAUDE.md` for the Linux Rust and GTK application.
- [x] Rewrite `linux/docs/production-audit.md` from current code and test evidence.
- [x] Make `linux/ROADMAP.md` a concise status view of this plan rather than a competing historical roadmap.
- [x] Correct workspace size, CI jobs, drivers, XDG locations, TLS limitations, audit behavior, cancellation behavior, and packaging maturity.
- [x] Remove retired product documentation and automation that described source no longer present in this repository.
- [x] Retain `external-audit.md` as advisory planning research while keeping this plan and current test evidence authoritative.
- [x] Distinguish implementation from release verification in every product claim across the root README, `linux/README.md`, `ROADMAP.md`, `ARCHITECTURE.md`, and `docs/`.
- [ ] Keep user-facing changes in `linux/CHANGELOG.md` under `[Unreleased]`. This is a standing rule for every change, not a task that closes.

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
- SSH, TLS, and reconnect, release-verified on PostgreSQL only
- Kerberos configuration and service identity, with no test of any kind and no KDC fixture
- MCP and headless agent foundations

### Priority A: daily DBA and data-engineering workflows

- Schema-aware SQL autocomplete (implemented)
- Named query parameters (implemented and release-verified)
- Saved SQL favorites and quick switcher (implemented and release-verified)
- SQL file open/save and external-change detection
- Views, materialized views, triggers, routines, sequences, and extensions
- PostgreSQL/MySQL users, roles, and privileges
- PostgreSQL backup/restore
- CSV, JSON, and SQL import
- SQL dump export and true large-result streaming
- Connection URL import, groups, tags, and favorites
- Reusable SSH profiles and custom CA/client certificates
- Connection-layer risks tracked in `linux/docs/connections.md`: SQL Server verification semantics, real Kerberos negotiation, client certificates, IPv6 URL construction, multi-hop/password SSH fixtures, secondary-driver cancellation/reconnect, and the broken optional Oracle build

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

In progress. Phases 1 through 4 are release-verified locally, so vertical slices have started.

Slice 1 shipped on 2026-08-18: named parameters, schema-aware completion, favorites, and Open Quickly.

Named query parameters: `:name` placeholders are rewritten to the driver's positional placeholders and bound as values. The scanner leaves literals, quoted identifiers, comments, dollar-quoted bodies, PostgreSQL casts, and existing placeholders alone. Evidence: core scanner unit tests, app binding tests, three PostgreSQL fixture tests including a SQL payload that stays data, and an installed GTK scenario that writes the bound value.

Completion: the editor recomputes candidates on every cursor move and edit. After FROM or JOIN it offers tables, a `alias.` or `table.` prefix narrows to that table's columns, and otherwise it offers the columns of tables named in the statement. Columns for a referenced table are fetched once through the policy-gated connection. Evidence: scanner and scope unit tests covering aliases, schema qualifiers, literals, and statement boundaries.

Favorites and Open Quickly: favorites live in `favorites.json` with name-based replacement, a 500-entry cap, and recency ranking. Open Quickly ranks exact, prefix, substring, statement, and initials matches over favorites, open tabs, and saved connections. Evidence: storage and ranking unit tests plus an installed GTK scenario that saves a favorite with Ctrl+D, finds it with Ctrl+P, opens it, and checks the recorded use.

Deliver in small vertical slices. The first post-safety slice stays editor productivity because it has the highest daily value:

1. SQL autocomplete, parameters, favorites, and quick switcher (complete)
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

An internal Arch RC is the first distribution target; public AUR publication is explicitly deferred. The package resolves the signed-off RC tag to an immutable commit archive and real checksum, installs GUI/agentd plus desktop/AppStream/icon/translation/license/policy files, and must pass `makepkg --cleanbuild`, `namcap`, `desktop-file-validate`, `appstreamcli validate`, and a fresh D-Bus launch.

Do not install or enable an agent daemon service for this RC. Package agentd as an on-demand stdio CLI requiring explicit client configuration, a valid policy, and a working audit journal.

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

# Phase 10: DBA operations at scale

## Status

In progress since 2026-08-21. Slice 10.1 is implemented and locally
release-verified: activation is additive, each window owns and releases its own
connection, and the single-connection limit is gone. Slices 10.2 through 10.7
are not started.

The product is strong on safety and thin on operations. A DBA who manages many
servers gets one active connection per process, an activity dialog that renders
tab-separated text, hardcoded driver pool sizes, no server health view, and no
read-only view of views, routines, triggers, sequences, roles, or grants. This
phase closes that gap without weakening any safety invariant.

Every connection this phase exposes is still a `PolicyGuard`. Health polling,
activity queries, session termination, and schema introspection are not
exceptions. Polling is bounded, cancellable, and never runs on the GTK thread.

Deliver in ordered slices. Slices 1 through 3 must land. If the phase runs
long, cut slice 6 first and slice 5 second. Do not cut tests.

## 10.1 Concurrent connections

The prerequisite for the rest of the phase. `DatabaseService` already keys
entries by UUID; exclusivity is one drain call in `activate_exclusive`.

Replace `activate_exclusive` with an additive `activate` plus an explicit
`close`, keep the active id as the focused connection for menu actions, and
extend the fail-closed tab-ownership invariant from one owner to many. A tab
must never run against a connection it does not own. Workspace restoration
reconnects each referenced connection, and a failed reconnect leaves that tab
inert rather than rebinding it. Per-tab chrome states the connection and its
environment so the target server is identifiable without clicking.

Primary files:

- linux/crates/app/src/services/database_service.rs
- linux/crates/app/src/services/connection_monitor.rs
- linux/crates/app/src/ui/app/connection.rs
- linux/crates/app/src/ui/app/workspace_tabs.rs
- linux/crates/app/src/ui/app/workspace_persist.rs
- linux/crates/app/src/ui/app/workspace_chrome.rs

This slice retires the single-connection limit recorded in
`linux/docs/production-audit.md`.

## 10.2 Connection organization

Saved connections gain a group, tags, and a favorite flag. The connection list
groups, filters, and searches over them, and every row carries its environment
colour. Connection URL import lands here. Reuse the Open Quickly ranking rather
than writing a second ranker.

Primary files:

- linux/crates/storage/src
- linux/crates/app/src/ui/welcome_view.rs
- linux/crates/app/src/ui/connection_row.rs
- linux/crates/app/src/ui/quick_switcher_dialog.rs

Legacy connection files without the new fields must still load.

## 10.3 Sessions and activity console

`ActivityQuery` becomes a capability-declared set so the UI hides what a driver
cannot answer instead of showing an empty result. Add PostgreSQL blocking trees
through `pg_blocking_pids`, MySQL lock waits joined to running transactions, and
SQL Server transaction locks joined to executing requests.

The activity dialog is replaced by a view that renders through the existing
result grid, so columns are typed and sortable. Refresh is bounded, has an
explicit interval, and is cancelled when the view closes.

Session termination stays governed. PostgreSQL backend control and MySQL KILL
already classify as administrative, so production termination requires approval
and must record a terminal audit state.

Primary files:

- linux/crates/core/src/activity.rs
- New: linux/crates/app/src/ui/server_ops/
- linux/crates/release-tests/tests/activity_locks.rs

## 10.4 Server health and performance

A per-driver metric set with an availability probe. PostgreSQL first:
connection saturation, cache hit ratio, longest running transaction,
idle-in-transaction count, replication lag and slot state, transaction id
wraparound headroom, deadlocks, bloat, autovacuum age, and top statements.

An absent statistics extension degrades one panel. It does not fail the view.
Every metric query is a read-only statement through the guard. No charting
library and no time series in this slice.

Primary files:

- New: linux/crates/core/src/server_health.rs
- New: linux/crates/app/src/ui/server_health/

## 10.5 Connection and resource control

Driver pool sizes are hardcoded and neither configurable nor introspectable.
Connect options gain pool size, acquire timeout, idle timeout, connect timeout,
and statement timeout. Defaults must reproduce current behavior exactly. Saved
connections carry the same fields, transport maps them, and each driver honours
them. Pool telemetry surfaces in the health view.

Server configuration is read-only in this slice. Writing server settings from
the client is out of scope.

Primary files:

- linux/crates/core/src/connection.rs
- linux/crates/transport/src/lib.rs
- linux/crates/drivers/postgres/src/lib.rs
- linux/crates/drivers/mysql/src/lib.rs
- linux/crates/drivers/sqlite/src/lib.rs
- New: a connect-dialog advanced section module

## 10.6 Safe schema review

Read-only introspection for views, materialized views, routines, triggers,
sequences, extensions, roles, and grants. The connection trait gains methods
whose defaults return nothing, matching the existing index and foreign-key
pattern, so drivers opt in without breaking. PostgreSQL implements all of them.

Database metadata is untrusted input. Identifiers are dialect-quoted and never
joined into SQL as text.

This slice is the read-only foundation that Phase 6 object administration
builds DDL on. It ships no DDL itself.

Primary files:

- linux/crates/core/src/query.rs
- linux/crates/core/src/connection.rs
- linux/crates/drivers/postgres/src/lib.rs
- linux/crates/app/src/ui/sidebar_row.rs

## 10.7 Python runner design spike

No shipped feature. The repository forbids an in-process scripting runtime and
does not need one. `agentd` already serves a policy-guarded, audited,
rate-limited MCP endpoint over stdio, so an out-of-process interpreter over
that transport inherits policy, audit, allowlists, and cancellation.

The spike produces an architecture decision record, a design document, and a
throwaway prototype that is not a workspace member and not in CI. The design
must resolve the principal question, because a script runs under a human
session but unattended and no one answers an approval prompt. It must also
resolve audit provenance against the hash-chained journal format, the sandbox
model, interpreter provenance, and the data handoff. Python must never become
a build or runtime requirement of the Arch package.

Primary files:

- New: linux/docs/decisions/0002-out-of-process-python-runner.md
- New: linux/docs/python-scripting.md
- New: linux/scripts/spike/

## Acceptance criteria

- [x] Several connections are open at once, and a query result reaches only the tab that owns its connection.
- [x] Closing one connection leaves every other connection usable and ends only its own monitor task.
- [ ] Workspace restoration reopens every referenced connection, and a failed reconnect leaves that tab inert.
- [ ] Saved connections group, tag, filter, and search, and legacy connection files without the new fields still load.
- [ ] Each driver declares which activity queries it supports, and the UI offers only those.
- [ ] A real blocking pair is reported as a blocking tree on PostgreSQL, MySQL, and SQL Server.
- [ ] Terminating a session in production requires approval and records a terminal audit state, and dismissal leaves the session alive.
- [ ] Activity and health refresh are bounded, cancelled on close, and never block the GTK thread.
- [ ] Health metrics return against the PostgreSQL fixture, and a missing statistics extension degrades one panel while the rest render.
- [ ] Pool size and statement timeout are configurable per saved connection, honoured by the driver, and proven server-side.
- [ ] Default pool and timeout values reproduce pre-slice behavior, and legacy saved connections deserialize.
- [ ] Views, materialized views, routines, triggers, sequences, extensions, roles, and grants are listed read-only against a real PostgreSQL schema.
- [ ] No introspection or metric path builds SQL by joining database metadata as text.
- [ ] Every connection handed to any consumer in this phase is a `PolicyGuard`.
- [ ] The Python spike lands an accepted decision record, a design document, and a measured prototype result, and ships no runner.

---

# Agent surface

Agents are a supported surface, not a bolt-on. Every agent-facing capability
composes the same layers in the same order:

```
saved connection ─▶ tablepro-transport ─▶ raw driver connection
                                              │
                          PolicyGuard ◀───────┘   (always, no exceptions)
                                              │
       ┌──────────────────────────────────────┼──────────────────┐
    GTK app                            tablepro-mcp          agentd
                                    (scopes + allowlist)   (composition)
```

Rules that follow from this shape:

- `tablepro-transport` is the only place that turns a saved connection into a
  live driver connection. The GUI and `agentd` connect identically, so a
  connection that requires a bastion requires it headlessly too, and TLS is
  always verified against the real database hostname.
- No surface hands out a raw driver connection. `PolicyGuard` wraps every
  handle before it leaves the provider.
- Token scope, connection allowlist, and `PolicyGuard` remain three
  independent checks. None substitutes for another.
- A new agent capability is a tool in `tablepro-mcp` over this transport, not
  a new connection path.
- Built-in AI chat stays a deferred product feature. This is the integration
  surface, not a chat client.

Known seam: saved connections can name a certificate authority, so a privately
issued server certificate now verifies on every surface. Client certificates
and pinned fingerprints remain unfinished storage, UI, and driver fields. That
is tracked as a Priority A capability, not a defect in this shape.

# First trusted Linux release gate

Run from `linux/`:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings
cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins
./scripts/test-sandbox.sh
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
