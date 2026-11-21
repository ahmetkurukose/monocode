# Orca "full browser" subsystems: analysis for a native (CEF/Chromium) port

Source: `/tmp/orca-research/orca` (Electron 43, TypeScript). All paths below are relative to that root.
Scope: Chrome extensions, passwords, passkeys/WebAuthn, certificate trust, site settings/permissions,
downloads, printing, system dialogs, edit protection, visibility reporting, guest (tab) discard.

The same pattern shows up everywhere: Electron gives Orca a Chromium `<webview>` guest plus a few session
hooks. Anything that needs browser UI (prompts, bubbles, pickers, toolbars) or browser services (password
manager, extension APIs Electron leaves out) Orca wrote itself, usually in TypeScript, with main-process
policy and isolated-world preloads. Many of these subsystems are also shaped by Orca's **agent-driven
browsing** model. Any page can be "agent-led". Whenever an agent drives it, no OS dialog may take the
foreground (print, Windows Hello, file chooser), and prompts must be answerable by an agent, a phone or a
remote client as well as the desktop user.

---

## 1. Chrome extensions

### Size
- `src/main/browser/extensions`: 175 non-test files, ~24.1k lines
- `src/preload/browser/extensions`: 13 files, ~2.6k lines. Plus `src/preload/browser-extension-preload.ts`, `browser-extension-worker-shim.ts`, `browser-extension-content-script-api.ts`
- `src/shared/browser/extensions`: 13 files, ~650 lines
- Renderer UI: `src/renderer/src/components/settings/browser/extensions/*`, `src/renderer/src/components/browser-pane/assemble-chrome/extensions/*`

### Overall architecture
1. **Loading**: Electron's own extension loader does the loading: `session.extensions.loadExtension(dir, { allowFileAccess: false })`
   (`profile-sessions/browser-extension-session-load.ts`). This loads extensions into one session per browser
   profile. Electron supplies the extension process, `chrome-extension://` origin, content-script injection
   into isolated worlds, MV3 service-worker hosting, `chrome.runtime` messaging, `chrome.storage.local/session`,
   `chrome.i18n`, `chrome.scripting.executeScript/insertCSS`, part of `chrome.tabs`, and `management.getSelf/uninstallSelf`.
   Orca treats everything else as missing or broken and replaces it.
2. **Orca-owned copy of each extension** (`installation/browser-extension-load-directory.ts`). Each extension
   loads from a folder Orca owns. Store installs are unpacked in place; unpacked dev folders are copied. That
   lets Orca rewrite the manifest without touching the original:
   - `background.service_worker` is pointed at a generated wrapper `orca-background.js`. The wrapper runs
     `importScripts('/orca-extension-api.js', '/<original>')`, or `import` for module workers
     (`installation/browser-extension-background-wrapper.ts`).
   - CSP `connect-src` gains `orca-extension-api:` so the worker can reach Orca's API.
   - It writes a per-extension secret token file (`orca-extension-api-token.json`) and keeps the original manifest (`orca-original-manifest.json`).
   - It inserts `orca-content-script-api.js` as the first entry of every isolated-world content script.
   - **Manifest V3 only**: "Orca supports Manifest V3 extensions only".
