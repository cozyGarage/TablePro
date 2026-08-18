#!/usr/bin/env bash
# Local CI mirror for linux/.
#
# Modes:
#   ./scripts/ci-local.sh           # default: full fast checks (GTK included)
#   ./scripts/ci-local.sh quick     # alias for ./scripts/preflight.sh (no GTK app)
#   ./scripts/ci-local.sh full      # fmt + clippy + build + unit tests (GTK)
#   ./scripts/ci-local.sh integration  # driver docker integration tests
#   ./scripts/ci-local.sh release      # integration plus the postgres release fixture
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-full}"

if [[ -f "$ROOT/scripts/dev-env.sh" ]]; then
  # shellcheck source=/dev/null
  source "$ROOT/scripts/dev-env.sh"
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$CARGO_TARGET_DIR" in
  /tmp/*) export CARGO_TARGET_DIR="$ROOT/target" ;;
esac

run_full() {
  echo "==> file size guardrail"
  "$ROOT/scripts/check-file-size.sh"

  echo "==> cargo fmt --check"
  cargo fmt --all -- --check

  echo "==> cargo clippy"
  cargo clippy --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings

  echo "==> cargo test --workspace --lib --bins"
  # Compiling tests already builds the crates; a separate `cargo build`
  # beforehand doubles wall time for little signal.
  # DuckDB is optional (--features duckdb) and expensive to compile.
  cargo test --workspace --exclude tablepro-driver-duckdb --lib --bins

  echo "Full fast checks passed."
  echo "Driver integration: ./scripts/ci-local.sh integration"
}

run_integration() {
  echo "==> Postgres integration"
  cargo test --test integration -p tablepro-driver-postgres -- --include-ignored --test-threads=1
  echo "==> MySQL integration"
  cargo test --test integration -p tablepro-driver-mysql -- --include-ignored --test-threads=1
  echo "==> MSSQL integration"
  cargo test --test integration -p tablepro-driver-mssql -- --include-ignored --test-threads=1
  echo "==> ClickHouse integration"
  cargo test --test integration -p tablepro-driver-clickhouse -- --include-ignored --test-threads=1
  echo "Integration checks passed."
}

run_release() {
  run_integration
  echo "==> PostgreSQL release fixture"
  "$ROOT/scripts/test-postgres-release.sh"
  echo "==> Installed GTK safety flows"
  "$ROOT/scripts/test-gtk-safety.sh"
  echo "Release checks passed."
}

case "$MODE" in
  quick | preflight)
    exec "$ROOT/scripts/preflight.sh"
    ;;
  full | "")
    run_full
    ;;
  integration)
    run_integration
    ;;
  release)
    run_release
    ;;
  *)
    echo "usage: $0 [quick|full|integration|release]" >&2
    exit 2
    ;;
esac
