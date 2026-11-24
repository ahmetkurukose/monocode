---
name: browser-extension
description: Add or fix a Chrome Web Store extension in Monocode's native browser (our own extension layer, K69): inventory the chrome.* APIs it uses, fill the gaps in the shim and dispatcher, verify it in the real app, hand the account-bound checks to the user. Use when asked to support, port, debug or "make work" an extension (Bitwarden, 1Password, uBlock, React DevTools, …) in the native app's browser.
---

# Adding an extension to Monocode's browser

Monocode does not use Chromium's extension system. It runs Manifest V3
extensions on its own layer (decision K69 in
`docs/native/browser/context.md`; Proton Pass was the first, phase 5a). An
extension works when every `chrome.*` call it makes is answered the way
Chrome answers it. Most of the work is finding the calls the layer does not
answer yet and adding them generically, so the next extension gets them free.

Read K69 and §11 "Eklentiler" in `docs/native/browser/context.md` before
starting; they hold the current limits.

## Where things are

| Piece | File |
|---|---|
| `chrome.*` for every context (pages, background worker, content scripts, externally connectable pages) | `crates/monocode-core/src/browser/extensions/shim.js` |
| Main world of externally connectable sites | `crates/monocode-core/src/browser/extensions/external.js` |
| Permission each namespace needs, unsupported permissions, Chrome's error texts | `crates/monocode-core/src/browser/extensions/api.rs` |
| Callback test over every method (QuickJS) | `crates/monocode-core/src/browser/extensions/shim_tests.rs` |
| Manifest (+ i18n), match patterns, web_accessible_resources | `manifest.rs`, `pattern.rs`, `access.rs` |
| CRX3 verify, unpack, install/update/remove, Web Store/Omaha | `crx.rs`, `zip.rs`, `registry.rs`, `webstore.rs`, `permissions.rs` |
| `chrome.storage` (SQLite, Chrome's quotas), `tabs.Tab`/`tabs.query` | `storage.rs`, `tabs.rs` |
| Answers to calls (main thread, our tab model) | `crates/monocode-gpui/src/browser/extensions/dispatch.rs` |
| Instances, contexts, events (`broadcast`), ports, alarms, background host | `crates/monocode-gpui/src/browser/extensions/instance.rs` |
| `mcext://` serving, bridge (`__mcext/api`, `/events`, `/fetch`) | `crates/monocode-gpui/src/browser/extensions/serve.rs` |
| Content scripts over CDP (isolated + MAIN world, OOPIFs) | `crates/monocode-gpui/src/browser/extensions/pages.rs` |
| Toolbar button, badge, popup window | `toolbar.rs`, `crates/monocode-cef/src/mac/ext_window.rs` |
| Scheme, fetch proxy (`CefURLRequest`) | `crates/monocode-cef/src/extensions.rs` |
| Settings → Extensions | `crates/monocode-gpui/src/settings/pages/extensions.rs` |
| Dev scripts | `scripts/browser-extensions/` (`run.sh`, `cdp.mjs`, `mcp.mjs`) |

## Rules

- Build and test only in the session's own worktree (K67). Agents in other
  worktrees may edit but never build; cargo duplicates every workspace crate
  per source path and fills the disk.
- `cargo nextest run`, never `cargo test` for the GPUI bin. Never
  `cargo fmt`: `rustfmt` the changed files only (core: `--edition 2021
  --style-edition 2024`; gpui/cef: `--edition 2024`).
- The user's own Monocode apps may be running: stop only the PIDs you
  started, never `pkill -f`/`killall` on "Monocode".
- Never use or ask for the user's accounts. You verify up to sign-in; the
  user does the signed-in checks.
- Fix things in the generic layer. Per-extension code is the last resort,
  and then keyed by extension id with a comment saying why.

## Workflow

### 1. Install it and take its inventory

Build the debug bundle and run it on a throwaway data folder:

```sh
export CARGO_TARGET_DIR=$PWD/crates/monocode-gpui/target CEF_PATH=$HOME/Library/Caches/Monocode/cef-sdk/156.0.31
(cd crates/monocode-gpui && cargo build --locked -p monocode-gpui --bin monocode-gpui \
  -p monocode-cef --bin monocode-cef-helper -p monocode-core --bin monocode-pty-exec)
bash scripts/bundle-native-macos.sh debug crates/monocode-gpui/target 0.16.0 dev-data
ROOT_DIR=/tmp/ext-<name> scripts/browser-extensions/run.sh https://example.com/
echo '{"install":"<web store id or url>"}' > /tmp/ext-<name>/dev/cmd
cat /tmp/ext-<name>/dev/out.jsonl
```

The unpacked extension lands in
`/tmp/ext-<name>/data/Browser Extensions/<id>/<version>/`. From there:

- `manifest.json`: permissions, optional_permissions, host_permissions,
  content_scripts (note `world: "MAIN"`, `all_frames`, `run_at`,
  `match_about_blank`), web_accessible_resources, background,
  action/popup, options_ui, externally_connectable, commands, CSP,
  `side_panel`, `declarative_net_request`.
- API calls:
  `rg -o --no-filename '\b(chrome|browser)\.[a-zA-Z]+\.[a-zA-Z.]+' -g '*.js' | sort | uniq -c | sort -rn`.
  Minified code also hides calls behind aliases (`const t = chrome.tabs`):
  look for `chrome[`, the webextension polyfill and wrapper modules.

Write the inventory down (namespace → methods/events), then compare it
with `shim.js`. A method that is missing, or present as `unsupported(...)`,
is a gap; so is a permission listed in `api.rs` `UNSUPPORTED_PERMISSIONS`.

### 2. Fill the gaps

For each missing call:

1. **Shim** (`shim.js`): add it to its namespace through the adapter,
   never as a bare function:
   - `remote("ns.method", prep)` when the app answers it (most calls);
   - `asyncMethod(fn)` when the shim answers it locally;
   - `unsupported("ns.method")` when it cannot run here (fails the Chrome
     way, with `runtime.lastError`);
   - events with `event("ns.onThing")`.
   A synchronous Chrome method (returns a value, no callback) goes in the
   `SYNC` list in `shim_tests.rs`.
2. **Permission** (`api.rs` `required_permission`): a new namespace that
   needs a manifest permission is mapped here; the dispatcher refuses the
   call without it.
3. **Answer** (`dispatch.rs`): a match arm with Chrome's semantics and
   Chrome's error texts (the helpers in `api.rs`). Respect the caller's
   kind: content scripts and externally connectable pages get only part of
   the API, as in Chrome.
4. **Events**: fire them with `instance::broadcast(key, "ns.onThing",
   json!([...]), filter)` from where the app learns of the change (tab
   model, navigation, network log).
5. **Tests**: `shim_tests.rs` walks every method of every context and fails
   if one doesn't call its callback exactly once; add unit tests for the
   new semantics in core and a dispatcher test in gpui where it has logic.

Bigger pieces not built yet (check §11 first; one may exist by now):
`declarativeNetRequest` (ad blockers), `sidePanel`, `contextMenus`,
`notifications`, `identity`, blocking `webRequest`, XHR through the fetch
proxy, real service worker lifecycle. Build such a piece as its own
generic module with tests, and document it in K69.

If it is a password manager, add its id to `PASSWORD_MANAGERS` in
`crates/monocode-core/src/browser/extensions/mod.rs`. Monocode's own
password manager then stays quiet while it is enabled (K60).

### 3. Verify in the real app

Drive the app with the dev hook (`MONOCODE_BROWSER_EXT_DEV`, set by
`run.sh`). Commands go to `$ROOT_DIR/dev/cmd`; results go to `out.jsonl`,
state to `state.json` (instances, contexts, tabs, action state):

- `{"install": id}`, `{"enable": id, "on": false}`, `{"uninstall": id}`,
  `{"updates": true}`
- `{"popup": id}` opens the popup
- `{"eval": "<js>", "id": id, "in": "background"|"popup"}` runs code in an
  extension context. Some extensions hide `chrome` from foreign code;
  `__mcextChrome` reaches it in dev runs.
- `{"crash": id}` kills the background renderer.

For pages, use `node scripts/browser-extensions/cdp.mjs $ROOT_DIR/agent.json
<url-part> '<js>' [shot.png]`. For agent tools,
`node scripts/browser-extensions/mcp.mjs $ROOT_DIR/agent.json <tool> '<json>'`.
The app's log is `$ROOT_DIR/data/Logs/gpui.log`.

Checklist (what applies):

- [ ] Installs from the Web Store id; the CRX3 verifies; update check runs.
- [ ] Background starts without errors (check `state.json`, the log, and
      `eval` in the background).
- [ ] Content scripts inject on matching pages, in iframes and
      cross-origin iframes when `all_frames`; MAIN-world scripts too.
- [ ] Messages and ports round-trip between content, background, popup.
- [ ] Popup opens under the toolbar button and renders; it closes on
      blur/Esc/`window.close()`.
- [ ] `tabs.query({active:true,currentWindow:true})` returns the page the
      user is on, after switching tabs and groups.
- [ ] Its in-page iframes (web_accessible_resources) render and are
      clickable; non-WAR files are refused to web pages.
- [ ] Sign-in pages open (in a Monocode tab); `externally_connectable`
      sites reach it.
- [ ] Survives a background crash, disable/enable, app restart (storage
      kept); uninstall removes files and profile data.
- [ ] Agent tools still mask passwords (K57); agent tabs of a throwaway
      profile (K22) run no extensions.
- [ ] With a MAIN-world WebAuthn override, our passkey watcher (K58) is
      still installed first.

If the screen is locked, GPUI doesn't draw and no page views exist. Agent
tabs (`browser_tab_open`) still create views; use them.

### 4. Hand over to the user

List what you could not check without an account (sign-in, vault or data
sync, autofill, saving) as numbered steps on the debug bundle with
`run.sh`. Wait for their result before calling the extension supported.

### 5. Record it

- `docs/native/browser/context.md`: the extension and what was added, in
  the K69 notes, plus new limits in §11 "Eklentiler". Keep it short.
- This skill: add a pitfall you hit that isn't listed below.

## Pitfalls (from Proton Pass)

- **Callbacks.** The webextension polyfill passes a callback to every call.
  A method that ignores it hangs the extension's startup with no error.
  Always use the adapter; the shim test catches the rest.
- **No receiver.** When nothing answers a message, the callback form sets
  `lastError` to "The message port closed before a response was received."
  The promise form resolves `undefined`.
- **Context kind.** A context comes to life on its first poll; settle its
  kind and place when `hello` arrives, or an iframe counts as a tab.
- **`lastFocusedWindow`.** It always has a value in Chrome. With no focused
  window (background, locked screen), fall back to the first window, or
  `tabs.query` returns nothing.
- **Bindings.** Name them per extension (`__monocodeExt_<id>`) and keep the
  `__monocode` prefix, which hides them from external CDP clients. Expose
  them only in the extension's isolated world.
- **OOPIFs.** Auto-attach with an iframe-only filter and
  `waitForDebuggerOnStart`, add the scripts, then `runIfWaitingForDebugger`.
- **`about:blank`.** Don't load the shim into `about:blank` documents unless
  an entry asks for them (`match_about_blank`).
- **Content script scope.** Each file runs in its own function scope;
  top-level `var`s aren't shared across files like in Chrome. If an
  extension relies on that, this is the place to change.
- **Custom scheme.** OPFS fails on `mcext://` (Proton falls back to
  IndexedDB). CORS ignores CEF's allow list for it: cross-origin fetches go
  through the app's fetch proxy (not XHR; `redirect: manual` isn't opaque).
- **Background.** A dedicated worker in a hidden page, not a service
  worker: SW-only APIs are missing. `onInstalled`/`onStartup` reach
  listeners added up to 15 s late, because extensions register after async
  setup.
- **CEF.** Fields in `wrap_*` macros must be `Clone` and can't have doc
  comments. A CEF-owned hidden window's `do_close` must return 0, or quit
  hangs.
- **Passkeys.** Our WebAuthn watcher must be added before an extension's
  MAIN-world script, so a declined request still falls through to
  Chromium's phone QR.
