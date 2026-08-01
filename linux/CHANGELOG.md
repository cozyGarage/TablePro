# Changelog — TablePro Linux

## [Unreleased]

### Added

- Policy crate with AST SQL classification, PolicyGuard, blast-radius rewrite, column masking, and policy.toml
- Environment field on saved connections (Local / Dev / Staging / Prod)
- Hash-chained audit journal at `$XDG_DATA_HOME/tablepro/audit.jsonl`
- Interactive `Connection::begin` transactions (PostgreSQL, MySQL, SQLite)
- TLS modes including VerifyFull default for new TLS connections
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

### Changed

- Linux UI and DDL modules split by domain: browse tab, grid, editor, app workspace helpers, and `sql_ddl` are directories of focused files instead of multi-thousand-line units
- Read-only is enforced by policy classification (data-modifying CTEs blocked)
- DatabaseService exposes only policy-gated connection handles
- Roadmap and production audit rewritten to match the governed data-plane plan
- Arbitrary SQL query soft row cap raised to 1,000,000 (truncated flag still set); browse pagination remains uncapped by that constant
- Agentd approval strategies: deny (default), auto, or tty
- DuckDB driver is an optional `duckdb` Cargo feature (bundled build is large)
- Linux CI runs a non-GTK preflight job before the full GTK checks; local `./scripts/preflight.sh` mirrors that gate
- Policy evaluation takes the resolved connection EnvPolicy once; connection overrides no longer re-run evaluate
- Connect dialog TLS control is a mode picker (Disabled / Prefer / Require / Verify CA / Verify Full), defaulting to Verify Full for network drivers

### Fixed

- ClickHouse integration tests connect over plain HTTP instead of default VerifyFull HTTPS
- Flatpak CI bootstraps Rust 1.93 via rustup so the GNOME 47 SDK's older rust-stable extension is not required
- MCP write preview and other `begin()` paths now run statements through PolicyGuard instead of a raw driver transaction
- Agents are denied (and humans must approve) when a blast-radius estimate cannot be computed for UPDATE/DELETE
- Approval dialog no longer offers "Approve for session" until session grants exist

### Security

- TLS certificate verification modes (VerifyCa / VerifyFull) replace encrypt-only Require
- Agent results masked for sensitive column name patterns by default
- MCP tokens with an empty connection allowlist can no longer touch any connection
