# Browser: a WebKit pane for development, driven by agents over MCP — plan

Status: built, revision 4 (2026-11-13). §4.2 says what is built and what is not; the sections before it are the plan as it was decided. No code yet. Branch `AK/monocode_enhancements`.
macOS only. File and line references are from the tree at `01cd52fe`.

## 1. Goal

A browser inside Monocode that is good enough to develop against, and no more:

- Opens as a **pane**: a title tab of its own, or split beside a chat, a file or a terminal.
- Runs on the system **WebKit** (`WKWebView`). No Chromium, no Swift code, no new
  dependency: `objc2-web-kit 0.3` is already in `monocode-gpui/Cargo.toml:69`.
- **Agents drive it over MCP**: open a page, read it, click, type, take a screenshot, read
  the console. Any provider that takes MCP servers gets it, not only Claude.
- **Has browser management**: profiles, cookies, cache and site data that can be listed and
  cleared; pages that can be put to sleep or destroyed so they stop using memory.
- Remembers sign-ins between launches, like any browser.

### Non-goals (decided)

- **No extensions.** WebKit can load them (`WKWebExtension`, macOS 15.4+), but the Search
  browser needed about 13,800 lines of Swift and months of per-extension fixes to make
  Chrome Web Store extensions work (`Extensions.swift` 2,509 lines, `ExtensionShims.swift`
  5,638, plus Crx, native messaging, popups, offscreen documents). Out of scope. The data
  model below leaves room: an extension controller attaches to the same profile object.
- **No embedded Swift**, no second engine.
- **No password manager, no sync, no bookmarks, no history UI.** A sign-in is kept by its
  cookies. Passwords are pasted. See §7 for what this means for some sites.
- **Not a replacement for the user's browser.** Claude in Chrome keeps working against Brave
  for anything that needs the user's own accounts and extensions.

## 2. What exists today (verified)

### 2.1 A WKWebView is already hosted inside a GPUI window
`monos/html.rs` (323 lines) shows generated HTML for Monos. What it proves and what it lacks:

- **Hosting works.** A `canvas` element's paint closure gets the element's bounds, takes the
  window's `NSView` from `HasWindowHandle` (`:197-205`), and keeps a native clip view with
  the `WKWebView` inside it in step with GPUI's layout every frame (`:206-296`): position,
  the content mask (so it clips while scrolling), and the interface zoom (`setPageZoom`).
- **Hiding works.** `Html::hide` (`:171-176`) hides the native view; the Shell calls
  `m.read(cx).hide_html(cx)` on every render (`shell.rs`, in `render`) and the paint closure
  shows it again if it is still on screen. This is the pattern for "the native view must not
  sit on top of something GPUI draws".
- **Clicks can be let through.** The clip view's `hitTest:` returns nothing when the view is
  not interactive (`:36-44`), so GPUI gets the event.
- **It is a sandbox on purpose**: `nonPersistentDataStore` (`:247-249`), no network
  (`connect-src 'none'`), navigation cancelled except `about:blank` (`:55-80`), no message
  handlers. **None of this configuration is reused.** The browser gets its own host type;
  the Mono sandbox stays exactly as it is.

### 2.2 Agents already get an MCP server from Monocode
`monocode-core/src/visual_bridge.rs` (1,017 lines) is the model to copy:

- One process-wide TCP listener on `127.0.0.1:0` (`hub()`, `:166-207`), at most 16
  connections.
- Each session gets a **capability**: a 256-bit random token, a generation, and the turn
  that may currently use it (`Server::start`, `:228-266`). A wrong session or an ended turn
  is refused (tests `:893`).
- The agent process is given the endpoint, token and session as environment variables and
  an MCP server definition `{type: stdio, command: <monocode binary>, args: ["visual-mcp"]}`
  (`mcp_server`, `:705-711`). The binary's `visual-mcp` subcommand speaks MCP on stdio and
  forwards calls to the hub (`mcp_stdio`, `client`).
- Wired per provider: Claude `--mcp-config` + allowed tools (`provider.rs:1463-1489`),
  Codex `-c mcp_servers…` (`configure_codex`, `:728`), ACP providers (`acp_servers`,
  `provider/acp.rs:103`), OpenCode (`provider/opencode.rs:513`).
- Started per turn in `runtime.rs:563`; the runtime drains what the agent published.

