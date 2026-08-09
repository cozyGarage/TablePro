I’ll review the current `linux` branch without changing files, compare it with the latest upstream refs, and trace approval, audit, pricing, branding, testing, and packaging paths. I’ll use that evidence to produce a phased plan with exact files, dependencies, and acceptance criteria.

The working tree is clean at `990dd2d1`, and the initial trace confirms the approval and audit risks described in the review. I’ll fetch upstream refs without merging, then split the deeper review into independent safety, release-testing, and branding/pricing investigations.

Upstream is not a simple update: local `linux` is 37 commits ahead and 1 commit behind `origin/linux`; the upstream-only commit adds MSSQL Kerberos support while dropping many fork-only safety features in its tree state. I’ll inspect the precise upstream delta and the current composition roots so the plan can prescribe a safe cherry-pick/reconciliation path rather than a destructive merge.

One upstream change is directly useful beyond MSSQL: it separates the tunnel socket endpoint from the database service identity, which addresses the current TLS `VerifyFull` hostname problem through SSH. I’ll inspect the fork’s richer `ConnectOptions` and driver code to define how to port that concept without regressing policy, TLS modes, or jump chains.

# Bookie development plan

I fetched the latest upstream refs without modifying the working tree:

- Local `linux`: `990dd2d1`
- `origin/linux`: `807d2809`
- `origin/main`: `b2d0d059`
- Local versus `origin/linux`: 37 commits ahead, 1 commit behind
- No new `origin/main` commit touches `linux/`, `build-linux.yml`, or `flatpak-linux.yml`

The working tree remains clean.

The upstream-only Linux commit adds MSSQL Kerberos authentication and introduces an important transport concept: the socket endpoint can differ from the database service identity when using SSH. Do not reset the fork to `origin/linux`. Port the relevant changes on an integration branch because the fork has policy, MCP, audit, jump chains, and packaging work that upstream does not contain.

## Product direction

Bookie should focus on:

1. Safe PostgreSQL operation during incidents
2. Fast native GTK workflows
3. Verified TLS and SSH
4. Dependable cancellation, reconnect, and transaction behavior
5. Clear production approval
6. Durable audit records
7. Local agents through a constrained MCP interface
8. Predictable installation and updates on Omarchy

Defer:

- AI chat
- ER diagrams
- Vim mode
- Broad driver expansion
- Parquet export
- Internal Cargo crate renaming
- Full Flatpak/Flathub work until the application identity is final

## Important pricing finding

There is no paywall, subscription, account requirement, license-key check, or entitlement gate under `linux/`.

The optional `duckdb` and `odpi` Cargo features are build controls, not commercial gates. `PolicyGuard`, MCP scopes, approvals, and connection allowlists are security controls and must not be removed.

The macOS application elsewhere in the repository does contain license activation and commercial features. This plan treats Bookie as the Linux product and does not change the macOS licensing system. Removing pricing from the entire repository would be a separate project with legal and product implications.

For Bookie, adopt this contract:

> Every locally shipped feature is available without an account, license key, receipt, subscription, or entitlement service.

Document that in `linux/README.md` before the first Bookie release.

---

# Phase 0: Reconcile upstream safely

## Status

**Complete on 2026-08-10.** The useful parts of upstream Linux commit `807d2809` were ported without replacing the fork's policy, MCP, audit, TLS, transaction, or SSH jump-chain architecture.

## Objective

Stay current without losing the fork's safety architecture.

## Delivered

The completed port includes:

Prioritize the transport identity model:

- Dial endpoint: local SSH forward, such as `127.0.0.1:49152`
- Service identity: original database host, such as `db.internal.example:5432`
- TLS server name: original database hostname
- Kerberos SPN: original SQL Server hostname

Relevant files:

