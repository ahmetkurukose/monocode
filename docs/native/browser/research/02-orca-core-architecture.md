# Orca browser: core embedding and lifecycle architecture

Source: `/tmp/orca-research/orca` (Electron 43, TypeScript). All paths below are relative to `src/`.
Goal: understand how Orca hosts and manages its built-in browser well enough to port the design to a native Rust + GPUI app.

---

## 0. One-paragraph summary

Orca has three hosting modes for one logical "browser page":

1. **Desktop guest**: an Electron `<webview>` tag inside the React renderer DOM. This is the main path.
2. **Offscreen backend**: a hidden `BrowserWindow({offscreen:true})` per page, used by the headless `orca serve` runtime, which has no renderer.
3. **Remote/streamed**: the page runs on another Orca runtime (server/SSH host/desktop). The viewer gets CDP `Page.startScreencast` JPEG frames in a small binary envelope, and sends input back as RPC calls (`browser.mouseMove/Down/Up/Wheel`, `browser.keypress`, `browser.keyboardInsertText`). A variant called **client-hosting** inverts this: a remote runtime asks the *desktop* to host the page locally in a `<webview>`, and the page's network egress is tunnelled back to the execution host.

The **main process owns authority**: which partitions are allowed, guest policy (popups, navigation, shortcuts, context menu, dialogs), memory/discard, metadata publication. The **renderer owns the tab model and the layout**. Pages are keyed by a stable `browserPageId` that outlives any one `WebContents`. Guests get created lazily, kept alive across UI remounts, discarded when idle (after history is snapshotted in memory), and restored under the same id via `navigationHistory.restore`.

---

## 1. Hosting primitive, positioning, z-order, occlusion

### 1.1 Primitive: `<webview>` (desktop), offscreen `BrowserWindow` (headless), `WebContentsView` only for side surfaces

- The main window enables `webviewTag: true` (`main/window/createMainWindow.ts:165`).
- Creation happens in the renderer: `renderer/src/components/browser-pane/host-guest/lifecycle/browser-page-webview.ts`:

```ts
webview = document.createElement('webview') as Electron.WebviewTag
webview.setAttribute('partition', webviewPartition)
webview.setAttribute('allowpopups', '')
webview.setAttribute('webpreferences', ORCA_BROWSER_PAGE_GUEST_WEB_PREFERENCES_ATTRIBUTE) // disableHtmlFullscreenWindowResize=true,transparent=false
webview.style.background = 'var(--background)'
guest.addEventListener('load-commit', (e) => { if (e.isMainFrame)
  guest.style.visibility = e.url === 'about:blank' || e.url === ORCA_BROWSER_BLANK_URL ? 'hidden' : 'visible' })
guest.addEventListener('render-process-gone', () => { guest.style.visibility = 'hidden' })
registerPersistentWebview(browserTabId, webview)
activeContainer.appendChild(webview)
```

  A blank guest is hidden so the React "New Tab" page under it stays visible and clickable.
- `WebContentsView` is used only where a `<webview>` cannot work:
  - Docked DevTools: `main/browser/guest-lifecycle/devtools/browser-guest-devtools-dock.ts`. A `<webview>` can't host the DevTools frontend on Electron 43. The renderer measures the DevTools slot and sends `browser:setDevToolsDockBounds`, and main calls `view.setBounds()`. This is the classic native-overlay problem.
  - Popup windows that show an origin bar: `main/browser/guest-lifecycle/popups/presentation/popup-origin-bar-window.ts`. This is a `BaseWindow` with two `WebContentsView`s: an origin bar on top and the page below.
- Headless: `main/browser/backends/offscreen-browser-backend.ts` creates `new BrowserWindow({show:false, webPreferences:{offscreen:true, partition, sandbox:true, ...}})`, then immediately calls `win.webContents.stopPainting()`. Nothing reads paint frames there. Captures and screencasts read from the compositor through CDP.

### 1.2 Strict creation handshake (main admits every guest)

The renderer never sets a real `src` first:

1. Renderer → `browser:preparePageAttach({browserPageId, partition, url})`. Main creates or updates a `Page` record and returns the marker `data:text/html,orca-browser-attach-<uuid>` (`main/browser/guest-lifecycle/browser-page-attach.ts`).
2. The renderer sets `webview.src = marker` (`host-guest/lifecycle/bind-browser-page-webview-listeners.ts:226-234`). It does this only after all listeners are attached, so a fast localhost failure isn't missed.
3. `will-attach-webview` in `main/window/security/main-window-webview-security.ts`:
   - Checks the partition against an allowlist (profile partitions, route partitions, local-SSH partitions).
   - Checks the marker token (single use, same renderer, same partition).
   - Deletes `params.preload` and forces Orca's preload (`browser-guest-preload.js`).
   - Pins `sandbox`, `contextIsolation`, `nodeIntegration=false`, `webSecurity`.
   - Deletes `params.src`. Main loads the URL itself.
