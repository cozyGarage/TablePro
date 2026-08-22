# Testing

Run test commands from the `linux/` workspace root.

## Current local checks

The quick non-GTK gate is:

```bash
./scripts/preflight.sh
```

It runs the file-size guard, formatting, Clippy for the non-GTK package list, library tests for those packages, and the sandbox regression tier (`./scripts/test-sandbox.sh`), which covers every integration target that needs no Docker, database service, or display.

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
./scripts/test-postgres-socket.sh
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

For the deterministic direct Unix-socket fixture:

```bash
./scripts/test-postgres-socket.sh
```

It exposes a PostgreSQL 16 socket directory from a disposable container and
verifies query, write, pre-dispatch cancellation, close, and reconnect without
opening a TCP database port.

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
4. A named `:parameter` in the editor prompts for a value and writes the bound value, not the placeholder text.
5. Ctrl+D saves the editor query as a favorite, and Ctrl+P finds it, opens it, and records the use.
6. A successful saved-connection switch tears down the old workspace and sends writes only to the new database.
7. A failed candidate connection leaves the original editor and database usable.
8. A running read is cancelled and fully settles before the candidate connection is activated; subsequent writes reach only the new database.
9. Current-page CSV export writes exactly the first 100 PK-ordered rows from a 150-row fixture through the real portal chooser.

Each scenario declares its own fixture shape through `environment` and `audit_available` attributes, so a scenario can run against a local or production saved connection.

Buttons and rows are invoked only through named AT-SPI actions; there is no Return-key fallback that can land in an unrelated dialog. Keyboard events remain only for the shortcuts under test. Each denial assertion requires the row count to hold for a settle window rather than matching once.

On Arch or Omarchy, install the harness dependencies with:

```bash
sudo pacman -S --needed dbus xorg-server-xvfb at-spi2-core python-atspi
```

Ubuntu CI uses `dbus-daemon`, `gnome-keyring`, `xvfb`, `xauth`, `at-spi2-core`, `python3-pyatspi`, and `scrot`. The PR smoke job is required. A separate daily workflow runs five retry-free attempts and uploads stdout, stderr, accessibility snapshots, and screenshots on failure. RC promotion requires 30 consecutive attempts across at least six runs. Service-level tests still cover pure state and policy behavior, but they do not replace these cross-layer tests.

For ordinary UI changes, test the affected flow manually and include before and after screenshots. Add deterministic automation when a regression can be reproduced without timing or desktop-session assumptions.

## CI

`.github/workflows/build-linux.yml` has these jobs:

1. Preflight on Rust 1.93 without the GTK application.
2. GTK formatting, Clippy, and `cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins` in Ubuntu 25.10.
3. Required installed GTK safety smoke under Xvfb and PyAT-SPI, including a real Secret Service round-trip.
4. PostgreSQL, MySQL, SQL Server, and ClickHouse integration tests on Docker.
5. The PostgreSQL release fixture with TLS, an SSH bastion, and Toxiproxy on Docker.
6. Scheduled and manually triggered Clippy on current stable Rust.
7. Supply-chain checks with `cargo deny` and `cargo audit` in the GTK job.

`.github/workflows/gtk-soak.yml` supplies the independent daily five-attempt soak ledger.

The Ubuntu 25.10 container provides the GLib version required by the selected libadwaita and Relm4 features.

## Measuring how good the tests are

Counting tests says nothing about whether they would catch a defect. Two
tools measure that directly. Neither gates a build: they take tens of
minutes, and a number moving the wrong way is a conversation, not a
failure. Both run weekly and on demand from
`.github/workflows/linux-quality.yml`.

### Mutation testing

`cargo mutants` changes the code in small ways - flips a comparison,
replaces a return value, swaps an operator - and reruns the tests. A
mutant the tests still pass is behaviour nothing pins.

```bash
cargo install cargo-mutants --locked
cd linux
cargo mutants --package tablepro-core --test-tool cargo -- --lib
cargo mutants --package tablepro-core --file crates/core/src/sql_lex.rs --test-tool cargo -- --lib
```

`core` and `policy` are the right targets: pure logic, no GTK, no driver,
and the two crates whose defects reach SQL text and authorization
decisions. Mutating the app crate mostly reports widget construction no
unit test can reach.

Read the output carefully. Many surviving mutants are *equivalent* - a
different program with identical behaviour - and can never be caught. In
`sql_lex::skip_span`, replacing `offset + 1` with `offset - 1` shortens a
comment span by one character, but the scanner then reads that character
as ordinary text and reaches the same result, so no test can tell the
difference. Chasing those wastes effort.

The first run on 2026-08-22 tested 87 mutants across `sql_lex.rs` and
`sql_literal.rs`: 65 caught, 12 missed, 9 timed out. Two of the twelve
were real gaps rather than equivalents:

- Nothing tested an underscore in a PostgreSQL dollar-quote tag, though
  the tag validator explicitly allows one. `$my_tag$` had no coverage.
- `extract_named_parameters` advances by whatever `skip_span` returns
  without checking it is non-zero, while `statement_spans` filters for
  exactly that. A zero-length span would hang the parameter scanner. No
  input produces one today, so this is a missing guard rather than a live
  defect - but the asymmetry between two callers of the same function is
  the kind of thing that becomes a defect later.

A timeout is a finding too: it usually means the mutant produced an
infinite loop, which tells you a loop depends on a value nothing bounds.

### Coverage

`cargo llvm-cov` reports which lines the unit and sandbox tiers execute.

```bash
cargo install cargo-llvm-cov --locked
rustup component add llvm-tools-preview
cd linux
cargo llvm-cov --workspace --exclude tablepro-driver-duckdb --exclude tablepro-app \
  --lib --bins --tests --summary-only
```

The app crate and the driver, TLS, release and installed-GTK tiers are
excluded: they need Docker, a network fixture or a display, so including
them would make the number depend on the runner rather than on the tests.
Treat coverage as a map of untested regions, not a score. A well-covered
line proves a test executed it, never that a test checked its result -
which is exactly the gap mutation testing measures.

## File-size guard

`scripts/check-file-size.sh` enforces the Rust source limits recorded in `file-size-baselines.txt`:

| Limit | Lines | Result |
|---|---:|---|
| Soft | 1200 | Split the file or record an approved baseline |
| Hard | 1800 | Unlisted files fail |
| Ratchet | Recorded maximum | A listed oversized file may not grow past its baseline |

Lower a baseline in the same change when an oversized file shrinks.