- `linux/crates/core/src/connection.rs`
- `linux/crates/core/src/driver.rs`
- `linux/crates/core/src/lib.rs`
- `linux/crates/app/src/services/connection_service.rs`
- `linux/crates/app/src/ui/connect_dialog.rs`
- `linux/crates/app/src/ui/connection_row.rs`
- `linux/crates/app/src/ui/error_text.rs`
- `linux/crates/storage/src/connections.rs`
- `linux/crates/drivers/mssql/src/lib.rs`
- `linux/Cargo.toml`
- `linux/Cargo.lock`
- `.github/workflows/build-linux.yml`
- `linux/flatpak/com.tablepro.linux.json`

The fork's `TlsConfig` and five TLS modes remain the source of truth. SQL Server now supports password and Windows Kerberos authentication in the GTK form, saved connections, reconnect path, and agent daemon. Existing saved files default to password authentication. Switching an unambiguous saved endpoint to Kerberos reuses its UUID and removes the old password.

Tiberius uses the fixed upstream GSSAPI revision, and release binaries link the MIT Kerberos libraries. CI, Debian, AUR, Flatpak, documentation, and supply-chain policy include the required dependencies. Translation startup also moved to the corrected `gettext-rs` 0.8 safety contract.

## PostgreSQL constraint

The design spike confirmed that sqlx 0.8.6 uses one host for both TCP connection and `VerifyFull` identity. It has no supported separate dial address or TLS server-name API. Phase 0 introduced the logical service versus physical dial endpoint model, but the PostgreSQL driver cannot consume it correctly until sqlx gains `hostaddr` behavior or the driver uses another connector.

Preferred implementation outcomes remain:

1. Use a supported sqlx API for separate dial and server identity.
2. Contribute or carry a small reviewed sqlx change.
3. Add a PostgreSQL-specific connector layer that can connect a supplied stream while verifying the original hostname.
4. Do not silently downgrade SSH connections from `VerifyFull` to `VerifyCa` or `Require`.

## Acceptance criteria

- [x] Existing saved connection files still deserialize.
- [x] Jump chains still work.
- [x] Policy, MCP, transactions, and audit crates remain present.
- [x] Direct TLS behavior does not change.
- [x] Tests prove that an SSH connection retains the original service hostname.
- [x] Legacy MSSQL saved connections default to password authentication.
- [x] The upstream port does not replace macOS application code.
- [x] Release binaries link GSSAPI and Kerberos.
- [x] Cargo tests, release checks, `cargo-deny`, and `cargo-audit` pass.
- [ ] PostgreSQL `VerifyFull` through SSH validates the original hostname. This remains blocked on separate sqlx dial and service identities.

## Ongoing upstream cadence

Every two weeks and before each release:

```text
git fetch origin main linux
git log linux..origin/linux -- linux .github/workflows/build-linux.yml
git log linux..origin/main -- linux .github/workflows/build-linux.yml
```

Integrate Linux-relevant commits individually. Record deferred upstream work in a small sync checklist rather than letting the branches drift silently.

---

# Phase 1: Close authorization and approval bypasses

This is the first release blocker.

## 1.1 Replace the unsafe approval default

GTK is the right approval mechanism for a human using the desktop application, but it must not depend on MCP startup.

Current problem:

- `DatabaseService` starts with `AutoApproveSink`.
- `GtkApprovalSink` is installed by `mcp_service::start_background()`.
- One mutable global sink serves human and agent principals.

Target design:

- Human GUI operations always route to `GtkApprovalSink`.
- In-app MCP operations may show GTK approval only when an active desktop window exists.
- An in-app agent without an active window is denied.
- `tablepro-agentd` defaults to deny.
- Automatic approval exists only in tests.
- No production code constructs `AutoApproveSink`.

Files:

- `linux/crates/app/src/main.rs`
- `linux/crates/app/src/services/database_service.rs`
- `linux/crates/app/src/services/mcp_service.rs`
- `linux/crates/app/src/services/gtk_approval.rs`
- `linux/crates/app/src/services/mod.rs`
- New: `linux/crates/app/src/services/approval_router.rs`
- `linux/crates/agentd/src/main.rs`
- `linux/crates/policy/src/approval.rs`

## 1.2 Fix read-only precedence