4. `app.on('web-contents-created')` → `associateBrowserPageAttach(guest)` binds the new guest to the admitted attach before any navigation can happen.
5. `did-attach-webview` → `browserManager.attachGuestPolicies(guest)` installs popup, navigation, metadata, shortcut, Escape and DevTools policies at creation time, so early redirects and `target=_blank` cannot outrun registration. Then `startBrowserPageAttach(guest)` runs either `guest.loadURL(url)` or `guest.navigationHistory.restore(snapshot)` for a discarded page.
6. Renderer → `browser:registerGuest({browserPageId, workspaceId, worktreeId, sessionProfileId, webContentsId})`. Main accepts only `getType()==='webview'` guests that already passed attach-time policy (`policyAttachedGuestIds`). Then it wires up the context menu, shortcut forwarding, wheel zoom, target-URL hover, and flushes queued load failures, popups and downloads (`main/browser/guest-lifecycle/manager/registration/browser-manager-registration.ts`).

### 1.3 Positioning: guests never reparent, so layout uses CSS anchors

Electron destroys a `<webview>` guest when it is removed from the DOM or reparented. Orca's whole layout strategy is built around that constraint.

- **Worktree-level overlay layer**: `renderer/src/components/browser-pane/assemble-chrome/BrowserPaneOverlayLayer.tsx`. Every browser tab of a worktree is rendered once at worktree level, as a sibling of the split/tab-group layout. Moving a tab between split groups only changes the CSS `position-anchor`:

```tsx
style = {
  position: 'absolute',
  positionAnchor: anchorName,                 // tabGroupBodyAnchorName(groupId)
  top: `anchor(${anchorName} top)`, left: `anchor(${anchorName} left)`,
  width: `anchor-size(${anchorName} width)`, height: `anchor-size(${anchorName} height)`,
  display: isPaintable ? 'flex' : 'none',
  pointerEvents: isActive ? 'auto' : 'none',
  opacity: isActive ? 1 : 0
}
```

- **Persistent viewport shells**: `host-guest/viewport/browser-page-viewport.ts`. Per page there is a tree of imperatively created DOM nodes: `shell > chromeInset + devToolsSplit > container > scroller > content > <webview>`. It is appended to a per-tab "slot root" that the overlay slot registers. React chrome (toolbar, address bar) can mount and unmount freely, because the webview lives in the shell that React does not own. On chrome unmount (for example a worktree switch) the shell is **parked** (`display:none; inert; opacity 0`), not destroyed. `chromeInset` reserves the toolbar height, so the page sits below the React toolbar inside the same box.
- **Viewport presets / agent viewport**: the `content` div is sized with CSS variables (`--browser-page-viewport-width/height`) and divided by UI zoom. The scroller becomes `overflow:auto`. Device emulation itself happens through CDP.
- **Client-hosted pages use another technique**: `host-guest/client-pages/browser-client-page-retained-elements.ts` and `viewport/browser-client-page-position-driver.ts`. Each webview lives in a `position:fixed` host under a `document.body` root, so it survives any pane re-render. A **wake-driven rAF position driver** re-reads the pane's container rect and moves the host. It runs 2 settle frames after any of about 20 window events (resize, scroll, transitions, pointer, focus, drag…), keeps running while finite CSS animations are playing, and does a 1 s frameless re-check. An idle app schedules no frames at all. This is the closest analogue to what a native app must do with a native child view.

### 1.4 Z-order and occlusion

- Modern `<webview>` is composited inside the embedder's compositor (OOPIF-style). DOM menus, popovers and modals in the React UI therefore **paint over it naturally**, so no manual occlusion is needed. That's the main reason Orca can use plain DOM overlays (find bar, context menu, annotation/markup overlays, agent cursor, load-failure overlays). See `assemble-chrome/browser-page-viewport-overlays.tsx`.
- Input locking:
  - `setBrowserPageWebviewInputLock` sets `pointer-events:none` on the webview. It is used while a phone drives the page (`browser-workspace-pane.tsx:213`).
  - Native drag passthrough: `host-guest/webview-registry.ts` installs capture-phase `dragstart/dragend/drop/pointerdown/pointermove` listeners on `window`. While a native drag is in progress, every webview gets `pointer-events:none`, so the drop target in the host UI receives it. This is ref-counted and attached only while guests exist.
