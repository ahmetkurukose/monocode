# Orca (stablyai/orca) Embedded Browser: Feature Catalog

Research date: 2026-11-20. Installed app: /Applications/Orca.app v1.4.259 (Electron 43). Source: /tmp/orca-research/orca (HEAD ~v1.4.266).

Sources used:
- GitHub release notes for 157 stable releases, v1.4.83 to v1.4.266 (`gh release view`). Browser-relevant extracts: /tmp/orca-research/rel_browser2.txt (v1.4.194+) and /tmp/orca-research/old_browser.txt (older).
- Site docs: `docs/site/content/docs/browser/{overview,profiles,design-mode}.mdx`, `settings.mdx`, `cli/reference.mdx`, `editing/viewers.mdx`, `mobile.mdx`, `remote-servers.mdx`, `terminal.mdx`, `model/quick-open.mdx`, `recipes/design-mode-fix.mdx`, and `README.md`.
- Agent skill guide: `skill-guides/orca-cli/references/browser.md`.
- UI strings from the Settings → Browser components (`src/renderer/src/components/settings/browser/**`), the browser pane components (`src/renderer/src/components/browser-pane/**`) and the keybinding titles.
- `orca --help` and per-command `--help` from /opt/homebrew/bin/orca (read-only).
- Web search. It turned up only third-party write-ups and mirrors; orca.dev/onorca.dev mirror the repo docs.

Legend for **Who**: **U** = human user, **A** = AI agent (CLI/skill), **U+A** = both.
"Where" cites a release tag (e.g. `v1.4.241`), a doc path, or a source/UI location.

---

## 0. The Chrome question: bundled Chromium only, or real Chrome?

**Short answer: the browser itself always runs on Electron's bundled Chromium (`<webview>` guests). Orca never drives your installed Google Chrome, never attaches to it, and never uses your Chrome profile directory as a live profile.** It reaches your real browsers in only a few indirect ways:

| Mechanism | What it does | Real Chrome involved? |
|---|---|---|
| Desktop browser pane | Electron `<webview>` guests in per-profile `persist:` partitions. Agents drive them through Orca's own CDP proxy (`src/main/browser/cdp/`) with the bundled `agent-browser` driver. | No. Bundled Chromium. |
| CLI docs | "Browser commands control Orca's embedded browser tab for the selected worktree. They do not control Chrome, Safari, or the Orca desktop UI." (`docs/site/content/docs/cli/reference.mdx` L287) | Explicitly no. |
| Agent-browser CDP proxy | Requires a per-session token and refuses ordinary web pages as clients (`v1.4.227`, `v1.4.252`). It ignores the user's own agent-browser env/config (`v1.4.261`). | No. |
| Cookie import | Reads the cookie SQLite DBs of Chrome, Edge, Arc, Chromium, Comet, Helium, Vivaldi, Firefox and Safari (or a cookie file), decrypts them with the OS keychain/keyring, and copies them into an Orca profile. A one-time copy, not a live link (`src/main/browser/cookies/sources/`). | Reads Chrome's data files only. |
| Password import | Reads Chromium-family `Login Data` / `Login Data For Account`, or a CSV (`src/main/browser-passwords/browser-password-chromium-import.ts`). | Reads Chrome's data files only. |
| Chrome Web Store extensions | Installs CRX packages from the Chrome Web Store into Orca's Electron sessions, with an Orca-built `chrome.*` API layer. | No. Uses CWS packages, not Chrome. |
| Headless server (`orca serve` / `orcad`) | Uses an Electron "browser sidecar" by default. Fallback: an **external Chromium binary** set with `ORCA_BROWSER_EXECUTABLE` (any Chromium/Chrome executable, Snap supported), launched by `agent-browser` with its **own Orca-owned user-data dir** (`src/main/orcad/browser-providers/orcad-browser-provider.ts`). | Can launch a Chrome/Chromium binary on servers, but as a separate headless instance, not your profile. |
| Computer Use (`orca computer …`) | Accessibility-based control of native apps, which can include the real Chrome app, "may use existing logged-in browser sessions" (Settings → Browser Use, notice text). Background computer use on Mac since `v1.4.245`. | Yes, but via OS accessibility, not as Orca's browser. |
| Chat UI "Claude in Chrome" | Passes `--chrome` to Claude Code when Claude-in-Chrome is "Enabled by default" (`v1.4.263`). That is Anthropic's Chrome extension, not Orca's browser. | Third-party path. |

---

## 1. Tabs, panes, windows & layout