3. **The API bridge**. There are three transports, because Electron's preload support differs by context:
   - **Extension pages** (popups, options, side panels, tabs): a session frame preload (`session.registerPreloadScript({type:'frame'})`)
     installs `chrome.*` members in the main world with `contextBridge.executeInMainWorld`. Calls go over IPC
     (`browserExtension:invoke`), and main reads the caller from `event.senderFrame`, never from the payload.
   - **Service workers**: a worker preload crashes the renderer during GC (electron#52310). Orca therefore runs
     its shim *inside* the worker and talks to main over `fetch()` to a privileged custom scheme,
     `orca-extension-api://api/call`. Events stream back over a long-lived NDJSON response on `orca-extension-api://api/events`.
     Main identifies the caller three ways: the session that handled the scheme, the request initiator origin,
     and the per-extension token, which only that extension's files can read
     (`shared/browser/extensions/browser-extension-worker-api.ts`, `api-runtime/browser-extension-api-scheme.ts`,
     `api-runtime/browser-extension-worker-api-host.ts`, `worker-event-stream.ts`).
   - **Content scripts** run in Chromium's per-extension isolated world, which no Orca preload reaches. Their
     calls (mainly `storage.sync`) are relayed as hidden `runtime` messages to the extension's own worker, which
     makes the call through main (`preload/browser/extensions/messaging/browser-extension-content-script-relay.ts`).
4. **Namespace registry** (`api-runtime/browser-extension-namespace-registry.ts`, `register-browser-extension-namespaces.ts`).
   Each namespace declares a required manifest permission, Zod-validated methods and event names. The preload
   mirror (`preload/browser/extensions/api/browser-extension-api-surface.ts`) says what to install. A method can be
   `native: {...}`: Electron's implementation runs first, then Orca post-processes the result. This is how
   `tabs.query` and `tabs.get` merge Electron's URL/title answer with Orca's window/active/discarded model.
5. **Tab/window model**: Orca's tabs are `<webview>` guests in a single main window. Orca builds a Chrome-like
   model for `tabs.*` and `windows.*` (`tabs-and-windows/*`):
   - Synthetic tab ids for *discarded* tabs (restored pages whose webview was never mounted), replaced via `tabs.onReplaced`.
   - Popout windows created by `windows.create` are real `BrowserWindow`s in the extension session.
   - documentIds and frame projection for webNavigation and scripting.
   - "agent caller" attribution: which agent-led page an extension call acts for.

### Chrome Web Store install (yes, real CRX installs)
- **Store page integration**: on `chromewebstore.google.com`, a main-world script installs
  `chrome.webstorePrivate` (`beginInstallWithManifest3`, `completeInstall`, `getExtensionStatus`) and the
  `chrome.management.*` calls the store page makes. The store's "Add to Chrome" button then works
  (`preload/browser/extensions/web-store/browser-extension-web-store-main-world.ts`, `web-store-bridge.ts`,
  `web-store-wording.ts`, which rewrites "Add to Chrome" text; `main/.../web-store/browser-extension-webstore-bridge-host.ts`
  validates each call against the sender with Zod). The approach credits **electron-chrome-web-store** (MIT, Samuel Maddock).
- **Download**: `https://clients2.google.com/service/update2/crx?response=redirect&acceptformat=crx3&prodversion=<chrome ver>&x=id%3D<id>%26installsource%3Dondemand%26uc`.
  Redirects are followed manually, and only https Google hosts are allowed (google.com, googleusercontent.com,
  googleapis.com, gvt1.com). Credentials are omitted. The body is capped at 256 MB, and size and sha256 are checked
  when known (`web-store/browser-extension-webstore-download.ts`). It deliberately avoids `session.fetch`, which
  breaks once a request-blocking extension is loaded (electron#54685).
- **CRX3 verification**: a hand-written protobuf reader and verifier ported from Chromium's
  `components/crx_file/crx_verifier.cc` (`installation/browser-extension-crx.ts`). It checks the `Cr24` magic and
  version 3, and verifies every RSA/ECDSA proof over `"CRX3 SignedData\0" + header + archive`. One key must hash
  to the declared id (SHA-256, first 16 bytes, hex mapped to a-p), and for store installs one proof must come from
  the Chrome Web Store publisher key (sha256 `61f7f2a6…`). The developer public key is written into `manifest.key`
  so Electron derives the same extension id.
- **Install pipeline**: stage, then show a permission prompt in Orca's UI with Chrome-equivalent permission
  warnings (`shared/.../browser-extension-permission-warnings.ts`, `permissions/browser-extension-install-prompt.ts`),
  then commit to `<root>/<id>/<version>/` side by side with the old version. Orca reloads the extension wherever it
  is enabled before deleting the old version, all under a mutation lock (`installation/browser-extension-webstore-install.ts`, `browser-extension-mutation-lock.ts`).
- **Auto-update**: Omaha 3.1 JSON POST to `https://update.googleapis.com/service/update2/json`. The first check runs
  60 s after start, then every 5 h, with local version comparison. An update that adds permission warnings is
  *held* until the user approves it (`web-store/browser-extension-webstore-updater.ts`, `browser-extension-held-updates.ts`).
- Also supported: loading an unpacked folder (dev), installing by store URL or ID from Settings
  (`BrowserExtensionsWebStoreInstall.tsx`), per-profile enablement, a live on/off switch for all extensions without
  restart (`browser-extension-support.ts`), and profile data removal on uninstall.

### API support matrix

| API | Status | Notes |
|---|---|---|
| `runtime.*` (messaging, getManifest, getURL, onMessage, connect…) | Electron native | Orca adds `openOptionsPage`, `getContexts` (+ Orca side panels), and its own `onInstalled`/`onStartup` (Electron never fires them) |
| `runtime.connectNative` / `sendNativeMessage` | **Orca-built** | Full native messaging, see below |
| `storage.local` / `storage.session` | Electron native | Electron never fires `storage.onChanged` in workers. Orca relays page writes to the worker (`api/storage-relay.ts`) |
| `storage.sync` | **Orca-built** | Electron disables it. Main stores it per profile and extension on disk, enforces Chrome quotas (100 KB, 8 KB/item, 512 items), fires onChanged. No cloud sync (like Chrome when signed out) |
| `storage.managed` | Inert | Returns `{}` (no enterprise policy) |
| `tabs.*` | Hybrid | query/get go through Electron, then Orca adds window/active/index/pinned/discarded. Orca implements create, duplicate, remove, update, move, discard, goBack/Forward, detectLanguage, `captureVisibleTab` (with activeTab and a 2/s quota), getCurrent; events onActivated/Created/Updated/Removed/Replaced. `tabs.sendMessage` is native |
| `windows.*` | **Orca-built** | getAll/get/getCurrent/getLastFocused/create/update/remove + events. Main window plus popouts |
| `action` / `browserAction` | **Orca-built** | All action state: title, icon, popup, badge text and colors, enable/disable, `openPopup`, `getUserSettings`, onClicked |
| `contextMenus` | **Orca-built** | Model ported from Chromium MenuManager (radio runs, checkbox flip, parent removal, ACTION_MENU_TOP_LEVEL_LIMIT=6). The menu is rendered by Orca's renderer |
| `commands` | **Orca-built** | Manifest shortcuts parsed per platform (Chromium commands.json). Orca's own shortcuts take priority |
| `sidePanel` | **Orca-built** | setOptions/getOptions/setPanelBehavior/getPanelBehavior/open/close/getLayout, onOpened/onClosed. Hosted as a webview next to the tab |
| `declarativeNetRequest` | **Orca-built, full engine** | Electron 43 "accepts every call and enforces nothing". Orca ported Chromium's RulesetManager, url_pattern_index (n-gram index), rule parsing, regex filters, redirect and header actions, static/dynamic/session rules, allowAllRequests frame state, getMatchedRules, testMatchOutcome, and Chrome's limits (30k static guaranteed, 5k dynamic/session…). Hooked into the session's `webRequest.onBeforeRequest` / `onHeadersReceived`. Requests are held until rulesets load (max 5 s). `onRuleMatchedDebug` stays native |
| `webRequest` | **Orca-built, observe-only** | Electron 43 delivers no webRequest events to MV3 workers. Orca forwards what its own session handlers see. No blocking (MV3 store extensions can't block in Chrome either). `onAuthRequired` stays native |
| `webNavigation` | **Orca-built** | Events plus getFrame/getAllFrames with documentId |
| `scripting` | Hybrid | executeScript/insertCSS are native. Orca persists and restores `registerContentScripts`/update/unregister/getRegistered by running Electron's native calls in a hidden extension page |
| `cookies` | **Orca-built** | get/getAll/set/remove/getAllCookieStores/onChanged on the profile session's cookie store ("0") |
| `downloads` | **Orca-built (partial)** | `download()` (with `filename` and `saveAs` via the system save dialog), onCreated/onChanged. No search/pause/resume/open |
| `notifications` | **Orca-built** | create/update/clear/getAll/getPermissionLevel + click/button/close events. Icons come only from the extension's own package |
| `permissions` | **Orca-built** | contains/getAll/request/remove + events, with an Orca prompt. Runtime host grants per profile. `addHostAccessRequest` is inert |
| activeTab | **Orca-built (partial)** | The grant covers the URL/title of the invoked tab and `captureVisibleTab`. It does **not** grant script access: Electron has no API to add Chromium's per-tab grant, so `scripting.executeScript` on an activeTab-only tab fails |
| `privacy` | **Orca-built (bookkeeping)** | ChromeSetting get/set/clear for `services.passwordSavingEnabled`, autofill*, network*, websites*. The values are stored but not enforced. Password managers (Bitwarden etc.) disable the built-in manager through these |
| `management` | Hybrid | getSelf/uninstallSelf are native. Orca adds getAll/get/setEnabled |
| `extension.*` | Hybrid | Orca adds isAllowedFileSchemeAccess/isAllowedIncognitoAccess (always false), getViews → [] |
| `omnibox`, `fontSettings`, `dom.openOrClosedShadowRoot`, `runtime.onPerformanceWarning` | Inert stubs | Correct shape, no-op (`preload/.../browser-extension-inert-apis.ts`) |
| `offscreen`, `alarms`, `i18n` | Left to Electron | No Orca code. Offscreen documents run without context isolation, so Orca skips its preload there |
| `identity` | **Unsupported** (flagged in UI) | `shared/.../browser-extension-unsupported-features.ts` |
| `history`, `bookmarks`, `tabGroups`, `debugger`, `proxy`, `tts`, `userScripts`, `devtools.*`, MV2 | Not implemented | Listed only for install warnings. MV2 is rejected |

### Toolbar, action popups, side panels, context menus
- **Toolbar** (`actions/browser-extension-toolbar.ts`, `toolbar-pins.ts`, `action-registry.ts`, `action-state.ts`, `action-icon.ts`).
  Orca keeps per-tab and global action state, renders icons as data URLs, and pins icons per profile. The renderer
  draws the buttons in Orca's browser chrome.
- **Popups** (`actions/browser-extension-popup-host.ts`, `popup-geometry.ts`, `popup-size-watch.ts`). A frameless,
  non-resizable child `BrowserWindow` in the extension session, anchored under the button. It is created at the
  maximum size (800×600) because pages lay out from `innerHeight` at creation, then shrinks to the content size the
  page reports. It closes on blur, and a click on the button within 400 ms of a blur-close counts as "close" (Chrome's behavior).
- **Side panels** (`side-panel/*`). The renderer asks for a webview src and main hands out a one-use attach token,
  checked in `will-attach-webview`. The guest is claimed on `web-contents-created` so it is never treated as a tab.
  There are per-tab and workspace-wide panels.
- **Context menus** (`context-menus/*`). On a page right-click, main snapshots the target (frame, link, selection,
  media, editable, documentId) under a token. The renderer shows extension items in Orca's own menu, and a click
  only names token and item key. The click is ignored if the tab has navigated since.

### Native messaging (`native-messaging/*`, `api/native-messaging.ts`, `preload/.../browser-extension-native-messaging.ts`)
- Host manifest lookup, in order: Orca's own `userData/NativeMessagingHosts`, then Chrome's and Chromium's user and
  system locations, then other Chromium browsers' folders (Brave, Edge, Vivaldi, Arc, Helium, Opera). Many desktop
  apps (1Password, Bitwarden…) register only with browsers they detect. On Windows this means HKCU/HKLM registry keys.
- Checks `allowed_origins`, spawns the host with Chrome's 4-byte little-endian length framing over stdio and a
  write-admission queue, gives a 2 s grace period after stdin closes before killing the host (Chrome's
  EnsureProcessTerminated), a 15 s start timeout, and a capped stderr log. On Windows a supervisor child puts the host
  in a kill-on-close Job object.

### Lifecycle
- Extensions load per profile session when the profile's first page attaches, and unload after a grace period when
  its last page closes, as Chromium does when a profile's last window closes (`profile-sessions/browser-extension-idle-profiles.ts`).
- Orca waits for worker registration, because `loadExtension` resolves before the SW is registered (`worker-registration.ts`).

### What a CEF/Chromium port would get natively
- **CEF Chrome runtime style** (Chrome-style browsers, the default since about M125; the legacy Alloy *runtime* was removed in about M128):
  Chrome's real extension system: Web Store install and update, all chrome.* APIs, MV3 service workers, DNR,
  webRequest, native messaging (it reads Chrome's NativeMessagingHosts locations for the CEF user data dir), and
  `storage.sync` (local only without sign-in). Toolbar, popups and side panel only appear if you use the Chrome
  toolbar or Views UI inside a `CefWindow`/`CefBrowserView` (`CEF_CTT_NORMAL` toolbar). Without it, you have no
  supported public API to draw action buttons in your own UI, open popups, or read action state.
- **Alloy style** browsers (required for **windowless/OSR rendering**) have *no* general extension support.
  `CefRequestContext::LoadExtension` and the `CefExtension` API were removed along with the Alloy runtime. Only the
  built-in PDF viewer extension remains.
- **Implication for a GPUI app**: rendering web content inside GPUI via OSR means Alloy style, so no extensions, and
  you would end up rebuilding most of what Orca built, without even Electron's base. Hosting Chrome-style
  `CefBrowserView`s as native child views gets real extensions, but the extension UI is Chrome's Views UI, not GPUI.
  A middle path: Chrome-style browsers plus your own toolbar driven by extension state, which needs patching CEF or
  using internal Chromium APIs (`ExtensionActionManager`, `ExtensionActionRunner`). A raw Chromium embedder
  (content_shell-style plus `//extensions`, as Brave/Vivaldi/Arc-style forks do) gets the full extension stack with
  your own UI, at the cost of maintaining a Chromium fork.
- Parts of Orca's design worth porting regardless: CRX3 verification (if you ever sideload), the Omaha update request
  shape, the permission-warning text table, and the native-messaging host search list, which goes beyond Chrome's to
  include other Chromium browsers' folders.

---

## 2. Passwords ("Orca Vault")

### Key files
- Main: `src/main/browser-passwords/*` (15 files, ~2.3k lines). IPC: `src/main/ipc/browser/browser-passwords-*.ts`
- Guest preload (isolated world): `src/preload/browser/passwords/*` (10 files, ~1.4k lines)
- Renderer API: `src/preload/api/browser/browser-passwords-api.ts`, `browser-passwords-bridge.ts`. Settings UI: `src/renderer/src/components/settings/browser/passwords/*`

### Mechanism
- **Storage** (`browser-password-vault-file.ts`, `browser-password-store.ts`, `browser-password-vaults.ts`).
  One vault file per browser profile (format `orca-browser-passwords-v1`). The index (sites, usernames, never-save
  origins) is sealed as one blob, and **each password and each note is sealed separately**, so a fill unseals exactly
  one secret. Sealing goes through the host `SecretStore` = Electron `safeStorage`, which uses a key from the **macOS
  Keychain**, Windows DPAPI, or Linux libsecret/kwallet. The store fails closed: if encryption is unavailable or the
  vault can't be unsealed, every operation reports "unavailable" and the file is never rewritten. There is a guard
  against headless Keychain prompt hangs (`main/host/safe-storage-availability.ts`).
- **Detection** (`preload/.../browser-password-form-scan.ts`, `form-parser.ts`, `field-rules.ts`). Runs in the
  guest's isolated world and exposes nothing to page script. It groups inputs by outermost `<form>` (plus one formless
  group), walks open shadow roots, and classifies forms as login/signup/change/username-only. The classifier is a
  TypeScript port of the behavior of **Chromium's `form_data_parser.cc`**, plus Bitwarden's newsletter rule and an
  OTP/2FA field exclusion regex. Element handles are random tokens held in the isolated world, never written onto DOM nodes.
- **Save flow** (`submit-tracker.ts` in the guest; `browser-password-login-tracker.ts`, `save-decision.ts`,
  `save-prompts.ts` in main). Submit gestures are reported, then main decides success Chrome/Proton-style:
  - same document: success only if the form disappears; a form still visible after 6 s means failure.
  - cross-document navigation: failure if the next document shows the same form again within 2.5 s.
  - a username typed on step 1 of a multi-step sign-in is kept for 5 minutes.
  - Chrome's save-or-update table decides the offer; the "never for this site" list is respected.
  - The prompt stays reachable for 5 minutes (Chrome's key icon). **Main keeps the password**; the renderer only
    learns the site and username.
- **Autofill** (`focus-tracker.ts`, `field-fill.ts`, `browser-password-picker-keyboard.ts`, `browser-password-guest-service.ts`).
  When a credential field gets focus, the guest reports its rectangle and Orca's renderer draws a dropdown over the
  webview. The dropdown hides on blur, scroll or resize. On pick, main re-checks the origin (exact origin match), and
  the guest re-checks that the fields are still present, visible and editable, then writes values so that React/Vue
  controlled inputs notice. Keyboard navigation in the picker is supported. Every guest payload is re-validated in
  main as untrusted (`guest-payloads.ts`).
- **Re-auth** (`browser-password-reauth.ts`): on macOS, Touch ID (`systemPreferences.promptTouchID`) before reveal or
  copy, valid for 5 minutes, and optionally before every fill. This only gates Orca's UI; it releases no key. Windows
  Hello and Linux have no Electron API, so they pass without a check.
- **Import**:
  - **CSV** (`browser-password-csv-import.ts`): RFC 4180 parser with header aliases for Chrome, Edge, Firefox,
    Safari, 1Password, Bitwarden, LastPass, Dashlane and Proton Pass exports.
  - **Directly from Chromium browsers** (`browser-password-chromium-import.ts`): copies `Login Data` and
    `Login Data For Account` (the Google-synced store) from the chosen profile and reads `logins` with
    `node:sqlite` (html scheme only, skips blacklisted rows, converts the 1601 epoch). It decrypts `password_value`
    with the browser's Safe Storage key, reusing the cookie-import key code (`browser/cookies/sources/browser-cookie-key.ts`):
    macOS `security find-generic-password -s "Chrome Safe Storage"`, Linux `secret-tool`, Windows DPAPI via PowerShell.
    App-bound (v20) encryption on Windows is counted as undecryptable.
  - Merge (`import-merge.ts`): a new origin+username is added; same password is a duplicate; a different password is a
    conflict, and the saved one is kept.
- Not in v1: password generation (explicitly), HTTP-auth credential saving, passkeys in the vault, sync, breach checks.
- `chrome.privacy.services.passwordSavingEnabled` is stored, so extension password managers can turn the built-in one off.

### Electron's part vs Orca's
Electron provides nothing beyond `safeStorage` and Touch ID; detection, parsing, save heuristics, fill, UI, import
and storage format are all Orca's.

### CEF/Chromium notes
- Alloy style has no password manager or autofill.
- Chrome style includes Chrome's `PasswordManager`/autofill components and bubbles. In CEF this support is partial:
  no sync or Google Password Manager, profile storage under the CEF cache path, and some UI may be disabled or
  unsupported. Don't count on it, especially for OSR or custom-UI hosting.
- A raw Chromium embedder can reuse `components/password_manager` (form parsing, save/update logic, LoginDatabase,
  OSCrypt with Keychain) but must supply the `PasswordManagerClient` UI.
- For a Rust/CEF app, Orca's design ports well: a native vault (Keychain/DPAPI via the `keyring` crate or
  Security.framework, ideally with Keychain access control or LocalAuthentication so biometrics actually release the
  key), a JS detector injected through `CefRenderProcessHandler` in an isolated world (CEF has no isolated-world API
  for script injection; use a V8 context extension, or CDP `Page.createIsolatedWorld` / `addScriptToEvaluateOnNewDocument` with `worldName`),
  and native dropdown UI. The Chromium-import code is directly reusable.

---

## 3. Passkeys / WebAuthn

### Key files
`src/main/browser/webauthn/browser-webauthn-access.ts`, `browser-webauthn-account-picker.ts`, `browser-passkey-requests.ts`;
`src/preload/browser/passkeys/browser-passkey-request-watch.ts`; `src/preload/browser-system-dialogs.ts`;
`src/shared/browser/prompts/browser-passkey-request.ts`, `browser-webauthn-account.ts`;
`src/renderer/src/components/browser-pane/page-prompts/browser-webauthn-account-dialog.tsx`.

### Mechanism
- **Security keys (USB HID)**: `setDevicePermissionHandler` allows only HID devices with FIDO usage page `0xF1D0` on
  secure origins (https or localhost). `select-hid-device` auto-picks the first FIDO device. The permission check
  grants `hid` on secure origins.
- **Account picker**: Electron's `select-webauthn-account` session event. With 0 or 1 account it answers directly;
  with several, Orca shows its own React dialog (60 s timeout, cancelled on navigation or destroy).
- **Platform authenticator**: Chromium hands the request to the OS (Windows Hello on Windows; on macOS, Touch ID /
  iCloud Keychain depending on Electron's build). Orca adds no macOS-specific code.
- **Pending-request watch** (main-world script via session frame preload). Electron tells main nothing about pending
  WebAuthn requests, so Orca wraps `navigator.credentials.get/create` (non-conditional ones) to report pending/settled
  per frame. The wrapper swaps the AbortSignal through a Proxy so main can cancel. On **Windows, when an agent leads
  the page**, requests are *held* and not sent to the OS, because Windows Hello would take the foreground. When the
  user returns, held requests are cancelled with `NotAllowedError` so the page offers a retry. Agents get the message
  "waiting for a passkey… ask the user or run `<dismiss>`".

### CEF/Chromium notes
- Chrome style: Chrome's authenticator request dialogs, including the account picker, caBLE/hybrid (phone QR), and
  security-key UI. Platform passkeys on macOS need the browser entitlement
  (`com.apple.developer.web-browser.public-key-credential`) and a proper signed app.
- Alloy style: WebAuthn works only to the extent the platform/HID path needs no Chrome UI. Account selection and
  PIN entry have no UI by default. Expect to patch or implement `AuthenticatorRequestClientDelegate`, which CEF does not
  expose as a public handler. Budget for this as a gap.
- Orca's request-watch and agent-hold logic is plain injected JS and ports as is.

---

## 4. Certificate trust

### Key files
`src/main/browser/certificate-trust/browser-certificate-trust-controller.ts`, `browser-certificate-request-guard.ts`,
`browser-certificate-challenge.ts`, `browser-certificate-identity.ts`; `src/shared/browser/certificate-trust/*`.

### Mechanism
- Runs on Electron's `certificate-error` event.
- Proceeding past an error is offered **only** for main-frame https navigations with `ERR_CERT_AUTHORITY_INVALID`
  (-202) to **local/dev hosts** (`isEligibleLocalCertificateHost`), i.e. self-signed dev certs. All other cert errors
  are refused, with an interstitial overlay in the renderer. The overlay only advises on exact Chromium cert error codes (-200…-219).
- A grant is bound to the identity triple {secure endpoint (scheme+host+port), leaf cert SHA-256, error}, and to
  **one WebContents**. Challenges expire after 5 minutes, with at most 32 pending and 32 grants.
- **Request guard**: Chromium caches "allow this cert" per session, which would leak a grant to every tab in the
  profile. Orca therefore holds `session.webRequest.onBeforeRequest` and cancels requests to a granted endpoint from
  any WebContents that wasn't granted. The guard also gates requests until the proxy configuration is ready.
- Grants are revoked when the guest is retired.

### CEF/Chromium notes
- CEF has `CefRequestHandler::OnCertificateError(browser, cert_error, url, ssl_info, callback)` and
  `OnSelectClientCertificate`. Chrome style can show Chrome's interstitials.
- CEF also caches decisions per request context, so per-tab scoping needs the same guard via
  `CefResourceRequestHandler::OnBeforeResourceLoad` (return `RV_CANCEL`) or a separate request context per tab.
- `CefSSLInfo`/`CefX509Certificate` give the DER encoding for fingerprinting.

---

## 5. Site settings and permissions

### Key files
`src/main/browser/site-settings/permissions/*` (`browser-session-permission-handlers.ts`, `browser-session-permission-policy.ts`,
`browser-site-permission-prompt.ts`, `browser-site-permission-store.ts`, `browser-site-permission-memory.ts`, `browser-media-access.ts`),
`src/main/browser/site-settings/display/*`, `browser-profile-site-store.ts`; `src/shared/browser/site-settings/*`;
`src/main/browser/sessions/partition-policies/browser-session-partition-policies.ts`.

### Mechanism
- **Prompted permissions** (Chrome-like per-origin allow/block): camera, microphone, location, notifications,
  clipboard (read), MIDI, window-management, idle-detection, local-network. Also `external-app:<scheme>`, which can
  only be allowed.
- **Auto-granted**: fullscreen, clipboard-sanitized-write, persistent-storage, pointerLock, storage-access, and
  fileSystem (File System Access; the user's pick is the consent). Everything else is denied, and the user is told.
- Uses `setPermissionRequestHandler` (async: Orca prompt → remember) and `setPermissionCheckHandler` (sync answer
  from memory). Answers are stored per browser profile on disk, or kept per partition in memory for "session" partitions.
- Notifications use the request's frame origin; embedded frames are matched carefully, and ambiguous opaque frames are refused.
- **macOS TCC**: after a site is allowed camera or mic, Orca also calls `systemPreferences.askForMediaAccess`. An OS
  refusal is reported separately from the site decision.
- Media-granted pages are protected from discard.
- **Screen sharing**: `setDisplayMediaRequestHandler` always answers with no source, so `getDisplayMedia` is
  effectively denied.
- `file-system-access-restricted` (blocklisted folders) is answered "deny" so the page doesn't hang.
- `openExternal` (custom schemes such as `zoommtg:`) goes through Orca's own "open in app?" prompt, and Orca launches
  the app itself.
- Remote-CLI/SSH-driven pages can never read the desktop clipboard.
- **Prompt infrastructure** (`guest-lifecycle/prompts/browser-page-prompts.ts`): one generic queue for
  `site-permission`, `javascript-dialog`, `http-auth` and `external-app` prompts. 120 s timeout, coalescing,
  "awaiting-registration" for tabs not yet mounted; a prompt can also be answered by an agent or phone.
- **Site display**: per-site zoom using Chrome's presets (25…500%; Electron level = log1.2(factor)), and per-site
  color-scheme override via CDP `Emulation.setEmulatedMedia` (`browser-site-display-controller.ts`).

### CEF/Chromium notes
- CEF (M116+) has `CefPermissionHandler::OnRequestMediaAccessPermission` and `OnShowPermissionPrompt` (geolocation,
  notifications, MIDI, clipboard, idle-detection, window-management, local fonts, etc.). Chrome style shows Chrome's
  bubbles and stores in Chrome's content settings (HostContentSettingsMap), including the per-site settings page logic.
- Alloy style: you implement the prompts and storage, as Orca did.
- macOS TCC still needs `AVCaptureDevice requestAccess` in the app, plus Info.plist usage strings.
- Zoom: `CefBrowserHost::SetZoomLevel`; Chrome style remembers zoom per host.
- Display capture: CEF Chrome style has a picker; in Alloy you must supply the media stream, which is hard. Orca simply denies it.

---

## 6. Downloads

### Key files
`src/main/browser/downloads/browser-session-download-policy.ts`, `browser-download-destination.ts`;
`src/main/browser/guest-lifecycle/manager/downloads/*` (creation, lifecycle, active download);
`src/shared/browser/downloads/browser-download-filename.ts`; remote: `src/main/browser/client-hosts/file-transfers/*`,
`src/main/runtime/browser/downloads/*`; extension: `src/main/browser/extensions/api/downloads*.ts`.

### Mechanism
- Built on Electron's `session.on('will-download')` and `DownloadItem` (setSavePath, updated/done, cancel).
- **No Save As dialog for page downloads**. Orca picks `~/Downloads/<sanitized name>` and handles collisions
  (`name (1).ext`…), reserving paths in memory so concurrent downloads don't collide. Filenames are normalized per platform.
- **Claims**: an agent's `download` command can claim the next download from a tab or its popups to a chosen path.
  `chrome.downloads` can set the filename or use saveAs (system save dialog). Extension downloads are attributed to
  the extension page.
- Progress, completion and failure are sent to the renderer for Orca's download UI. Interrupted downloads are
  cancelled (no manual resume). Pending downloads are cancelled when their page closes.
- **Remote workspaces**: downloads from pages of a client-hosted/SSH workspace are staged and transferred into
  `<workspace>/.orca/browser-downloads`. Pages driven by SSH/remote CLI may not write to desktop Downloads, and
  doc-preview partitions deny downloads.

### CEF/Chromium notes
- `CefDownloadHandler::CanDownload`, `OnBeforeDownload` (callback with path, optional `show_dialog`) and
  `OnDownloadUpdated` (`CefDownloadItem` + pause/resume/cancel callbacks) cover all of this.
- Chrome style can use Chrome's download bubble and shelf instead.
- Resume support comes free from `CefDownloadItemCallback::Resume`.

---

## 7. Printing (`browser-print-guard`)

### Key files
`src/preload/browser/printing/browser-print-guard.ts`, `src/preload/browser-system-dialogs.ts`,
`src/main/browser/guest-lifecycle/prompts/browser-system-dialogs.ts`, `browser-manager-visibility.ts` (`reportRefusedPrint`).

### Mechanism
- Electron's `window.print()` opens the **native OS print dialog**; there is no print preview UI in Electron.
- A session-wide frame preload replaces `window.print` in the main world. It prints only when main has said the page
  is *not* agent-led (`agentLed === false`). If the lead state is still unknown, it refuses, because script can print
  before main answers. A refused print returns silently, like a throttled print, and main is told so it can show a notice.
- PDF output for agents uses `webContents.printToPDF` (CDP `Page.printToPDF` is proxied to it).

### CEF/Chromium notes
- Chrome style: full Chrome print preview.
- Alloy style: `CefPrintHandler` (Linux custom dialogs); system dialog on macOS/Windows.
- `CefBrowserHost::Print()` / `PrintToPDF()`.
- Blocking `window.print` for agent-driven pages is still JS injection, or intercept at `CefPrintHandler::OnPrintStart` on Linux.

---

## 8. System dialogs

### JS dialogs (`alert`/`confirm`/`prompt`): `src/main/browser/guest-lifecycle/prompts/browser-javascript-dialogs.ts`
- Orca removes Electron's private `-run-dialog` listener on the guest WebContents and installs its own. Electron's
  default showed a sheet over the whole Orca window and refused `prompt()`. Agents answering over CDP also left that sheet stuck.
- One dialog per page; a second one is auto-dismissed. The dialog carries the origin of the frame that asked, as
  Chrome does. It is shown as an in-pane Orca prompt, waits without a timeout for an answer, and can be answered by
  an agent or phone. `-cancel-dialogs` (navigation) clears it. The screencast stream is told about open dialogs.
- **HTTP auth** (`browser-http-auth-prompt.ts`): Electron `login` event → Orca credential prompt.
- **External app launch** prompts for custom schemes.

### File picker
- Native `<input type=file>` uses Electron's OS dialog. For agents, `src/main/browser/cdp/input/guest-file-chooser.ts`
  uses CDP `Page.setInterceptFileChooserDialog` to catch choosers (within the 5 s user-activation window) and set
  files instead of showing the OS dialog. `withFileChooserBlocked` keeps agent command queues from opening OS dialogs.

### Passkey / print / Windows Hello
- Handled by the same "system dialogs" preload (sections 3 and 7); only Windows passes passkeys to a system dialog.

### CEF/Chromium notes
- `CefJSDialogHandler::OnJSDialog` / `OnBeforeUnloadDialog` / `OnResetDialogState` map one-to-one to Orca's design.
- `CefDialogHandler::OnFileDialog` lets you show your own picker or auto-answer for agents, with no CDP needed.
- `CefRequestHandler::GetAuthCredentials` covers HTTP auth.
- `CefRequestHandler::OnOpenURLFromTab` and protocol handling cover external apps.

---

## 9. Edit protection (discard guard)

### Key files
`src/preload/browser/edit-protection/browser-guest-edit-protection.ts`,
`src/main/browser/guest-lifecycle/manager/registration/browser-guest-discard-protection.ts`, `browser-guest-idle-protection.ts`,
`src/shared/browser/guest-lifecycle/browser-guest-discard-protection.ts`.

### Mechanism
- Private preload state that page code can neither read nor fake. Capture-phase `input`/`change` listeners mark the
  document dirty. It also tracks picture-in-picture (video and Document PiP) and `contentType`, used for PDF.
- Reports to main only after main sends a per-document **challenge generation** string, so stale or forged reports are ignored.
- Main computes `discardable | protected | unknown | gone` with a reason. Protected reasons include edits, POST
  result, PiP, PDF, audible, media capture granted, non-http(s)/data URL, notifications, and attached
  debugger/devtools. Unknown covers child frames (input doesn't bubble out of iframes) and not-yet-reported observers.

### CEF/Chromium notes
- Chromium's own logic for this lives in `TabLifecycleUnit` / `DiscardEligibilityPolicy` (in `//chrome`, not exposed by CEF).
- With CEF you rebuild it. `CefBrowserHost` gives audio state via `CefAudioHandler`, and media capture via
  `CefDisplayHandler::OnMediaAccessChange`. Edit tracking still needs injected JS (`CefRenderProcessHandler::OnContextCreated` + process messages).

---

## 10. Visibility reporting

### Key files
`src/preload/browser/visibility/browser-guest-page-visibility-report.ts`,
`src/main/browser/guest-lifecycle/visibility/browser-guest-page-visibility.ts`.

### Mechanism
- Electron's `<webview>` overrides `document.visibilityState`/`hidden` from the *embedder window* state, so a
  background tab still reports `visible` (#44520).
- The preload reads Chromium's real state from the un-overridden `document.webkitVisibilityState` on each
  visibilitychange and DOMContentLoaded, then reports to main. Main combines it with the window's hidden/minimized
  state and pushes the result back on Electron's internal `GUEST_INSTANCE_VISIBILITY_CHANGE` channel, per frame,
  including subframes, which Electron's broadcast never reaches.
- Feeds Memory Saver (section 11) and agent logic.

### CEF/Chromium notes
- Not an issue in CEF: each tab is its own `CefBrowser`. `CefBrowserHost::WasHidden(true)` (and `WasResized`/
  `NotifyScreenInfoChanged` for OSR) drive real Page Visibility, timers throttling and rAF pausing. Hidden native child
  views report hidden automatically on most platforms.
- What remains is remembering to call WasHidden for tabs that aren't shown.

---

## 11. Guest discard (Memory Saver)

### Key files
`src/main/browser/guest-lifecycle/browser-page-idle-discard.ts`, `src/shared/browser/guest-lifecycle/browser-memory-saver.ts`,
`src/main/ipc/browser/browser-guest-discard-ipc.ts`, `src/preload/api/browser/browser-bridge-guest-discard.ts`,
plus the protection logic in section 9 and the extension discarded-tab model (`extensions/tabs-and-windows/browser-extension-discarded-tab*.ts`).

### Mechanism
- Chrome-like Memory Saver with configurable idle hours. Each page has visibility "episodes". Once a page has been
  hidden longer than the policy (on the **awake clock**, aware of sleep and power), the renderer is asked to discard it.
- Bounded batches: at most 4 pending discards at a time, using a priority index keyed by hiddenAt. Pages revisited
  more than 15 times are skipped.
- Protocol: renderer `reserveGuestDiscard`, then main checks protection, then the webview is unmounted
  (`acknowledgeGuestRemoval`). The renderer can refuse. Restoring re-attaches through `preparePageAttach` and reloads
  the URL; a POST page is protected because restore would reload it as GET.
- Extensions keep seeing discarded tabs through synthetic tab ids (`tabs.discard`, `discarded: true`,
  `onReplaced` when the tab reloads). Profiles whose only remaining tabs are discarded still count as "live" for
  extension lifecycle.

### CEF/Chromium notes
- CEF has no tab discarding. Close the `CefBrowser` (`CloseBrowser(true)`) and recreate it with the saved URL. CEF
  exposes no navigation-history restore API: Chrome style can't use `SessionService`, and there is no public
  `NavigationEntry` restore in Alloy. You lose back/forward history unless you patch CEF or keep your own history list.
- Chromium's own `TabLifecycleUnitSource`/`TabDiscarder` and freezing (`WebContents::SetPageFrozen`) are in `//chrome`
  and `//content`; only a raw-Chromium embedder gets them.

---

## Cross-cutting takeaways for the port

1. **Decide rendering mode first.** OSR/windowless rendering into GPUI means Alloy style: no extensions, no Chrome
   prompts, password manager, print preview, WebAuthn UI or download bubble. You rebuild almost everything Orca did
   (and more, since Electron gave Orca a working extension base). Native child `CefBrowserView` in Chrome style gives
   you most of it natively, but the UI is Chrome's Views UI.
2. **Things needed in either style** (Orca's app-specific policies):
   - agent-led blocking of OS dialogs (print, Windows Hello, file chooser)
   - prompts answerable remotely or by agents (JS dialogs, permissions, HTTP auth)
   - per-tab certificate grants
   - remote-workspace download routing
   - SSH-driven page restrictions
   - Memory Saver with discard protection
   - CEF handler mapping: CefJSDialogHandler, CefDialogHandler, CefPermissionHandler, CefDownloadHandler,
     CefRequestHandler/CefResourceRequestHandler, CefPrintHandler, CefAudioHandler/CefDisplayHandler.
3. **Reusable directly** (language-agnostic logic): CRX3 verifier, Web Store CRX URL and Omaha update protocol,
   permission-warning table, native-messaging host search paths and framing, DNR engine design (or use Chromium's
   when in Chrome style), password form classification heuristics, login-success state machine, CSV import aliases,
   Chromium `Login Data` import with Safe Storage key retrieval, and the Chrome zoom presets.
4. **Electron-specific workarounds that disappear with CEF/Chromium**:
   - `webkitVisibilityState` visibility hack
   - worker-fetch API transport (no SW preload)
   - `storage.onChanged` relay
   - re-implemented `storage.sync`, DNR and webRequest (Chrome style has real ones)
   - private `-run-dialog` takeover
   - `session.fetch` workaround for the Web Store
