# Changelog: TablePro Linux

## [Unreleased]

### Added

- Saved connections can name a certificate authority, so a server whose certificate is issued privately can be verified with Verify Ca or Verify Full
- Policy crate with AST SQL classification, PolicyGuard, blast-radius rewrite, column masking, and policy.toml. Statements that read host files, run a program, or send SQL to another server are treated as administrative, so an agent is refused and a read-only connection denies them
- Environment field on saved connections (Local / Dev / Staging / Prod)
- Hash-chained audit journal at `$XDG_DATA_HOME/tablepro/audit.jsonl`
- Interactive `Connection::begin` transactions (PostgreSQL, MySQL, SQLite)
- TLS modes including VerifyFull default for new TLS connections, with certificate hostname and authority failures reported as TLS errors
- MCP bridge (interactive-app loopback HTTP plus on-demand agentd stdio) with scoped tokens and rate limiting
- Headless on-demand `tablepro-agentd` with required `--policy`, issuing read-only tokens by default, with an optional expiry and a way to write the token to an owner-only file instead of the terminal
- GTK approval dialog for policy `RequireApproval` decisions
- Server activity SQL templates (sessions, locks, long-running, replication)
- Example policy file and MSSQL in AppStream metainfo
- SSH jump-host chains via nested `ssh.jump` in saved connections and `SshTunnel::open_chain`
- Keyset pagination helper for browse Next past the OFFSET threshold when primary keys are known
- Current-page CSV export with explicit non-snapshot semantics, written so another program never reads a half-finished file
- EXPLAIN plan dialog from the SQL editor and main menu
- Server version cached on connect and shown in the window subtitle
- ClickHouse, Redis, DuckDB, and MongoDB drivers; Oracle remains excluded until its optional implementation and fixture work
- Preferences → MCP token pairing (libsecret + loopback endpoint)
- Multi-window via New Window, with each window connecting on its own
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
- Named `:parameter` placeholders in the SQL editor, with a per-value type choice, sent to the database as bound parameters. Statement splitting and placeholder scanning follow the connected engine's own quoting, so a PostgreSQL function body is run whole and a MySQL `#` comment hides nothing but itself
- Schema-aware editor completion that offers tables after FROM and JOIN, and the columns of the tables in the statement, including through table aliases
- Saved query favorites in `favorites.json`, saved with Ctrl+D
- Open Quickly (Ctrl+P) over favorites, open tabs, and saved connections
- Direct PostgreSQL Unix-socket connections shared by GUI and agentd, with saved-path compatibility and a real socket fixture
- Required GTK connection-isolation smoke plus a daily five-attempt retry-free soak workflow
- Internal Arch RC packaging from an immutable commit archive and verified checksum
- Screen readers now announce the browse toolbar's insert-row button by name
- Installed GTK checks that a connection switch gates pending row edits and that a browse tab reads the connection it was reopened against
- Each window holds its own database connection, so several databases can be open at the same time in separate windows

### Changed

- Stop is offered for the engines that can actually abort a statement, because the policy layer no longer hides a driver's cancellation support
- A cell edit is discarded with an explanation if the row moved out from under the editor, instead of being written to whichever row took its place
- Copy row as INSERT leaves out generated columns, which the database always rejects, and escapes a value the way the connected engine reads it, so a row holding a backslash copies as data rather than as SQL
- Stop and the query timeout now abort the running statement on MySQL, ClickHouse and SQLite, so the statement stops in the database and the session can keep writing afterwards
- A cancelled or timed-out SQL Server statement now reports that its outcome is unknown and closes the connection, instead of leaving every later query on that connection waiting forever
- SQLite results show the value of a computed column instead of a blank cell, so counts, aggregates and literal expressions read correctly, and a browse tab shows its total row count
- Browsing, row counts, structure reads, DDL, saving row edits, server activity, EXPLAIN and connecting all honour the configured query timeout instead of running without limit
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
- Connection changes validate a candidate before exclusively replacing the old workspace; failed candidates and failed saves keep the old connection active
- PostgreSQL browse pages resolve primary-key ordering before the first row fetch and append PK tie-breakers to user sorts
- Public package metadata names only drivers that are actually shipped

### Fixed

- The headless agent daemon reuses one database session per connection instead of opening a new one on every tool call
- The agent rate limiter forgets idle callers instead of growing for the lifetime of the process
- Saved connections are written so only the owner can read them, and a save that fails part way leaves the previous file intact
- Query plans use the connected engine's own EXPLAIN form, and engines without a query plan statement say so instead of failing on malformed SQL
- Reading a query plan through an agent no longer requires write access, while EXPLAIN ANALYZE stays governed as the write it performs
- Agent CSV export quotes values containing separators, quotation marks, or line breaks instead of producing corrupt rows
- Renaming a column in the structure editor applies the rename instead of failing, and the column's other edits apply to the new name
- PostgreSQL foreign key ON DELETE and ON UPDATE actions are reported as they are defined instead of always reading as NO ACTION
- Integer cells wider than 2^53 keep their exact value when edited instead of being rounded
- CSV export is limited to the loaded filtered page instead of silently paging an unordered changing table
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
- Broken stdio-under-systemd agentd unit and generated Python bytecode tracked in the repository
- MCP `search_query_history` until history can be connection-isolated, redacted, rate-limited, and audited

### Security

- The headless agent daemon opens a saved connection through its configured SSH chain and verifies the certificate against the real database hostname, instead of dialling the database directly
- MongoDB connections honour the selected TLS mode and certificate authority instead of always connecting without encryption
- MySQL Verify Ca connections no longer crash the application
- ClickHouse connections distinguish encrypt-only from verifying TLS and can name a certificate authority, instead of always verifying against the bundled roots
- Redis connections can use TLS, including a named certificate authority, instead of failing whenever encryption is selected
- MongoDB and Redis connection attempts stop after five seconds instead of waiting on library defaults
- SSH tunnels stop waiting on a host that accepts a connection and never answers, reporting which host and port timed out
- MySQL server-control functions and SQL Server extended and system procedures are recognised as administrative, so agents are denied them the same way they are on PostgreSQL
- Agent write-scope checks classify SQL with the connected engine's dialect instead of always assuming PostgreSQL
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