| # | Feature | Where | Who |
|---|---|---|---|
| 1.1 | Browser is a tab type in the unified tab system. It can be split beside terminals, diffs and editors in nested split panes, with drag-to-split. | docs/model/tabs-panes-splits.mdx | U |
| 1.2 | Per-worktree browser: tabs are scoped to a worktree. Switching worktrees restores that worktree's tabs and scroll positions. | docs/browser/overview.mdx | U+A |
| 1.3 | Floating Workspace browser tabs, not tied to any repo worktree. | settings.mdx (Floating Workspace), `v1.4.110` | U |
| 1.4 | `Cmd-T` opens a new tab scoped to the worktree. | overview.mdx | U |
| 1.5 | `Cmd-Shift-T` reopens the last closed tab. | overview.mdx | U |
| 1.6 | Closing a tab restores the previously active tab. | `v1.4.191` | U |
| 1.7 | New browser tab in a right split (`Cmd+Opt+B` on macOS, rebindable). | `v1.4.245` | U |
| 1.8 | Tab strip shows favicons, live titles, a loading state and a tab scrollbar. Tabs size to fit their titles; minimum width 72px. | `v1.4.197`, `v1.4.198`, `v1.4.254` | U |
| 1.9 | Speaker icon on tabs playing audio, with tab mute. | `v1.4.241` | U |
| 1.10 | Tabs move between tab groups without going blank. | `v1.4.218` | U |
| 1.11 | Popups: `target=_blank` and unnamed popups open as Orca tabs, Chrome-style. Named or OAuth popups open in an Orca "origin-bar" popup window, labeled with the committed origin, with an auto-hide menu bar. | overview.mdx, `v1.4.138`, `v1.4.227`, `v1.4.232` | U+A |
| 1.12 | Links open beside their source pane. Tabs opened from links get focus. `window.close()` is honored. | `v1.4.239`, `v1.4.241` | U |
| 1.13 | Background tabs load on open. Pages in background tabs report `hidden`, as in Chrome. | `v1.4.200`, `v1.4.266` | U+A |
| 1.14 | Tab persistence across restarts: restored tabs are listed but lazily loaded, and come back non-blank. | `v1.4.246`, `v1.4.259` | U+A |
| 1.15 | Memory Saver frees tabs unviewed for N hours (default 4) and reloads them on return. Tabs with unsaved edits, PiP etc. are protected. Interval is configurable or off. | `v1.4.261`, Settings → Browser | U |
| 1.16 | Idle hidden pages rest while Orca is hidden and wake for agent commands and screenshots. | `v1.4.244` | U+A |
| 1.17 | Responsive toolbar folds tools into an overflow menu on narrow panes. | `v1.4.217` | U |
| 1.18 | Viewport size bubble shown while you resize a browser pane. | `v1.4.245` | U |
| 1.19 | Command palette (`Cmd/Ctrl+J`) lists browser tabs ("Switch to tab"), opens typed websites in a new tab, and searches the web. | `v1.4.199`, `v1.4.229`, `v1.4.230`, model/quick-open.mdx | U |
| 1.20 | Browsers appear in the Resource Manager (memory/process view). | `v1.4.136` | U |
| 1.21 | `Ctrl+Tab` switcher works from a focused browser guest. | `v1.4.156` | U |
| 1.22 | Shortcuts (back/forward/reload/zoom/grab/find) are scoped to the split or floating panel that received them. | `v1.4.163`, `v1.4.211` | U |
| 1.23 | Orca app shortcuts, split focus and mouse back/forward buttons work inside web pages. | `v1.4.241` | U |
| 1.24 | Customizable "+" menu for opening tabs, including a browser tab. | `v1.4.261` | U |

## 2. Navigation, address bar, search, new tab, history