The browser needs one thing this does not have: a **reply from the UI thread**. A visual is
published and acknowledged; a browser call has to run on the main thread against a web view
and wait for WebKit's completion handler. §3.6 covers it.

### 2.3 Panes that are not chats already exist
The workspace knows panes with negative ids that borrow a session's project and worktree:
file panes (`review_owners`), Mono panes (`mono_panes`), and **terminal panes**
(`terminal_panes: HashMap<i64, TerminalPane { owner, cwd }>`, `workspace.rs:268-271`,
`shell/terminal_panes.rs`). A terminal pane can be a title tab or sit in a split, is pruned
when its owner goes (`Workspace::restore`), is confirmed before closing, and is listed
under its project in the tree rail (`project_rail_tree.rs`, `RailTerminal::Pane`).
**The browser pane follows this pattern**, not the app-view pattern: an app view replaces
the whole workspace body and cannot sit beside a chat (see `docs/native/project-apps.md`
§1 non-goals).

### 2.4 Project apps on the rail
`PROJECT_APPS` (`project_rail_tree.rs:17-22`) lists Inbox, Notes, Database and API as icons
on a project's row. The browser gets an icon there too, but it opens a pane, not an app
view.

## 3. Design

### 3.1 Model

```
BrowserProfile          a WebKit data store: cookies, cache, storage
  └─ BrowserPage        one page: address, title, history; maybe a live web view
       └─ shown in a BrowserPane (a workspace pane), or asleep, or destroyed
```

- **Profile** (`monocode-core`): `{ id: Uuid, name, kind }`, kind `Shared | Project(path) |
  Private`. `Shared` is one persistent store for all projects (sign in once). `Project` is
  a persistent store of one project's own (`WKWebsiteDataStore(forIdentifier:)`, macOS
  14+), which also keeps two projects on `localhost:3000` out of each other's cookies and
  storage. `Private` is `nonPersistentDataStore`. A project uses `Shared` unless set
  otherwise in its settings.
- **Page**: `{ id, project, profile, url, title, user_agent, viewport, pinned_awake,
  last_used, state }` (`user_agent` and `viewport` are `None` for the defaults, §3.4.1) with
  `state` one of:
  - `Live`: has a web view and a web content process.
  - `Asleep`: web view destroyed; address, title, scroll position and a snapshot image
    kept. Showing it loads the address again.
  - Destroyed pages are gone: closed by the user, or by "Close all pages".
- **Pane**: `Workspace.browser_panes: HashMap<i64, BrowserPane { owner, page }>` with a
  negative pane id, owner = the session whose project it belongs to. Saved like terminal
  panes; pages are saved in the workspace too (address, title, profile), never their
  content. A restored page starts `Asleep`: nothing loads until it is shown.
- One pane shows one page. More pages are more panes (tabs or splits); there is no tab
  strip inside the pane. This keeps "tabs" a single concept in Monocode.

### 3.2 Where it shows

- Rail: a globe icon in `PROJECT_APPS` opens the project's most recent browser pane, or a
  new one at the project's start address (§3.8). The project's open pages are listed
  under it in the tree with the terminals, each with a sleeping/live mark.
- A browser pane is a title tab by default. Dragging it onto a chat splits it, as a
  terminal pane does. "Open beside chat" is in the tab's menu.
- Links: a `http(s)` link in a transcript or a terminal opens in the project's browser
  pane (setting: `In Monocode | In my browser`, default in Monocode). "Open in my
  browser" is in the link's menu.