`linux/crates/policy/src/rules.rs` evaluates unparseable SQL before the read-only rule. A human can therefore approve unparseable SQL on a read-only connection.

Change the order so that any potential write on a read-only connection is denied before approval is considered.

## 1.3 Classify administrative operations

`SELECT pg_terminate_backend(pid)` is currently treated as a read. This means a read-only MCP token can terminate a PostgreSQL session.

Add an explicit operation category for administrative side effects rather than relying only on `SELECT` versus DML.

Files:

- `linux/crates/policy/src/classify.rs`
- `linux/crates/policy/src/rules.rs`
- `linux/crates/policy/src/guard.rs`
- New: `linux/crates/policy/src/operation.rs`
- `linux/crates/core/src/activity.rs`
- `linux/crates/app/src/ui/activity_dialog.rs`

Change activity termination APIs to accept a parsed numeric PID, not arbitrary text interpolated into SQL.

## 1.4 Close MCP side paths

`search_query_history` currently bypasses connection allowlists, rate limits, masking, policy, and audit.

Initially, remove it from the MCP tool list. Restore it only after it can:

- Filter to explicitly allowlisted connection IDs
- Redact literals
- Apply rate limits
- Produce an audit event

Files:

- `linux/crates/mcp/src/tools.rs`
- `linux/crates/mcp/src/server.rs`
- `linux/crates/mcp/src/bridge.rs`
- `linux/crates/mcp/tests/enforce_policy.rs`
- New: `linux/crates/mcp/tests/history_isolation.rs`

## 1.5 Fix partial policy inheritance

A partial production policy currently receives generic `EnvPolicy::default()` values. This can change production `agent_writes` from `Deny` to `Approve`.

Make environment configuration override the secure environment defaults field by field.

Files:

- `linux/crates/policy/src/config.rs`
- `linux/packaging/policy.example.toml`
- New: `linux/crates/policy/tests/policy_matrix.rs`

## Acceptance criteria

- GUI startup without MCP still uses GTK approval.
- `AutoApproveSink` is absent from non-test GUI and daemon paths.
- Closing an approval dialog denies the operation.
- No active GTK window means an in-app agent approval is denied.
- Read-only connections deny all potential writes, including unparseable SQL.
- Agents cannot call `pg_terminate_backend`.
- Session IDs must parse as integers.
- An empty MCP allowlist exposes no connection data or history.
- Partial production policy retains `agent_writes = deny`.
- A batch requests at most one human approval per user action.

---

# Phase 2: Make audit enforcement fail closed

## Required behavior matrix

| Caller and operation | Audit unavailable |
|---|---|
| Human Local/Dev read | Allow with persistent warning |
| Human Local/Dev write | Allow only if explicitly accepted as best-effort policy |
| Human Staging/Prod read | Allow with visible warning initially |
| Human Staging/Prod mutation, DDL, or admin operation | Deny |
| In-app MCP operation | Deny |
| `bookie-agentd` operation | Refuse to start or deny |
| Transaction commit | Deny before commit if intent cannot be recorded |

For simpler semantics, you may choose to deny all Staging/Prod operations when audit is unavailable. Do not silently fall back to `NullAuditSink`.

## Audit event model

Use two records for state-changing operations:

1. Durable intent before contacting the database
2. Outcome after completion

The intent should contain:

- Operation ID
- Batch ID and statement index where applicable
- Principal
- Connection ID and environment
- Operation class
- Normalized or redacted SQL
- SQL hash
- Target objects
- Approval outcome
- Preview status

The outcome should contain:

- Operation ID
- Success, denial, cancellation, timeout, or unknown result
- Rows affected
- Duration
- Transaction outcome
- Error category

A missing outcome means the result is unknown. It must not imply that nothing happened.

## Storage changes

Change `AuditSink::record` to return a result.

Make `AuditJournal::open`:

- Create the directory and journal
- Use file mode `0600`
- Verify the existing chain
- Prove that the journal is writable
- Reject a corrupt chain
- Serialize writers in-process
- Lock across GUI and daemon processes
- Append a record and newline as one protected operation
- Call `sync_data` for required intent and commit records

