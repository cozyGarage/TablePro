#!/usr/bin/env bash
# Every database call the GUI makes must carry an OperationControl, so a
# slow statement is bounded and a Stop can reach the driver. A bare
# `conn.query(...)` in the app has no deadline and no cancellation: it
# runs until the server answers, however long that takes.
#
# The controlled forms end in `_controlled`. Anything else on a
# connection handle in `crates/app/src` is a regression.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

UNCONTROLLED='(conn|connection)\.(query|query_params|execute|execute_params|execute_in_transaction|fetch_rows|fetch_columns|list_tables)\('

if hits=$(grep -rnE "$UNCONTROLLED" crates/app/src 2>/dev/null); then
  echo "error: unbounded database calls in the GUI (use the *_controlled form):" >&2
  echo "$hits" >&2
  exit 1
fi

echo "bounded-operations: ok (every GUI database call carries an OperationControl)"