- The pane's own chrome, drawn by GPUI above the web view: back, forward, reload/stop,
  address field, a profile mark, a menu (open in my browser, find, zoom, sleep, clear this
  site's data, Web Inspector, allow/stop the agent). One row, 36 px.

### 3.3 The native host

New `monocode-gpui/src/browser/host.rs`, written from `monos/html.rs`'s paint logic but as
its own type, so the two cannot share a configuration by accident.

- Geometry: same clip-view-follows-the-content-mask code.
- **Overlays.** A native view is above everything GPUI draws in its rectangle. Menus,
  popovers, dialogs, Quick Open, drag previews and tooltips that reach into the pane would
  be covered. Rule: while anything is open over the workspace (the Shell already knows:
  `menu`, modal dialogs, `quick_open`, `tab_drag`, `pane_drag`, settings), the host is
  hidden and the pane paints the page's **last snapshot** as an image instead
  (`takeSnapshot(with:completionHandler:)`, taken on a timer while the page is idle and
  just before hiding). The page looks frozen for the moment, not gone.
- Focus: clicking the page makes the web view first responder; ⌘L, ⌘R, ⌘F, ⌘[ and ⌘] are
  bound in the pane's key context and work while the page has focus through a
  `performKeyEquivalent:` override that hands unhandled chords back to GPUI.
- Zoom follows the interface zoom and the page's own zoom (⌘+ / ⌘-).

### 3.4 What a development browser has to do

Each is a delegate method or a small piece of UI; none is hard, all are needed.

| Behaviour | How | Phase |
|---|---|---|
| Links that open a new window or tab | `createWebViewWith:` → a new page in a new pane | 2 |
| `https://localhost` with a self-signed certificate | `didReceive challenge` → a "trust for this profile" sheet | 2 |
| HTTP auth | same challenge → a name/password sheet | 2 |
| File upload | `runOpenPanelWith:` → the system open panel | 2 |
| Downloads | `WKDownload` → `~/Downloads`, a notice with "Show in Finder" | 2 |
| `alert` / `confirm` / `prompt` | GPUI sheets over the pane | 2 |
| Find in page | `find(_:configuration:)` with a find bar | 2 |
| Web Inspector | `isInspectable = true`; "Web Inspector" opens Safari's, which gives console, network, elements and storage for free | 2 |
| User agent and device size | §3.4.1: `customUserAgent`, a fixed-size host | 2 |
| Camera, microphone, location | `requestMediaCapturePermissionFor:` → ask per site, remembered per profile | 7 |
| Crashed or killed page | `webViewWebContentProcessDidTerminate` → the page becomes `Asleep` with a "reload" card | 4 |
| Reload on save | optional per page: reload when a file under the project changes | 7 |

### 3.4.1 User agent and device size

Testing how a site answers different browsers and devices is part of development, so
the user agent can be changed.

- **Default**: Safari's, so sites serve what Safari gets (and sign-ins are not refused for
  looking like an embedded view).
- **Per page**, from the pane's menu and shown as a small mark in the address row when it
  is not the default: a list of presets and "Custom…" for any string.
  Presets: Safari (Mac), Chrome (Mac), Chrome (Windows), Firefox (Mac), Edge (Windows),
  Safari (iPhone), Safari (iPad), Chrome (Android), Googlebot. The strings are a table in
  `monocode-core/src/browser/user_agent.rs` with the versions written next to them, to be
  refreshed now and then; a preset that names a version is only as current as the table.
- **Per project**, in project settings: the user agent new pages of that project start
  with. Custom strings the user has typed are remembered and offered again.
- How: `WKWebView.customUserAgent`. It changes both the `User-Agent` request header and
  `navigator.userAgent`. It applies to loads made after it is set, so changing it reloads
  the page. It is saved with the page and restored with it.
- **What it does not change**, said in the menu's footnote so nobody chases a ghost:
  `navigator.platform`, `navigator.vendor`, touch support and the engine itself. A site
  that sniffs those, or that needs a Chromium-only feature, still sees WebKit. Chrome's
  client hints (`Sec-CH-UA`) are not sent by WebKit under any user agent.
- **Device size**, beside it, because a phone user agent in a desktop-wide page tests
  little: a preset fixes the page's width and height inside the pane (iPhone 390 × 844,
  iPad 820 × 1180, a few common Android sizes, or typed numbers) and centres it, with
  "Fit pane" to go back. The page's layout viewport then matches; touch events are still
  mouse events.
- Agents: `browser_open` and `browser_navigate` take an optional `user_agent` (a preset
  name or a string) and `viewport` (width × height), and `browser_pages` reports both, so
  an agent can check a page as a phone and as a desktop browser in one task.

### 3.5 Data: sign-ins, cookies, cache, and clearing them

All of this is WebKit API (`WKWebsiteDataStore`, `WKHTTPCookieStore`); the work is the UI.

Settings → **Browser**:

- **Profiles**: the shared one and each project's own, with what each holds (number of
  sites, size on disk from `fetchDataRecords`). Actions per profile:
  - Clear **cache** (disk + memory cache, fetch cache, service worker registrations).
  - Clear **cookies and site data** (cookies, local and session storage, IndexedDB,
    WebSQL): signs out everywhere in that profile.
  - Clear **everything**, optionally only what changed in the last hour / day
    (`removeData(ofTypes:modifiedSince:)`).
  - Delete the profile (project profiles only; `remove(forIdentifier:)`).