| # | Feature | Where | Who |
|---|---|---|---|
| 2.1 | Back / Forward / Reload / Stop. Back/Forward are greyed out when there is no history. | overview.mdx, `v1.4.263` | U+A |
| 2.2 | Hard Reload (cache bypass) from right-click or long-press on reload, with shortcut hints shown on hover. | `v1.4.174`, overview.mdx | U |
| 2.3 | Immediate reload feedback. Reload reports offline failures and retries loads that never finished. | `v1.4.195`, `v1.4.259` | U |
| 2.4 | Address bar with history and fuzzy URL completion, plus Chrome-style suggestions. | overview.mdx, `v1.4.238` | U |
| 2.5 | Chrome-like address bar input: first click selects the full URL, double-click selects a word, Escape behaves as in Chrome. | `v1.4.199`, `v1.4.229`, `v1.4.240` | U |
| 2.6 | The exact typed address wins over a more-visited page on the same site. Explicit ports and scheme-less URLs work. | `v1.4.244`, `v1.4.217`, `v1.4.265` | U+A |
| 2.7 | Default search engine: Google, DuckDuckGo, Bing or Kagi (optional Kagi private session link). Prefix `?` to force a search. | Settings → Browser, `src/shared/browser/navigation/browser-url.ts` | U |
| 2.8 | Default home page (or a blank tab). | Settings → Browser | U |
| 2.9 | Page-load progress line along the address bar (desktop and phone). | `v1.4.238` | U |
| 2.10 | Hovered or focused link URL shown in a status bubble at the bottom. | `v1.4.241` | U |
| 2.11 | Workspace-aware New Tab page: a workspace card (branch, PR/issue status, checks), suggested sites, workspace dev servers, and tiles with "Remove from history". | `v1.4.238`, `v1.4.241`, new-tab/ components | U |
| 2.12 | Address-bar convergence: previews and browser tabs convert in place. | `v1.4.194` | U |
| 2.13 | Searchable browsing history (`Cmd+Y` on macOS, `Ctrl+H` elsewhere). Remove entries, or clear the last hour / today / all time, with Undo. | `v1.4.240` | U |
| 2.14 | Load-failure page explains why a page failed, with Retry, auto-retry and "Reopen". | `v1.4.239`, `v1.4.258` | U |
| 2.15 | Focus address bar shortcut (rebindable). | keybinding titles | U |
| 2.16 | "Open in default browser" (page menu and context menu). | toolbar strings | U |
| 2.17 | Local `file://` HTML pages can follow their own file links. Dropping an HTML file on a page opens it. | `v1.4.232`, `v1.4.263` | U |
| 2.18 | `*.orca.localhost` worktree-specific localhost URLs ("Localhost worktree labels"), including Next.js Server Actions support. | Settings → Browser, `v1.4.245` | U |
| 2.19 | Open links from SSH and paired hosts' localhost in that host's Orca browser. | `v1.4.266` | U |

## 3. Link routing (from terminal, chat, markdown, editor)

| # | Feature | Where | Who |
|---|---|---|---|
| 3.1 | Link Routing setting: open http(s) links from the terminal, markdown and editor in Orca's browser or the system browser. Sign-in pages always go to the system browser. | settings.mdx, overview.mdx | U |
| 3.2 | "Hold Shift…" modifier (`⇧⌘-click`) inverts the routing for one click. | `v1.4.163` | U |
| 3.3 | "Open links in the background": links join the browser without stealing focus. | Settings strings | U |
| 3.4 | Smart target reuse: an existing page with the exact URL, else the focused or MRU visible browser pane, else a new split beside the source. | overview.mdx | U |
| 3.5 | Terminal URL click popover (Orca Browser / System Browser) and middle-click behavior. | terminal.mdx, Settings | U |
| 3.6 | Markdown links: click opens in Orca, middle-click opens in the system browser, link menu on hover. | `v1.4.259` | U |
| 3.7 | Linked GitHub reviews open in the Orca browser. | `v1.4.194` | U |
| 3.8 | Orca's own buttons and links honor the open-links-in-Orca setting. | `v1.4.245` | U |

## 4. Profiles, sessions, cookies, identity