- The context menu is rendered by the host, not the guest. Main listens for `context-menu` on the guest, builds a payload (link, selection, editFlags, spelling, image, nav state, screen cursor point) and sends `browser:context-menu-requested`. React draws the menu above the guest (`main/browser/guest-lifecycle/input/browser-guest-context-menu.ts`).
- The DevTools dock is a real native view, so the renderer owns slot bounds and pushes them to main. A docked host that outlives its guest is explicitly closed ("would stay painted over the pane").

### 1.5 Hidden tabs, visibility truth and throttling

- `applyBrowserPageViewportLayout(pageId, {paintable, active})` sets:
  - not paintable → `display:none`. The compositor stops producing frames.
  - paintable but inactive → `opacity:0`, `pointer-events:none`, `z-index:0`, `inert`.
  - active → full visibility.

  "Paintable" = active OR a consumer needs frames (an agent is automating it, a phone is driving it, a paired remote is viewing it) OR a one-shot "mount admission" for a background-opened tab's first load (`host-guest/retention/browser-guest-paint-retention.ts`, `browser-page-mount-admission.ts`). All ancestors must avoid `display:none` for retained guests.
- `document.visibilityState` correctness: Electron only sends `GUEST_INSTANCE_VISIBILITY_CHANGE` to webviews when the *window* hides, so a background tab stays 'visible'. Orca's guest preload reports Chromium's real per-frame visibility (`browser:guestPageVisibility`). Main monkey-patches `guest._sendInternal` so the visibility it delivers is the page's own value combined with window-hidden (`main/browser/guest-lifecycle/visibility/browser-guest-page-visibility.ts`).
- `RendererPublicationThrottle` (`main/window/visibility/renderer-publication-throttle.ts`) is a process-wide ref-counted lease on `setBackgroundThrottling(false)`. When the last lease is released it re-throttles and does `capturePage({0,0,0,0},{stayHidden:true})` to replay a swallowed "hide". Screencasts, agent claims and captures take leases on both the guest and the host window.
- Chromium flag: `IntensiveWakeUpThrottling` is disabled app-wide (`main/startup/configure-process.ts:87`).

---

## 2. Process/session model, persistence, memory

### 2.1 Partitions (one Electron `Session` each)

- Default: `persist:orca-browser` (`shared/browser/browser-session-partition.ts`).
- Browser profiles: `persist:orca-browser-session-<profileId>` for the default Orca profile, else `persist:orca-profile-<seg>-browser-session-<id>` (`shared/orca-profiles/orca-profiles.ts:344`). Profiles have scope `default | isolated | imported`. Cookies can be imported from Chrome, Arc, Edge, Firefox, Safari and others.
- Managed per-repo/per-worktree profiles: `BrowserProfileScope = 'shared'|'repo'|'worktree'` (`shared/browser/profiles/browser-profile-scope.ts`). A workspace can get its own isolated cookie jar automatically.
- Route partitions (browsing *through* a remote execution host): `persist:orca-browser-v1-<sha256(orcaProfile, browserProfile, authorityConnection, executionHost)>` (`main/browser/routing/partitions/browser-route-identity.ts`). The session gets `setProxy({proxyRules:'socks5://127.0.0.1:<port>'})` to a local SOCKS server that tunnels over SSH or the runtime connection (`routing/sessions/browser-route-session-policy.ts`, `routing/local-host-partitions/local-ssh-browser-route.ts`). WebRTC gets `disable_non_proxied_udp` per WebContents. Route guests stay on a blank page until main registers them.
- Unminted profiles map to `persist:orca-browser-unminted-<id>`, which main never admits. Fail closed.
- Per-partition policies are installed once: UA, proxy, downloads, permissions, site permissions, WebAuthn (`main/browser/sessions/partition-policies/browser-session-partition-policies.ts`). The registry (`sessions/profiles/browser-session-registry.ts`) is the allowlist that `will-attach-webview` consults.
- A partition is immutable per webview. Changing a tab's profile **rebuilds** the webview (`ensureBrowserPageWebview` checks `getAttribute('partition') !== webviewPartition`).

### 2.2 Renderer processes

No custom process limits. Each `<webview>` is its own guest `WebContents`, with Chromium's normal site-isolation renderer processes. Orca's memory control is about *how many guests exist*, not process tuning. Memory is reported through `app.getAppMetrics()` (`main/memory/collector.ts`).

### 2.3 Lazy creation

- `DeferredBrowserContent` mounts a pane's React chrome only once it is paintable, then keeps it mounted while the worktree is active.
- `attachBrowserPageWebview` returns early unless the page is paintable or mount-admitted. Restored tabs therefore stay **lazy** (no guest) until shown.
- Background tabs (`disposition: background-tab`) get one-shot mount admission for their first load, then follow the normal paint rules.

### 2.4 Discarding ("Memory Saver")

Policy: `shared/browser/guest-lifecycle/browser-memory-saver.ts`. The default is 4 awake-hours hidden, configurable from 1 to 168 h.

