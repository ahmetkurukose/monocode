# Browser pane self-test

The browser pane is a native web view. GPUI's test harness cannot click in
it or type into it, so what matters most is checked in a real window of the
development build, run in a world of its own (its own data directory, its own
downloads folder; never the installed app's).

Set up once per boot (the scripts expect `/tmp/mc-selftest`):

    mkdir -p /tmp/mc-selftest/proj && cp -R scripts/browser-selftest/site /tmp/mc-selftest/
    cp scripts/browser-selftest/{run.sh,agent.py,steps1.py} /tmp/mc-selftest/
    (cd /tmp/mc-selftest/proj && git init -q)
    (cd /tmp/mc-selftest/site && python3 -m http.server 8765 --bind 127.0.0.1 &)
    (cd crates/monocode-gpui && cargo build --profile native-dev)

- `run.sh` opens the test page in a browser pane and sends real mouse and key
  events through the app's own event path: a click focuses a field, typing
  reaches it, ⌘A ⌘C ⌘V work, the page gives way to a picture of itself under
  an overlay and takes the keyboard back, a download is saved, a link for a
  new window opens a page of its own. Prints a JSON report.
- `agent.py steps1.py` starts the app and drives the MCP tools the way a
  provider does (`monocode-gpui browser-mcp`): open, read, find, type, click,
  console, navigate, wait, screenshot, user agent and size.

Both set `MONOCODE_DRAW_HIDDEN=1`, which makes GPUI draw a window that is
behind others or behind a locked screen (vendor/gpui-pre-macos/src/window.rs),
so they can run unattended. The code is `shell/browser_selftest.rs`.
