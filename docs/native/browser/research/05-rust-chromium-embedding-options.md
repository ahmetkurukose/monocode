# 06 — Embedding a full Chromium in a Rust + GPUI macOS app

Research date: 2026-11-20. Target: replace the `WKWebView` that `crates/monocode-gpui/src/browser/host.rs` hosts today. The goals are a Chromium engine that scrolls at 60–120 fps with low input latency and modest memory, and a path toward extensions, a password manager, DevTools, CDP agent control and profiles.

Facts are labelled by how they were obtained:

- **[verified]**: read in source, headers, or the shipped app bundles on this machine.
- **[doc]**: from vendor or project documentation.
- **[inferred]**: my own engineering judgement.

---

## 0. TL;DR

- **Phase 1 recommendation: CEF through the `cef` crate (tauri-apps/cef-rs), in *windowed* mode.**
  - Use the Chrome bootstrap with an Alloy-style browser, hosted as a child `NSView` inside the GPUI window. It sits exactly where the `WKWebView` sits today.
  - This keeps Chrome's own GPU compositor path: GPU process → `CAContext` → `CALayerHost` in our view, the same path Chrome.app uses. So scrolling and input latency equal Chrome's.
  - It is the least work, because IME, accessibility, drag-and-drop, cursors and native popups come for free.
  - CDP agent control comes from `CefBrowserHost::SendDevToolsMessage` in-process, with no open port.
- **Phase 1b (optional, behind a flag): CEF off-screen rendering (OSR) with `shared_texture_enabled`.**
  - Each frame arrives as an IOSurface, is wrapped as a Metal texture and is drawn by GPUI as a normal element. GPUI can then draw over the page, clip it and animate it.
  - This needs a small patch to our vendored `gpui-pre-apple`: today's `paint_surface` only accepts NV12 and `assert_eq!`s on anything else, so it needs a BGRA path.
  - Costs: about one frame of extra latency, plus IME, accessibility, drag-and-drop, popups and context menus all become our job.
- **"Full Chrome" features are the hard part, and no option gives them for free in an embedded view on macOS.**
  - CEF's Chrome extension API works only with **Chrome-style** browsers [doc]. On macOS, Chrome-style browsers can only be hosted in CEF's own Views `NSWindow`. Native-parent (`SetAsChild`) Chrome style is Windows/Linux only, and issue #3294 has been open since 2022 [doc/verified].
  - Phase 2 options, cheapest first:
    - (a) Use Chrome-style CEF windows for "pop-out" and extension-management surfaces.
    - (b) Contribute or patch macOS Chrome-style child-view support into CEF.
    - (c) Do what Atlas and Dia do: build our own Chromium "host" (the `//chrome` layer plus a thin embedder API) and composite it via `CALayerHost`. This is a dedicated-team-sized investment.