Flow, with main owning the deadlines and the renderer owning the DOM:

1. The renderer reports visibility per guest: `browser:reportGuestVisibility({browserPageId, webContentsId, generation, visible})` (`host-guest/browser-guest-activity.ts`).
2. Main's `createBrowserPageIdleDiscard` (`main/browser/guest-lifecycle/browser-page-idle-discard.ts`) keeps a priority index keyed by `hiddenAt`, measured on an awake clock that pauses during system sleep. It sends at most 4 requests in flight and skips pages revisited more than 15 times. When a page is due, it sends `browser:guest-idle-discard-due`.
3. The renderer re-checks synchronously: still hidden, same generation, same webContents, no local veto. Then it calls **sync IPC** `reserveGuestDiscard`, so no await separates reservation from removal (`webview-registry.ts::discardPersistentWebview`).
4. Main `reserveBrowserPageDiscard` refuses if any of these hold: claimed by an agent, operations in flight, loading, DevTools open, debugger attached, or discard-protected. Discard protection covers unsaved form edits detected by the guest preload (`preload/browser/edit-protection`), audio, and similar cases. Otherwise main snapshots `navigationHistory.getAllEntries()` **including `pageState`** (scroll and form state), held in memory only because it may contain secrets.
5. The renderer runs `webview.remove()` and then `acknowledgeGuestRemoval`. Main `guest.close()`s it.
6. When the tab is shown again, a new webview goes through the same attach handshake and main calls `navigationHistory.restore({entries,index})` (`browser-page-attach.ts::startBrowserPageAttach`), with outcome tracking (committed/failed).

The offscreen backend does the same: `OffscreenPageIdleClock` plus `mustKeepOffscreenPage` (agent using it, network intercept, open dialog, audible, has popups) → destroys the window, keeps `freedPages[id] = {entries,index}`, and on wake restores under the same id.

### 2.5 Frame pacing / perf tricks

- Offscreen: `setFrameRate(5)` when unwatched, `60` while any publication lease exists, with a 20 s grace period after the last lease ("spans an agent's thinking between steps"). The rate is reapplied on `did-navigate` because a cross-site view keeps the old rate (`backends/offscreen-page-frame-rate.ts`).
- Offscreen: `stopPainting()`, because delivering every animation frame to main costs hundreds of MB per animated tab.
- Screencast frame pacer (`main/browser/cdp/screencast/browser-screencast-frame-pacer.ts`):
  - `minFrameIntervalMs` throttle that keeps the *newest* throttled frame (a static page may never emit another).
  - `Page.screencastFrameAck` is delayed for throttled frames, which also throttles Chromium's encoder.
  - 50 ms backpressure retry.
- Listener budgets: `setMaxListeners(WEB_CONTENTS_MAX_LISTENERS_PER_EVENT)` on guests.
- Store selectors: memoized per-slot components and `useShallow` everywhere. There are explicit notes to avoid re-rendering every BrowserPane on unrelated store writes.

### 2.6 Persistence

- Tabs and pages are persisted in the workspace session: `BrowserWorkspace`/`BrowserPage` records with url, title, favicon, profile, partition, viewport preset, remote page id. History entries (url/title/favicon/visitCount/lastVisitedAt) are kept separately.
- Discard snapshots (`pageState`) are **not** persisted. Only the URL is restored across app restarts.
- Cookies and storage persist via `persist:` partitions.

---

## 3. "Backends"

`main/browser/browser-backend.ts`:

```ts
export type BrowserBackend = {
  createTab(params: { url: string; worktreeId?: string; profileId?: string; browserPageId?: string }): Promise<{ browserPageId: string }>
  closeTab(browserPageId: string): Promise<void>
  ownsTab(browserPageId: string): boolean
  destroyAll?(): void | Promise<void>
}
```

- **Renderer webview backend** (implicit): the renderer creates pages and `BrowserManager` registers them.
- **`OffscreenBrowserBackend`**: headless `orca serve`. Hidden offscreen windows, `registerOffscreenGuest`. Needs Xvfb on Linux; `--headless` segfaults.
- Both share `BrowserManager` (`main/browser/guest-lifecycle/manager/*`). It is a class chain (Registration → GuestPolicy → GuestPopupPolicy → Navigation → EventForwarding → BrowserManager) holding the maps `webContentsIdByTabId`, `tabIdByWebContentsId`, `rendererWebContentsIdByTabId`, `worktreeIdByTabId`, `sessionProfileIdByPageId`, pending-event queues keyed by guest id, and so on.
- There is **no external-Chrome CDP backend** for page hosting. CDP is used *on Electron's own guests* via `webContents.debugger.attach('1.3')`, behind a lease (`main/browser/cdp/debugger/electron-debugger-lease.ts`), for automation, screencast, emulation and input.
- Placement abstraction for remote runtimes (`shared/browser/client-hosting/runtime-browser-placement.ts`):