- **Sites**: a searchable list of sites with data in the profile; remove one site's data
  (`removeData(ofTypes:for:)`). The pane menu has "Clear this site's data" for the site on
  screen, which is the everyday "sign me out here / reset this app" action.
- **Trusted certificates** and remembered **permissions**, listed and removable.

Where it lives: WebKit's own folder for the app's bundle id (the shared profile is the
app's default store) and per-identifier folders for project profiles. Monocode stores only
the profile list and page list in the workspace. Nothing is copied or synced.

Removing a project asks whether to delete its profile.

### 3.6 Memory: sleeping and destroying pages

Each live page is a WebKit content process, typically 150–500 MB for a development app.
The browser must never be why Monocode is slow.

- **Sleep** destroys the web view (the process goes with it) and keeps a snapshot. A page
  sleeps:
  - when the user says so (pane menu, rail row menu, "Sleep all pages" in the footer's
    memory chip and in Settings);
  - when its pane has not been on screen for *N* minutes (default 15; setting; never while
    the page plays audio, has a form with unsaved input, or an agent used it in the last
    two minutes);
  - when more than *M* pages are live (default 4): the least recently used sleeps;
  - when the system says memory is tight (`DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` warn):
    every page not on screen sleeps;
  - when WebKit kills it (`webViewWebContentProcessDidTerminate`).
  "Keep awake" on a page opts it out of the timed and counted rules, not out of memory
  pressure.
- **Destroy**: closing a pane destroys its page. "Close all pages of this project" and
  "Close every browser page" are in the rail menu and in Settings → Browser.
- **Quit and relaunch**: no page loads until its pane is shown.
- **What is shown**: per page, live or asleep and for how long. Memory per page is *not*
  available through public API (WebKit does not expose a page's process). Phase 0 checks
  whether the content processes can be attributed to Monocode by process inspection
  (`proc_pidinfo` over processes named `com.apple.WebKit.WebContent` whose responsible
  process is ours). If that works, the Browser settings page and the footer chip show
  megabytes per page and a total; if not, they show counts only and the rules above stay
  count- and time-based. The rules do not depend on the measurement.

### 3.7 Agents: the MCP server

`monocode-core/src/browser_bridge.rs`, built like `visual_bridge.rs`: same hub pattern,
same per-session capability, same stdio subcommand (`browser-mcp`), same per-provider
wiring. Server name `monocode_browser`. It is only offered to a session when the project's
"Agents may use the browser" is not Off (§3.9).

**Request/response.** A tool call arrives on the hub thread as `{session, token, tool,
arguments}`. The hub checks the capability and puts a `BrowserCall { session, tool,
arguments, reply: SyncSender<Result<Value, String>> }` on a channel. A task on the GPUI
main thread (started with the Shell) takes calls, finds the session's project, runs the
call against that project's pages through `BrowserManager`, and sends the reply when
WebKit's completion handler fires. The hub thread waits with a deadline (15 s default,
60 s for `browser_wait_for`); a call that outlives it is answered with a timeout and its
late result is dropped. Unlike visuals, calls are accepted between turns of a live session
too: an agent may read the page while it works.

**Tools.** Names are stable; the contract follows what works for the Chrome and Arc
drivers: a handle names exactly one thing and fails rather than finding a substitute.

| Tool | Does | Reads only |
|---|---|---|
| `browser_pages` | Lists the project's pages: id, address, title, live/asleep, which is on screen | yes |
| `browser_open` | Opens an address in a new page (shown in a pane beside the chat unless `background`) | no |
| `browser_navigate` | Goes to an address, back, forward, or reloads; waits for the load to settle | no |
| `browser_read` | The page as text, or as an outline of roles, names, values and element handles (`e12`). A password or card field's value is never returned | yes |
| `browser_find` | Elements matching text, role or a CSS selector, as handles | yes |
| `browser_click` | Clicks a handle: scrolled into view, hit-tested, refused if covered, hidden or detached | no |
| `browser_type` | Types into a handle, optionally submits; checked against what the field then holds | no |
| `browser_press` | A key or chord to the focused element | no |
| `browser_scroll` | By amount, or a handle into view | no |
| `browser_wait_for` | Until text appears or goes, a selector matches, or the network is quiet | yes |
| `browser_screenshot` | The visible page or one element, as an image | yes |
| `browser_console` | Console messages and uncaught errors since a mark | yes |
| `browser_network` | Requests made by the page since a mark: method, address, status, timing; bodies on request, size-capped | yes |
| `browser_eval` | Runs JavaScript in the page and returns the result as JSON | no |
| `browser_close` | Closes a page the agent opened | no |

How they are built:

- Reading and acting run through one injected script per page (`WKUserScript` at document
  start, in its own `WKContentWorld` so the page cannot see or change it). It builds the
  outline, keeps the handle table for the current read, and performs clicks and typing
  with real event sequences. Handles end with the next `browser_read` or navigation.
- Console and network: the same script wraps `console.*`, `error`/`unhandledrejection`,
  `fetch` and `XMLHttpRequest` in the page's world and posts to a
  `WKScriptMessageHandler`. A ring buffer per page (2,000 entries). This sees what the page
  does from script; it does not see images, stylesheets or navigations, which
  `browser_network` says in its description. Web Inspector remains the complete view for
  the user.
- Screenshots: `takeSnapshot`.
- A sleeping page is woken by any call that needs it; the reply says it was reloaded.
- Everything a page says (text, console, network) is returned inside a marked
  untrusted-content block, and the server's instructions tell the agent that page content
  is data, not instructions.

### 3.8 Development conveniences (last phase)

- A project's **start address**, set in project settings or picked up from a terminal
  line like `Local: http://localhost:5173` in that project's dock.
- **Reload on save**, per page.
- "**Add console errors to chat**" on the pane, which puts the last errors in the composer
  the way Add to Chat does for a selection.
- `browser_open` from an agent shows the pane beside the chat, so the user watches.

### 3.9 Safety

The browser will hold sign-ins. An agent connected to it can act with them, and a page can
carry text written to steer an agent. The rules:

- **Per project, "Agents may use the browser"**: `Off` | `Local addresses` |
  `Ask for other sites` | `Any site` (default, §7). Local means `localhost`, `127.0.0.1`, `[::1]`,
  `*.localhost`, `*.local`, and hosts the user adds for the project.
- Under `Ask`, a navigation or action on a new site raises the same approval card a tool
  call raises today, naming the site; allowed once, for the session, or always for the
  project. A redirect to a site not allowed stops the call.
- **Profile.** In `Local addresses` mode the agent's pages use the project's own profile
  (created on first use), not the shared one: an agent on localhost never sits next to the
  user's signed-in sessions. In the other modes the pages use the profile the project is
  set to, and the settings page says so in plain words.
- The agent acts only on **pages of its session's project**, and only on pages on screen
  or opened by it; it cannot read another project's pages.
- Never returned by any tool: values of password and payment fields, cookies, and the
  profile's storage. `browser_eval` is refused outside local addresses unless the project
  is on `Any site`.
- While an agent is using a page the pane shows a banner with the agent's name and
  **Stop**, which ends the session's access until the user turns it back on.
- The capability is the session's, as with visuals: no token, no access; a token for
  another session's project is refused.

## 4. Phases

Each ships alone: builds, clippy `-D warnings`, tests green, signed build installed.

| # | Package | Estimate |
|---|---|---|
| 0 | **Spike**, outside Monocode's UI (a scratch binary): persistent store survives relaunch; sign-in to GitHub, Google and the SOCRadar platform; `isInspectable`; process attribution for memory; `takeSnapshot` cost; key equivalents reaching GPUI | 1 day |
| 1 | Pane and page model, native host, address bar and navigation, rail icon and tree rows, save/restore (asleep) | 3.5 days |
| 2 | Development behaviours of §3.4 marked 2, user agent and device size (§3.4.1); overlay snapshot rule | 3.5 days |
| 3 | Profiles and data management (§3.5), Settings → Browser | 2 days |
| 4 | Sleep/destroy rules, memory pressure, crash recovery, the memory chip (§3.6) | 2 days |
| 5 | `browser_bridge`, provider wiring, read-only tools (`pages`, `open`, `navigate`, `read`, `find`, `wait_for`, `screenshot`, `console`, `network`), safety modes | 3.5 days |
| 6 | Acting tools (`click`, `type`, `press`, `scroll`, `eval`, `close`), approval cards, agent banner and Stop | 3 days |
| 7 | Conveniences (§3.8), permissions for camera/microphone/location | 2 days |

Total about **20.5 days**. Phases 0–2 give a usable browser pane (8 days). Phase 5 is when
an agent first sees a page (after 16 days if done in order; phases 3 and 4 can move
after 5 if agent access matters more than management, which brings it to 11.5 days).

Phase 0 decides three things before any of the rest is built: whether the sites the user
needs accept sign-in from an embedded WebKit view (Google is known to refuse some), whether
memory per page can be shown, and whether keyboard handling needs more than §3.3 says.

### 4.1 Phase 0 results (2026-11-13)

A scratch binary outside the repository (`/tmp/monocode-browser-spike`, objc2 0.6 and
objc2-web-kit 0.3.2, an `NSWindow` with one `WKWebView` on the default data store).

| Checked | Result |
|---|---|
| Default data store is persistent | Yes. A cookie and a `localStorage` entry written on `example.com` were read back after quitting and starting again. |
| `isInspectable` | Sets and reads back `true`. Attaching Safari's Web Inspector was not tried. |
| Custom user agent | `customUserAgent` set to a Chrome (Android) string; `navigator.userAgent` in the page returned exactly that string. |
| Snapshot cost | `takeSnapshot` of a 1200 × 800 page: 12 ms. Cheap enough to take before hiding the view (§3.3). |
| Memory per page | **Available, through a private call.** `-[WKWebView _webProcessIdentifier]` gives the page's content process; `proc_pidinfo(PROC_PIDTASKINFO)` on it gives its resident size (38 MB for `example.com`). Guarded by `respondsToSelector:`, falling back to counts if the selector ever goes. |
| Memory without a private call | Not from the scratch binary: the content process's responsible pid was the terminal that launched it, not the binary. Inside the bundled app it may be Monocode; not relied on. |
| API coverage in `objc2-web-kit` 0.3.2 | Compiles: per-identifier data stores, `removeData…modifiedSince`, `fetchDataRecords`, `WKHTTPCookieStore`, `WKContentWorld`, `WKUserScript` in a content world, `WKFindConfiguration`, `WKUIDelegate`, `WKScriptMessageHandler`, `WKDownload`. |

Not checked yet, and why:

- **Sign-in on the user's sites** (GitHub, Google, the SOCRadar platform) and **password
  AutoFill / passkeys**: need the user at the keyboard. Note for the test and for §3.4.1:
  a `WKWebView`'s own default user agent has no `Version/… Safari/…` part, which is one of
  the things Google looks at; "Safari's user agent" therefore means setting the full
  Safari string as the default, not leaving the field empty.
- **Key equivalents reaching GPUI** while the page has focus: needs the view inside a GPUI
  window; first task of phase 1.

Tried by the user in the scratch binary (2026-11-13): signing in works on the sites that
matter, and a sign-in is still there after quitting and starting again. **Copy and paste
did not work** there. The scratch binary has no menu, and ⌘C / ⌘V / ⌘X / ⌘A are key
equivalents of the Edit menu's items, which send `copy:`, `paste:`, `cut:` and
`selectAll:` to the first responder. Monocode has an Edit menu, but GPUI's menu items
dispatch GPUI actions rather than those selectors, so the same failure is expected inside
Monocode until the browser pane handles it: while a page has focus, the pane's Copy, Paste,
Cut, Select All, Undo and Redo actions call the web view's own responder methods. This is
checked first in phase 1, on the installed build.

### 4.2 What is built (2026-11-13)

Built and installed in one pass, in the order of §7. Checked by unit tests, by
`#[gpui::test]`s with no web view, and by the self-test of
`scripts/browser-selftest/`, which drives a real window of the development build.

| Part | State |
|---|---|
| Pane: its own tab or beside a chat, saved and restored asleep, closing frees it | Built. Model and restore are unit tested. |
| Rail: globe icon on the project row, pages listed under the project with its terminals | Built. Not looked at on screen. |
| Address row: back, forward, reload/stop, address or search, sleep, open in my browser, menu | Built. |
| Keyboard: click focuses, typing, ⌘A ⌘C ⌘V ⌘X ⌘Z, ⌘L ⌘R ⌘[ ⌘] ⌘F | Built. Click, typing, select all, copy and paste pass the self-test. |
| A page under a menu, dialog or drag shows a picture of itself, then comes back with the keyboard | Built; passes the self-test with Quick Open. The list of what counts as "over the panes" is `browser_covered` in `shell/browser_panes.rs`; an overlay missing from it is drawn under the page. |
| New-window links open a page of their own | Built; passes the self-test. `window.opener` is empty in the new page. |
| Downloads to `~/Downloads`, never over an existing file, with a line in the pane | Built; passes the self-test. |
| `alert` / `confirm` / `prompt`, HTTP sign-in, file upload | Built as AppKit sheets. Compiles; not exercised (a sheet would block an unattended run). |
| Certificates of this machine and its network accepted | Built: localhost, `.localhost`, `.local`, loopback and private addresses. Others are judged by the system; there is no "trust anyway". |
| Find in page, Web Inspector, clear this site's data, keep awake | Built. Find (`⌘F`) and custom user-agent inputs dismiss with Escape and return focus to the page; Web Inspector opens via `⌘⌥I` (`Page options › Web Inspector`) with WebKit developer extras enabled (falling back to a Safari Develop notice if WebKit's private `_inspector` selector is unavailable). Clear this site's data uses `host_of` (including IPv6 `[::1]`). |
| Per-page zoom (`25%`–`300%`) | Built. Stored per pane (`zoom_percent`), applied on top of window `content_zoom()` via `WKWebView.setPageZoom`, shown as a clickable `browser-zoom-badge` pill in the address row when `!= 100%`, and driven by `⌘+` / `⌘-` / `⌘0` while the active uncovered browser pane has focus (plus `Page options › Zoom`). Unit and live self-tested. |
| Reload on save | Built per page (`reload_on_save` in `Page options`). `Shell` runs a debounced (200 ms) `fs_watch::watch_checkout` watcher only for local projects that currently have an awake page with Reload on Save enabled, tearing down watchers when disabled or asleep. A change only reloads when at least one changed path is not ignored by Git (`fs_watch::source_changed`, `git check-ignore`): a server's own logs, databases and build output would otherwise reload the page without end. Outside a Git checkout every change counts. Unit and live self-tested. |
| Project start address & terminal dev-server detection | Built. Configured per project via `Project › Browser start page…` or `Page options › Use Current Page as Project Start Page` (`workspace.project_browser_urls`). When opening a new page with no explicit URL, falls back from the configured URL to the newest local dev-server URL detected in that project's dock/pane terminals (`terminal::recent_lines` + `monocode_core::browser::detect_dev_server_url`). Unit tested. |
| User agent presets and custom, device sizes | Built. A page as Chrome (Android) at 412 × 915 reported that user agent and that size in the agent self-test. |
| MCP server `monocode_browser`, 15 tools, for Claude, Codex and ACP providers | Built. Every tool was run against a real page by `agent.py` (OpenCode is not wired: it takes no MCP server from Monocode for visuals either). |
| Agent access: Any site (default), Local addresses, Off; Stop on the page's banner | Built and unit tested. **"Ask for other sites" is not built.** |
| Sleep: by hand, after N minutes off screen, more than M loaded, memory pressure, a killed page | Built; the rules are unit tested. They do not look for playing audio or unsaved form input. |
| Settings › Browser: agent access, links, sleep, clear cache / site data, sites list | Built. Renders in the settings test; clearing is not exercised. |
| Links in replies and terminals open in a browser pane | Built for transcript links and terminal links; a setting sends them to the user's browser instead. |
| Add Console Errors to Chat | Built. Not exercised. |

