# TablePro

TablePro is a native Linux database client built with Rust, GTK4, libadwaita, GtkSourceView, and Relm4. Current development is on the `linux` branch. The Cargo workspace is under `linux/` and requires Rust 1.93.

Every shipped feature is free to use. TablePro has no account, license, subscription, paid-tier, or remote entitlement gate.

## Status

The Linux client is under active development. It includes database browsing, SQL editing, structure editing, inline row changes, query history, SSH tunnels, policy checks, audit records, MCP access, and a headless MCP process.

Database support is provided by static Rust crates compiled into the app:

| Database | Status |
|---|---|
| PostgreSQL | Stable |
| MySQL and MariaDB | Stable |
| SQLite | Stable |
| Microsoft SQL Server | Stable |
| ClickHouse | Stable |
| Redis | Experimental |
| MongoDB | Experimental |
| DuckDB | Optional build feature |
| Oracle Database | Optional ODPI-C build feature |

See [`linux/docs/driver-maturity.md`](linux/docs/driver-maturity.md) for current limits.

## Architecture

- `linux/crates/app`: GTK4/libadwaita application and Relm4 components
- `linux/crates/core`: domain types and database driver contracts
- `linux/crates/drivers/*`: static database driver crates
- `linux/crates/policy`: SQL classification, approvals, masking, and audit types
- `linux/crates/mcp`: MCP authentication, scopes, allowlists, rate limits, and tools
- `linux/crates/agentd`: headless MCP process
- `linux/crates/storage`: Secret Service integration, saved connections, history, and audit journal
- `linux/crates/ssh`: SSH tunnels

All GUI, MCP, and agent database access passes through policy-gated connection handles. MCP scopes and connection allowlists do not replace SQL policy checks.

Read [`linux/ARCHITECTURE.md`](linux/ARCHITECTURE.md) for crate boundaries and data flow.

## Build

Install Rust 1.93 and the GTK development packages listed in [`linux/README.md`](linux/README.md), then run from the repository root:

```bash
cargo run --manifest-path linux/Cargo.toml -p tablepro-app
```

Optional drivers:

```bash
cargo run --manifest-path linux/Cargo.toml -p tablepro-app --features duckdb
cargo run --manifest-path linux/Cargo.toml -p tablepro-app --features odpi
```

## Validate

```bash
bash linux/scripts/check-file-size.sh
cargo fmt --manifest-path linux/Cargo.toml --all -- --check
cargo clippy --manifest-path linux/Cargo.toml --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings
cargo test --manifest-path linux/Cargo.toml --workspace --exclude tablepro-driver-duckdb --lib --bins
```

Container-backed driver tests and more setup details are in [`linux/docs/testing.md`](linux/docs/testing.md).

## Contributing

Read [`CONTRIBUTING.md`](CONTRIBUTING.md). Pull requests for current development target the `linux` branch.

## Changelog

Release notes and unreleased changes are in [`linux/CHANGELOG.md`](linux/CHANGELOG.md).

## License

TablePro is licensed under the [GNU Affero General Public License v3.0 or later](LICENSE).
