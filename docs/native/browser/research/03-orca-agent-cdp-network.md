# Orca: agent browser control, CDP, element grab, network, cookies, providers, screencast

Source: `/tmp/orca-research/orca` (Electron 43, TypeScript). All paths are relative to that root.

## TL;DR: the architecture in one picture

```
agent (Claude/Codex in a terminal)
   │  `orca click --page <id> --element @e3 --json`
   ▼
orca CLI (src/cli/browser/*.ts)  ── NDJSON RPC over a Unix socket (0600) / named pipe ──►
Electron main: runtime RPC (src/main/runtime/rpc/methods/browser/browser-core.ts → runtime.browserClick)
   ▼
AgentBrowserBridge (src/main/browser/agent-browser/*)  per-page FIFO queue, effects, presence
   │  execFile(agent-browser --session orca-tab-<pageId> batch --json), stdin = [["click","@e3"]]
   │  env AGENT_BROWSER_CDP=ws://127.0.0.1:<port>/<token>
   ▼
agent-browser (vercel-labs/agent-browser ~0.43, a native binary bundled as a resource, with a per-session daemon)
   │  CDP over WebSocket
   ▼
CdpWsProxy (src/main/browser/cdp/transport/cdp-ws-proxy.ts) one per tab, loopback + token,
   fakes Target/Browser discovery, rewrites Input/screenshot/navigate/reload/focus
   ▼
webContents.debugger.sendCommand(...)   (Electron's in-process CDP, attach('1.3'), ref-counted lease)
   ▼
<webview> guest page
```

Orca does **not** implement snapshot/refs/actions itself in production. It reuses the **agent-browser** CLI
(Vercel Labs, MIT) as the "brain" (a11y snapshot, `@eN` refs, click/fill/wait/console/network/HAR), and
it presents each Electron guest to that CLI as a **fake single-target Chrome** through a per-tab CDP WebSocket proxy.
All Orca-specific value lives in the proxy (focus protection, agent cursor, screenshots that do not hang, lifecycle
fixes) and the bridge (queueing, routing, effects, dialogs, presence).

---

## 1. Agent control surface

### 1.1 Command list (CLI → RPC → bridge)

Doc: `skill-guides/orca-cli/references/browser.md` (shipped to agents as a skill). CLI specs: `src/cli/specs/browser-basic.ts`,
`browser-advanced.ts`, `browser-page-target.ts`. Handlers: `src/cli/browser/{browser-nav,browser-interact,browser-capture,browser-tab,browser-cookie,browser-storage,browser-profile,browser-extension,browser-identity}.ts`.
RPC registry: `src/main/runtime/rpc/methods/browser/browser-core.ts` (`browser.snapshot`, `browser.click`, `browser.goto`, ...).

| Group | Commands (CLI) | Bridge impl |
|---|---|---|
| Tabs | `tab list/current/create [--split right] [--headless]/switch/close/show`, `tab annotations`, `open-url` | `session-routing/agent-browser-bridge-tabs.ts`, queue `tabSwitch` |
| Navigation | `goto --url`, `back`, `forward`, `reload`, `wait --text/--url/--selector/--load load|networkidle` | `commands/agent-browser-bridge-core-commands.ts`, `-page-commands.ts`, `-interaction-commands.ts` |
| Observe | `snapshot [--interactive -i] [--delta] [--full] [--selector css] [--depth n] [--compact] [--urls]`, `screenshot [--annotate] [--if-changed]`, `full-screenshot`, `pdf`, `exec "read"` (Markdown), `get text/attr @e3`, `is visible @e3`, `find` | `agent-browser-bridge-core-commands.ts` `snapshot()`, `-capture-commands.ts` |
| Act (refs) | `click`, `dblclick`, `fill [--value-stdin]`, `type --input`, `inserttext`, `select`, `check/uncheck`, `hover`, `focus`, `clear`, `select-all`, `drag`, `upload --files`, `scroll`, `scrollintoview`, `keypress --key`, `highlight`, `download` | `-interaction-commands.ts`, `-input-commands.ts`, `-key-commands.ts` |
| Act (coords) | `mouse move/down/up/wheel/click x y` | `-pointer-commands.ts`, `-mouse-commands.ts` (direct CDP `Input.dispatchMouseEvent`) |
| JS | `eval --expression` | `-capture-commands.ts evaluate()` |
| Dialogs | `dialog accept [--text]`, `dialog dismiss` (also cancels passkey prompts) | `-page-commands.ts` |
| Emulation | `set device "iPhone 15"`, `viewport --width --height --scale --mobile`, `set geo`, `set offline`, `set headers`, `set credentials`, `set media` | `agent-browser-bridge.ts setViewport()` (direct CDP), others → agent-browser |
| Storage/cookies | `cookie get/set/delete`, `storage local|session get/set/clear`, `clipboard read/write` | `agent-browser-bridge.ts`, `-page-commands.ts` |
| Network/console | `console --limit`, `network --limit`, `capture start/stop` (HAR), `intercept enable/disable/list` | `agent-browser-bridge.ts` |
| Escape hatch | `exec --command "<any agent-browser command>"` | `agent-browser-bridge.ts exec()` with `assertAgentBrowserPassthroughStaysOnPage` |

