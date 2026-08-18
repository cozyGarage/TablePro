# Testing

Run test commands from the `linux/` workspace root.

## Current local checks

The quick non-GTK gate is:

```bash
./scripts/preflight.sh
```

It runs the file-size guard, formatting, Clippy for the non-GTK package list, library tests for those packages, and the MCP policy integration test.

The current full default workspace gate is:

```bash
./scripts/ci-local.sh
```

Its Cargo commands are:

```bash
cargo clippy --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings
cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins
```

Keep both `--lib` and `--bins`. `tablepro-app` is a binary crate, so `--lib` alone skips its tests. DuckDB is excluded from the default gate because its optional native build is large.

To run the same unit-test shape without the helper script:

```bash
cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins
```

## Unit tests

Place focused tests beside the Rust module under `#[cfg(test)]`. Pure parsing, SQL generation, policy, mapping, persistence, and state-transition behavior should be tested without GTK where possible.

The full `--lib --bins` command builds and runs tests from library crates and binary crates. Application service tests are included through the `tablepro-app` binary target.

## Storage tests

Storage tests use temporary directories and explicit file paths for JSON and audit behavior. Query-history tests use SQLite. The Secret Service round-trip test is ignored by default because it needs a working desktop keyring session.

Tests that change XDG environment variables must avoid racing with other tests. Prefer internal functions that accept a path when the module already provides them.

## Real-driver integration tests

Run all configured Docker suites with:

```bash
./scripts/ci-local.sh integration
```

The script runs:

```bash
cargo test --test integration -p tablepro-driver-postgres -- --include-ignored --test-threads=1
cargo test --test integration -p tablepro-driver-mysql -- --include-ignored --test-threads=1
cargo test --test integration -p tablepro-driver-mssql -- --include-ignored --test-threads=1
cargo test --test integration -p tablepro-driver-clickhouse -- --include-ignored --test-threads=1
```

These tests require Docker or a compatible Podman API socket. Keep each container handle alive for the full test because dropping it stops the container.

PostgreSQL integration coverage includes controlled cancellation and timeout against a real server. The tests confirm the query appears in `pg_stat_activity`, trigger cancellation or a deadline, confirm the query leaves server activity, and verify that the pool remains usable. Transaction cancellation is followed by rollback and a data check.

## PostgreSQL release fixture

Run the Phase 3 release gate with:

```bash
./scripts/test-postgres-release.sh
```

The script generates fixture certificates and SSH keys, starts PostgreSQL 16 with TLS, an OpenSSH
bastion, and Toxiproxy, then runs `tablepro-release-tests` with `--include-ignored --test-threads=1`.
Only Toxiproxy publishes host ports, so the database is reachable through the proxied path or the
bastion and either path can be cut during a test.

The suite verifies certificate hostname and authority checks, tunnelled access to a database with no
published port, that a tunnelled `VerifyFull` verifies the original database hostname while a TCP-forwarded one refuses to verify the local dial address,
read-only denial of data-changing CTEs and administrative functions, batch and interactive rollback,
activity and blocking-lock queries, and direct and SSH reconnect.

`tests/fixtures/postgres-release/README.md` documents the topology, generated materials, and
`TABLEPRO_FIXTURE_KEEP_UP=1` for keeping the containers running.

## Local PostgreSQL smoke test

For a PostgreSQL server you already run:

```bash
./scripts/smoke-postgres.sh
```

The ignored smoke test creates and drops `tablepro_smoke_items`. Use a disposable database. Configure another target with `SMOKE_PG_HOST`, `SMOKE_PG_PORT`, `SMOKE_PG_USER`, `SMOKE_PG_PASS`, and `SMOKE_PG_DB`.

## GTK tests

Run the installed safety suite with:

```bash
./scripts/test-gtk-safety.sh
```

The script builds `tablepro-app`, starts an isolated D-Bus session and Xvfb display, and drives the real application through PyAT-SPI. Each scenario gets temporary XDG directories and a production SQLite saved connection.

The suite verifies:

1. Dismissing a production approval leaves SQLite unchanged.
2. Approving once performs one mutation and prompts again for the next operation.
3. Unavailable audit storage denies the mutation without showing an approval path around the failure.

The harness sends synthetic keyboard events only to a focused push button, so a stray key cannot reach an approval dialog, and each denial assertion requires the row count to hold for a settle window rather than matching once.

On Arch or Omarchy, install the harness dependencies with:

```bash
sudo pacman -S --needed dbus xorg-server-xvfb at-spi2-core python-atspi
```

Ubuntu CI uses `dbus-daemon`, `xvfb`, `xauth`, `at-spi2-core`, and `python3-pyatspi`. The CI step remains non-blocking until it completes 30 retry-free scheduled runs. Service-level tests still cover pure state and policy behavior, but they do not replace these cross-layer tests.

For ordinary UI changes, test the affected flow manually and include before and after screenshots. Add deterministic automation when a regression can be reproduced without timing or desktop-session assumptions.

## CI

`.github/workflows/build-linux.yml` has these jobs:

1. Preflight on Rust 1.93 without the GTK application.
2. GTK formatting, Clippy, and `cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins` in Ubuntu 25.10.
3. Installed GTK safety flows under Xvfb and PyAT-SPI, non-blocking during the soak period.
4. PostgreSQL, MySQL, SQL Server, and ClickHouse integration tests on Docker.
5. The PostgreSQL release fixture with TLS, an SSH bastion, and Toxiproxy on Docker.
6. Scheduled and manually triggered Clippy on current stable Rust.
7. Supply-chain checks with `cargo deny` and `cargo audit` in the GTK job.

The Ubuntu 25.10 container provides the GLib version required by the selected libadwaita and Relm4 features.

## File-size guard

`scripts/check-file-size.sh` enforces the Rust source limits recorded in `file-size-baselines.txt`:

| Limit | Lines | Result |
|---|---:|---|
| Soft | 1200 | Split the file or record an approved baseline |
| Hard | 1800 | Unlisted files fail |
| Ratchet | Recorded maximum | A listed oversized file may not grow past its baseline |

Lower a baseline in the same change when an oversized file shrinks.
