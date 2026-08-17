#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command_name in dbus-run-session xvfb-run python3; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 1
  fi
done

if ! python3 -c "import pyatspi" >/dev/null 2>&1; then
  echo "missing Python module: pyatspi" >&2
  exit 1
fi

if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
  exec dbus-run-session -- "$0" "$@"
fi

if [[ -z "${DISPLAY:-}" ]]; then
  exec xvfb-run --auto-servernum --server-args="-screen 0 1280x1024x24 -nolisten tcp" "$0" "$@"
fi

export GDK_BACKEND=x11
export GSK_RENDERER=cairo
export GTK_A11Y=atspi
unset NO_AT_BRIDGE

cargo build --locked -p tablepro-app --bin tablepro-app

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
timeout 300s python3 "$ROOT/crates/app/tests/gtk_safety.py" "$TARGET_DIR/debug/tablepro-app"