Files:

- `linux/crates/policy/src/guard.rs`
- `linux/crates/policy/src/lib.rs`
- New: `linux/crates/policy/src/audit.rs`
- `linux/crates/storage/src/audit_journal.rs`
- `linux/crates/storage/src/lib.rs`
- `linux/crates/storage/Cargo.toml`
- `linux/Cargo.toml`
- `linux/crates/app/src/services/database_service.rs`
- `linux/crates/app/src/ui/app/mod.rs`
- `linux/crates/app/src/ui/app/status_pages.rs`
- `linux/crates/agentd/src/main.rs`
- `linux/crates/mcp/src/bridge.rs`

Tests:

- New: `linux/crates/storage/tests/audit_journal_concurrency.rs`
- New: `linux/crates/storage/tests/audit_journal_recovery.rs`
- New: `linux/crates/mcp/tests/timeout_audit.rs`

## Cancellation interaction

The current editor races a timeout or cancellation token against a query future. Dropping the future does not prove PostgreSQL stopped execution, and it can prevent the audit outcome from being recorded.

Add a cancellable operation contract to the driver boundary.

Likely files:

- `linux/crates/core/src/connection.rs`
- `linux/crates/core/src/transaction.rs`
- `linux/crates/drivers/postgres/src/lib.rs`
- `linux/crates/app/src/ui/editor/mod.rs`
- `linux/crates/app/src/ui/editor/outcomes.rs`
- `linux/crates/mcp/src/bridge.rs`
- `linux/crates/policy/src/guard.rs`

For PostgreSQL:

- Obtain and retain a server-side cancellation handle.
- Send cancellation when the user cancels or a deadline expires.
- Confirm the operation is no longer present in `pg_stat_activity`.
- Discard the connection if its protocol state cannot be trusted.
- Record a terminal `cancelled`, `timed_out`, or `unknown` audit outcome.

## Acceptance criteria

- Audit initialization failure cannot become `NullAuditSink` in production paths.
- A read-only or unwritable journal prevents production mutations before driver execution.
- Agentd does not serve MCP when its required journal cannot open.
- One thousand concurrent appends produce a valid chain.
- Two application processes cannot fork the sequence.
- Audit files use mode `0600`.
- A corrupt existing chain is reported instead of silently continued.
- Every state-changing operation has an intent before the driver is called.
- Cancellation leaves an intent and terminal outcome.
- Audit failure after database execution returns an explicit “operation may have succeeded” result and disables further governed writes.

---

# Phase 3: Restore documentation as the source of truth

Rewrite `linux/docs/production-audit.md`. Do not merely flip old “missing” items to “implemented.” Record what exists, what is tested, and what remains risky.

Files:

- `linux/docs/production-audit.md`
- `linux/docs/testing.md`
- `linux/ARCHITECTURE.md`
- `linux/ROADMAP.md`
- `linux/docs/storage.md`
- `linux/packaging/README.md`
- `linux/README.md`
- `linux/CHANGELOG.md`
- `CHANGELOG.md`

Required corrections:

- Policy, MCP, agentd, audit, transactions, jump chains, activity, and keyset work exist.
- CI uses `--lib --bins`.
- CI has three jobs, not two.
- CI runs `cargo-deny` and `cargo-audit`.
- MSSQL and ClickHouse integration tests run in CI.
- Document current audit failure behavior after Phase 2.
- Correct actual XDG locations.
- Mark TLS fingerprint support as incomplete until it is wired through storage, UI, and drivers.
- Mark cancellation as verified only after the PostgreSQL server-side test passes.

## Acceptance criteria

- Every production-readiness claim references an implementation and test.
- `docs/testing.md` matches `.github/workflows/build-linux.yml`.
- Roadmap checkboxes distinguish implemented from release-verified.
- Storage documentation matches the path constructors in code.
- User-facing changes have entries under `[Unreleased]`.

---

# Phase 4: PostgreSQL release and smoke matrix

Use one controlled Docker Compose fixture instead of many unrelated testcontainers.