Not built, against this plan:

- **Per-project profiles.** Every page uses the one shared store. The model
  has no profile field yet; §3.1's `Project` and `Private` kinds are open.
- **"Ask for other sites"**, with its approval card.
- **Camera, microphone and location** prompts: a page asking is refused.
- **Memory per page in Settings**; it is shown in the pane's menu only.

Differences from the design worth knowing:

- The native view is not hidden before each frame and shown again by paint, as
  the Mono HTML view is: AppKit takes the keyboard from a hidden view, which is
  why a field could not be typed into at first. A page is taken off screen only
  after a frame that did not paint it (`PageFrame`).
- A page reports its state both through WebKit's navigation callbacks and by
  being read twice a second, since the callback for a finished load did not
  always arrive and a page can retitle itself with no load at all.
- An agent's click is a real mouse event when the page is in front of the
  user, and a scripted `click()` when it is not being drawn. Typing and keys
  are always scripted (`insertText`, then the value setter as a fallback).
- An agent's page that the user is not looking at still has a web view, kept
  off screen; a screenshot of it may be blank.

## 5. Tests

- `monocode-core` (no UI): profile and page model round-trips through the saved
  workspace; a page restored is asleep; the origin rules of §3.9 (local, added hosts,
  redirects); the bridge's capability checks (wrong session, wrong token, ended session);
  tool schemas; the outline builder's script run against fixture HTML in a headless
  `WKWebView` where the test host allows it, otherwise against a JS engine-free snapshot
  test of the script's input/output contract.