```ts
type RuntimeBrowserPlacement =
  | { kind: 'server' }                                     // runtime hosts page (offscreen) and streams it
  | { kind: 'client'; browserHostClientId; browserHostGeneration; pageHostGeneration } // a desktop hosts it
```

- **Desktop browser bridge** (`main/browser/desktop-browser-bridge/*`, `shared/browser/client-hosting/desktop-browser-bridge-protocol.ts`): an SSH host's CLI (`orca browser ...`) may drive the *desktop's* tabs through an allowlisted method set (`DESKTOP_BROWSER_BRIDGE_METHODS`). It is capability-gated per method (`browser.desktopBridge.page-tools.v1` and others), with retry and backoff.

---

## 4. Remote/client-hosted browsers and screencast

### 4.1 Server-hosted → streamed to viewer (desktop pane for a remote worktree, or phone)

- Viewer → RPC `browser.screencast` with `{format, quality, maxWidth, maxHeight, viewportWidth/Height, deviceScaleFactor, mobile, everyNthFrame, minFrameIntervalMs, frameAck:1}` (`shared/rpc-contract/browser/browser-params.ts:48`).
- Host: `RuntimeBrowserScreencasts` (`main/runtime/browser/screencast/browser-screencasts.ts`):
  - **One CDP screencast per page**, fanned out to N subscribers.
  - A single *viewport owner* subscriber drives emulation (`Emulation.setDeviceMetricsOverride`). When the owner leaves, the most recent sized subscriber takes over.
  - The frame budget is the merge of all subscribers' budgets.
  - Throttle leases on the guest and the host window ("hidden host compositors stop producing guest frames").
  - Ghost-subscriber eviction. Reconnects replace a device's old socket only after the new one joins.
  - Webview guests are clamped to embedder size because a webview compositor can't exceed its embedder (`browser-screencast-guest-view-limit.ts`). Offscreen windows can grow instead.
- Frame wire format (`shared/browser/screencast/browser-screencast-protocol.ts`): a 16-byte header `[kind 0x62][ver 1][opcode Frame=1][format jpeg=1|png=2][u32 seq][u32 metadataLen][u32 0]`, then JSON metadata `{offsetTop,pageScaleFactor,deviceWidth,deviceHeight,imageWidth,imageHeight,scrollOffsetX,scrollOffsetY,timestamp,zoomFactor}`, then the raw image bytes. Frames travel as binary RPC frames over the runtime WebSocket or relay.
- Flow control (`browser-screencast-frame-credit.ts`): the viewer acks cumulatively with `browser.screencast.ack {subscriptionId, seq}`.
  - Window: 2 base frames in flight, max 16.
  - Beyond the base window, a frame is sent only while the oldest unacked frame is younger than `minRtt + 100 ms`, a queue-delay test like a mini-BBR.
  - If acks stop, a stall probe goes out every 2 s, backing off to 8 s.
  - If there's no credit, only the newest frame is held.
- Viewer rendering: `renderer/src/components/browser-pane/stream-remote/*`. Frames are decoded via `Image.decode()`; liveness, restart scheduler and stream tokens handle reconnects. `zoomFactor` is used for pointer mapping.
- Input back: plain RPCs, not raw CDP:
  - pointer: `browser.mouseMove` + `browser.mouseDown`/`mouseUp`, `browser.mouseWheel {dx,dy}`
  - keys: `browser.keypress {key}`, `browser.keyboardInsertText` (chunked)
  - clipboard and undo: `browser.eval`
  - navigation: `browser.goto/back/forward/reload/stop`

  On the host these become CDP `Input.dispatchMouseEvent/KeyEvent/TouchEvent` (`main/browser/cdp/input/*`). Agent-injected input is flagged (`isAgentInjectingInput`), so it never triggers Orca shortcuts.
- Mobile driving: when a phone drives a desktop page, the desktop pane is input-locked and shows `BrowserMobileDriverOverlay`. The guest is kept paintable even when hidden.

### 4.2 Client-hosted (runtime asks the desktop to host)