## Fixture

Add:

- `linux/tests/fixtures/postgres-release/compose.yml`
- `linux/tests/fixtures/postgres-release/init/`
- `linux/tests/fixtures/postgres-release/certs/`
- `linux/scripts/test-postgres-release.sh`
- `linux/crates/drivers/postgres/tests/release_smoke.rs`

The fixture should include:

- PostgreSQL 16 with TLS enabled
- Test CA and hostname-specific server certificate
- SSH bastion with deterministic host and user keys
- Private network path for the SSH-only database
- Toxiproxy or equivalent for reconnect tests
- Seed tables and lock-test functions

## Test matrix

| Test | Acceptance criteria |
|---|---|
| Direct `VerifyFull` | Matching hostname and trusted CA succeed |
| Wrong hostname | Connection fails |
| Unknown CA | Connection fails |
| SSH bastion | Database is unreachable through the direct test path but works through SSH |
| TLS through SSH | Original database hostname is verified, not `127.0.0.1` |
| Read-only CTE | Data-changing CTE is denied and row count is unchanged |
| Timeout | `pg_sleep` is stopped on the server within the deadline |
| Cancel | Query disappears from `pg_stat_activity`; connection remains usable or is replaced |
| Batch rollback | Failure in statement two commits neither statement |
| Interactive rollback | Updated value remains unchanged after rollback |
| Activity | Shipping session query finds the test session |
| Blocking locks | Shipping lock query identifies blocker and blocked PID |
| Reconnect | Health enters reconnecting, connection is replaced, and a later query succeeds |
| SSH reconnect | Old tunnel is dropped and the replacement tunnel works |

## Reconnect testability

Refactor `linux/crates/app/src/services/connection_monitor.rs` so timing and reconnect behavior can be injected.

Use paused Tokio time for unit tests. Keep one real network interruption test in the release fixture.

## CI placement

Update `.github/workflows/build-linux.yml`:

- Required PR checks:
  - Preflight
  - GTK build/unit tests
  - Existing driver integrations
  - Deterministic PostgreSQL release scenarios
- Weekly scheduled:
  - Real reconnect timing
  - Full release-profile PostgreSQL smoke
  - GTK safety E2E
- Release candidate:
  - All of the above
  - Package installation and launch smoke

---

# Phase 5: Add two targeted GTK safety tests

Do not build a broad GUI automation suite.

Use SQLite for the GTK approval tests so the test does not depend on Docker or network timing.

Add:

- `linux/tests/e2e/gtk_safety.py`
- `linux/scripts/test-gtk-safety.sh`

Environment:

- Temporary `XDG_CONFIG_HOME`
- Temporary `XDG_DATA_HOME`
- Preseeded SQLite database
- Preseeded Prod connection
- `dbus-run-session`
- `xvfb-run`
- `pyatspi` or dogtail

## Flow 1: deny by default

1. Open a production connection.
2. Run a write.
3. Verify the dialog shows environment, principal, target, SQL, rule, and row estimate.
4. Close the dialog.
5. Verify the database is unchanged.
6. Verify a denied audit event exists.

## Flow 2: approve once

1. Run the same write.
2. Select “Approve once.”
3. Verify exactly one mutation occurred.
4. Verify the approval and outcome were audited.
5. Run another write.
6. Verify approval is requested again.

## Flow 3: unavailable audit

1. Start with an unwritable journal path.
2. Run a production write.
3. Verify no approval can bypass the audit failure.
4. Verify the database is unchanged and the UI explains why.

Run these tests as scheduled and release-candidate checks first. Promote them to required PR checks after 30 consecutive runs without retries or intermittent failures.

---

# Phase 6: Rename TablePro Linux to Bookie gradually

## 6.1 Confirm ownership first

Before publishing:

- Confirm the permanent GitHub owner and repository.
- Check AUR, Flathub, package registries, domains, and trademarks.
- Pick the final reverse-DNS application ID.

If the permanent repository is `cozygarage/Bookie`, a reasonable candidate is:

```text
io.github.cozygarage.Bookie
```

