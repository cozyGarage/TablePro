#!/usr/bin/env bash
# Driver TLS tier: proves each network driver's TLS mode mapping against a
# real server holding a privately issued certificate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/tests/fixtures/driver-tls"
cd "$ROOT"

if ! command -v docker >/dev/null 2>&1; then
  echo "missing required command: docker" >&2
  exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "docker compose is required" >&2
  exit 1
fi

KEEP_UP="${TABLEPRO_FIXTURE_KEEP_UP:-0}"

compose() {
  docker compose --project-directory "$FIXTURE" -f "$FIXTURE/docker-compose.yml" "$@"
}

teardown() {
  if [[ "$KEEP_UP" == "1" ]]; then
    echo "leaving the driver-tls fixture running (TABLEPRO_FIXTURE_KEEP_UP=1)"
    return
  fi
  compose down --volumes --remove-orphans >/dev/null 2>&1 || true
}

bash "$FIXTURE/generate-materials.sh"

trap teardown EXIT
compose down --volumes --remove-orphans >/dev/null 2>&1 || true
compose up -d --build --wait

export TABLEPRO_FIXTURE_DRIVER_TLS=1
export TABLEPRO_DRIVER_TLS_MATERIALS="$FIXTURE/materials"

cargo test --locked -p tablepro-driver-tls-tests --tests -- --include-ignored --test-threads=1
