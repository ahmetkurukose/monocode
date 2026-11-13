#!/bin/bash
# Runs the development build's browser self-test in a world of its own.
rm -f /tmp/mc-selftest/result.json; rm -rf /tmp/mc-selftest/data /tmp/mc-selftest/downloads; mkdir -p /tmp/mc-selftest/data /tmp/mc-selftest/downloads
MONOCODE_DRAW_HIDDEN=1 MONOCODE_BROWSER_DOWNLOADS=/tmp/mc-selftest/downloads MONOCODE_NATIVE_DATA_DIR=/tmp/mc-selftest/data \
MONOCODE_BROWSER_SELFTEST=/tmp/mc-selftest/result.json \
MONOCODE_BROWSER_SELFTEST_URL="${1:-http://127.0.0.1:8765/}" \
  /Users/ahmet/Dev/monocode/crates/monocode-gpui/target/native-dev/monocode-gpui --dev-data --cwd /tmp/mc-selftest/proj >/tmp/mc-selftest/app.log 2>&1 &
pid=$!
for i in $(seq 1 60); do sleep 1; [ -f /tmp/mc-selftest/result.json ] && break; kill -0 $pid 2>/dev/null || break; done
sleep 1; kill $pid 2>/dev/null
cat /tmp/mc-selftest/result.json 2>/dev/null || { echo "NO RESULT"; tail -20 /tmp/mc-selftest/app.log; }