Every page command takes `--page <browserPageId>`; optional `--expect-url <abs url>` guard (refuses with
`browser_expected_url_mismatch`, `mayHaveStarted` flag) – `src/main/browser/cdp/input/browser-expected-page-url.ts`.

Bridge → agent-browser argv mapping is thin, e.g. `src/main/browser/agent-browser/agent-browser-bridge.ts`:
```ts
async captureStart(...) { ... this.execAgentBrowser(sessionName, ['network', 'har', 'start']) }
async consoleLog(limit, ...) { const output = await this.execAgentBrowser(sessionName, ['console']); return toBrowserConsoleResult(output, limit) }
async interceptEnable(patterns) { ... ['network', 'route', patterns?.[0] ?? '**/*'] }
```

### 1.2 How element refs are produced

**Production path: agent-browser's own snapshot.** `agent-browser-bridge-core-commands.ts`:
```ts
const result = await this.execAgentBrowser(sessionName, agentBrowserSnapshotArgs(options))   // ['snapshot','-i','-c','-d',n,'-s',css,'-u','--delta','--full']
this.forgetDeltaBaseline(sessionName)
return { ...result, browserPageId, url: wc.getURL(), title: wc.getTitle(), passkeyRequest }
```
agent-browser builds the tree via CDP `Accessibility.*` + DOM resolution and mints `@e1..@eN` refs held **inside its daemon**
(per `--session orca-tab-<pageId>`). Refs are scoped to one tab, invalidated by navigation or tab switch; stale ⇒ `browser_stale_ref`.
Orca records which page minted refs (`recordRefPage`) and refuses refs from another page when `--page` is omitted (`assertRefsFromPage`).

**Legacy/unused in-house engine (good reference for a Rust port):** `src/main/browser/cdp/snapshots/snapshot-engine.ts` +
`snapshot-ax-tree-walk.ts` + `snapshot-cursor-interactive-elements.ts`, used only by `cdp/commands/cdp-bridge.ts`, which nothing
instantiates outside its own folder. Algorithm:
1. `Accessibility.enable`, `Accessibility.getFullAXTree` → walk from `nodes[0]`.
2. Skip `ignored`/`none`/`presentation`/`generic` nodes (descend into them). Emit landmarks (`[Header]`, `[Main Content]`, ...), headings, static text,
   and interactive roles (`button, link, textbox, searchbox, combobox, checkbox, radio, switch, slider, spinbutton, menuitem*, tab, option, treeitem`) with a ref `@eN`.
3. Supplement: `Runtime.evaluate` a DOM walker for `[onclick]`, `[tabindex]`, `[contenteditable]`, and `cursor:pointer` div/span/li/td/img/svg/label (max 50),
   resolve each to `backendNodeId` with `DOM.describeNode`, add as role `clickable`.
4. Cross-origin iframes: per-iframe CDP session, same walk, remember `ref → sessionId`.
5. Ref map entry: `{ backendDOMNodeId, role, name, sessionId?, nth }` (nth is used to recover stale refs by role+name).
   Output lines: `  [@e3] button "Save (2nd)"`.

### 1.3 "Effects" (why agents rarely re-snapshot)

`src/main/browser/agent-browser/observation/agent-browser-action-effects.ts` wraps each agent action:
before the action it ensures a `snapshot --delta` baseline; after it waits for a started navigation (50 ms grace, 5 s settle)
then a 100 ms DOM-quiet window (750 ms budget, polling in isolated world 1208), and returns
`effects: { url, title, newTabs, popups, downloads, consoleErrors (from guest 'console-message'), passkeyRequest, blockedPrint, page: "unchanged" | delta lines, loading, settled }`.
This is high value: cheap feedback loop without full snapshots.

### 1.4 Transport

