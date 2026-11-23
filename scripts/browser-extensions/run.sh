#!/bin/bash
# Runs the debug bundle against a throwaway data folder with the extension
# layer's development hooks on (browser/extensions/mod.rs `dev`), opened
# through LaunchServices so its window comes to the front:
#   scripts/browser-extensions/run.sh [url]     (data in ${ROOT_DIR:-/tmp/p5a})
#   echo '{"install":"ghmbeldphafepmbegfdlkpapadhbakde"}' > $ROOT_DIR/dev/cmd
#   cat $ROOT_DIR/dev/state.json $ROOT_DIR/dev/out.jsonl
# The app's log goes to $ROOT_DIR/data/Logs/gpui.log, never the user's.
# Its pid goes to $ROOT_DIR/app.pid; stop it with `kill $(cat …/app.pid)`.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BUNDLE="$REPO/crates/monocode-gpui/target/debug/Monocode Native.app"
ROOT_DIR="${ROOT_DIR:-/tmp/p5a}"
mkdir -p "$ROOT_DIR/data" "$ROOT_DIR/dev" "$ROOT_DIR/proj"
open -n -a "$BUNDLE" \
  --env MONOCODE_NATIVE_DATA_DIR="$ROOT_DIR/data" \
  --env MONOCODE_BROWSER_EXT_DEV="$ROOT_DIR/dev" \
  --env MONOCODE_BROWSER_OPEN="${1:-http://127.0.0.1:18765/login.html}" \
  --env MONOCODE_BROWSER_AGENT_DEV="$ROOT_DIR/agent.json" \
  --stdout "$ROOT_DIR/app.log" --stderr "$ROOT_DIR/app.log" \
  --args --dev-data --cwd "$ROOT_DIR/proj"
# The new process: the one whose data folder is ours.
for _ in $(seq 1 50); do
  for pid in $(pgrep -f "Monocode Native.app/Contents/MacOS/monocode-gpui" || true); do
    if ps -E -p "$pid" -o command= 2>/dev/null | rg -q "MONOCODE_NATIVE_DATA_DIR=$ROOT_DIR/data"; then
      echo "$pid" > "$ROOT_DIR/app.pid"; echo "pid $pid"; exit 0
    fi
  done
  sleep 0.2
done
echo "could not find the app's pid" >&2