Do not use a domain namespace you do not own.

## 6.2 Visible branding first

Change user-facing branding while retaining existing technical identifiers.

Files include:

- `linux/crates/app/src/ui/app/mod.rs`
- `linux/crates/app/src/ui/app/status_pages.rs`
- `linux/crates/app/src/services/mcp_service.rs`
- `linux/crates/agentd/src/main.rs`
- `linux/crates/mcp/src/server.rs`
- `linux/crates/drivers/mongodb/src/lib.rs`
- `linux/crates/drivers/clickhouse/src/lib.rs`
- `linux/flatpak/com.tablepro.linux.desktop`
- `linux/flatpak/com.tablepro.linux.metainfo.xml`
- `linux/README.md`
- `linux/ARCHITECTURE.md`
- `linux/ROADMAP.md`
- `linux/CONTRIBUTING.md`
- `linux/docs/`
- `linux/po/tablepro.pot`

At this stage, intentionally retain:

- Cargo package names such as `tablepro-app`
- `$XDG_CONFIG_HOME/tablepro`
- `$XDG_DATA_HOME/tablepro`
- Secret Service schema `com.tablepro.linux.Password`
- Existing connection UUIDs
- Existing MCP token hashes

Use “Bookie, formerly TablePro Linux” for one or two releases.

## 6.3 Change public package identity

Before the first public AUR or Flatpak release:

- GUI binary: `bookie`
- Daemon binary: `bookie-agentd`
- Native package: `bookie`
- Desktop name: Bookie
- Final GTK and Flatpak application ID
- Gettext domain: `bookie`

Cargo package names can remain unchanged while `[[bin]] name` becomes `bookie`. Renaming every internal `tablepro-*` crate has little user value and should wait.

Files:

- `linux/crates/app/Cargo.toml`
- `linux/crates/agentd/Cargo.toml`
- `linux/crates/app/src/main.rs`
- `linux/crates/app/src/i18n.rs`
- `linux/flatpak/`
- `linux/packaging/aur/PKGBUILD`
- `linux/packaging/debian/`
- `linux/scripts/build-deb.sh`
- `linux/scripts/build-flatpak.sh`
- `.github/workflows/flatpak-linux.yml`

## 6.4 Preserve data identifiers

Do not rename the XDG directories or Secret Service schema during the initial Bookie releases.

Changing `com.tablepro.linux.Password` immediately would make existing database, SSH, and MCP secrets invisible.

If technical storage renaming is wanted later:

1. Read the new schema first.
2. Fall back to the legacy schema.
3. Copy the secret with the same UUID and `kind`.
4. Verify the new copy.
5. Keep legacy fallback for several releases.
6. Delete from both schemas when the user deletes a connection.

## Bookie acceptance criteria

- Existing saved connections open after the rename.
- Existing database and SSH secrets remain available.
- Existing history and workspace state load.
- The audit chain remains readable.
- Walker shows one Bookie launcher.
- The window class and application ID are consistent.
- No feature checks an account, license, subscription, or entitlement.
- The macOS product remains unchanged unless separately requested.

---

# Phase 7: Ship to Omarchy through AUR first

A stable AUR package is the most practical initial path for Omarchy.

Omarchy’s normal package flow can install and update it:

```text
omarchy pkg aur add bookie
omarchy update
```

Walker should discover the installed `.desktop` file automatically. No `~/.config/walker/` or Hyprland customization should be necessary.

## Fix the PKGBUILD

Update `linux/packaging/aur/PKGBUILD` to:

- Use `pkgname=bookie`
- Download an exact release tag or archive
- Use a real SHA-256 checksum
- Set `pkgver` from the release
- Install the Bookie binary
- Install desktop, icon, AppStream, translations, and license files
- Include required Kerberos build dependencies if MSSQL integrated authentication is included
- Stop tracking a floating `linux` branch
- Remove `sha256sums=('SKIP')`

For the transition:

```text
provides=('tablepro')
conflicts=('tablepro')
replaces=('tablepro')
```

Use `replaces` only for the initial migration period.

