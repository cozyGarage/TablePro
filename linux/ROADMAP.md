# TablePro Linux roadmap

Last audited: 2026-09-04

The repository-level [`PLAN.md`](../PLAN.md) is the source of truth for sequencing, detailed acceptance criteria, and the Linux capability backlog. This file is the concise status view.

## Current state

TablePro Linux is a substantial GTK4/libadwaita database client, not a prototype. Its core daily-driver workflows are implemented. Production approval, fail-closed audit, PostgreSQL cancellation, and PostgreSQL TLS, SSH, lock, and reconnect behavior are release-verified locally. Exclusive connection switching, deterministic PostgreSQL ordering, direct local sockets, the internal Arch recipe, and a required GTK job are implemented in the current candidate. What still blocks an RC is exact-commit CI, 30/30 GTK soak attempts, and installed-package verification.

Every claim below states whether it is implemented, integrated, or release-verified. A feature with unit tests only is never described as verified.

Status terms:

- **Implemented**: code and core unit tests exist.
- **Integrated**: all intended production entry points use it.
- **Release-verified**: deterministic real-driver, UI, or installed-package tests prove it.
- **Complete**: all phase acceptance criteria are release-verified.
- **Partial**: release-verified on some drivers or paths and unproven or absent on others.

## Verified inventory

| Area | Status | Notes |
|---|---|---|
| PostgreSQL, MySQL, SQLite, SQL Server, ClickHouse | Implemented | PostgreSQL is release-verified through the fixture; the other engines have container integration tests only. Server-side cancellation is verified against a real engine on PostgreSQL, MySQL, ClickHouse and SQLite; SQL Server declares it unsupported because tiberius cannot send the TDS attention packet |
| Redis, MongoDB, DuckDB | Implemented | Experimental; DuckDB requires a Cargo feature. Redis and MongoDB TLS is release-verified |
| Oracle | Broken | Does not compile under its `odpi` feature against oracle 0.6.3 |
| Browse/edit/filter/sort/pagination | Implemented | Keyset helper exists; integers wider than 2^53 edit exactly; large-result behavior needs release tests |
| SQL editor and multiple result tabs | Integrated | PostgreSQL timeout and cancel stop the server query and wait for terminal audit state. One dialect-aware lexer sets statement boundaries, so a PostgreSQL function body runs whole |
| Bounded operations | Integrated | Every database call the interface starts carries a deadline, gated by `scripts/check-bounded-operations.sh` |
| Structure editor | Implemented | Tables, columns, indexes, and foreign keys |
| Saved connections and libsecret | Implemented | Keyring failure UX needs hardening |
| SSH and jump chains | Integrated | A verifying PostgreSQL connection forwards through a private Unix socket and is release-verified, headlessly as well as in the GUI; jump chains are JSON-only in the current GTK form |
| TLS modes | Partial | Release-verified on PostgreSQL, including `VerifyFull` through SSH. Release-verified on MySQL, ClickHouse, MongoDB, and Redis through the driver TLS fixture. Mapped but untested on SQL Server, which also cannot name a certificate authority. Saved connections carry a certificate authority. See [docs/connections.md](docs/connections.md) |
| Query history | Implemented | MCP access must be isolated before being re-exposed |
| CSV/JSON export | Implemented | GUI CSV and JSON export the loaded page only; full-table snapshot streaming and Parquet are deferred |
| Activity and EXPLAIN | Implemented | Administrative classification and numeric session-ID validation are covered |
| Policy, MCP, and agentd | Integrated | Approval and audit failures deny governed operations; a policy file that cannot be read keeps the last good policy and leaves MCP off; `list_tables` and `describe_table` use the same timeout and identifier checks as the other metadata tools; read-only denial is release-verified against PostgreSQL; the GUI and agentd share one connection transport, release-verified through the fixture bastion |
| Audit journal | Integrated | Durable intent/outcome records, recovery, private mode, and cross-process locking are locally verified |
| Internal Arch RC | Implemented | Immutable-commit/checksum recipe exists; install, upgrade, rollback, and Wayland verification remain |
| Debian, Flatpak, AUR | Scaffolded | Not release targets and not ready for public publication |
| i18n and accessibility | Infrastructure | English strings/checklist exist; end-user verification is incomplete |

## Active phases

### 0: Development baseline

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

### 1: Authorization and approval

- [x] Replace production automatic approval with principal-aware routing
- [x] Enforce read-only before unparseable-statement fallback
- [x] Classify administrative and PostgreSQL side-effecting functions
- [x] Preserve DDL and unscoped DML restrictions across mixed transaction batches
- [x] Validate numeric session identifiers before building SQL
- [x] Require explicit saved-connection allowlists when agentd issues tokens
- [x] Remove or isolate MCP query-history search
- [x] Merge partial policies onto secure environment defaults

Phase 1 is implemented, security-reviewed, and locally verified. Phase 4 still owns release-level GTK proof for dialog dismissal and approve-once behavior.

### 2: Fail-closed audit

- [x] Disable governed writes and in-app MCP when audit initialization fails
- [x] Record durable intent before mutations and transaction completion
- [x] Record explicit, sanitized outcomes after operations
- [x] Verify journal mode, writability, legacy migration, recovery, and cross-process locking
- [x] Refuse agent service when audit storage is unavailable or unresolved writes exist
- [x] Persist unresolved write state across restarts and concurrent processes

Phase 2 is implemented, security-reviewed, and locally verified. PostgreSQL cancellation and terminal outcomes are now verified in Phase 3.

### 3: PostgreSQL release safety