1. **CLI → main:** `orca` CLI calls `client.call('browser.click', { element, ...target })` (`src/cli/browser/browser-interact.ts`).
   Local transport: `src/main/runtime/rpc/transports/unix-socket-transport.ts` – "NDJSON, 1-MB frames, 32 connections, Unix mode 0600 and 30s idle timeout".
   Also WebSocket and relay transports for paired phones/remote desktops (`transports/websocket`, `relay-transport.ts`).
   Caller identity is transport-proven: `src/main/runtime/rpc/callers/rpc-caller-identity.ts`
   `isAgentRpcCaller = kind==='local-cli' || (paired-device && agent)`. This identity drives "agent presence".
2. **orcad daemon:** used for headless/server hosts (`orca serve`). The daemon picks a *browser provider* (section 6) and forwards the
   same `RuntimeBrowserCommands` interface. On a desktop, RPC is served by the Electron main process directly.
3. **SSH-host CLI driving the desktop browser:** `src/main/runtime/rpc/methods/browser/desktop-browser-bridge.ts`, guarded by
   `remote-cli-browser-scope` / `remote-cli-page-admission` (revocation is re-checked before each helper spawn and every proxied CDP message).
4. **main → agent-browser:** `helper-process/agent-browser-bridge-execution.ts`:
```ts
const args = ['--session', sessionName]
// The endpoint carries the proxy token, so it goes through env: argv is visible to other users.
execOptions = { ...execOptions, envOverrides: { AGENT_BROWSER_CDP: session.cdpEndpoint } }
return this.runAgentBrowserRaw(sessionName, [...args, 'batch', '--json'], { stdinText: agentBrowserBatchInput(commandArgs) })
```
   `execFile` with 90 s timeout, 50 MB buffer; 3 consecutive timeouts ⇒ destroy session. Env isolation
   (`agent-browser-process-environment.ts`): private socket dir `/tmp/orca-ab-<sha(userData)>` (0700), `AGENT_BROWSER_IDLE_TIMEOUT_MS=600000`,
   `AGENT_BROWSER_NO_AUTO_DIALOG=1`, `AGENT_BROWSER_NO_VERSION_CHECK=1`, `AGENT_BROWSER_CONFIG=<empty json>`.
   Binary: `resources/agent-browser-<platform>-<arch>` (`agent-browser-binary.ts`), npm dep `"agent-browser": "~0.43.0"`.

### 1.5 The CDP proxy and `webContents.debugger`