## Agent daemon packaging

The current systemd unit is not suitable:

- It runs `%h/.local/bin/tablepro-agentd` while packages install to `/usr/bin`.
- It uses stdio without an MCP client attached.
- It hardcodes the policy path.
- It is intended to restart a process whose transport cannot work as a resident service.

For the first AUR release:

- Install `bookie-agentd` if it passes Phase 2.
- Do not enable a user service automatically.
- Either omit the systemd unit or change it to loopback HTTP with explicit user activation.
- Require a valid policy and audit journal before startup.

## Secret Service behavior

Native AUR packaging should continue using `org.freedesktop.secrets` through `oo7`.

Improve current failure handling in `linux/crates/storage/src/secrets.rs`:

- Distinguish “secret not found” from “Secret Service unavailable.”
- Show a clear UI error when the keyring is locked or unavailable.
- Never turn keyring failure into a generic database authentication failure.

Test on Omarchy with the installed Secret Service implementation and a fresh login session.

## Release workflow

Add a Linux release workflow, for example:

- New: `.github/workflows/release-linux.yml`

It should:

1. Run all release gates.
2. Build `bookie` and `bookie-agentd` with `--locked`.
3. Build the Arch package in a clean Arch container.
4. Install the package.
5. Validate desktop and AppStream metadata.
6. Launch Bookie under D-Bus and Xvfb.
7. Publish source and binary artifacts for a versioned tag.
8. Update the AUR repository separately with the pinned checksum.

## AUR acceptance criteria

- `makepkg --cleanbuild` succeeds.
- `namcap` reports no release-blocking package errors.
- `desktop-file-validate` passes.
- `appstreamcli validate` passes.
- Installing the package creates one Walker launcher.
- `gtk-launch <final-desktop-id>` opens Bookie.
- `hyprctl clients` reports the expected Bookie class.
- `omarchy update` sees later AUR releases.
- Uninstall removes binaries and launcher metadata without deleting user data.
- Secret storage survives upgrades.

---

# Phase 8: Flatpak later

Do not make a local Flatpak the main Omarchy distribution path yet.

Before publishing Flatpak:

- Finalize the Bookie app ID.
- Generate offline Cargo sources.
- Remove network access from the build.
- Install and launch the built artifact in CI.
- Test Secret Service access.
- Reduce `--filesystem=home` if file portals and narrower SSH access can replace it.
- Decide how SSH keys, exports, Kerberos configuration, and ticket caches work in the sandbox.
- Publish to Flathub or a signed Flatpak repository so updates work.

A downloaded `.flatpak` bundle without a repository has no useful automatic update path.

---

# Release gate for the first trusted Bookie build

Run:

```bash
cd linux

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

Also run the repository’s mandatory lint gate:

```bash
swiftlint lint --strict
```

A release is ready when:

- Production writes cannot bypass GTK approval.
- Read-only mode blocks data-changing CTEs and administrative functions.
- Agent production writes are denied.
- Production mutation is impossible without a durable audit intent.
- PostgreSQL cancellation is confirmed server-side.
- `VerifyFull` succeeds through SSH without hostname downgrading.
- Reconnect restores a usable connection and tunnel.
- The AUR package installs and updates on Omarchy.
- Existing TablePro Linux data and secrets open under Bookie.
- Every Bookie feature remains available without a price or license gate.

## Suggested commit sequence

1. `fix(linux): preserve database service identity through tunnels`
2. `fix(policy): close read-only and admin-operation bypasses`
3. `fix(coordinator): route approvals by principal and runtime`
4. `fix(audit): fail closed for governed database operations`
5. `fix(editor): cancel PostgreSQL queries on the server`
6. `docs(linux): replace the stale production readiness audit`
7. `test(plugin-postgresql): add the release smoke matrix`
8. `test(linux): cover GTK approval and audit failure flows`
9. `feat(linux): adopt Bookie user-facing branding`
10. `build(linux): package Bookie for AUR and Omarchy`
11. `ci(linux): publish validated Bookie release artifacts`