- **Ruled out for the main browser:**
  - CDP `Page.startScreencast` (JPEG over a websocket, about 10–30 fps, high latency).
  - An Electron helper (Chromium plus Electron's thin extension and password support, and a cross-process texture problem).
  - Servo/Verso and Ladybird (not Chromium; site compatibility).

---

## 1. Ground truth from shipped apps on this Mac [verified]

I inspected the bundles in `/Applications` (read-only):

| App | How Chromium is shipped | Chromium version | Notes |
|---|---|---|---|
| **ChatGPT Atlas** | The main app has no Chromium in-process. It nests a second app, `Contents/Support/ChatGPT Atlas.app` (`com.openai.atlas.web`, `LSUIElement=1`, 308 MB). That app holds `ChatGPT Atlas Framework.framework` and helpers named `(GPU)`, `(Renderer)`, `(Plugin)`, `(Alerts)` and `(Service)`. The UI app links `OwlBridge.framework` (5 MB) and `Aura.framework` (100 MB, Swift UI). | 144.0.7559.110, while Chrome stable here is 157 | This is the OWL host/client split. `OwlBridge` exports a small C API: `OwlBridge_Initialize`, `OwlBridge_LaunchHost`, `OwlBridge_GetMojoAPI`, `OwlBridge_GetIpczAPI/Node`, `OwlBridge_MapBuffer`, key-code translators (`OwlBridge_KeyboardCodeFromNSEvent`, `DomKeyFromNSEvent`, …), URL formatters, and `OwlBridge_ICloudKeychain_*`. Strings in Aura include `caContextId` and "CALayerHost unsupported", which confirms `CALayerHost` compositing. |
| **Dia** (The Browser Company) | `ArcCore.framework` (335 MB) loads **in-process**. Helpers are `Browser Helper (GPU/Renderer/Alerts/…)`, plus separate "Aperitif" helper variants. | 156.0.8078.74 | It ships 204 public ObjC++ headers (`ArcWebContents`, `ArcBrowserContext{Extensions,DevTools,Downloads,History,Autocomplete}`, `ArcDevToolsProtocolCommand`, `*.mojom-objc.h`). `ArcWebContents` exposes `- (NSView*)nativeView` and `attachToPaneHostView:`, so it uses windowed `NSView` embedding. Arc uses the same ArcCore (877 MB app). |
| **Comet** (Perplexity) | `Comet Framework.framework` (1.3 GB with versions). | 148/149 | A classic Chromium fork that keeps Chrome's own UI. |
| **Orca** | Electron 43.8.7, `Electron Framework.framework` 278 MB. | 150.0.7871.250 | The browser pane is an Electron `WebContentsView`: an `NSView` (RenderWidgetHostViewCocoa) inside the Electron `BrowserWindow`. It is windowed, not OSR. |
| **Google Chrome** | `Google Chrome Framework.framework` (1.4 GB here because two versions sit side by side). | 157.0.8103.80 | |

Takeaways:

1. Every serious Chromium-based macOS browser with a custom UI uses windowed `NSView` or `CALayerHost` embedding of Chromium's own compositor output. None of them use OSR pixel copies.
2. Expect **about 250–330 MB** of added, uncompressed bundle size for one Chromium version. Atlas's host app is 308 MB and Electron's framework is 278 MB. Compressed in a DMG it is roughly a third of that [inferred from the Arch CEF package: 110 MB compressed vs 354 MB installed].

---

## 2. Option A — CEF from Rust

### 2.1 Rust bindings: status

| Crate | Status (Nov 2026) | Notes |
|---|---|---|
| **`cef` (tauri-apps/cef-rs)** | **Very active.** The latest release is `156.7.0+156.0.31`, published 2026-11-19, and the repo was pushed the same day. 493 stars, about 424k downloads. Crate versions follow CEF major versions, with many releases per CEF branch [verified via crates.io/GitHub API]. | Apache-2.0/MIT. Covers macOS, Windows and Linux on x64 and arm64. Ships tools: `export-cef-dir` and `download-cef` (fetch the CEF binary distribution) and **`bundle-cef-app`**, which builds the macOS `.app` with helpers. Includes the `cefsimple` example (with a helper binary). Has an `accelerated_osr` feature with `osr_texture_import/iosurface.rs`: it wraps the CEF IOSurface with `newTextureWithDescriptor:iosurface:plane:` (BGRA8/RGBA8) and copies into a wgpu-owned texture. That code targets wgpu, not GPUI. |
| **`gpui-cef` (bokuweb/gpui-cef)** | Small (3 stars) but very relevant. Pushed 2026-10-26, uses CEF 151. | **CEF OSR inside GPUI on macOS** (WebView2 fallback on Windows). It already solved the GPUI-specific problems: `NSApplication`/CefAppProtocol shimming (`nsapp.rs`), an external message pump on the main `CFRunLoop` (`pump.rs`, median IPC latency cut from about 15 ms to about 0.3–1 ms), IME via GPUI's `EntityInputHandler` mapped to `ImeSetComposition/ImeCommitText`, popups, dialogs, downloads and file choosers. **It confirms the NV12-only blocker in GPUI's `surface()`.** Its default path is CPU `OnPaint` → `RenderImage` (about 3.9 MB per frame at 1200×820). The README says roughly 38% CPU at 60 fps of continuous repaint. Good reference code, not a dependency to ship. |
| `wef` (longbridge) | Experimental; the authors say "still not good enough". Longbridge still ships Wry/WKWebView. | Cites about 1 GB of app-size impact (all architectures). |
| `cef-ui`, `cef-sys`, `bunlet-cef-native`, `bevy_cef`, Electrobun's CEF bundling | Various | Older, niche, or not GPUI. |
| `cbf` / `cbf-chrome` ("Chromium Browser Framework") | Alpha (0.1.0-alpha.8, May 2026). The GitHub repo returned 404. | Interesting precedent: its macOS `SurfaceHandle` is `MacCaContextId(u32)`, the Atlas-style `CALayerHost` approach from Rust. Too immature to rely on. |

**Verdict:** depend on the `cef` crate, which is maintained under the Tauri org and is a fast follower of CEF stable. Borrow integration ideas from `gpui-cef`.

### 2.2 CEF on macOS: bundling, signing and size [doc]

- **Bundle layout is fixed:**
  - `MyApp.app/Contents/Frameworks/Chromium Embedded Framework.framework`
  - `MyApp.app/Contents/Frameworks/MyApp Helper.app`, `MyApp Helper (GPU).app`, `MyApp Helper (Renderer).app`, `MyApp Helper (Plugin).app`, `MyApp Helper (Alerts).app`
  - The helpers are separate bundles so they get no Dock icons and can carry different entitlements. The helper is a tiny Rust binary that calls `cef::execute_process`; cef-rs's `cefsimple_helper.rs` shows the pattern.
- **Loading:** the main executable and the helpers load the framework dynamically through `CefScopedLibraryLoader` (`LoadInMain` / `LoadInHelper`), and helpers initialize the macOS sandbox. Running a bare binary does not work; you must launch through the bundle (also noted by gpui-cef).
- **Signing and notarization:** CEF's `docs/code_signing.md` was updated in October 2026.
  - Enable the Hardened Runtime on every executable.
  - Renderer and GPU helpers need `com.apple.security.cs.allow-jit`. Base the main app's entitlements on Chromium's unbranded `app-entitlements.plist`; the generic, Alerts and Plugin helpers get an empty dictionary.
  - Sign inside-out without `--deep`, explicitly signing the dylibs in the framework's `Libraries`.
  - Then `notarytool submit` and `stapler`.
  - Our existing release pipeline needs a per-helper signing loop.
- **Size:** about 250–300 MB uncompressed per architecture for framework plus helpers (Electron's comparable framework is 278 MB). Ship arm64-only if possible; a universal build doubles this.
- **Startup and memory:** a browser process lives in our app process, plus a GPU process and a network/utility process. There is one renderer per site instance, and you can cap renderers with `--renderer-process-limit` [doc/inferred]. Initialize lazily on first browser open so app startup is unaffected.
- **GPUI-specific integration requirements** [verified in gpui-cef README]:
  - CEF on macOS requires the `NSApplication` subclass to implement `CefAppProtocol` (`isHandlingSendEvent` / `setHandlingSendEvent:`). GPUI creates its own `GPUIApplication`, so add those methods at runtime (class_addMethod) before `CefInitialize`, after GPUI creates NSApp.
  - `multi_threaded_message_loop` is not supported on macOS. Use `external_message_pump = true` and drive `CefDoMessageLoopWork()` from `OnScheduleMessagePumpWork` via a `CFRunLoopSource` and timer on the main run loop.
  - Never call `CefDoMessageLoopWork` while GPUI's `App` is borrowed. Re-entrancy is a real crash source.

### 2.3 Windowed mode (child NSView), the Phase 1 pick

- **How it works:** `CefWindowInfo::SetAsChild(parent_nsview, rect)` with `runtime_style = CEF_RUNTIME_STYLE_ALLOY` (native parent requires Alloy style on macOS). CEF creates the browser `NSView`, a `RenderWidgetHostViewCocoa`, inside our parent view.
  - The GPU process renders into a `CAContext`. Its id travels to the browser process, which hosts it in a `CALayerHost` under that NSView [doc: Chromium `ui/base/cocoa/remote_layer_api.h`, JxBrowser write-up].
  - This is the **same presentation path as Chrome.app**: no copies, display-synchronized, 120 Hz on ProMotion when Chrome would do it.
- **Performance:** identical to Chrome. Scrolling runs on the compositor thread and input goes directly from AppKit to the RWHV. No extra frame is added.
- **Comes for free:**
  - IME (RWHVCocoa implements `NSTextInputClient`), VoiceOver accessibility, drag-and-drop in and out, cursors.
  - `<select>` and color-picker popups (separate native windows).
  - Native context menus, which can be overridden via `CefContextMenuHandler`.
  - Fullscreen video, print preview (but see §2.5), and spellcheck.
- **Costs and limits:**
  - GPUI cannot draw *over* the page. Layering, rounded-corner clipping and animated transitions of the page don't work. These are the same limits as today's `WKWebView` in `host.rs`, so nothing regresses.
  - Keyboard routing between GPUI shortcuts and the page. Use `CefKeyboardHandler::OnPreKeyEvent` to claim app shortcuts. That replaces the responder juggling that `host.rs` does today.
  - Our clip view and `place()` logic carry over almost 1:1.
- **Hidden tabs:** `setHidden:` on the NSView plus occlusion lets Chromium throttle hidden renderers. `CefBrowserHost::WasHidden` is meant for OSR; on macOS windowed, NSView visibility drives it [inferred; verify in spike].

### 2.4 Off-screen rendering (OSR) with shared IOSurface into GPUI

- **API [verified in CEF headers, master]:**
  - `cef_window_info_t.shared_texture_enabled = 1` plus `SetAsWindowless(parent_nsview)`. Pass a parent so context menus and some dialogs still work.
  - CEF calls `CefRenderHandler::OnAcceleratedPaint(browser, type, dirtyRects, const CefAcceleratedPaintInfo& info)` instead of `OnPaint`. On macOS, `info.shared_texture_io_surface` is an `IOSurfaceRef`, `info.format` is the color type (BGRA8 in practice), and `info.extra` holds common metadata.
  - The header is explicit: **"The underlying implementation uses a pool to deliver frames… The handle's resource cannot be cached and cannot be accessed outside of this callback… contents should be copied to a texture owned by the client application."** Notes:
    - The comment in `cef_types_mac.h` saying "currently only supported on Windows" is stale. The render-handler header and the viz-OSR reimplementation (CEF PR #285, tested on Mac) cover macOS.
    - `windowless_frame_rate` now documents only a minimum of 1 and a default of 30; set it to 120, or use `external_begin_frame_enabled` + `SendExternalBeginFrame()` driven from GPUI's display link to phase-lock CEF frames to our vsync.
- **The pipeline into GPUI:**
  1. In `OnAcceleratedPaint` (CEF UI thread, which is the main thread on macOS), wrap the IOSurface with `MTLDevice newTextureWithDescriptor:iosurface:plane:0`.
  2. Blit it into one of 2–3 app-owned `IOSurface`-backed BGRA textures (a GPU copy of about 4–16 MB per frame; trivial bandwidth on Apple silicon).
  3. Wrap the destination in a `CVPixelBuffer` with `CVPixelBufferCreateWithIOSurface`.
  4. `cx.notify()`. The element calls `window.paint_surface(bounds, cvpixelbuffer)`.
  - Alternatively, add a new `PaintTexture` primitive that takes an `MTLTexture` directly.
- **GPUI blocker [verified in `vendor/gpui-pre-apple/src/metal_renderer.rs` ~L1983–2075]:** `draw_surfaces` does `assert_eq!(CVPixelBufferGetPixelFormatType(..), kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)` and samples Y and CbCr planes. It was written for Zed screen-share. Because we vendor `gpui-pre-apple`, the fix is small:
  - Branch on `kCVPixelFormatType_32BGRA`.
  - Create one `BGRA8Unorm` texture via `CVMetalTextureCache`.
  - Add a `surface_bgra_fragment` shader (and its `runtime_shaders` variant) that samples and outputs premultiplied color, honouring `content_mask`.
  - Corner radii are still TODO in GPUI's Surface, but could be added in the same shader.
- **Latency [inferred]:** CEF composites, then hands the frame off, then GPUI re-composites on its next `CVDisplayLink` tick. That is about 1 extra frame (8 ms at 120 Hz, 16 ms at 60 Hz) compared with windowed. With external begin frames it is about 1 frame; without them, beat-frequency jitter is possible. Scrolling is still compositor-driven inside Chromium, so it stays smooth. The extra frame shows up mainly as input-to-photon latency.
- **What you must implement yourself in OSR:**
  - Mouse, wheel and trackpad: `SendMouse*Event` and `SendMouseWheelEvent`, plus momentum and phase for precise scrolling. CEF OSR's touchpad-phase support is weaker than native RWHV, which can make OSR scrolling feel non-native [inferred, known CEF OSR pain].
  - Keys: `SendKeyEvent` with NSEvent → `cef_key_event_t` translation.
  - **IME:** an `NSTextInputClient` bridge to `ImeSetComposition/ImeCommitText/ImeCancelComposition` and `OnImeCompositionRangeChanged` (gpui-cef has this).
  - Accessibility: `OnAccessibilityTreeChange` gives you a tree, but you must build an NSAccessibility tree yourself. That is substantial.
  - Drag-and-drop: `StartDragging`, `UpdateDragCursor` and `DragTarget*`.
  - Popups: `OnPopupShow/OnPopupSize` with `PET_POPUP` paints; you composite the second surface.
  - Cursors, tooltips, and DevTools, which can't be windowless [doc].
- **When OSR is worth it:** you need GPUI UI over or around web content (command palette over the page, rounded-corner cards, tab-switch animations, thumbnails, agent highlight overlays). OSR also gives a free per-frame texture for agent screenshots and thumbnails.

### 2.5 Chrome style vs. Alloy style: what "full Chrome" you get [doc]

- **Timeline:** since M125 the Chrome *bootstrap* can create Chrome-style or Alloy-style browsers. The Alloy bootstrap was **removed in M128**. CEF 156 always uses the Chrome bootstrap, and style is chosen per browser or window via `runtime_style`.
- **Chrome style** gives Chrome's own browser features and UI from the `//chrome` layer: Chrome extension API, permission bubbles, datalist and autofill, password-manager UI (save/fill bubbles), downloads UI, settings pages, device pickers, error pages, gamepad, WebHID and WebMIDI. **But:**
  - It requires the CEF **Views** framework (`CefWindow` / `CefBrowserView`), so CEF owns the `NSWindow`.
  - It doesn't support OSR.
  - "External (native) parent" Chrome style works on **Windows/Linux only** (#3685 test matrix). Issue #3294, macOS native-parent Chrome style, is still open; the latest commits are from July 2026 and are teardown fixes.
  - **On macOS today you can't put a Chrome-style browser inside the GPUI window.**
- **Alloy style** (what embedding on macOS forces):
  - Has the content-layer plus CEF callbacks, OSR and native parent.
  - **The Chrome extension API is not supported.** The old Alloy extension API was removed in M128.
  - DevTools popups are Chrome-style-only windows. Print preview doesn't work with native parent on macOS.
  - No Chrome permission prompts: gpui-cef notes that with Alloy, camera, mic and notification requests are not granted unless you implement `CefPermissionHandler`.
  - The password manager and autofill bubbles depend on `Browser`/Chrome UI; treat them as absent and plan our own [inferred; verify].
- **Profiles:** `CefRequestContext` with its own `cache_path` under `CefSettings.root_cache_path` maps to a Chrome Profile in the Chrome bootstrap. An empty `cache_path` gives an in-memory, incognito-like context. That fits agent "logged-out" sessions, which Atlas builds on `StoragePartition`.
- **PDF viewer:** works in both styles. The PDF extension is internal [inferred from CEF behaviour; verify].
- **Chrome Web Store:** I found no confirmation that CWS install works even in Chrome style. CEF extension API coverage is added piecemeal (issue #1947). Treat "install from CWS" as unproven.

---

## 3. Option B — Drive the user's installed Chrome or Chrome for Testing

- **Screencast:** `Page.startScreencast` delivers JPEG/PNG frames base64-encoded over the CDP websocket. Each frame needs a `screencastFrameAck`.
  - Real-world rates are about 10–30 fps with encode, decode and transfer latency of tens of milliseconds, a CPU cost on both ends, no IME and no native scrolling physics. Input goes in via `Input.dispatch*Event`.
  - Unusable as the primary view at 60–120 fps. Fine for agent thumbnails and "watch the agent" previews.
- **Launching a real Chrome window and positioning it over our window:** macOS cannot reparent another process's window into ours. You can only float a borderless `--app=` window and chase our window's frame, which breaks z-order, Spaces, focus and full-screen. It is fragile.
  - Capturing the Chrome window with ScreenCaptureKit (IOSurface, up to 120 fps) and injecting input through CDP is technically possible, but adds latency, triggers the screen-recording permission and is a hack.
- **Chrome 136+ restriction:** `--remote-debugging-port/pipe` are **ignored on the default user-data-dir**, an anti-infostealer change. So you can't silently drive the user's real profile; you need a separate `--user-data-dir`, which starts with no logins. Chrome for Testing and Chromium are not affected. Recent Chrome also has the user-consented `chrome://inspect/#remote-debugging` toggle for attaching to a running Chrome.
- **Verdict:** valuable as a *secondary* agent mode ("act in my real Chrome"), not as the embedded browser. Zero bundle cost, and features are complete because it's real Chrome, but it isn't embedded.

## 4. Option C — Build against Chromium's content or chrome layer directly (Atlas, Dia, Brave model)

- **What it is:** your own Chromium checkout plus patches, either `//content` (like Electron or CEF Alloy) or `//chrome` with your own embedder API (like Atlas's OWL and Dia's ArcCore), producing a framework or host app.
- **Benefits:**
  - You can expose *real Chrome* extensions, password manager, autofill, sync-less profiles, Chrome permission UI and DevTools *into your own embedded views*. Atlas even projects Chrome's Views UI, such as permission prompts, into its windows via `CALayerHost`.
  - You control the patch set and the rendering hand-off.
- **Costs:**
  - About 100 GB checkout. A full arm64 build takes hours on an M-series Max; incremental builds take minutes.
  - Dedicated CI with build caching (Reclient/Siso remote execution, or a large ccache).
  - A rebase every 4 weeks (Chrome's stable cadence) plus security respins. Falling behind is visible even for OpenAI: Atlas ships Chromium 144 while Chrome is at 157, which is a security liability.
  - Mojo or IPC bindings for the client language, and a signing pipeline.
  - Realistically **2–4 engineers who know Chromium, ongoing**. Brave, Arc and Atlas all have Chromium teams.
- **For us:** this is the long-term "own browser" end state, and only worth it if extensions plus password manager *in embedded tabs* become core product. See §6 for how OWL structures it.

## 5. Option D — Electron as a helper process

- **Electron's shared-texture OSR:** `webPreferences.offscreen.useSharedTexture` hands paint events an IOSurface on macOS. Electron's docs say the IOSurface is **local to its process**. Getting it into our process needs `IOSurfaceCreateMachPort` and a mach-port hand-off through a native module, or a `CALayerHost` contextId hack. A JS round trip also sits in the frame path.
- **Electron's extensions** are a limited `chrome.*` subset: no Chrome Web Store, no password manager, no autofill UI. You'd be shipping Node.js and an IPC protocol of your own.
- **Verdict:** strictly worse than CEF for us: the same engine size, fewer embedder hooks, more moving parts. Orca gets away with it because its whole UI *is* Electron, so `WebContentsView` is a native in-window `NSView`, the same windowed path as Phase 1 here.

## 6. ChatGPT Atlas's OWL in detail (most relevant precedent)

Source: OpenAI, "How we built OWL, the new architecture behind our ChatGPT-based browser, Atlas" (2025-10-30), plus the bundle inspection in §1.

- **Process split:**
  - The *OWL Client* is the Atlas Swift app (`Aura.framework` UI plus `OwlBridge.framework`).
  - The *OWL Host* is Chromium's **browser process** running in a separate, hidden app (`Support/ChatGPT Atlas.app`, LSUIElement). That app has its own GPU, renderer, utility and alerts helpers.
  - If Chromium's main thread hangs or crashes, the Atlas UI survives.
  - Chromium boots asynchronously, so the first pixels of Atlas UI appear immediately.
- **IPC:**
  - **Mojo**, Chromium's IPC, with OpenAI-written **Swift (and TypeScript) Mojo bindings**.
  - `OwlBridge` exports the Mojo C system API and the ipcz node (`OwlBridge_GetMojoAPI`, `GetIpczAPI`, `GetIpczNode`), `MapBuffer` for shared memory and `LaunchHost`. Generated Swift bindings speak `owl.mojom.*` interfaces on top. One visible type is `owl::mojom::PublicKeyCredentialDescriptor`, used for passkeys and iCloud Keychain.
- **Client API surface:**
  - `Session`: global host control.
  - `Profile`: per-profile state.
  - `WebView`: one `WebContents` covering rendering, input, navigation and zoom.
  - `WebContentRenderer`: input in, renderer feedback out.
  - `LayerHost/Client`: exchange of compositing info.
  - Service endpoints for bookmarks, downloads, **extensions** and **autofill**.
- **Rendering:**
  - On the host, each tab container is a `gfx::AcceleratedWidget` backed by a `CALayer`. Its **`CAContext` id** is sent to the client, which hosts it with the private **`CALayerHost`** inside an `NSView`.
  - Tabs swap into one shared compositing container. `<select>` and color pickers use the same trick via their own `RenderWidgetHostView`.
  - **Chromium Views UI** (permission prompts and so on) is projected into Atlas the same way.
  - Geometry and device scale are kept in sync so Chromium produces layers at the right size.
- **Input:**
  - The client translates `NSEvent` → Blink `WebInputEvent` itself (hence the exported keycode translators) and sends them down over Mojo.
  - Events the page didn't consume come back and are re-synthesized as `NSEvent` for app shortcuts.
- **Agent mode:**
  - Screenshots for the computer-use model composite popups back into one image.
  - Agent input goes **straight to the renderer**, bypassing the privileged browser layer.
  - Logged-out agent sessions use in-memory **`StoragePartition`s**: isolated and discarded at the end, distinct from Incognito.
- **Engineering:**
  - OWL is shipped internally as a **prebuilt binary**, so app engineers don't build Chromium and Atlas builds take minutes.
  - A deliberately small patch set eases Chromium uprevs, though shipping 144 when stable is 157 shows the lag.
  - Theo (t3.gg) critiques the hand-written Mojo and input layers as heavy to maintain and hard to port beyond macOS.
- **Private-API caveat:** `CALayerHost`/`CAContext` are undocumented. Chromium resolves them at runtime (`remote_layer_api.mm`), and Electron disables them for Mac App Store builds. They are fine for Developer-ID distribution.
- **Lesson for us:** OWL is the "Option C done right" template. A Rust version would be:
  - Our own Chromium host app.
  - A thin mojom (or simpler) protocol.
  - Rust bindings over the Mojo C API, or a non-Mojo channel such as a Unix socket with a flatbuffers/capnp schema, plus `CAContext` ids for frames.
  - A `CALayerHost` inside a GPUI-managed `NSView`.
  - CEF in windowed mode gives us about 80% of the same *runtime* behaviour today, minus the process isolation and the Chrome-layer features.

**Dia/Arc (ADK/ArcCore) contrast:** the Chromium browser process runs **in-process** as an ObjC++ framework, on a current Chromium (156). Its rich API includes extensions, DevTools, CDP commands, history and autocomplete, and `WebContents.nativeView` is attached into Swift views. It is architecturally like "CEF windowed, but with the chrome layer exposed". The Browser Company has a Chromium team to sustain it.

## 7. Option E — Non-Chromium engines

- **Servo (and the Verso shell built on it):** Rust-native and embeddable, but not Chromium. Site compatibility, DRM, and Chrome extensions and CWS rule it out. Worth watching for lightweight previews only.
- **Ladybird:** pre-alpha (alpha targeted for 2026), its own engine, not Chromium.
- **WKWebView (status quo):** already child-`NSView` embedded. It isn't Chrome, has no Chrome extensions, and has limited CDP (Web Inspector protocol only).

---

## 8. Comparison

Scores are 1–5 (5 is best). For effort, 5 means least effort.

| Option | Scroll/input perf | Chrome feature completeness (embedded) | Effort | Bundle size added | Risk | Notes |
|---|---|---|---|---|---|---|
| **A1. CEF windowed, Alloy style, child NSView** | **5**, identical to Chrome | 3: Chromium engine, DevTools (separate window), CDP, profiles, PDF; no extensions or Chrome password UI | **4** (weeks) | about 250–300 MB | Low–med (CEF uprevs; NSApp shim; key routing) | Drop-in for today's `WKWebView` host |
| **A2. CEF OSR + IOSurface → GPUI (patched)** | 4: +1 frame; trackpad and IME are our job | 3 (same as A1) | 2–3 (IME, a11y, DnD, popups) | same | Medium (input fidelity, a11y) | Enables GPUI overlays and animations |
| **A3. CEF Chrome style (Views window)** | 5 | **4–5**: extensions, permission/autofill/password UI | 4 | same | Medium: separate NSWindow; not inside GPUI layout on macOS | Use for pop-out windows and the extension manager |
| A4. CEF + upstream patch: macOS Chrome style in native parent (#3294) | 5 | 4–5 | 2 (needs C++/Chromium skill; carry a patch until upstreamed) | same | Med–high | Best mid-term path to full features inside GPUI |
| B1. CDP screencast of Chrome/CfT | 1 (10–30 fps JPEG) | 5 (real Chrome), but not embedded | 4 | 0 | Low | Agent previews only |
| B2. External Chrome window chase or SCK capture | 2–3 | 5 | 3 | 0 | High (fragile, needs permissions) | Not recommended |
| C. Own Chromium host (OWL/ArcCore-style) | 5 | **5** (we decide) | **1** (team-years, ongoing) | about 300 MB | High (staffing, security cadence) | End state if browser becomes the product |
| D. Electron helper with shared texture | 3–4 | 2 (Electron extension subset) | 2 | about 280 MB + Node | Medium–high | Dominated by A |
| E. Servo, Ladybird, WKWebView | 3–5 | 1–2 (not Chrome) | — | — | — | Not Chrome |

---

## 9. Recommendation

### Phase 1: "open a Chrome browser in the app with extreme performance"

Use **A1: CEF (cef-rs `cef` = 156.x) with the Chrome bootstrap, Alloy-style browsers, windowed as child NSViews** inside `browser/host.rs`'s existing clip view.

- It is the only option with Chrome-identical frame pacing (120 Hz ProMotion) and input latency at low effort. Native IME, a11y, DnD and popups are free.
- It keeps everything we built for the `WKWebView` host: placement, clip, zoom, hide-on-switch, keyboard release.
- Agent tools move from injected JS to **CDP** via `SendDevToolsMessage`, which removes our HOOKS shim for console and network: CDP `Runtime`/`Network`/`Log` domains give those natively.

**Spike checklist (1–2 weeks):**

1. Bundle via `bundle-cef-app`, or our own xtask that mirrors it, and run with GPUI.
2. Install the NSApp CefAppProtocol shim.
3. Run an external message pump on `CFRunLoopSource`.
4. Embed with `SetAsChild` and measure scroll fps, using Chrome's FPS meter or `CADisplayLink` traces at 120 Hz.
5. Wire up `OnPreKeyEvent` shortcut routing.
6. Connect CDP via `SendDevToolsMessage`.
7. Measure memory per tab.
8. Verify Hardened-Runtime signing and notarization.
9. Verify hidden-tab throttling.

### Phase 1b

Patch the vendored `gpui-pre-apple` for BGRA `CVPixelBuffer` surfaces and prototype **A2 (OSR + shared IOSurface)** for the specific surfaces that need GPUI on top: tab thumbnails, the agent overlay, animated transitions. Measure added latency with external begin frames driven by GPUI's display link. Keep windowed as the default for the main browsing surface.

### Path to full features

1. **DevTools:** `ShowDevTools` in a Chrome-style window, or point a remote frontend at the CDP endpoint. Inline DevTools in GPUI would be an OSR DevTools browser — not supported for popups. Use a separate window.
2. **Profiles:** one `CefRequestContext` per monocode profile under `root_cache_path/Profiles/<id>`. Use an in-memory context for agent "logged-out" sessions, mirroring Atlas's StoragePartition approach.
3. **Permissions, downloads, dialogs:** implement `CefPermissionHandler`, `CefDownloadHandler` and `CefJSDialogHandler` with GPUI UI. gpui-cef shows the shapes.
4. **Password manager:** Alloy has no Chrome password UI. Options:
   - (a) Our own credential store using the macOS Keychain and AuthenticationServices (passkeys, iCloud Keychain, which Atlas's OwlBridge also does), filled via CDP or injected script.
   - (b) Rely on Chrome-style windows.
   - (c) Use 1Password/Bitwarden once extensions work.
5. **Extensions and Chrome Web Store:**
   - Short term: Chrome-style CEF windows (A3) where extensions run. First validate which extension APIs and whether CWS install work in CEF 156; this is unproven.
   - Mid term: fund or upstream **macOS Chrome style with native parent (#3294)** so Chrome-style browsers live inside GPUI (A4).
   - Long term, if the browser becomes a core product: an **OWL-style own Chromium host** (C).
6. **CDP agent control at scale:** expose `remote_debugging_port` only bound to localhost, or better, an in-process CDP multiplexer, so Playwright or our MCP tools can attach per tab. Agent input goes via `Input.dispatch*` to the renderer, like Atlas's renderer-direct agent input.

---

## 10. Technical sketch of the recommended pipeline

### 10.1 Process layout

```
Monocode.app (GPUI, Rust)                       ← CEF browser process lives here (UI thread = main thread)
 ├─ Contents/Frameworks/Chromium Embedded Framework.framework   (~250 MB, CEF 156.x)
 ├─ Contents/Frameworks/Monocode Helper.app             (utility/network; empty entitlements)
 ├─ Contents/Frameworks/Monocode Helper (GPU).app       (allow-jit; one GPU process)
 ├─ Contents/Frameworks/Monocode Helper (Renderer).app  (allow-jit; one per site instance)
 ├─ Contents/Frameworks/Monocode Helper (Plugin).app
 └─ Contents/Frameworks/Monocode Helper (Alerts).app
Helpers = one tiny Rust bin `monocode-cef-helper` (CefScopedLibraryLoader::LoadInHelper + sandbox init + cef::execute_process), copied 5× with different Info.plists.
```

- `CefSettings`:
  - `root_cache_path = ~/Library/Application Support/Monocode/Browser`
  - `external_message_pump = 1`, `no_sandbox = 0`
  - `browser_subprocess_path` = the helper path
  - `windowless_rendering_enabled = 1` only if Phase 1b is enabled
  - `remote_debugging_port = 0` by default, using in-process CDP
- `CefInitialize` runs lazily the first time a browser pane opens, after GPUI has created NSApp and after the CefAppProtocol methods are added to `GPUIApplication`.
- **Message pump:** `CefBrowserProcessHandler::OnScheduleMessagePumpWork(delay_ms)` records a deadline and signals a `CFRunLoopSource`. The source handler (main thread, outside any GPUI borrow) re-arms a `CFRunLoopTimer` that calls `cef::do_message_loop_work()`. A 1/60 s fallback timer covers missed wakeups. Re-entrancy guard: if called during nested loops such as menus or modal panels, defer.

### 10.2 Rendering path

**Windowed (default):**

```
Renderer (Blink/cc) → GPU process (viz, Metal/ANGLE) → CAContext(id)
      → browser process: CALayerHost(id) inside CEF's RenderWidgetHostViewCocoa
      → child of our `clip` NSView (existing host.rs) → above GPUI's CAMetalLayer content view
      → WindowServer composites at display rate (120 Hz ProMotion)
```

- GPUI lays out a placeholder element and, in `prepaint`, reports its bounds. `host.place(parent, frame, web_frame, zoom)` sets the clip and CEF view frames, using the existing code path.
- For zoom, use `CefBrowserHost::SetZoomLevel` instead of `WKWebView.pageZoom`.

**OSR (Phase 1b, opt-in per surface):**

```
GPU process → pooled IOSurface (BGRA) → OnAcceleratedPaint(info.shared_texture_io_surface)  [main thread]
   → MTLTexture(src) = device.newTextureWithDescriptor:iosurface:plane:0
   → blit into ring[i] (app-owned IOSurface-backed BGRA, 3 slots) ; commit; (no CPU wait—GPUI's later command buffer on same MTLDevice/queue orders after it, or use MTLSharedEvent)
   → CVPixelBufferCreateWithIOSurface(ring[i]) → store in entity → cx.notify()
GPUI frame: div().child(surface(pixel_buffer).object_fit(Fill))  → patched draw_surfaces BGRA branch
Frame pacing: external_begin_frame_enabled=1; GPUI display-link tick → host.SendExternalBeginFrame()
```

- On resize, call `WasResized()`, supply `GetViewRect` and `GetScreenInfo` with `device_scale_factor` from the GPUI window, and reallocate the ring.
- Popups (`PET_POPUP`) are a second ring drawn at `OnPopupSize`.

### 10.3 Input path

- **Windowed:** AppKit delivers mouse, scroll (with phases and momentum), keys and IME directly to `RenderWidgetHostViewCocoa`, which is first responder. App shortcuts are handled in `CefKeyboardHandler::OnPreKeyEvent`:
  - If the key matches a GPUI keybinding (⌘T, ⌘L, ⌘W, ⌘K…), return `true` and dispatch it to GPUI via `window.dispatch_keystroke`.
  - Otherwise return `false` so the page gets the key.
  - `OnKeyEvent` handles keys the page didn't consume (like Atlas's re-synthesis).
  - Focus moves between GPUI and the page with `CefBrowserHost::SetFocus(bool)` plus `makeFirstResponder:`. That replaces `release_keyboard` in host.rs.
- **OSR:**
  - GPUI `MouseDown/Up/Move/ScrollWheel` → `SendMouseClickEvent/MoveEvent/WheelEvent`, in DIP coordinates relative to the element origin, with modifiers mapped and precise deltas.
  - Keys: GPUI `KeyDown` → `cef_key_event_t`. Use `native_key_code` from the NSEvent keyCode and `character` and `unmodified_character`; `RAWKEYDOWN`, then `CHAR` for printable keys.
  - **IME:** implement GPUI `EntityInputHandler` for the webview entity:
    - `replace_and_mark_text_in_range` → `ImeSetComposition`
    - `replace_text_in_range` → `ImeCommitText`
    - `unmark_text` → `ImeFinishComposingText`
    - `bounds_for_range` ← `OnImeCompositionRangeChanged` rects
  - Use `ImeCommitText` for astral characters, not `CHAR`, which carries only one UTF-16 unit (gpui-cef learned this).
- **Agent input:** CDP `Input.dispatchMouseEvent/KeyEvent/insertText` over `SendDevToolsMessage`. This is independent of the UI path, so the agent can drive background tabs.

### 10.4 Multi-tab

- One `CefBrowser` per tab, created with `CreateBrowserSync` for immediate handles. Tab state (URL, title, favicon, loading, history) comes from `CefDisplayHandler` and `CefLoadHandler`.
- The active tab's NSView is visible; others get `setHidden:YES`, so Chromium sees occlusion and throttles rAF and timers. In OSR, call `WasHidden(true)` and stop begin frames.
- **Memory:** renderer per site instance. Add a tab-discard policy: after N minutes hidden or under memory pressure (`DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`), close the browser and keep URL and session snapshot, then restore on focus. Optionally set `--renderer-process-limit`.
- **Popups:** `CefLifeSpanHandler::OnBeforePopup` becomes a new monocode tab, or a GPUI-hosted child for OAuth windows (keep `opener` for OAuth flows).

### 10.5 Profiles and data directories

```
~/Library/Application Support/Monocode/Browser/            ← root_cache_path
   Profiles/default/        ← CefRequestContext(cache_path=...)  (cookies, localStorage, IndexedDB, cache)
   Profiles/<project-id>/   ← optional per-project isolation
   (agent sessions)         ← CefRequestContext with empty cache_path = in-memory, discarded on close
```

- Contexts are created with `CefRequestContext::CreateContext(settings, handler)`. Share one per profile across tabs.
- Never point CEF at the user's real Chrome profile. The cookie encryption differs (Chrome bootstrap uses Keychain "… Safe Storage"), and Chrome 136+ forbids debugging the default dir anyway.
- Import from Chrome could be a later feature: read Chrome's cookies and passwords via Keychain with user consent.

### 10.6 CDP / agent control

- `CefBrowserHost::AddDevToolsMessageObserver` plus `SendDevToolsMessage(json)` / `ExecuteDevToolsMethod(id, method, params)` per browser. Map our MCP `browser_*` tools onto `Runtime.evaluate`, `DOM`/`Accessibility.getFullAXTree`, `Input.*`, `Page.captureScreenshot`, `Network.*` and `Log.*`.
- In OSR mode, screenshots can come from the last IOSurface for free.
- Optional: `remote_debugging_port` on 127.0.0.1 with a random port, protected, so external Playwright can `connectOverCDP`. Off by default for security.

---

## 11. Open questions to verify in the spike

1. In CEF 156, do Chrome extension *content scripts* run in **Alloy-style** browsers that share a profile with a Chrome-style window? If yes, a hybrid is possible: extensions installed via a Chrome-style window, active in embedded tabs.
2. Does Chrome-style plus CWS install work in CEF 156 on macOS?
3. Hidden-tab throttling for windowed Alloy on macOS (NSView hidden vs. `WasHidden`).
4. Trackpad momentum and rubber-band fidelity in OSR vs. windowed.
5. Latency of OSR external-begin-frame vs. windowed, measured end to end at 120 Hz.
6. Memory baseline: CEF browser process inside our app plus GPU and network processes, vs. today's WebKit (`com.apple.WebKit.*` XPC).

---

## Sources

- tauri-apps/cef-rs: https://github.com/tauri-apps/cef-rs ; crate: https://crates.io/crates/cef ; docs.rs history: https://docs.rs/crate/cef/latest
- cef-rs IOSurface import: https://docs.rs/crate/cef/latest/source/src/osr_texture_import/mod.rs and https://docs.rs/crate/cef/latest/source/src/osr_texture_import/iosurface.rs
- bokuweb/gpui-cef (CEF OSR in GPUI; NV12 blocker; IME; pump): https://github.com/bokuweb/gpui-cef
- longbridge/wef: https://github.com/longbridge/wef ; gpui-component WebView (Wry): https://longbridge.github.io/gpui-component/docs/components/webview ; gpui-wry: https://docs.rs/crate/gpui-wry/0.5.0
- GPUI Surface element: https://docs.rs/crate/gpui/0.2.2/source/src/elements/surface.rs ; Zed "Pass CVImageBuffers into GPUI": https://git.secluded.site/zed/commit/531ffc01c91bb0bccc82be6f90e3ff0870d7ea4b ; local: `vendor/gpui-pre-apple/src/metal_renderer.rs` (`draw_surfaces`)
- CEF headers (master): https://github.com/chromiumembedded/cef/blob/master/include/cef_render_handler.h , https://github.com/chromiumembedded/cef/blob/master/include/internal/cef_types_mac.h , https://github.com/chromiumembedded/cef/blob/master/include/internal/cef_types.h
- CEF docs: https://chromiumembedded.github.io/cef/general_usage.html , https://github.com/chromiumembedded/cef/blob/master/docs/architecture.md , https://github.com/chromiumembedded/cef/blob/master/docs/code_signing.md
- CEF Chrome/Alloy style split announcement: https://groups.google.com/g/cef-announce/c/s1WaovAopFo ; Alloy bootstrap removal #3685: https://github.com/chromiumembedded/cef/issues/3685 ; Chrome style native parent #3294: https://github.com/chromiumembedded/cef/issues/3294 ; style split #3681: https://github.com/chromiumembedded/cef/issues/3681 ; extensions #1947: https://github.com/chromiumembedded/cef/issues/1947
- CEF viz OSR shared texture reimplementation: https://bitbucket.org/chromiumembedded/cef/pull-requests/285 ; cef-mixer sample: https://github.com/twobrainsgmbh/cef-mixer ; CEF+Metal POC: https://github.com/encse/cef-test
- OpenAI, Building ChatGPT Atlas / OWL: https://openai.com/index/building-chatgpt-atlas ; mirror: https://bittide.aicompass.dev/article/eefbaba4-84a7-4c82-8a5a-15c1232e169c ; ByteByteGo: https://blog.bytebytego.com/p/the-architecture-behind-atlas-openais ; Theo digest: https://recapio.com/digest/chatgpt-atlas-isnt-just-a-chrome-wrapper-by-theo-t3-gg
- Chromium remote layer API: https://chromium.googlesource.com/chromium/src/+/HEAD/ui/base/cocoa/remote_layer_api.mm ; JxBrowser CALayer cross-process rendering: https://teamdev.com/jxbrowser/blog/cross-process-rendering-using-calayer ; Electron MAS remote-layer patch: https://ayakael.net/mirrors/electron/blame/commit/90af7d7fe2fc70984404474feec4ac2c827794fe/patches/chromium/mas_disable_remote_layer.patch
- cbf / cbf-chrome: https://docs.rs/crate/cbf/latest , https://docs.rs/cbf-chrome/latest/src/cbf_chrome/data/surface.rs.html
- Electron offscreen rendering (shared texture): https://electronjs.org/docs/latest/tutorial/offscreen-rendering ; PR: https://github.com/electron/electron/pull/42001
- Chrome 136 remote-debugging change: https://developer.chrome.com/blog/remote-debugging-port
- CDP screencast: https://pub.dev/documentation/puppeteer/latest/protocol_page/PageApi/startScreencast.html
- Dia/Arc: https://www.thurrott.com/cloud/web-browsers/321435/the-browser-company-discusses-the-future-of-arc-dia-and-the-web ; https://en.wikipedia.org/wiki/Arc_(web_browser)
- Arch CEF package size (compression ratio proxy): https://www.archlinux.org/packages/extra/x86_64/cef
- Local bundle inspection (read-only): `/Applications/ChatGPT Atlas.app`, `/Applications/Dia.app`, `/Applications/Arc.app`, `/Applications/Comet.app`, `/Applications/Orca.app`, `/Applications/Google Chrome.app`