- `monocode-gpui` (`#[gpui::test]`): a browser pane opens as a tab and splits beside a
  chat; closing its owner's project closes it; the tree lists pages; sleep rules with a
  fake clock (idle, count, pinned awake); the overlay rule hides the host when a menu
  opens; an agent call for a project with access Off is refused; the approval card for a
  new site. The native web view is behind a trait (`PageHost`) so these tests run with a
  fake host that records calls; the real host is exercised by the live check.
- Live check on the installed build: sign in on a site, quit, relaunch, still signed in;
  clear that site's data, signed out; a page reloaded as Chrome (Android) at 412 × 915
  reports that user agent to a request-echo page; `https://localhost` with a self-signed
  certificate;
  a download and an upload; an agent session opens the project's dev server, reads it,
  clicks, and reports a console error.
- Never touched by tests: the real WebKit data stores (test profiles are non-persistent)
  and the real data directory.

## 6. Risks

- **Sign-in refused in an embedded view.** Google blocks some embedded web views;
  sites that sign in through Google inherit it. Mitigation: Safari's user agent, and
  phase 0 tests the user's sites first. If a needed site refuses, that site stays in the
  user's own browser; the plan does not depend on it.
- **No password filling.** Whether macOS password AutoFill and passkeys work in a
  third-party `WKWebView` is not verified; phase 0 finds out. Worst case passwords are
  pasted once and the cookie keeps the session.