| # | Feature | Where | Who |
|---|---|---|---|
| 4.1 | Named browser profiles, each with an isolated storage partition (cookies, localStorage, cache). Create, delete and switch per tab from the toolbar (switching reloads the page). | browser/profiles.mdx | U+A |
| 4.2 | Profile scope for new tabs: Shared (default), Per repo, or Per workspace (auto-created, deleted with the workspace). | `v1.4.239`, Settings | U |
| 4.3 | Per-project profile override in Repository settings. Several projects can share one sign-in. | `v1.4.261` | U |
| 4.4 | Agent commands inherit the tab's profile. CLI: `tab profile list/create/delete/set/show/use-default/clone`, and `tab create --profile`. | profiles.mdx, `orca --help` | A |
| 4.5 | Cookie import from Chrome, Edge, Arc, Chromium, Comet, Helium, Vivaldi, Firefox, Safari or a cookie file. Pick the source browser profile from the toolbar or Settings. | Settings strings, `v1.4.108`, `v1.4.223`, `v1.4.244` | U |
| 4.6 | Import replaces cookies only for the imported domains, keeps you signed in, and reports undecryptable or unreadable cookies. | profiles.mdx, `v1.4.188`, `v1.4.243` | U |
| 4.7 | Google cookies are deliberately never imported. A "Sign in to Google directly in Orca" notice is shown. | `v1.4.183`, profiles.mdx | U |
| 4.8 | Clear Google cookies from a profile, to fix a sign-in stuck on an old account. | `v1.4.244` | U |
| 4.9 | Clear default cookies, with confirmation. | `v1.4.261` | U |
| 4.10 | macOS Full Disk Access prompt when cookie import is blocked. Linux `secret-tool` missing hint. | `v1.4.252`, `v1.4.243` | U |
| 4.11 | Cookie import into a remote host's browser. Settings shows which machine's browsers are read. | `v1.4.245`, Settings | U |
| 4.12 | Session cookies are restored across restarts, so sites stay signed in. | `v1.4.261` | U |
| 4.13 | Browser identity (UA) is app-wide: Native (Electron UA, default since `v1.4.263`) or Cleaned (Chrome-shaped). Per-site overrides for Google sign-in, Microsoft Teams and Chase. Client hints stay consistent. | profiles.mdx, `v1.4.198`, `v1.4.263` | U |
| 4.14 | CLI `browser identity get/set --mode --reset`. | Settings strings, cli command keys | A |
| 4.15 | Agent cookie CRUD: `cookie get/set/delete` (delete removes only the named cookie). | `orca cookie --help`, `v1.4.261` | A |
| 4.16 | Agent storage CRUD: `storage local/session get/set/clear`. | `orca --help` | A |
| 4.17 | App-wide HTTP proxy and system proxy applied to browser sessions. | `v1.4.194`, `v1.4.244` | U |
| 4.18 | Profiles changed outside the window refresh without a reload. | `v1.4.240` | U |
| 4.19 | Browser data import onboarding hint ("Import browser data", "Hide Hint"). | pane strings | U |

## 5. Extensions (Chrome Web Store)

| # | Feature | Where | Who |
|---|---|---|---|
| 5.1 | Install Chrome extensions from the Chrome Web Store with an in-page "Add to Orca" button. Experimental in `v1.4.150`/`v1.4.243`; on by default in Settings → Browser since `v1.4.258`. | `v1.4.243`, `v1.4.258` | U |
| 5.2 | Add by CWS link or extension ID in Settings. "Load unpacked…", "Reload from folder". | Settings strings | U |
| 5.3 | Install and permission-escalation dialogs with Chrome-equivalent permission warnings ("Read and change all your data…" etc.). Not-yet-supported APIs are listed. | page-prompts, extension strings | U |
| 5.4 | Per-profile enablement. Extensions start with a profile's first page. | Settings, `v1.4.261` | U |
| 5.5 | Toolbar extensions menu: pin/unpin, badges, options page, remove, manage. | pane strings | U |
| 5.6 | Extension side panels in a resizable dock (`chrome.sidePanel`). | `v1.4.263` | U |
| 5.7 | Extension popups support Paste/Copy/Select All and close on blur. | `v1.4.263`, `v1.4.245` | U |
| 5.8 | Implemented `chrome.*` APIs: action, commands, contextMenus, cookies, declarativeNetRequest, downloads (+saveAs), extension, management, nativeMessaging (password managers such as 1Password and Bitwarden), notifications, permissions, privacy, runtime, scripting, sidePanel, storage (+sync), tabs (+captureVisibleTab), webNavigation, webRequest, windows. | `src/main/browser/extensions/api/` | U |
| 5.9 | Background CWS auto-updates. Updates that need more access are held for review ("Review access"). | `web-store/browser-extension-webstore-updater.ts`, Settings strings | U |
| 5.10 | Known limits: ad blockers install but do not block yet; Tampermonkey does not work. | `v1.4.243` | U |
| 5.11 | CLI `extension list`, `extension reload` (for unpacked extensions). | `v1.4.255` | A |
| 5.12 | Tabs and windows that extensions open for an agent stay out of the user's view. | `v1.4.261` | A |

## 6. Passwords, passkeys, autofill, HTTP auth

