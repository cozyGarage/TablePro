#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

for command_name in dbus-run-session gdbus xvfb-run python3; do
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

if [[ "${TABLEPRO_GTK_XVFB_ACTIVE:-0}" != "1" ]]; then
  exec env TABLEPRO_GTK_XVFB_ACTIVE=1 \
    xvfb-run --auto-servernum --server-args="-screen 0 1280x1024x24 -nolisten tcp" "$0" "$@"
fi

export GDK_BACKEND=x11
export GSK_RENDERER=cairo
export GTK_A11Y=atspi
unset NO_AT_BRIDGE

# A bare dbus-run-session does not run a desktop autostart phase. Explicitly
# activate the AT-SPI bus before importing pyatspi so installed-package runs
# behave the same on Arch and Ubuntu CI images.
gdbus call --session \
  --dest org.a11y.Bus \
  --object-path /org/a11y/bus \
  --method org.a11y.Bus.GetAddress >/dev/null

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
if [[ -n "${TABLEPRO_GTK_BINARY:-}" ]]; then
  test_binary="$TABLEPRO_GTK_BINARY"
else
  cargo build --release --locked -p tablepro-app --bin tablepro-app
  test_binary="$TARGET_DIR/installed/usr/bin/tablepro"
  install -Dm755 "$TARGET_DIR/release/tablepro-app" "$test_binary"
fi
if [[ ! -x "$test_binary" ]]; then
  echo "installed GTK test binary is not executable: $test_binary" >&2
  exit 1
fi
timeout 300s python3 "$ROOT/crates/app/tests/gtk_safety.py" "$test_binary"
