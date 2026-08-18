# Changelog: TablePro Linux

## [Unreleased]

### Added

- Policy crate with AST SQL classification, PolicyGuard, blast-radius rewrite, column masking, and policy.toml
- Environment field on saved connections (Local / Dev / Staging / Prod)
- Hash-chained audit journal at `$XDG_DATA_HOME/tablepro/audit.jsonl`
- Interactive `Connection::begin` transactions (PostgreSQL, MySQL, SQLite)
- TLS modes including VerifyFull default for new TLS connections, with certificate hostname and authority failures reported as TLS errors
- MCP bridge (stdio + loopback HTTP) with scoped tokens and rate limiting
- Headless `tablepro-agentd` with required `--policy` and systemd user unit
- GTK approval dialog for policy `RequireApproval` decisions
- Server activity SQL templates (sessions, locks, long-running, replication)
- Example policy file and MSSQL in AppStream metainfo
- SSH jump-host chains via nested `ssh.jump` in saved connections and `SshTunnel::open_chain`
- Keyset pagination helper for browse Next past the OFFSET threshold when primary keys are known
- Streaming CSV table export that pages rows instead of materializing the full set
- EXPLAIN plan dialog from the SQL editor and main menu
- Server version cached on connect and shown in the window subtitle
- ClickHouse, Redis, DuckDB, MongoDB, and Oracle drivers (Oracle needs Instant Client)
- Preferences → MCP token pairing (libsecret + loopback endpoint)
- Multi-window via New Window
- Flathub submission notes and screenshot capture guide
- Rust file-size guardrail in preflight: soft 1200 / hard 1800 lines, with ratchet ceilings in `file-size-baselines.txt`
- Driver maturity labels in the Connect dialog (Experimental subtitle for Redis, MongoDB, DuckDB, and Oracle)
- Driver maturity matrix in `docs/driver-maturity.md`
- Windows integrated authentication for SQL Server through the current Kerberos ticket cache
- Principal-aware GUI approval routing with fail-closed behavior when no active window exists
- Explicit administrative SQL classification for PostgreSQL backend-control functions and MySQL KILL
- Weekly and manually dispatched full-workspace Clippy on current stable Rust, alongside the Rust 1.93 MSRV gate
- Arch/Omarchy Rust toolchain guidance and a traceable upstream Linux sync log
- Installed GTK safety checks for approval dismissal, approve-once scope, and unavailable audit storage
- Deterministic PostgreSQL release checks for TLS hostname and authority verification, tunnelled access, read-only denial, rollback, blocking locks, and reconnect
- Verify Full for PostgreSQL over SSH, using the original database hostname for certificate checks
- Named `:parameter` placeholders in the SQL editor, with a per-value type choice, sent to the database as bound parameters

### Changed

- MSSQL uses native TLS and keeps its Kerberos service identity when SSH forwards the socket through localhost
- Workspace crates declare AGPL-3.0-or-later; cargo-deny allows that license and documents the rsa Marvin advisory ignore
- Linux UI and DDL modules split by domain: browse tab, grid, editor, app workspace helpers, and `sql_ddl` are directories of focused files instead of multi-thousand-line units
- Read-only is enforced by policy classification (data-modifying CTEs blocked)
- DatabaseService exposes only policy-gated connection handles
- Roadmap and production audit rewritten to match the governed data-plane plan
- Arbitrary SQL query soft row cap raised to 1,000,000 (truncated flag still set); browse pagination remains uncapped by that constant
- Agentd approval strategies are deny (default) or interactive TTY; automatic approval is test-only
- DuckDB driver is an optional `duckdb` Cargo feature (bundled build is large)
- Oracle appears in the driver list only when built with `--features odpi`
- Linux CI runs a non-GTK preflight job before the full GTK checks; local `./scripts/preflight.sh` mirrors that gate
- Policy evaluation takes the resolved connection EnvPolicy once; connection overrides no longer re-run evaluate
- Connect dialog TLS control is a mode picker (Disabled / Prefer / Require / Verify CA / Verify Full), defaulting to Verify Full for network drivers
- Local and development human writes require audit by default; best-effort unaudited writes require an explicit policy setting
- PostgreSQL query timeout and cancellation now wait within bounded deadlines for server confirmation before returning to GTK or MCP callers

### Fixed

- Integer cells wider than 2^53 keep their exact value when edited instead of being rounded
- CSV export reads from the connection that owns the table's tab instead of whichever connection is active
- ClickHouse integration tests connect over plain HTTP instead of default VerifyFull HTTPS
- Flatpak CI bootstraps Rust 1.93 via rustup so the GNOME 47 SDK's older rust-stable extension is not required
- MCP write preview and other `begin()` paths now run statements through PolicyGuard instead of a raw driver transaction
- Agents are denied (and humans must approve) when a blast-radius estimate cannot be computed for UPDATE/DELETE
- Read-only connections deny unparseable SQL before any human approval fallback
- Activity termination accepts only validated positive numeric session identifiers
- Partial environment and connection policies inherit secure environment defaults and default masking rules
- Transactional batches request approval once instead of once per statement
- Mixed SQL batches retain DDL and unscoped UPDATE or DELETE restrictions regardless of statement order
- The agent daemon requires at least one existing saved connection when issuing a token
- Verified legacy audit journals rotate intact when upgrading to durable intent and outcome records
- PostgreSQL cancellation now stops the server query, records a terminal audit outcome, supports parameterized operations and transaction rollback, and discards sessions with unknown outcomes
- Approval dialogs attach to a visible application window when the desktop does not report an active window
- Saved connections expose a keyboard and screen-reader accessible open action

### Removed

- Unused `Approve for session` approval outcome (no session-grant store existed)
- MCP `search_query_history` until history can be connection-isolated, redacted, rate-limited, and audited

### Security

- MCP HTTP rejects untrusted browser origins before JSON-RPC dispatch
- SSH host-key changes across key algorithms are treated as mismatches instead of first use
- Linux GitHub Actions are pinned to immutable commits
- TLS certificate verification modes (VerifyCa / VerifyFull) replace encrypt-only Require
- Agent results masked for sensitive column name patterns by default
- MCP tokens with an empty connection allowlist can no longer touch any connection
- SSH stack upgraded to russh 0.60.3 (fixes unbounded allocation advisories and drops vulnerable libcrux 0.0.4)
- Translation locale initialization runs before worker threads and uses the corrected gettext safety contract
- Production GUI and daemon paths no longer construct automatic approval sinks
- Mutations, DDL, administration, and transaction completion persist durable audit intent before database execution and fail closed when required records cannot be written
- Unknown or interrupted write outcomes block later governed writes across restarts and concurrent app and daemon processes
- The audit journal enforces private file permissions, verifies its hash chain, recovers interrupted appends, and serializes writers across processes
- Agentd and in-app MCP refuse service when required audit storage is unavailable
- PostgreSQL administrative and side-effecting function calls are denied to agents and read-only connections while literals and comments remain read-only