- Eligibility: the runtime advertises the client-host, page-metadata, automation, network-tunnel and execution-hosts capabilities. The client isn't mobile, and the preference isn't 'server' (`browser-client-hosting-eligibility.ts`).
- A lease protocol, `BrowserHostLeaseAuthority {authorityRuntimeId, authorityEpoch, browserHostClientId, browserHostGeneration}`, plus a per-page `pageHostGeneration`, fences stale hosts. Page commands are `createPage | navigate | reclaimPage | closePage | restorePage | profileSwitch`, each with `commandSequence` and `commandId`. Results are chunked and staged (`browser-client-host-protocol.ts`). After a runtime restart, the desktop's page inventory (≤256 pages, 768 KB) rebuilds the runtime's records.
- Networking: the desktop guest's session proxies through a **multiplexed binary tunnel** (`shared/browser/network/tunnels/browser-network-tunnel-protocol.ts`). Opcodes: Open/Opened/Data/WindowUpdate/HalfClose/Close/Error/Ping/Pong. Frames carry 64 KB data and use window-update flow control, so `localhost:3000` resolves on the execution host (native/SSH/WSL, `browser-network-execution-host.ts`).
- Automation of client-hosted pages: the runtime issues commands and the desktop executes them against its local guest (`main/browser/client-pages/execution/*`), then replies with `browser.clientHost.commandResult`.

---

## 5. Tab model and state

### 5.1 Data structures (`shared/browser/workspaces/browser-workspace-types.ts`)

- `BrowserWorkspace` is the **outer tab** in Orca's unified tab strip. It holds `pageIds[]` and `activePageId`, so one outer tab can contain several pages. It also holds `sessionProfileId`, `sessionPartition`, and mirrored active-page fields (url/title/loading/favicon/canGoBack/canGoForward/loadError).
- `BrowserPage` holds `{id, workspaceId, worktreeId, url, title, loading, faviconUrl, canGoBack, canGoForward, loadError, createdAt, viewportPresetId, browserRuntimeEnvironmentId, remoteBrowserPageId, docLocation, convertedFrom/To}`.
- Zustand slice (`renderer/src/store/slices/browser/browser-slice-contract.ts`):
  - `browserTabsByWorktree`, `browserPagesByWorkspace`
  - `browserAudiblePageIds`, `browserAudioMutedTabIds`
  - `browserCertificateFailuresByPageId`
  - `remoteBrowserPageHandlesByPageId` (environmentId, remotePageId, placement, staged/restored flags)
  - client-hosted close intents
  - history
- Unified tabs live in `unifiedTabsByWorktree` / `groupsByWorktree`. The browser overlay maps a unified tab's `entityId` to a `BrowserWorkspace`.
- Non-React registries live outside the store, to avoid cycles and re-renders:
  - `webviewRegistry: Map<pageId, WebviewTag>`, `registeredWebContentsIds` (`host-guest/webview-registry.ts`)
  - `browserGuestActivity` (visibility, generation, protection)
  - the `browserPageViewports` shells

### 5.2 Metadata flow (title/url/loading/nav)

- Main is the single source: `installBrowserGuestMetadata` (`main/browser/guest-lifecycle/navigation/browser-guest-metadata.ts`) publishes `browser:guest-metadata`:

```ts
{ browserPageId, webContentsId, revision, navigationRevision,
  kind: 'snapshot'|'navigation'|'in-page'|'title'|'start'|'stop'|'failure'|'retired',
  url, title, loading, canGoBack, canGoForward, errorPage, loadError, hasCertificateFailure }
```

  Unchanged values are deduped. Chromium internal error URLs are filtered out.
- The renderer applies metadata to the store (`bind-browser-page-webview-listeners.ts`) and uses webview DOM events for favicon (`page-favicon-updated`), `did-fail-load`, `did-start-navigation`, `did-redirect-navigation`, and find-bar close on full navigation.
- Load errors are queued until registration and replayed (`flushPendingLoadFailure`). A navigation stall watchdog sends `browser:load-stall`.

### 5.3 Navigation API

The renderer sets `webview.src` / calls `goBack()` etc. for local guests (`navigate/navigate-browser-page-url.ts`). The runtime and CLI use RPC `browser.goto/back/forward/reload/stop` through `RuntimeBrowserCommands`. Main validates and normalizes every URL (`shared/browser/navigation/browser-url.ts`).

### 5.4 Popups / `window.open` / `target=_blank`

`main/browser/guest-lifecycle/manager/popups/browser-manager-guest-popup-policy.ts`, via `guest.setWindowOpenHandler`:

- **Tab intent** (no frameName, disposition foreground/background-tab, no POST body, not about:blank, not an OAuth authorize URL; `popups/browser-popup-new-tab-intent.ts`) → `deny` and open an Orca tab instead. Main sends `browser:open-link-in-orca-tab` to the owning renderer, which creates the page in the same workspace.
- **Gesture accounting**: `trackBrowserGuestInputGesture` records user activation (and whether it came from an agent). Scripted opens without a gesture spend a **budget of 4 tabs per 2 s** per root guest (`popups/browser-page-initiated-tab-budget.ts`). Agent-opened tabs don't steal the user's focus.
- **OAuth / named / opener-dependent popups** → `allow` with `SAFE_POPUP_WINDOW_OPTIONS`, a child window with an **origin bar** (BaseWindow + 2 WebContentsViews). It is bound to the opener's session, and its policies are inherited (`did-create-window` → `attachGuestPolicies(child, ownerContext)`).
- Non-http(s) links → `shell.openExternal` only on a real user click. Kagi tokens are redacted. Otherwise an external-app prompt or blocked; the renderer is notified with `browser:popup {origin, action: opened-in-orca|opened-external|blocked}`, origin only, never full URLs.