- **The native view covers GPUI.** §3.3's snapshot rule covers the cases known today.
  Any new overlay has to be added to the rule; a test asserts the list.
- **Keyboard.** A focused web view takes key events before GPUI. The key-equivalent
  hand-back is the part most likely to need iteration.
- **Memory numbers** may not be available (§3.6); the rules work without them.
- **Prompt injection through pages** cannot be removed, only bounded: the default mode
  keeps agents on local addresses in a profile with no sign-ins.
- **Upstream.** All of this is macOS-only fork code. It lives in new files
  (`browser/*.rs`, `browser_bridge.rs`); upstream files get small hunks where terminal
  panes already have them (`workspace.rs`, `title_tabs.rs`, `project_rail_tree.rs`,
  `provider.rs`). If upstream adds a browser of its own, ours is dropped.
- **WebKit API coverage in `objc2-web-kit`.** The features needed (`WKDownload`,
  `WKUIDelegate`, `WKScriptMessageHandler`, `WKContentWorld`, `WKHTTPCookieStore`,
  `WKWebsiteDataRecord`) are separate cargo features of the crate; phase 0 enables them
  and confirms they build.

## 7. Decisions (2026-11-13)

- **Profile**: a project uses the **shared** profile unless it is given its own.
- **Agents**: "Agents may use the browser" defaults to **Any site**, in the project's
  profile, which is the shared one: an agent can go anywhere and acts with the sign-ins
  that profile holds. The setting stays, so a project can be narrowed to local addresses,
  to asking, or to Off. What §3.9 keeps regardless: the session capability, pages of the
  session's own project only, the banner with Stop, page content returned as untrusted
  data, and no tool returning password or payment field values, cookies or storage.
- **Order**: 0, 1, 2, 5, 6, 4, 3, 7. Agents reach the browser after the pane works;
  the automatic sleep rules come before data management. Closing a page and "Sleep" /
  "Close all pages" by hand are part of phase 1, so memory can be freed from the start.
- **Links** in chats and terminals all open in Monocode's browser. "Open in my browser"
  is in the link's menu and the pane's.
- **Sleep defaults**: 15 minutes off screen, at most 4 live pages.