- [x] Add the deterministic PostgreSQL TLS, SSH, lock, and reconnect fixture
- [x] Verify direct `VerifyFull`, wrong hostname, and unknown certificate authority
- [x] Prove a TCP-forwarded `VerifyFull` never verifies the local dial address
- [x] Implement and verify server-side cancellation, parameterized cancellation, rollback, and pool reuse
- [x] Verify batch and interactive rollback, activity, blocking locks, direct reconnect, and SSH reconnect
- [x] Connect a tunnelled `VerifyFull` session using the original database hostname

The fixture runs from `./scripts/test-postgres-release.sh` and in the `postgres-release` CI job. PostgreSQL tunnels a verifying connection over a private Unix socket, so the TCP dial path and the TLS server name are independent.

### 4: GTK safety tests

- [x] Dialog close and unexpected responses map to denial
- [x] Approve-once is not cached across policy operations
- [x] Disabled audit state takes precedence over an approving sink
- [x] Installed GTK dismissal leaves SQLite unchanged
- [x] Installed GTK approve-once prompts again for the next operation
- [x] Installed GTK audit failure cannot be approved around
- [x] Installed GTK pending row edits gate a connection switch without writing to the old database
- [x] Installed GTK browse tabs read the connection they were reopened against
- [x] Installed GTK connection switch keeps the previous connection's tabs after the persist debounce
- [x] Installed GTK pending edits stay in their own window when another window switches

Phase 4 has a required PR smoke job and a separate daily five-attempt soak. Promotion still requires 30 consecutive retry-free attempts across at least six workflow runs, all at one pinned commit. Both workflows now take an explicit `ref` and record the commit they resolved; before that the scheduled soak followed the branch tip, so attempts could not be attributed to a candidate. Adding a scenario restarts the ledger. The ledger stands at 0 of 30.

### 5: Documentation and capability tracking

- [x] Replace the historical roadmap with evidence-based status
- [x] Rewrite repository guidance for the Linux Rust and GTK application
- [x] Synchronize the production audit with server-side PostgreSQL cancellation evidence
- [x] Replace source-parity tracking with a Linux capability backlog
- [x] Keep external planning research advisory rather than authoritative
- [x] Distinguish implementation from release verification in every product claim

Phase 5 documentation is current as of 2026-08-18. Keeping it current is a standing rule for every change, not a one-time task.

### 6: DBA and data-engineering depth

- [x] Named query parameters bound by the driver, release-verified against PostgreSQL and in the installed GTK suite
- [x] Schema-aware editor completion, saved favorites, and Open Quickly, release-verified in the installed GTK suite
- [ ] SQL file open/save with external-change detection
- [ ] PostgreSQL objects, users/roles, and administration
- [ ] Import/export and backup/restore
- [x] Connection groups, tags, favourites, search, URL import, and environment colour (Phase 10.2)
- [ ] Reusable SSH and transport profiles
- [ ] True result streaming and optional Parquet export

### 7: Driver expansion

Prioritize real workflows: Redshift and CockroachDB profiles, Trino, Snowflake, then BigQuery. Other drivers remain demand-driven.

### 8: Identity and packaging

Promote the internal Arch package only after its installed-package and soak
gates pass. Finalize the product name, repository, and application ID before
any later AUR/Omarchy or Flatpak publication.

### 9: Linux-only repository extraction

- [x] Remove retired non-Linux source, tests, build projects, runtime driver bundles, documentation, and release automation
- [x] Keep the Cargo workspace under `linux/` to preserve stable build and packaging paths
- [x] Rewrite root and Linux documentation around the Rust and GTK architecture
- [x] Retain optional upstream behavior review without source-tree merging
- [x] Preserve the root license, Linux changelog, Linux workflows, and package scaffolding

The repository extraction completed on 2026-08-17. Product planning now follows Linux user needs and release evidence.

### 10: DBA operations at scale

- [x] Several connections open at once, with fail-closed per-tab ownership across all of them
- [x] Connection groups, tags, favorites, search, URL import, and environment colour on each row
- [ ] A typed sessions and locks console with capability-declared driver support and governed session termination
- [ ] A PostgreSQL server health panel that degrades cleanly when a statistics extension is absent
- [ ] Configurable pool size and timeouts per saved connection, honoured by the driver
- [ ] Read-only review of views, routines, triggers, sequences, extensions, roles, and grants (PostgreSQL views are listing)
- [ ] A decision record, design, and measured prototype for an out-of-process Python runner

Phase 10 is in progress. Slice 10.2 added connection organisation: groups, tags, favourites, search across name/group/tag/driver, and URL import whose password reaches the keyring and never the saved file. Its first slice retired the one-active-connection limit: activation is additive and every window owns and releases its own connection, proven by two windows writing to two databases in the installed suite. Every connection it exposes stays policy-guarded, and no slice ships DDL or server configuration writes.

## Next implementation target

The immediate target is the internal Arch RC. The exact-commit hosted jobs first ran fully green on 2026-08-21 at `c8f91f06`, so the Phase 4 soak ledger has started and needs 30 consecutive retry-free attempts across at least six runs. What remains is accumulating that ledger and verifying install/upgrade/rollback on Wayland. Those gates are mostly waiting, so Phase 10 feature work runs in parallel on `linux` while the candidate stays frozen on a release branch. Phase 10 comes before full-table snapshot export and Phase 6 object administration; new drivers come after both.

Which macOS features we take, skip, and in what order is in [docs/upstream-adoption.md](docs/upstream-adoption.md), reviewed through their v0.71.0 release as of 2026-09-04. The next product slice after views is the typed activity console (10.3), then the rest of 10.6. The 2026-09-04 review pass also flagged four upstream bug fixes (MongoDB collection drop, an SSH stale-tunnel-port race, server-owned-column edit refusal, identity-column DEFAULT on insert) to check against our own drivers — see "Since 0.69" in that file.