`src/main/browser/cdp/transport/cdp-ws-proxy.ts` (one per tab session, created in
`session-lifecycle/agent-browser-bridge-lifecycle.ts ensureSession()`):
- `http.createServer` + `ws` on `127.0.0.1:0`; endpoint `ws://127.0.0.1:<port>/<64-hex token>`.
- `cdp-loopback-request-guard.ts`: reject any request with an `Origin` header (web pages), require Host `127.0.0.1:<port>|localhost:<port>` (DNS rebinding), timing-safe token path prefix.
- `cdp-target-discovery.ts`: answers `/json/version`, `/json/list` and `Target.getTargets/getTargetInfo/attachToTarget/attachToBrowserTarget/setDiscoverTargets/detachFromTarget`, `Browser.getVersion` **locally** with one synthetic target `orca-proxy-target` ("Chrome/<process.versions.chrome>"); synthetic session ids (`cdp-synthetic-session-registry.ts`).
- Single client at a time (new connection replaces the old and clears its state).
- Special cases in `handleClientMessage`:
  - `Page.bringToFront` / `Target.activateTarget` → ack without doing anything (do not steal the user's focus).
  - `DOM.resolveNode` → optionally first move the agent cursor to the node (`pointAtNode` via `DOM.getBoxModel`) for `check/select/focus` commands.
  - `DOM.focus` → `CdpDomFocusReplay` (focus deferred / replayed to avoid moving real focus).
  - `Page.captureScreenshot` → `cdp-page-capture-commands.ts`/`cdp-screenshot.ts`: hold paint, `webContents.invalidate()`, frame probes, emulated-DPR clip fix, fallback to `webContents.capturePage()` ("raw CDP capture can hang" for guests).
  - `Page.printToPDF` → native `webContents.printToPDF` + fake `IO.read/IO.close` stream handles.
  - `Input.*` (root session) → `cdp/input/cdp-page-input-commands.ts`: serialized input queue, agent cursor before mouse events, key events via `guest-direct-key-input.ts`, text via `guest-renderer-text-insertion.ts`, focus guard for focus-taking input.
  - `Page.navigate`/`Page.reload` → native navigation with lifecycle events re-synthesized (process swaps drop subscriptions).
  - `Target.setAutoAttach` → force `waitForDebuggerOnStart:false`.
  - everything else → `debuggerChannel.forwardCommand` → `webContents.debugger.sendCommand(method, params, sessionId)`.
- Events: `dbg.on('message', (e, method, params, sessionId))` → forwarded to the client, with Electron's empty root sessionId replaced by the synthetic one; Orca-owned frame sessions are hidden.

`webContents.debugger` usage (`cdp/debugger/electron-debugger-lease.ts`): ref-counted lease per WebContents
(`dbg.attach('1.3')` on first owner, `detach()` when the last owner releases) shared by the proxy, screencast, mouse commands, cookie ops.
`guest-cdp-command.ts sendGuestCdpCommand()` is the single choke point: refuses viewport-resizing CDP on crashed guests (segfault),
wraps all `Input.*` in `withAgentInputInjection` (`setIgnoreMenuShortcuts(true)` so agent keys never trigger app shortcuts), and tracks remote input admission.

**External tools attaching?** No. There is no `--remote-debugging-port` in production code (only in test fixtures
`src/main/browser/electron-fixtures/popup-identity-electron-probe.ts`, `.../browser-popup-compatibility-cdp-fixture.ts`).
The per-tab proxy is technically a Chrome-compatible endpoint (Playwright `connectOverCDP` or chrome-devtools-mcp would work against it),
but its URL and token are passed only to the helper via env and are never published. Extension point for agents = `orca exec "<agent-browser cmd>"`.

---

## 2. Agent presence and human/agent concurrency

Files: `src/main/browser/guest-lifecycle/page-activity/browser-agent-activity.ts`,
`src/main/browser/guest-lifecycle/manager/browser-manager-visibility.ts`,
`src/shared/browser/automation/browser-agent-activity-contract.ts`,
`src/main/browser/agent-browser/session-routing/person-pick-hold.ts`,
`src/main/browser/cdp/input/agent-input-focus-guard.ts`, `agent-input-injection.ts`,
renderer `src/renderer/src/components/browser-pane/agent-cursor/*`, `components/tab-bar/browser-tabs/BrowserTab.tsx`.

- **Marking:** every queued page command calls `browserManager.beginAgentCommand(pageId)` → `BrowserAgentActivity.beginCommand`.
  Only agent callers count (`isAgentRpcCaller`). It publishes IPC `browser:agentActivity`
  `{ browserPageId, worktreeId, active, controllerHostLabel?, agentSessionId? }`; the page stays marked while commands are queued and for
  `AGENT_PAGE_ACTIVITY_IDLE_MS = 20_000` after the last one. The tab shows "An agent is using this page"; the pane draws a glow
  (`data-agent-active`) and an animated **agent cursor** (spring motion, `agent-cursor-motion.ts`).
- **Show-before-act cursor handshake:** before each root-session `Input.dispatchMouseEvent` (or before `fill/check/select/focus` via `DOM.resolveNode`),
  main sends `browser:agentCursor { sequence, x, y, viewportWidth, viewportHeight, press, awaitsArrival }`; if the window is visible it waits
  for `browser:agentCursorArrived {sequence}` (timeout 1.5 s) before sending the real input. So the user sees where the agent clicks.
- **No hard input lock.** Instead:
  - Agent commands are serialized per page in a FIFO (`command-queue/agent-browser-bridge-queue.ts`); a person's relayed input (phone) goes through the same queue.
  - **Focus protection:** focus-taking input (`mousePressed`, `touchStart`, tap, IME) asks the pane over IPC (`browser:agentInputFocusGuard` → `...Ready`, 2 s timeout) so the renderer can restore the user's keyboard focus afterwards; if the user is mid-IME composition the input is refused (`browser_user_composing`); `Emulation.setFocusEmulationEnabled` keeps the page "focused" without real focus. `Page.bringToFront` is swallowed.
  - **Agent "lead":** pages driven by the agent are "led"; led guests refuse system dialogs (print, file chooser → `browser_file_chooser_opened`, external app launch). The desktop user's own activating input (`trackBrowserGuestInputGesture` → `!byAgent`) calls `releaseAgentLead` (`browser-manager-guest-popup-policy.ts`).
  - **Tab routing:** without `--page`, a worktree's "current" tab stays the agent's for 20 s even if the user clicks another tab (`personPickWaits`); the user's pick applies once the agent idles.
  - JS dialogs: never auto-accepted; commands fail with `browser_dialog_opened` and the agent must `dialog accept/dismiss`.
  - Paint holds: `holdPaintForCapture` lifts renderer throttling so a hidden/minimized pane still produces frames for screenshots.

---

## 3. Element grab (user picks an element → agent)

Files: `src/main/browser/element-grab/*`, `src/shared/browser/element-grab/browser-grab-types.ts`,
renderer `components/browser-pane/annotate/grab/*`, `annotate/delivery/*`, `src/shared/browser/annotations/browser-annotation-output.ts`.

- **Injection:** `webContents.executeJavaScript(buildGuestOverlayScript('arm'|'awaitClick'|'finalize'|'extractHover'|'teardown'))`.
  The arm script concatenates foundation (budgets, `clampStr`, secret check, URL sanitizer), composed-tree helpers (shadow DOM aware),
  content (selected text, HTML snippet, safe attributes, a11y name/role incl. `aria-labelledby`), element context (selector, paths, nearby text/elements, computed styles),
  React (fiber `__reactFiber$*` → component names + `_debugSource` file), overlay.
- **Overlay:** full-viewport fixed host `#__orca-grab-host` (z-index 2147483647, `cursor:crosshair`, `pointer-events:all`) with a **closed shadow root**; the
  highlight lives in a `popover="manual"` element so it paints in the top layer above modal dialogs. Hover label: `tag role="..." "text" WxH`.
  Left click = select and copy; right click = context menu (`__orcaContextMenu`). Escape = cancel. 120 s timeout; main-frame navigation or guest destroy cancels
  (`browser-grab-session-controller.ts`).
- **Selector:** `buildSelector` uses id / stable classes (filters hash-looking classes, `looksHashy`) / `:nth-of-type`, growing ancestors until `querySelectorAll(sel).length===1`, max 700 chars.
- **Payload (`BrowserGrabPayload`)**, re-validated in main by `clampGrabPayload()` (`browser-grab-payload.ts`):
  `page { sanitizedUrl (query+hash stripped), title, viewport, scroll, dpr, capturedAt }`,
  `target { tagName, selector, elementPath, fullPath, cssClasses, nearbyElements[≤6], selectedText, isFixed, reactComponents, sourceFile, textSnippet(200), htmlSnippet(4096), attributes (allowlist id,class,name,type,role,href,src,alt,title,placeholder,for,action,method + aria-*; secrets → "[redacted]"), accessibility{role, accessibleName, ariaLabel, ariaLabelledBy}, rectViewport, rectPage, computedStyles{display, position, width, height, margin, padding, color, backgroundColor, border, borderRadius, fontFamily, fontSize, fontWeight, lineHeight, textAlign, zIndex} }`,
  `nearbyText[≤10]`, `ancestorPath[≤10]`, `screenshot`.
  Secret patterns: `access_token, auth_token, api_key, apikey, client_secret, oauth_state, x-amz-, session_id, sessionid, csrf, secret, password, passwd`.
- **Screenshot crop** (`browser-grab-screenshot.ts`): hide overlay → `guest.capturePage()` → scale = bitmapWidth / `window.innerWidth` → `image.crop(rect*scale)` → PNG data URL, dropped if >2 MB.
- **Delivery:** formatted as text/Markdown (`GrabConfirmationSheet.tsx formatGrabPayloadAsText`: "Attached browser context from <url> / Selected element / Selector / Source / React / Dimensions / Text / Nearby / Computed styles / HTML / Ancestor path"). Multiple grabs become page **annotations** (≤20 per page, comment ≤2000 chars) which the user copies or sends to a chosen agent session (`use-browser-page-annotation-send.ts` → agent send popover, with image paths for crops). Agents can also pull unsent notes: `orca tab annotations --page <id>`.

---

## 4. Network and console capture

- **Agent-facing logs are owned by agent-browser's daemon**, which holds a persistent CDP connection to the proxy and buffers `Runtime.consoleAPICalled`/`Log.entryAdded` and `Network.*` events. Orca just reads them:
  `console` → `{messages:[{type,text}]}`; `network requests` → `{requests:[{url,method,status,mimeType,timestamp}]}`;
  Orca keeps the last `--limit` entries (`observation/agent-browser-log-entries.ts keepLast`). No bodies/headers in the typed result.
- **HAR:** `capture start/stop` → agent-browser `network har start|stop`. `session.activeCapture` is tracked, and `holdsPageNetworkState()` prevents Orca from discarding/freeing the page while HAR capture or routes are active.
- **Interception:** `network route <pattern>` / `unroute`; patterns are restored automatically after a process swap or daemon idle-out (`pendingInterceptRestore`). Per-request continue/block is a TODO.
- **Per-action console errors:** captured live by Orca from `guest.on('console-message')` (level `error`, first 5, clipped) into `effects.consoleErrors`.
- `src/main/browser/network/` is **not** capture. It is **egress routing**: makes a browser tab's traffic exit from an SSH host, WSL distro or a paired runtime.
  The tab's Electron session gets `session.setProxy({ proxyRules: 'socks5://127.0.0.1:<port>' })` (`routing/sessions/browser-route-session-policy.ts`);
  a local SOCKS server multiplexes TCP streams over a framed tunnel (`shared/browser/network/tunnels/browser-network-tunnel-protocol.ts`: OPEN/DATA ≤64 KiB/WINDOW_UPDATE ≤16 MiB credit flow control, outbound memory budget)
  to the remote side, which dials via `ssh` SOCKS (`system-ssh-socks-client-socket.ts`), WSL relay or the runtime. Destination policy blocks cloud metadata IPs and Orca's own listeners
  (`browser-network-tunnel-destination-policy.ts`); WebRTC is forced to `disable_non_proxied_udp` so it cannot leak.

---

## 5. Cookies

Files: `src/main/browser/cookies/**`.

- **Sources** (`sources/browser-cookie-source-profiles.ts`, `browser-cookie-detection.ts`): Chromium family (Chrome, Edge, Arc, Brave, Comet, Helium, Vivaldi, ... with mac/win/linux roots and Keychain service names like `Chrome Safe Storage`), Firefox (`cookies.sqlite` in profiles), Safari (`Cookies.binarycookies`, `browser-cookie-safari-parser.ts`, mac epoch +978307200), and JSON file import (`imports/import-cookie-file.ts`, array of cookie objects). Profiles per browser are listed for the user to choose.
- **Key retrieval** (`sources/browser-cookie-key.ts`):
  - macOS: `security find-generic-password -s "<Browser> Safe Storage" -a "<Browser>" -w` (async, 30 s, may show a Keychain prompt) → `PBKDF2-SHA1(pw, "saltysalt", 1003, 16)` → AES-128-CBC, IV = 16 spaces, prefix `v10`.
  - Linux: v10 key = PBKDF2("peanuts", saltysalt, 1); v11 = PBKDF2(`secret-tool lookup ...` password, saltysalt, 1).
  - Windows: `Local State` → `os_crypt.encrypted_key` (strip "DPAPI") → PowerShell `ProtectedData.Unprotect` (data over stdin) → AES-256-GCM (12-byte nonce, 16-byte tag).
- **Decryption** (`browser-cookie-decryption.ts`): version prefix `v10/v11/v20`; `v20` (Chrome App-Bound Encryption) is reported as undecryptable; after decrypt strip the 32-byte `SHA256(host_key)` prefix (schema ≥24). Warnings classify failures (app-bound, keyring unavailable).
- **Pipeline** (`chromium/import-cookies.ts`): consistent copy of `Cookies` + `-wal`/`-shm` into a temp dir (retry until file state is stable, `chromium-cookie-snapshot.ts`) → read with `node:sqlite` → skip Google source-bound cookies (SIDCC/STRP/AEC) and non-transplantable domains → decrypt → derive URL → `session.fromPartition(target).cookies.set` (via a cookie clear-store with CDP-style scope keys) → `flushStore()`. Serialized per session (`mutation/cookie-mutation-lock.ts`). CHIPS partition keys are preserved where readable.
- **Staging** (`staging/*`): an import for a profile whose session is not live is written as a staged SQLite DB and merged into the partition's `Cookies` file on next start (scoped by imported domains).
- **Session cookie persistence** (`session-restore/*`): Electron never writes expiry-less cookies to disk, so Orca snapshots session cookies per partition to a file and restores them on launch (Chrome-like "continue where you left off").
- **Per-profile:** each browser profile is an Electron partition (`persist:orca-browser`, `persist:orca-browser-...`), managed by `sessions/profiles/browser-session-registry`. No continuous sync with the real browser: import is one-shot (repeatable). Google cookie clear (`browser-google-cookie-clear.ts`) is a helper to fix Google sign-in loops.
- Agent cookie commands go through agent-browser (`cookies get`, `cookies set`), but `delete` uses Orca's own CDP code (`browser-cookie-delete-by-name.ts`) because agent-browser's `cookies clear` wipes the whole jar.

---

## 6. Browser providers in orcad

`src/main/orcad/browser-providers/orcad-browser-provider.ts` `resolveOrcadBrowserProvider()`, in order:
1. **`electron`**: if an installed Orca app exists (`/Applications/Orca.app/...`, `%LOCALAPPDATA%\Programs\Orca\Orca.exe`, `~/.local/bin/orca-ide`, ...),
   spawn it as a headless **sidecar**: `Orca --serve --serve-port <reserved loopback port> --serve-json --serve-no-pairing --user-data-dir=<isolated profile>` with headless Ozone switches on Linux
   (`electron/electron-serve-browser-process.ts`). orcad waits for runtime metadata + `status.get` advertising `browser.headless.v1`, then forwards browser RPCs to the sidecar (`orcad-sidecar-runtime-client.ts`, `electron-sidecar-method-routing.ts`). Inside the sidecar the same AgentBrowserBridge + CDP proxy stack runs on offscreen guests.
2. **`chromium`**: if `ORCA_BROWSER_EXECUTABLE` is set and the agent-browser binary is present, agent-browser itself launches and owns that Chrome/Chromium
   (`chromium/external-chromium-browser-session.ts`, env `AGENT_BROWSER_EXECUTABLE_PATH`, `AGENT_BROWSER_PROFILE`, `AGENT_BROWSER_SESSION=orca-orcad-<hash>`, `AGENT_BROWSER_IDLE_TIMEOUT_MS=0`, `AGENT_BROWSER_ARGS`). Tabs are agent-browser tabs (`agent-browser tab <id>`), mapped to Orca page ids by `external-chromium-tab-registry.ts`; commands are serialized through one queue; profile commands refuse.
3. Otherwise unavailable with a cause (`unconfigured`, `driver_missing`, `executable_not_found`, `electron_start_failed`, `chromium_start_failed`).

No cloud browsers (no Browserbase/Steel/Hyperbrowser code). "Client-hosted" vs "server-hosted" pages are a placement concept: a client-hosted page renders in the paired desktop's Electron (needs it online, `browser_host_unavailable` otherwise); server-hosted pages run in the provider above (e2e: `tests/e2e/browser/client-hosted/*`, `remote-hosted/*`).

---

## 7. Mobile / remote screencast

Main side `src/main/browser/cdp/screencast/*`, RPC `src/main/runtime/rpc/methods/browser/browser-screencast.ts`, client `mobile/src/browser/**`.

- **Capture:** CDP `Page.startScreencast { format, quality, maxWidth, maxHeight, everyNthFrame }` on the guest via the debugger lease; `Page.screencastFrame` events (base64 JPEG + metadata).
  Optional viewport emulation (`Emulation.setDeviceMetricsOverride` + touch + `setVisibleSize`) when the phone asks for "mobile view"; clamped to the guest view size for embedder-sized guests.
  A snapshot frame is emitted on start, navigation, and viewport changes (`browser-screencast-snapshot-capture.ts`); 10 s no-frame timeout; JS dialogs are pushed as stream events.
- **Pacing / back-pressure:** `browser-screencast-frame-pacer.ts` drops to the latest frame within `minFrameIntervalMs` and **delays `Page.screencastFrameAck`** for throttled frames, which throttles Chromium's encoder. Viewer budgets are merged (min quality/size, max interval) when several clients watch (`browser-screencast-frame-budget.ts`). End-to-end credit: client sends cumulative `browser.screencast.ack {subscriptionId, seq}` (`browser-screencast-frame-credit`).
- **Defaults from phone** (`mobile/src/browser/screencast/browser-screencast-request-parameters.ts`): JPEG q=72, everyNthFrame=1, min interval 100 ms (≈10 fps), frame size = pane size × min(devicePixelRatio, 2.5) clamped to 320×240..2400×2160; mobile view uses DSF 2 and `mobile:true`.
- **Wire format** (`mobile/src/browser/screencast/browser-screencast-protocol.ts`, mirrored in `src/shared/browser/screencast/`): binary message, 16-byte header
  `[0]=0x62 kind, [1]=version 1, [2]=opcode 1 (Frame), [3]=format (1 jpeg, 2 png), [4..8]=seq u32 LE, [8..12]=metadata JSON length u32 LE, [12..16]=0`, then metadata JSON
  (`offsetTop, pageScaleFactor, deviceWidth, deviceHeight, imageWidth, imageHeight, scrollOffsetX, scrollOffsetY, timestamp, zoomFactor`), then raw image bytes. Sent via `sendBinary` on the streaming RPC (WebSocket / E2EE relay).
- **Input mapping** (`mobile/src/browser/page-interaction/browser-touch-geometry.ts`): letterbox the frame into the pane (`scale = min(w/deviceWidth, h/deviceHeight)`, centered offsets), undo pinch-zoom state, then divide by `pageScaleFactor*zoomFactor` to get **viewport CSS px**
  (no scroll offset added: CDP input is viewport-relative). Gestures become RPCs: `browser.mouseClick {x,y,radius,modifiers}` (tap; radius used for touch hit-target adjustment `resolveMobileTouchClickPoint`),
  `mouseMove/mouseDown/mouseUp` (drag), `mouseWheel` (coalesced wheel queue, `use-mobile-browser-wheel-queue.ts`), `keyboardInsertText`, `keypress`, `goto/back/forward/reload`, `dialogAccept/Dismiss`, `certificate.proceed`.
  These run through the same per-page queue as agent commands but are credited as a *person* (not agent) so they don't mark agent presence. Main dispatches them as `Input.dispatchMouseEvent` via `sendGuestCdpCommand`.

---

## 8. Other in-scope modules (brief)

- `src/main/browser/document-preview/*`: privileged custom scheme (`protocol.registerSchemesAsPrivileged`, standard+secure+fetch+stream) serving local/remote files (PDF, HTML, media with Range) into an isolated partition using **grant ids** in the URL (`doc-preview-grant-registry.ts`), with a strict CSP and network blocking so previewed files cannot be exfiltrated. Grab payloads replace the grant URL with the file path.
- `src/main/browser/dictation/browser-guest-dictation-text.ts`: inserts dictated text into the focused field of the focused frame via `frame.executeJavaScript` (pierces open shadow roots, adds spacing from caret context, `beforeinput` for rich editors, `document.execCommand('insertText')`).
- `tests/e2e/browser/*`: Playwright-on-Electron specs for agent focus isolation (`browser-agent-input-focus-isolation.spec.ts`), grab over top-layer elements, split-pane grab shortcut, guest crash recovery, client-hosted/remote-hosted pages, FedCM fallback, ghost subscriber rejoin.

---

## 9. What to replicate in a native Rust app (priority order)

1. **One CDP endpoint per page, owned by the app.** In Rust you will likely embed CEF/WebView2/WKWebView or drive a real Chromium. With Chromium/CEF, CDP is available natively (CEF `SendDevToolsMessage` or `--remote-debugging-port`/pipe). Put a **filtering proxy** in front of it, like `CdpWsProxy`: loopback only, random token path, reject `Origin` header, fake `/json/version` + `/json/list` + `Target.*` for a single page target, swallow `Page.bringToFront`, and route `Input.*` through your own focus-protection layer. This lets you reuse agent-browser, Playwright `connectOverCDP` or chrome-devtools-mcp unchanged.
2. **Reuse agent-browser as the automation brain** (it is a native binary; spawn with `--session <page>` + `AGENT_BROWSER_CDP=<endpoint>` in env, feed `batch --json` on stdin, isolate config/socket dir/idle timeout/no auto-dialog). Or port the small AX-tree snapshot (`snapshot-engine.ts`) to Rust: `Accessibility.getFullAXTree` → role filter → `@eN` → `backendDOMNodeId` map, plus a cursor:pointer DOM sweep and per-iframe sessions.
3. **Stable agent RPC:** CLI (or MCP server) → app over a 0600 Unix socket, NDJSON, transport-proven caller identity (agent vs person). Typed commands with `--page` targeting, `--expect-url` guard, stable error codes (`browser_stale_ref`, `browser_dialog_opened`, `browser_file_chooser_opened`, `browser_focus_guard_unavailable`, ...).
4. **Per-page FIFO command queue + action "effects"** (settle: navigation wait then DOM-quiet; return url/title/new tabs/popups/downloads/console errors/snapshot delta).
5. **Presence UX:** activity events with a 20 s idle window, tab badge, glow, and a show-before-act cursor handshake (main waits for the UI's "arrived" ack, ≤1.5 s, before injecting a click). Protect user focus (never call bringToFront; restore focus after agent presses; refuse during IME composition); release the agent's "lead" on the user's own input; block OS dialogs (file chooser, print) for agent-led pages.
6. **Element grab:** injected overlay in a closed shadow root + top-layer popover, unique-selector builder, allowlisted attributes with secret redaction, computed-style subset, React fiber source, 2 MB PNG crop; re-validate in the host; format as Markdown and deliver to the selected agent session.
7. **Screencast for remote viewers:** `Page.startScreencast` + delayed acks for back-pressure + the 16-byte binary frame header + viewport-CSS-px input mapping.
8. **Cookie import:** Keychain/libsecret/DPAPI key → AES-CBC/GCM decrypt of a WAL-consistent copy of `Cookies` → strip SHA256(host) prefix → set into the profile's store; persist session cookies yourself if your engine does not.