| # | Feature | Where | Who |
|---|---|---|---|
| 6.1 | Built-in Chrome-compatible password manager: offer to save or update after sign-in, with "Never for this site" and a never-saved list. | `v1.4.250`, pane strings | U |
| 6.2 | Fill dropdown under sign-in fields ("Saved logins", "Other logins on this site"). | passwords/BrowserPasswordFillDropdown.tsx | U |
| 6.3 | Settings vault: add, edit, delete, show/copy password (clipboard auto-cleared after 2 min), per-profile wipe. | Settings strings | U |
| 6.4 | Import passwords from Chrome-family browsers and from CSV, with a merge report (already saved, conflicts, unreadable, protected). | Settings strings | U |
| 6.5 | Optional Touch ID before filling. OS secure storage is required (no vault otherwise). | Settings strings, `v1.4.250` | U |
| 6.6 | Vault works on local, SSH and This-device pages, but not on streamed server pages. | `v1.4.250`, `v1.4.265` | U |
| 6.7 | Password managers can also be used as extensions (1Password, Bitwarden). | `v1.4.243` | U |
| 6.8 | WebAuthn: account picker for discoverable passkeys on USB security keys. Platform passkeys are not supported. | `v1.4.187`, profiles.mdx | U |
| 6.9 | Agents see `passkeyRequest` in effects and can cancel it with `dialog dismiss`. | `v1.4.258`, skill browser.md | A |
| 6.10 | HTTP Basic/Digest auth prompt ("asking for a username and password"). | `v1.4.232` | U |
| 6.11 | Agent `set credentials --user --pass`. `fill --value-stdin` for secrets. | `orca --help`, `v1.4.261` | A |
| 6.12 | FedCM is disabled as unsupported. | `v1.4.182` | U |

## 7. Site permissions & security prompts

| # | Feature | Where | Who |
|---|---|---|---|
| 7.1 | Allow/Block permission prompts, remembered per site, for camera, microphone, location, notifications, clipboard, MIDI, window management, idle detection ("Device use") and local network. | `v1.4.238`, `v1.4.241`, `shared/browser/site-settings/browser-site-permission.ts` | U |
| 7.2 | Site permissions manager in Settings, per profile ("Ask again"). Local files are treated as one site. | Settings strings | U |
| 7.3 | Auto-granted: fullscreen, pointerLock, storage-access (`requestStorageAccess`). | `v1.4.135`, `v1.4.186` | U |
| 7.4 | Screen capture (`getDisplayMedia`) is denied. | session partition policies | U |
| 7.5 | External-app (custom scheme) launch confirmation, with "Always allow <site> to open <scheme> links" remembered per site. | `v1.4.232`, `v1.4.262` | U |
| 7.6 | Permission-denial notices name the requesting frame and the permission. | `v1.4.188` | U |
| 7.7 | File System Access API: pages can use files you pick, save or drop. | `v1.4.240` | U |
| 7.8 | JavaScript alert/confirm/prompt dialogs, and an "always allow leaving" beforeunload policy. | `v1.4.246`, `v1.4.229` | U+A |
| 7.9 | Per-site appearance (Match Orca / Light / Dark) from the page menu. | `v1.4.238` | U |

## 8. Certificates

| # | Feature | Where | Who |
|---|---|---|---|
| 8.1 | Certificate error interstitial ("Your connection to this site isn't private") with proceed. Local HTTPS: "Try HTTPS" plus proceed. | `v1.4.145`, `v1.4.147`, `main/browser/certificate-trust/` | U |
| 8.2 | Actionable certificate warnings on remote pages. Proceed past them on the phone. | `v1.4.261`, `v1.4.265` | U |

## 9. Downloads & uploads

| # | Feature | Where | Who |
|---|---|---|---|
| 9.1 | Downloads shelf under the toolbar: progress, cancel, open, show in folder, dismiss. | overview.mdx, `v1.4.90` | U |
| 9.2 | Remote-workspace downloads land on the workspace host. Uploads are staged there. | overview.mdx, `v1.4.251` | U |
| 9.3 | Windows-reserved filename sanitization. | `v1.4.184` | U |
| 9.4 | Agent `download --selector --path`, saving to the requested path, including from popups and new tabs. | `v1.4.246` | A |
| 9.5 | Agent `upload` through file inputs or picker buttons in any frame, including cross-origin iframes. The system picker is suppressed. | `v1.4.246`, `v1.4.258` | A |
| 9.6 | Downloads are disabled in document previews. | editing/viewers.mdx | U |

## 10. DevTools

| # | Feature | Where | Who |
|---|---|---|---|
| 10.1 | "Open browser devtools" from the menu. "Inspect" in the context menu. | toolbar strings | U |
| 10.2 | DevTools docked inside the pane (dock right or bottom), resizable, or popped out to a separate window. | `v1.4.241` | U |
| 10.3 | DevTools window shortcuts. | `v1.4.239` | U |
| 10.4 | DevTools opt-in in settings. Refused for offscreen guests. | settings.mdx, `v1.4.196` | U |