### 5.5 Context menus

Main sends `browser:context-menu-requested` with a rich payload. React renders the menu (`assemble-chrome/context-menu/*`). Actions come back as IPC (`copyImageAt`, `addWordToDictionary`, open link in new tab, and so on).

### 5.6 Keyboard shortcut routing (host vs. guest)

The problem: key events in a focused guest never reach the renderer's DOM handlers. `main/browser/guest-lifecycle/input/shortcuts/browser-guest-shortcut-forwarding.ts` + `browser-guest-shortcut-dispatch.ts`:

- `guest.on('before-input-event')` resolves `resolveWindowShortcutAction(input, platform, keybindings)` against the **same shared keybinding table** the renderer uses (`shared/keybindings/window-shortcut-policy.ts`).
- Rules:
  - IME composition keeps the key.
  - Ctrl+Alt (possible AltGr) stays with the page.
  - Primary-modifier app chords are forwarded and `preventDefault()`ed.
  - Non-primary chords stay with the page.
  - Ctrl+Tab switcher keydown/keyup are forwarded specially (`ui:ctrlTabKeyDown/Up`).
  - Modifier double-tap is detected.
  - Agent-injected keys are ignored.
- Zoom: `zoom-changed` and wheel zoom are deduplicated and forwarded as `ui:zoomBrowserPage`.
- Escape is forwarded separately (`browser:guestEscape`) to exit find, grab mode and so on.
- Mouse presses in the guest are reported (`before-mouse-event` → `onBrowserPagePointerDown`), so the owning split group becomes active even though the click never touched host DOM.

### 5.7 Focus

- Before hiding or removing a focused webview the renderer blurs it and calls `window.focus()`, so macOS can't reactivate the previous frontmost app (`webview-registry.ts::moveFocusToRendererBeforeWebviewDetach`).
- Focus requests (`'webview' | 'address-bar'`) are queued per page with a 30 s TTL, so they survive pane mounting (`host-guest/input/browser-focus.ts`).
- Explicit New Tab focuses the address bar; link-opened tabs do not.
- An agent input focus guard stops agent CDP input from stealing OS focus (`main/browser/cdp/input/agent-input-focus-guard.ts`).

### 5.8 Guest preload (`preload/browser-guest.ts`)

Sandboxed and minimal. Nothing is exposed to page script via contextBridge. It installs:

- edit protection (unsaved-form detection feeding discard protection)
- a `window.close` guard (main world → `sendToHost` → the renderer closes the tab)
- a `window.prompt` shim
- the page visibility report
- the password detector
- the theme bridge

---

## 6. IPC / RPC contract

### 6.1 Renderer → main (invoke/send), from `preload/api/browser/*`

| Area | Channels |
|---|---|
| Guest lifecycle | `browser:preparePageAttach`, `browser:registerGuest`, `browser:repairGuestRegistration`, `browser:isGuestRegistered`, `browser:unregisterGuest`, `browser:reserveGuestDiscard` (sync), `browser:acknowledgeGuestRemoval`, `browser:reportGuestVisibility`, `browser:refuseGuestIdleDiscard`, `browser:getGuestMetadata`, `browser:getGuestDiscardProtection`, `browser:activeTabChanged` |
| Viewport/zoom | `browser:setViewportOverride`, `browser:reportViewportScrollState`, `browser:zoomPage`, `browser:setAnnotationViewportBridge`, `browser:setPageSiteColorScheme`, `browser:setPageAudioMuted`, `browser:getPageSiteDisplay` |
| DevTools | `browser:openDevTools`, `browser:closeDevTools`, `browser:setDevToolsDockBounds` |
| Prompts/security | `browser:respondPagePrompt`, `browser:respondWebAuthnAccount`, `browser:proceedCertificate`, `browser:sitePermissions:list/set` |
| Downloads | `browser:acceptDownload`, `browser:cancelDownload` |
| Context/grab | `browser:copyImageAt`, `browser:addWordToDictionary`, `browser:setGrabMode`, `browser:awaitGrabSelection`, `browser:cancelGrab`, `browser:captureSelectionScreenshot`, `browser:extractHoverPayload`, `browser:insertDictationText` |
| Sessions/profiles | `browser:session:listProfiles/createProfile/deleteProfile/ensureManagedProfile/resolvePartition/importCookies/importFromBrowser/detectBrowsers/clearDefaultCookies/clearGoogleCookies/...`, `browser:prepareSshWorkspacePartition`, `browser:identity:get/set` |
| Client hosting | `browser:clientPageRendererReply`, `browser:publishClientPageMetadata`, `browser:remoteCliScopeReply`, `browser:tabActivateReply`, `browser:tabRequestsReady` |

