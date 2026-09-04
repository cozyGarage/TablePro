#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/tests/fixtures/postgres-release"
STATE="$FIXTURE/state"
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
    echo "leaving the fixture running (TABLEPRO_FIXTURE_KEEP_UP=1)"
    return
  fi
  compose down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf -- "${secret_root:-}"
}

wait_for_port() {
  local host="$1" port="$2" label="$3"
  for _ in $(seq 1 120); do
    if (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; then
      exec 3>&- 2>/dev/null || true
      return 0
    fi
    sleep 1
  done
  echo "$label did not accept connections on $host:$port" >&2
  return 1
}

bash "$FIXTURE/generate-materials.sh"

mkdir -p "$STATE/config"
rm -f "$STATE/config/tablepro/known_hosts"

trap teardown EXIT
compose down --volumes --remove-orphans >/dev/null 2>&1 || true
compose up -d --build --wait

wait_for_port 127.0.0.1 8474 "toxiproxy api"
wait_for_port 127.0.0.1 5433 "postgres path"
wait_for_port 127.0.0.1 2223 "bastion path"

# A forwarded Unix socket's path (under XDG_RUNTIME_DIR) is capped at
# 100 bytes, so the isolated runtime dir has to stay short -- the
# fixture's own state directory, deep under the checked-out repo, is
# already too long for that once a socket name is appended.
secret_root="$(mktemp -d)"
mkdir -p "$secret_root/home" "$secret_root/data" "$secret_root/cache" "$secret_root/state" "$secret_root/runtime"
chmod 0700 "$secret_root/runtime"

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
CARGO_HOME="$cargo_home" \
  HOME="$secret_root/home" \
  XDG_CONFIG_HOME="$STATE/config" \
  XDG_DATA_HOME="$secret_root/data" \
  XDG_CACHE_HOME="$secret_root/cache" \
  XDG_STATE_HOME="$secret_root/state" \
  XDG_RUNTIME_DIR="$secret_root/runtime" \
  TABLEPRO_FIXTURE_POSTGRES_RELEASE=1 \
  TABLEPRO_FIXTURE_MATERIALS="$FIXTURE/materials" \
  dbus-run-session -- bash -c '
    set -euo pipefail
    eval "$(printf "tablepro-test" | gnome-keyring-daemon --daemonize --unlock --components=secrets)"
    cargo test --locked -p tablepro-release-tests --tests -- --include-ignored --test-threads=1
  '