## 11. Find, zoom, viewport, printing, PDF

| # | Feature | Where | Who |
|---|---|---|---|
| 11.1 | Find in page (`Cmd-F`): match count, previous/next. Restarts if the view is replaced. | overview.mdx, `v1.4.244` | U |
| 11.2 | Page zoom: in, out and reset (`Cmd +/-/0`, `Cmd`-wheel), remembered per site. Default zoom for new tabs. Zoom is shown in the address field. | `v1.4.238`, `v1.4.256`, settings.mdx | U |
| 11.3 | Viewport presets (Mobile S/M/L, Tablet, Laptop, Laptop L, Desktop) and custom sizes via CDP device emulation. Oversized viewports scroll. | toolbar strings, overview.mdx, `v1.4.196` | U+A |
| 11.4 | Agent `viewport --width --height --scale --mobile`, `set device --name` (touch and coarse pointer), `set media` (color scheme, reduced motion), `set offline`, `set headers`, `geolocation`. | `orca --help`, cli command keys, `v1.4.245` | A |
| 11.5 | PDF export: `orca pdf` (native `printToPDF`). | skill browser.md | A |
| 11.6 | Print: no user Print command was found. A system print dialog triggered by a page during agent work is refused (`blockedPrint` effect, use `pdf`). | `v1.4.261`, skill browser.md | A |

## 12. Context menu & clipboard

| # | Feature | Where | Who |
|---|---|---|---|
| 12.1 | Chrome-style page context menu: Undo/Redo, Cut/Copy/Paste, Paste and Match Style, Paste as Plain Text, Select All, spellcheck "Add to Dictionary", Open link in Orca or default browser, Copy link address, Open/Save/Copy image, Copy image address, "Search <engine> for …", Open page in default browser, Copy page URL, Inspect. | `v1.4.239`, pane strings | U |
| 12.2 | Agent `clipboard read/write`. | `orca --help` | A |
| 12.3 | Click-to-zoom image viewer (Alt-drag to pan). | `v1.4.246` | U |

## 13. Element grab, annotate, draw (agent feedback)

| # | Feature | Where | Who |
|---|---|---|---|
| 13.1 | Grab page element ("Design Mode"): hover-highlight picker. Captures outer HTML plus neighborhood, computed CSS, role, selector, a cropped screenshot, and the React component / source file:line when source maps exist. Attach to AI, Copy Contents (C), or Copy Screenshot (S). | design-mode.mdx, README, element-grab/ | U→A |
| 13.2 | Annotate page element: multiple numbered notes (max 20), each with a comment and an intent. Badges and outline follow the element. Tray to edit, delete, collapse and Copy All. | `v1.4.195`, `v1.4.229`, `v1.4.245` | U→A |
| 13.3 | Send annotations to an agent: pick an existing agent or terminal. Compact by default, with "Include page details" for full output. Pasted images are supported. | `v1.4.129`, `v1.4.245` | U→A |
| 13.4 | Unsent notes persist across reload and restart, with confirmation before closing a tab that has them. | `v1.4.257` | U |
| 13.5 | Agent reads unsent notes: `orca tab annotations`. | `v1.4.257` | A |
| 13.6 | Grab and Annotate work in shadow roots, modal dialogs, popovers and iframes. The composer stays off the target element. | `v1.4.243`, `v1.4.246`, `v1.4.224` | U |
| 13.7 | Draw on screenshot (markup): highlighter, arrow, rectangle, ellipse, text, eraser, color, thickness, font size, "Copy Markup" to paste to the agent. | `v1.4.142`, `v1.4.223`, pane strings | U→A |
| 13.8 | One unified Annotate button with Grab, Annotate and Draw modes. Rebindable shortcuts for Grab and Annotate. | `v1.4.256`, `v1.4.220` | U |
| 13.9 | "Open source file" from a grabbed element. | toolbar strings | U |
| 13.10 | HTML preview annotations name the previewed file. | `v1.4.252` | U→A |

## 14. Agent control (CLI / skill / CDP)

Driver: bundled `agent-browser` (v0.43.0 as of `v1.4.266`) through Orca's own CDP proxy. Exposed as the `orca` CLI plus the bundled "Browser Use" skill (`skill-guides/orca-cli/references/browser.md`). There is no separate MCP server for the browser; agents use the CLI and skill.

