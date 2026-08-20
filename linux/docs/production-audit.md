# Production readiness audit

**Updated**: 2026-08-21

**State**: useful for development and personal database work, not yet approved for trusted production writes or unattended agents

This audit describes the Linux Rust and GTK repository. [ROADMAP.md](../ROADMAP.md) tracks broader product work. This document focuses on behavior that must be proven before a public release.

## Verified foundations

### Application

- Native GTK4 and libadwaita UI built with Relm4
- Saved connections in XDG JSON files and secrets in Secret Service
- Browse, SQL editor, and structure tabs
- Inline inserts, updates, and deletes with save and discard paths
- Workspace, preferences, window state, filters, and column widths persisted as JSON
- Query history stored in SQLite with FTS5
- SSH tunnels and nested jump chains
- Authenticated loopback HTTP from the interactive application, plus the
  on-demand stdio-only `tablepro-agentd` CLI

### Policy and audit

- SQL classification through `sqlparser`
- Environment and read-only policy checks before execution
- Token scopes and connection allowlists for MCP callers
- Principal-aware approval routing in the GTK application
- Result masking for governed agent access
- Hash-chained JSONL audit records
- Durable mutation intents and terminal outcomes
- Fail-closed behavior when required audit storage is unavailable
- Recovery detection for unresolved write outcomes across restarts
- Cross-process journal locking and restrictive file permissions

### Driver verification

The Docker integration suites cover PostgreSQL, MySQL, SQL Server, and ClickHouse against real servers. Unit tests cover shared core, policy, storage, MCP, SSH, driver, and application logic.

The PostgreSQL release fixture adds deterministic transport and safety checks: certificate hostname and authority verification, a verifying session through an SSH tunnel using the original database hostname, read-only denial of data-changing CTEs and administrative functions, batch and interactive rollback, activity and blocking-lock reporting, and reconnect after the database path or the bastion path is cut. MySQL, SQL Server, and ClickHouse have no equivalent fixture, so their TLS and reconnect behavior is implemented but not release-verified.

PostgreSQL server-side cancellation is implemented and real-driver verified. Controlled query and execute paths use a separate PostgreSQL control pool to request cancellation. Integration tests start `pg_sleep`, confirm the tagged query is active in `pg_stat_activity`, cancel or time it out, confirm it leaves `pg_stat_activity`, and run another query through the pool. Transaction cancellation is also verified with a later rollback.

Cancellation is no longer a release blocker by itself. Release testing must still prove the installed GTK cancel flow and audit outcome behavior in the PostgreSQL release fixture.

## Closed release blockers

### PostgreSQL TLS through SSH and reconnect fixture

Closed on 2026-08-18. `tests/fixtures/postgres-release/` runs PostgreSQL 16 with TLS, an OpenSSH bastion, and Toxiproxy, and the database publishes no host port. The suite proves that a verifying connection succeeds against a hostname the certificate names, fails on a wrong hostname or an unrelated authority, reaches the database through the bastion, and recovers from a cut database path and a cut bastion path.

A tunnelled verifying connection dials a private Unix socket bound by the tunnel's last hop, so certificate checks use the saved database hostname while the transport stays inside the tunnel. A TCP-forwarded tunnel still refuses to verify the local dial address rather than lowering verification.

Release testing must still prove the installed GTK cancel flow and audit outcome behavior against this fixture, and the hosted `postgres-release` job is external confirmation.

## Remaining release blockers

### Targeted GTK safety tests

The installed GTK suite runs the real application under an isolated D-Bus session and Xvfb display with PyAT-SPI and disposable XDG directories. Local release verification proves that dismissing a production approval leaves SQLite unchanged, approve-once does not authorize the next mutation, and unavailable audit storage cannot be approved around.

The PR smoke is now a required independent job. A daily workflow runs five retry-free attempts and preserves per-attempt output plus failure accessibility/screenshots. Promotion still waits for 30 consecutive attempts across at least six runs, and adding a scenario restarts that ledger. The suite covers twelve scenarios, including a connection switch that gates pending row edits, a browse tab that reads the connection it was reopened against, and two windows holding two databases without crossing writes. PostgreSQL-specific installed cancel, timeout, and terminal audit behavior remains part of the Phase 3 release fixture rather than this deterministic SQLite suite.

A local soak on 2026-08-18 hit one failure in 14 runs, caused by a synthetic Return reaching an approval dialog. The harness no longer has a Return-key activation fallback: named AT-SPI actions drive controls, while synthetic keys are limited to shortcuts being tested. It also verifies successful and failed saved-connection switching against separate SQLite files.

### Release packaging verification

An internal Arch RC is the first release target. Its recipe requires an immutable commit archive and real checksum and ships GUI plus on-demand agentd without a systemd unit. Installation, desktop launch, keyring access, SSH, Kerberos, policy file handling, upgrades, rollback, and uninstall behavior have not yet been release-verified as one installed system. No AUR publication is authorized.

Flatpak files exist, but Flatpak publication is later and is not the first release gate.

## Known limits

- GUI CSV and JSON export only the loaded page. Snapshot-consistent full-table streaming and Parquet are deferred.
- Arbitrary query results are materialized up to configured limits rather than streamed end to end.
- TLS fingerprint fields are not a finished storage, UI, and driver workflow.
- Only PostgreSQL verifies TLS against the original hostname through an SSH tunnel. MySQL, SQL Server, and ClickHouse forward TCP for every mode, so a verifying tunnelled connection to those engines is not available.
- SSH jump chains can be loaded from saved connection JSON but are not fully editable in the connection form.
- gettext infrastructure exists, but translation coverage is incomplete.
- Accessibility work needs full keyboard and screen-reader validation.
- Stable driver labels cover common connection, browse, query, and write paths. They do not mean every TLS, reconnect, transaction, large-result, and packaging case has passed a release fixture.

## Current decision

Reasonable uses today:

- Development against disposable databases
- Personal testing against non-critical databases
- SQLite-based UI work
- Read-only exploration with independently restricted database credentials
- Driver integration work in containers

Not approved yet:

- Trusted production mutations
- Unattended MCP or agent writes
- Public stable package distribution

Re-run this audit after the exact candidate's hosted jobs, GTK soak ledger, and clean Arch install/upgrade/rollback checks pass.