### 6.2 Main → renderer (events)

- Page state: `browser:guest-metadata`, `browser:navigation-update`, `browser:guest-load-failed`, `browser:load-stall`, `browser:certificate-failure-changed`, `browser:audio-state-changed`, `browser:guest-discard-protection-changed`, `browser:guest-idle-discard-due`
- Tabs, popups and menus: `browser:open-link-in-orca-tab`, `browser:popup`, `browser:context-menu-requested/dismissed`, `browser:target-url-changed`, `browser:permission-denied`
- Prompts and downloads: `browser:page-prompt-requested/closed`, `browser:webauthn-account-requested/closed`, `browser:download-requested/progress/finished`
- Grab, DevTools, focus and agents: `browser:grabModeToggle`, `browser:grabActionShortcut`, `browser:guestEscape`, `browser:devToolsDockChanged`, `browser:capturePaintHold`, `browser:pane-focus`, `browser:activateView`, `browser:requestTabActivate`, `browser:agentActivity`, `browser:agentCursor`, `browser:agentViewportApplied`, `browser:clientPageRendererRequest`, `browser:remoteHostControlNotice`
- UI: `ui:ctrlTabKeyDown/Up`, `ui:zoomBrowserPage`, `onBrowserPagePointerDown`

### 6.3 Guest → main

`browser:guestPageVisibility`, edit-protection reports (`browser:guest-protection-report/challenge`), password channels, passkey watch (`browser:passkeyRequestPending/Cancel`), `browser:printRefused`, and the window-close message via `sendToHost` → `ipc-message` on the webview element.

### 6.4 Runtime RPC (CLI, agents, remote viewers, phones)

Zod schemas live in `shared/rpc-contract/browser/*`. Methods include:

- Tabs and navigation: `browser.tabList/tabCreate/tabClose/tabSwitch/tabShow/tabCurrent/tabSetProfile/tabProfileClone`, `browser.goto/back/forward/reload/stop/openUrl`
- Reading the page: `browser.snapshot/screenshot/fullScreenshot/pdf/eval/exec/get/is/find/console/network/wait`
- Element actions: `browser.click/dblclick/hover/fill/type/select/check/drag/upload/scroll/scrollIntoView/focus/highlight/keypress/keyboardInsertText`
- Raw input: `browser.mouseMove/Down/Up/Click/Wheel`
- Screencast: `browser.screencast` (streaming plus binary frames), `browser.screencast.ack/unsubscribe`
- Emulation and storage: `browser.viewport/setDevice/setMedia/setOffline/setHeaders/setCredentials/geolocation/zoom`, `browser.cookie.*`, `browser.storage.local|session.*`, `browser.intercept.*`
- Dialogs, certificates and downloads: `browser.dialogAccept/Dismiss`, `browser.certificate.proceed`, `browser.download`
- Profiles: `browser.profile*`
- Client hosting: `browser.clientHost.attach/commandResult/requestResult/pageMetadata/fileChannel.read|write|abort`, `browser.clientPage.initializationAction`

Every page-addressed call can carry `expectUrl`, which guards against acting on a page that navigated away.

---

## 7. Porting notes for Rust + GPUI

- **Native view hosting is the hard part.** Orca avoids it on desktop because `<webview>` composites inside the DOM. In GPUI a WKWebView/WebView2/CEF child view is a separate native layer and will paint **above** GPUI content. You need what Orca does for its DevTools dock and client-hosted pages:
  - a per-frame (or wake-driven) bounds sync from the layout to the native view
  - explicit hide/`setHidden` when a GPUI popover, menu or modal intersects the page rect, or snapshot the page into a texture while an overlay is open
  - pointer passthrough toggling during drags

  Alternative: CEF in off-screen rendering mode, rendering into a GPUI texture. Overlays then become free and you can reuse Orca's screencast and input model for the local case too.
- **Keep the page alive across layout changes**: one owner per page outside the view tree (Orca's `webviewRegistry` plus viewport shells). The layout only *anchors* it.
- **Copy the guest-policy layer**: popup classification, gesture-gated tab budget, a host-rendered context menu, and shortcut forwarding from the shared keybinding table on `before-input-event` (WKWebView: `performKeyEquivalent`/`keyDown` interception; WebView2: `AcceleratorKeyPressed`).