| # | Feature | Where | Who |
|---|---|---|---|
| 14.1 | Settings → Browser Use: enable agent browser use, install the skill, import cookies, example prompts, and a Computer Use fallback. | Settings strings | U |
| 14.2 | Tab commands: `tab list/show/current/create [--split dir] [--headless] [--profile]/switch/close`, `open-url`. Pages are targeted by stable `browserPageId` (`--page`). | `orca --help`, `v1.4.246`, `v1.4.266` | A |
| 14.3 | Accessibility snapshot with refs (`@e1`): `--interactive`, `--delta` (diff since last), `--selector`, `--depth`, `--compact`, `--urls`, `--full`. | skill browser.md | A |
| 14.4 | Navigation: `goto`, `back`, `forward`, `reload`, `wait --text/--url/--selector/--load/--timeout`. | `orca --help` | A |
| 14.5 | Interaction: `click`, `dblclick`, `fill` (`--value-stdin`), `type`, `inserttext`, `select`, `check`/`uncheck`, `focus`, `clear`, `select-all`, `keypress`, `hover`, `drag`, `scroll`, `scrollintoview`, `mouse move/down/up/wheel`, `find --locator role\|text\|label`, `highlight`. | `orca --help`, cli command keys | A |
| 14.6 | Query: `get --what text\|html\|value\|url\|title`, `is --what visible\|enabled\|checked`, `eval --expression`, `exec "read"` (page as Markdown). | `orca --help`, skill | A |
| 14.7 | Raw passthrough: `exec --command "<agent-browser cmd>"`. Exec commands that leave Orca's page are refused. | skill, `v1.4.261` | A |
| 14.8 | Action effects report: url/title changes, newTabs, popups, downloads, consoleErrors, passkeyRequest, blockedPrint, page diff lines, loading/settled flags. | `v1.4.246`, skill | A |
| 14.9 | `--expect-url` guard refuses to act if the page is at a different URL. | `v1.4.248` | A |
| 14.10 | Dialogs: `dialog accept [--text]/dismiss`. File chooser and JS dialog errors (`browser_dialog_opened`, `browser_file_chooser_opened`). | `v1.4.246` | A |
| 14.11 | Popups addressable as `popup-<n>` pages with `openerBrowserPageId` (sign-in and payment popups). | `v1.4.246` | A |
| 14.12 | Focus protection: agent clicks and keys never steal the user's keyboard focus, never type into the terminal, refuse during IME composition, keep popups and new tabs in the background, and do not raise the Orca window. | `v1.4.227`, `v1.4.232`, `v1.4.259`, `v1.4.265` | A (protects U) |
| 14.13 | Agent/user tab arbitration: the agent keeps its tab for about 20s after its last command even if the user switches tabs. | skill, `v1.4.246` | A |
| 14.14 | Live agent cursor overlay (spring-animated glyph) showing agent browsing. Chat shows a "Show" jump to the page. | `v1.4.241`, browser-pane/agent-cursor | U |
| 14.15 | Scoping: commands resolve the caller's worktree or chat workspace. `--worktree`, `--worktree all`. Unscoped commands are refused when ambiguous. | `v1.4.264` | A |
| 14.16 | SSH-host agents can control desktop browser tabs (per-host opt-in). The tab shows a "<host> is controlling a browser tab" banner with Stop. | `v1.4.229`, `v1.4.252` | U+A |
| 14.17 | Local CDP proxy is protected by a per-session token and refuses web-page clients. | `v1.4.252`, `v1.4.227` | security |
| 14.18 | Remote control: `--environment`/`--pairing-code` run browser commands on a paired Orca server. | `orca --help` | A |
| 14.19 | Hooks: plugins and Claude Code tool-call hooks can block or rewrite agent actions. | `v1.4.247` | A |

## 15. Screenshots, capture, console & network

| # | Feature | Where | Who |
|---|---|---|---|
| 15.1 | `screenshot` (PNG/JPEG, CSS-pixel coordinates), `--annotate` ref labels, `--if-changed`. | skill browser.md | A |
| 15.2 | `full-screenshot` (whole page, no tiling artifacts). | `v1.4.261` | A |
| 15.3 | `capture start/stop`, `console --limit`, `network --limit`. | `orca --help`, `v1.4.245` | A |
| 15.4 | Request interception: `intercept enable/disable/list` (pause matching requests). | `orca intercept --help` | A |
| 15.5 | User screenshot via the Grab "S" key and the Draw markup copy. | pane strings | U |

## 16. Remote, client-hosted & streamed browser

