#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
  secret_root="$(mktemp -d)"
  mkdir -p \
    "$secret_root/home" \
    "$secret_root/config" \
    "$secret_root/data" \
    "$secret_root/cache" \
    "$secret_root/state" \
    "$secret_root/runtime"
  chmod 0700 "$secret_root/runtime"
  trap 'rm -rf -- "$secret_root"' EXIT
  cargo_home="${CARGO_HOME:-$HOME/.cargo}"
  CARGO_HOME="$cargo_home" \
    HOME="$secret_root/home" \
    XDG_CONFIG_HOME="$secret_root/config" \
    XDG_DATA_HOME="$secret_root/data" \
    XDG_CACHE_HOME="$secret_root/cache" \
    XDG_STATE_HOME="$secret_root/state" \
    XDG_RUNTIME_DIR="$secret_root/runtime" \
    dbus-run-session -- "$0" "$@"
  exit
fi

eval "$(gnome-keyring-daemon --daemonize --components=secrets)"
printf 'tablepro-test' | gnome-keyring-daemon --unlock
printf 'probe' | secret-tool store --label='TablePro CI probe' application tablepro-ci
secret-tool clear application tablepro-ci
cargo test -p tablepro-storage secrets::tests::round_trip_via_secret_service -- --ignored --exact