| # | Feature | Where | Who |
|---|---|---|---|
| 16.1 | Remote workspace placement "This device" (client-hosted): the page renders locally while HTTP(S), WebSocket, DNS and loopback traffic tunnel through the remote host. | overview.mdx, `v1.4.191` | U |
| 16.2 | "Server (streamed)": the page renders on the server (headless Electron sidecar or external Chromium) and streams via screencast. Supports input, clipboard paste/copy, Ctrl+scroll/pinch zoom, Stop/Hard Reload, JS dialogs, context menu and favicons. | `v1.4.243`, `v1.4.259`, `v1.4.265`, stream-remote/ | U |
| 16.3 | "Reopen on server" fallback. Toolbar host indicator shows where traffic exits. | overview.mdx, pane strings | U |
| 16.4 | SSH workspaces: browse through the SSH host (TCP forwarding) or from this device, honoring the host's outbound proxy and IPv6 bypass rules. Clear errors when `AllowTcpForwarding no`. | Settings, `v1.4.245`, `v1.4.239` | U |
| 16.5 | Headless `orca serve`: browser providers (Electron sidecar, or `ORCA_BROWSER_EXECUTABLE` Chromium). `tab create --headless`. GPU opt-in via `ORCA_SERVE_GPU=1`. | `v1.4.191`, remote-servers.mdx | A |
| 16.6 | Logins persist across host re-pairing and orcad restarts. | `v1.4.237`, `v1.4.265` | U |
| 16.7 | A server agent's new tab opens on the paired desktop. | `v1.4.259` | A |

## 17. Mobile (phone) browser

| # | Feature | Where | Who |
|---|---|---|---|
| 17.1 | Phone views desktop browser tabs via a screencast stream, with taps, typing and long-press. | `v1.4.209`, mobile.mdx | U |
| 17.2 | Web / Mobile switch: asks the desktop for a phone-size viewport. | mobile.mdx, `v1.4.90` | U |
| 17.3 | "Mobile is driving this browser": desktop input is paused, with Take back / Take back all. | pane strings, `v1.4.261` | U |
| 17.4 | Phone address-bar search, prompts, retry, tab icons, certificate proceed, profile choice. | `v1.4.265` | U |
| 17.5 | Streaming continues while the desktop sleeps or is minimized. | `v1.4.224` | U |

## 18. Document / HTML preview

| # | Feature | Where | Who |
|---|---|---|---|
| 18.1 | "Open in Orca Browser" / "Open Preview to the Side" for local, SSH or paired `.html` files in a sandboxed tab. Relative assets are read through the workspace. | editing/viewers.mdx, `v1.4.192` | U |
| 18.2 | Folder access grants per tab. External links need confirmation. Downloads and network are blocked. Preview options: reload, open source, copy path or relative path. | viewers.mdx, pane strings | U |
| 18.3 | Share local HTML as a public artifact link. | overview.mdx | U |
| 18.4 | Dark-scheme documents render on a dark canvas. Media plays in remote previews. | `v1.4.240`, `v1.4.266` | U |

## 19. Dictation & input

| # | Feature | Where | Who |
|---|---|---|---|
| 19.1 | Dictate (on-device or cloud) into the focused field of a browser page. | `v1.4.256` | U |
| 19.2 | IME/CJK input support. Agent input waits for IME composition. | `v1.4.103`, `v1.4.232` | U |

## 20. Misc / anti-detection / reliability

| # | Feature | Where | Who |
|---|---|---|---|
| 20.1 | Passes Cloudflare Turnstile (Native UA). Anti-detection identity signals are aligned. | `v1.4.198`, `v1.4.186` | U |
| 20.2 | Crashed-guest recovery. Resizing a crashed page does not crash Orca. | `v1.4.176`, `v1.4.218` | U |
| 20.3 | Loading surfaces match the Orca theme. | `v1.4.198` | U |
| 20.4 | Translated UI (i18n) across the browser. | `v1.4.254` | U |
| 20.5 | Browser emulator neighbor: iOS Simulator and Android emulator tabs (separate feature, `orca emulator`). | `orca --help` | U+A |

---

## Not found / gaps (useful for parity planning)
- No bookmarks UI. Bookmarks appear only as an extension permission string.
- No user-facing Print command. Agents get a PDF instead.
- No incognito/private window as such. Per-workspace throwaway profiles fill that role.
- Platform passkeys (Touch ID / iCloud Keychain passkeys) are unsupported. Only USB security keys work.
- Screen sharing (`getDisplayMedia`) is denied.
- No dedicated browser MCP server. Agent control is CLI + skill.

## Count
**181 catalogued feature rows** across 20 groups (sections 1-20), plus the Chrome-question table and the gap list.
