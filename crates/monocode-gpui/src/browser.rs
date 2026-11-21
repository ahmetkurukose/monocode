//! The browser pane: a Chromium page (CEF, `monocode-cef`) shown as a native
//! child view of the window (docs/native/browser/context.md §4).
//!
//! Chromium ships in the app bundle and starts in this process the first
//! time a page is shown ([`Engine`]).
//!
//! A page's view sits above GPUI's own view, so GPUI cannot draw over it.
//! Each frame every shown page records that it painted; the [`sweep`] element,
//! painted last, hides the pages that did not (their tab is not in front) and
//! moves away the ones a menu, a modal or a notification covers ([`cover`]),
//! drawing their last frame in their place instead (K16). A page's docked
//! DevTools (K18) is a second view, placed and swept the same way.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Corners, Entity, EventEmitter, FocusHandle,
    Focusable, Global, Image, ImageFormat, KeyBinding, KeyDownEvent, Pixels, RenderImage,
    Subscription, Window, WindowId, actions, canvas, div, prelude::*, px,
};

use crate::{
    input::{self, Input},
    modal,
    popover::{Align, Anchor, AnchorSlot, popover},
    theme::{content, icon, icon_button, icon_button_enabled},
};

actions!(
    workspace,
    [
        /// ⌥⌘I: the focused page's DevTools, in a window of its own.
        ToggleBrowserDevTools
    ]
);

/// Whether Chromium runs, shared by every window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EngineState {
    /// No page asked for Chromium yet.
    Idle,
    Ready,
    Failed(String),
}

/// The app's one Chromium runtime.
pub(crate) struct Engine {
    pub state: EngineState,
    /// A start is queued for after the current frame.
    queued: bool,
}

struct GlobalEngine(Entity<Engine>);
impl Global for GlobalEngine {}

impl Engine {
    pub(crate) fn global(cx: &mut App) -> Entity<Engine> {
        if let Some(engine) = cx.try_global::<GlobalEngine>() {
            return engine.0.clone();
        }
        let engine = cx.new(|_| Engine {
            state: EngineState::Idle,
            queued: false,
        });
        cx.set_global(GlobalEngine(engine.clone()));
        engine
    }

    /// Starts Chromium after the current frame, unless it runs. Starting
    /// takes a few hundred milliseconds on the main thread (CEF's UI thread),
    /// so the page shows that it is starting first. `profiles` is the root of
    /// the browser profiles of this data mode.
    pub(crate) fn ensure(&mut self, profiles: PathBuf, cx: &mut Context<Self>) {
        if self.state == EngineState::Ready || std::mem::replace(&mut self.queued, true) {
            return;
        }
        if matches!(self.state, EngineState::Failed(_)) {
            self.state = EngineState::Idle;
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |engine, cx| engine.start_now(&profiles, cx));
        })
        .detach();
    }

    fn start_now(&mut self, profiles: &Path, cx: &mut Context<Self>) {
        self.queued = false;
        let started = std::time::Instant::now();
        self.state = match start(profiles) {
            Ok(()) => {
                let took = started.elapsed();
                if took > std::time::Duration::from_secs(1) {
                    log::warn!("browser: Chromium took {took:?} to start");
                }
                EngineState::Ready
            }
            Err(e) => {
                log::warn!("browser: Chromium is unavailable: {e}");
                EngineState::Failed(e)
            }
        };
        cx.notify();
    }
}

/// The running app's `Contents/Frameworks`.
fn bundle_frameworks() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let contents = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("the app has no bundle")?;
    Ok(contents.join("Frameworks"))
}

/// The one `* Helper.app` in `frameworks` (the GPU, Renderer, … variants
/// carry a suffix after "Helper").
fn find_helper(frameworks: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(frameworks).ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().into_string().ok()?;
        let app = name.strip_suffix(" Helper.app")?;
        let helper = format!("{app} Helper");
        let path = entry.path().join("Contents/MacOS").join(&helper);
        path.is_file().then_some(path)
    })
}

fn start(profiles: &Path) -> Result<(), String> {
    if monocode_cef::runtime::is_started() {
        return Ok(());
    }
    let frameworks = bundle_frameworks()?;
    let framework_dir = frameworks.join("Chromium Embedded Framework.framework");
    let missing = "Chromium is missing from the app: run it from its bundle (npm run native:dev)";
    if !framework_dir.is_dir() {
        return Err(missing.into());
    }
    let config = monocode_cef::RuntimeConfig {
        framework_dir,
        helper: find_helper(&frameworks).ok_or(missing)?,
        cache_root: profiles.to_path_buf(),
        // A development build's signature changes with every build, which
        // would ask for keychain access each time.
        mock_keychain: cfg!(debug_assertions),
        locale: String::new(),
        log_file: Some(profiles.join("chromium.log")),
    };
    let _ = std::fs::create_dir_all(profiles);
    monocode_cef::runtime::start(&config).map_err(|e| e.to_string())
}

/// What the user typed, as an address to load: a URL as it is, a host or a
/// local address with a scheme added, anything else as a search.
pub(crate) fn address(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    let lower = typed.to_ascii_lowercase();
    if [
        "http://",
        "https://",
        "file://",
        "about:",
        "chrome://",
        "data:",
    ]
    .iter()
    .any(|scheme| lower.starts_with(scheme))
    {
        return Some(typed.to_owned());
    }
    let host = lower.split(['/', '?', '#']).next().unwrap_or_default();
    let name = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            name
        } else {
            host
        }
    });
    let local = name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.parse::<std::net::Ipv4Addr>().is_ok();
    if !typed.contains(char::is_whitespace) {
        if local {
            return Some(format!("http://{typed}"));
        }
        if name.contains('.') && !name.starts_with('.') && !name.ends_with('.') {
            return Some(format!("https://{typed}"));
        }
    }
    let query: String = typed
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Some(format!("https://www.google.com/search?q={query}"))
}

/// A page's tab title: its own, else its host, else "Browser".
pub(crate) fn page_title(url: &str, title: &str) -> String {
    if !title.trim().is_empty() {
        return title.to_owned();
    }
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .filter(|host| !host.is_empty());
    host.unwrap_or("Browser").to_owned()
}

/// A page's address, title or zoom changed; the pane saves them.
pub(crate) struct PageChanged;

// ---- Shortcuts (context.md K17) ----

/// A browser page's key context. Its bindings outrank the workspace's, so
/// ⌘+ zooms the page rather than the interface while a page is focused; the
/// shell's shortcut forwarding adds it while Chromium has the keyboard.
pub(crate) const KEY_CONTEXT: &str = "BrowserPage";
const FIND_CONTEXT: &str = "BrowserFind";

actions!(
    browser,
    [
        FocusAddress,
        Reload,
        ReloadIgnoringCache,
        Find,
        FindNext,
        FindPrevious,
        CloseFind,
        ZoomIn,
        ZoomOut,
        ResetZoom,
    ]
);

/// ⌘[ and ⌘] stay the workspace's Back and Forward (visited places), so
/// page history has only the toolbar buttons.
pub(crate) fn bind(cx: &mut App) {
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let page = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new(&format!("{m}-l"), FocusAddress, page),
        KeyBinding::new(&format!("{m}-r"), Reload, page),
        // Over the workspace's ⇧⌘R (restart the app) while a page is focused.
        KeyBinding::new(&format!("{m}-shift-r"), ReloadIgnoringCache, page),
        KeyBinding::new(&format!("{m}-f"), Find, page),
        KeyBinding::new(&format!("{m}-g"), FindNext, page),
        KeyBinding::new(&format!("{m}-shift-g"), FindPrevious, page),
        KeyBinding::new(&format!("{m}-="), ZoomIn, page),
        KeyBinding::new(&format!("{m}-+"), ZoomIn, page),
        KeyBinding::new(&format!("{m}--"), ZoomOut, page),
        KeyBinding::new(&format!("{m}-0"), ResetZoom, page),
        // The find field is a Composer, whose ⇧Enter would add a newline.
        KeyBinding::new("shift-enter", FindPrevious, Some("BrowserFind > Composer")),
        KeyBinding::new("escape", CloseFind, Some(FIND_CONTEXT)),
    ]);
}

// ---- Zoom ----

/// Chrome's zoom steps, in percent.
const ZOOM_PRESETS: [u16; 17] = [
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
];

/// The next preset above `percent` (`step` > 0) or below it (< 0); 0 resets.
fn zoom_step(percent: u16, step: i32) -> u16 {
    match step.signum() {
        0 => 100,
        1 => ZOOM_PRESETS
            .iter()
            .copied()
            .find(|p| *p > percent)
            .unwrap_or(ZOOM_PRESETS[ZOOM_PRESETS.len() - 1]),
        _ => ZOOM_PRESETS
            .iter()
            .rev()
            .copied()
            .find(|p| *p < percent)
            .unwrap_or(ZOOM_PRESETS[0]),
    }
}

/// Chromium's zoom level for a percentage: 0 is 100%, each level ×1.2.
fn zoom_level(percent: u16) -> f64 {
    (f64::from(percent) / 100.).ln() / 1.2f64.ln()
}

fn zoom_percent(level: f64) -> u16 {
    (1.2f64.powf(level) * 100.)
        .round()
        .clamp(1., f64::from(u16::MAX)) as u16
}

// ---- Find in page ----

/// The find bar under the toolbar (⌘F).
struct FindBar {
    field: Entity<Input>,
    /// Matches, and the selected one counting from 1; `None` until the page
    /// answers.
    result: Option<(i32, i32)>,
    _subscriptions: Vec<Subscription>,
}

/// "3 of 12", or nothing to say yet.
fn find_status(text: &str, result: Option<(i32, i32)>) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    match result? {
        (0, _) => Some("No results".into()),
        (count, active) => Some(format!("{} of {count}", active.clamp(1, count))),
    }
}

/// The profile a page browses with; Chromium's own type, or its stand-in
/// where Chromium is not available yet.
pub(crate) type Profile = monocode_cef::Profile;

/// What a page asks of the pane around it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PageRequest {
    /// A new page beside this one (a popup, `target=_blank`, a middle
    /// click). `popup` is a created popup the new page adopts
    /// ([`BrowserPage::adopt_popup`]); without it the page loads `url`.
    Open {
        url: String,
        foreground: bool,
        popup: Option<u64>,
    },
    /// The page closed itself (`window.close()`).
    Close,
}

/// Something the page waits on the user for, shown as a modal over it.
enum PagePrompt {
    Auth(monocode_cef::AuthRequest),
    Dialog(monocode_cef::JsDialog),
}

/// One browser page in a pane: its toolbar, and the Chromium view below.
pub(crate) struct BrowserPage {
    pub url: String,
    pub title: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    progress: f64,
    failure: Option<String>,
    address: Entity<Input>,
    /// Page zoom in percent; Chromium keeps it per host ([`Self::sync_zoom`]).
    pub zoom: u16,
    /// The saved zoom has yet to reach Chromium: it applies to the host of
    /// the first committed address.
    zoom_pending: bool,
    find: Option<FindBar>,
    /// Takes the keyboard from Chromium whenever the address field gets the
    /// focus (a click, ⌘L, a new blank page); made at the first render,
    /// which has the window.
    address_keyboard: Option<Subscription>,
    /// Where the page last drew, to take GPUI focus when Chromium gets it.
    window: Option<AnyWindowHandle>,
    engine: Entity<Engine>,
    profiles: PathBuf,
    host: Rc<Host<monocode_cef::BrowserView>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    // Downloads and the dialogs the page asks for.
    downloads: Vec<monocode_cef::Download>,
    downloads_open: bool,
    downloads_anchor: AnchorSlot,
    /// The first one shows; the rest wait their turn.
    prompts: VecDeque<PagePrompt>,
    /// The shown prompt's fields: user name and password, or a prompt's text.
    prompt_fields: Vec<Entity<Input>>,
    prompt_focus: FocusHandle,
    /// The shown prompt takes the keyboard on the next frame.
    focus_prompt: bool,
    _prompt_subscriptions: Vec<Subscription>,
}

impl EventEmitter<PageChanged> for BrowserPage {}
impl EventEmitter<PageRequest> for BrowserPage {}

impl Focusable for BrowserPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserPage {
    pub(crate) fn new(
        url: String,
        title: String,
        zoom: u16,
        profiles: PathBuf,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = Engine::global(cx);
        let field = cx.new(|cx| {
            let mut field =
                Input::field("Address", "Search or enter an address", content(0.35), cx);
            field.set_text(url.clone(), cx);
            field
        });
        let subscriptions = vec![
            cx.observe(&engine, |_, _, cx| cx.notify()),
            cx.subscribe(&field, |this, field, _: &input::Submitted, cx| {
                let typed = field.read(cx).content.to_string();
                if let Some(url) = address(&typed) {
                    this.load(url, cx);
                }
            }),
            // Snapshots sit in GPU atlases until given back.
            cx.on_release(|page, cx| page.host.release(cx)),
        ];
        Self {
            url,
            title,
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            progress: 0.,
            failure: None,
            address: field,
            zoom,
            zoom_pending: zoom != 100,
            find: None,
            address_keyboard: None,
            window: None,
            engine,
            profiles,
            host: Rc::new(Host::default()),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
            downloads: Vec::new(),
            downloads_open: false,
            downloads_anchor: AnchorSlot::default(),
            prompts: VecDeque::new(),
            prompt_fields: Vec::new(),
            prompt_focus: cx.focus_handle(),
            focus_prompt: false,
            _prompt_subscriptions: Vec::new(),
        }
    }

    pub(crate) fn display_title(&self) -> String {
        page_title(&self.url, &self.title)
    }

    /// The address field takes the keyboard while there is nothing to show;
    /// otherwise the page does. A field of the page that has it (the
    /// address, find, a prompt) keeps it.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.focus.contains_focused(window, cx) && !self.focus.is_focused(window) {
            return;
        }
        if self.url.is_empty() {
            window.focus(&self.address.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
            self.host.with_view(|view| view.set_focus(true));
        }
    }

    fn load(&mut self, url: String, cx: &mut Context<Self>) {
        self.url = url.clone();
        self.failure = None;
        if !self.host.with_view(|view| view.load_url(&url)) {
            cx.notify();
        }
        cx.emit(PageChanged);
    }

    fn on_event(&mut self, event: monocode_cef::BrowserEvent, cx: &mut Context<Self>) {
        use monocode_cef::BrowserEvent;
        match event {
            BrowserEvent::Address(url) => {
                self.sync_zoom(cx);
                if url != self.url {
                    self.url = url.clone();
                    self.address.update(cx, |field, cx| field.set_text(url, cx));
                    cx.emit(PageChanged);
                }
            }
            BrowserEvent::Title(title) => {
                if title != self.title {
                    self.title = title;
                    cx.emit(PageChanged);
                }
            }
            BrowserEvent::Loading {
                loading,
                can_go_back,
                can_go_forward,
            } => {
                self.loading = loading;
                self.can_go_back = can_go_back;
                self.can_go_forward = can_go_forward;
                if loading {
                    self.failure = None;
                } else {
                    self.sync_zoom(cx);
                    self.snapshot_when_settled(cx);
                }
            }
            BrowserEvent::Progress(progress) => self.progress = progress,
            BrowserEvent::LoadFailed { url, text, .. } => {
                self.failure = Some(format!("{url} could not be loaded: {text}"));
            }
            BrowserEvent::Closed => {}
            BrowserEvent::FindResult {
                count,
                active_ordinal,
                final_update,
            } => {
                if let Some(find) = &mut self.find
                    && (final_update || count > 0)
                {
                    find.result = Some((count, active_ordinal));
                }
            }
            BrowserEvent::Focused => self.follow_page_focus(cx),
            other => self.on_page_event(other, cx),
        }
        cx.notify();
    }

    /// Whether the find bar is open, and whether the address field or the
    /// find field has GPUI focus.
    #[cfg(test)]
    pub(crate) fn keyboard_state(&self, window: &Window, cx: &App) -> (bool, bool, bool) {
        let find = self.find.as_ref();
        (
            find.is_some(),
            self.address.focus_handle(cx).is_focused(window),
            find.is_some_and(|f| f.field.focus_handle(cx).is_focused(window)),
        )
    }

    /// A click in the page gives Chromium the keyboard without GPUI seeing
    /// it; GPUI focus follows, so the page's shortcuts apply.
    fn follow_page_focus(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.window else { return };
        let focus = self.focus.clone();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                if !focus.is_focused(window) {
                    window.focus(&focus, cx);
                }
            });
        });
    }

    /// Keeps [`Self::zoom`] in step with Chromium, which keeps one level per
    /// host: shared by the pages of a profile on that host and kept across
    /// navigations within it. Following it, rather than forcing a level per
    /// page, avoids rewriting the host's level under every other page on it.
    /// The saved zoom applies once, to the first host the page commits.
    fn sync_zoom(&mut self, cx: &mut Context<Self>) {
        let pending = std::mem::take(&mut self.zoom_pending);
        let wanted = self.zoom;
        let level = self.host.view.borrow().as_ref().map(|hosted| {
            if pending {
                hosted.view.set_zoom_level(zoom_level(wanted));
            }
            hosted.view.zoom_level()
        });
        let Some(level) = level else {
            self.zoom_pending = pending;
            return;
        };
        let percent = zoom_percent(level);
        if !pending && percent != self.zoom {
            self.zoom = percent;
            cx.emit(PageChanged);
            cx.notify();
        }
    }

    fn set_zoom(&mut self, percent: u16, cx: &mut Context<Self>) {
        if percent == self.zoom {
            return;
        }
        self.zoom = percent;
        // Before the page exists, its first address takes it.
        if !self
            .host
            .with_view(|view| view.set_zoom_level(zoom_level(percent)))
        {
            self.zoom_pending = true;
        }
        cx.emit(PageChanged);
        cx.notify();
    }

    fn reload(&mut self, ignore_cache: bool) {
        self.host.with_view(|view| {
            if ignore_cache {
                view.reload_ignore_cache();
            } else {
                view.reload();
            }
        });
    }

    /// ⌘L: the address field, all of it selected.
    fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_keyboard(window);
        window.focus(&self.address.focus_handle(cx), cx);
        self.address.update(cx, |field, cx| {
            let len = field.content.len();
            field.select_range(0..len, cx);
        });
    }

    /// Takes the keyboard back from Chromium for a GPUI field.
    fn take_keyboard(&self, window: &mut Window) {
        self.host.with_view(|view| view.set_focus(false));
        #[cfg(target_os = "macos")]
        mac::make_gpui_first_responder(window);
        #[cfg(not(target_os = "macos"))]
        let _ = window;
    }

    /// ⌘F: opens the find bar, or selects its text when it is open.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            let field = cx.new(|cx| Input::field("Find in page", "Find", content(0.35), cx));
            let subscriptions = vec![
                // A click into the field after the page had the keyboard.
                cx.on_focus_in(&field.focus_handle(cx), window, |this, window, _| {
                    this.take_keyboard(window)
                }),
                cx.subscribe(&field, |this, _, _: &input::Changed, cx| {
                    this.search(true, false, cx)
                }),
                cx.subscribe(&field, |this, _, _: &input::Submitted, cx| {
                    this.search(true, true, cx)
                }),
            ];
            self.find = Some(FindBar {
                field,
                result: None,
                _subscriptions: subscriptions,
            });
        }
        let Some(field) = self.find.as_ref().map(|find| find.field.clone()) else {
            return;
        };
        self.take_keyboard(window);
        window.focus(&field.focus_handle(cx), cx);
        field.update(cx, |field, cx| {
            let len = field.content.len();
            field.select_range(0..len, cx);
        });
        self.search(true, false, cx);
    }

    /// Searches the page for the find field's text: a new search while it
    /// is typed, else the next or previous match.
    fn search(&mut self, forward: bool, next: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let text = find.field.read(cx).content.to_string();
        if text.is_empty() {
            find.result = None;
            self.host.with_view(|view| view.stop_finding(true));
        } else {
            if !next {
                find.result = None;
            }
            self.host
                .with_view(|view| view.find(&text, forward, false, next));
        }
        cx.notify();
    }

    /// Esc: closes the find bar, clears its highlights and gives the page
    /// the keyboard back.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_none() {
            return;
        }
        self.host.with_view(|view| view.stop_finding(true));
        self.focus(window, cx);
        cx.notify();
    }

    fn find_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let find = self.find.as_ref()?;
        let text = find.field.read(cx).content.to_string();
        let status = find_status(&text, find.result);
        let can_step = find.result.is_some_and(|(count, _)| count > 0);
        Some(
            div()
                .key_context(FIND_CONTEXT)
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .h(px(30.))
                .flex_shrink_0()
                .border_b_1()
                .border_color(content(0.08))
                .child(
                    div()
                        .w(px(260.))
                        .min_w_0()
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .bg(content(0.05))
                        .child(find.field.clone()),
                )
                .child(
                    div()
                        .min_w(px(64.))
                        .px_1()
                        .text_size(px(11.))
                        .text_color(content(0.5))
                        .children(status),
                )
                .child(
                    icon_button_enabled(
                        "browser-find-previous",
                        "Previous Match",
                        "chevron-up",
                        can_step,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.search(false, true, cx))),
                )
                .child(
                    icon_button_enabled(
                        "browser-find-next",
                        "Next Match",
                        "chevron-down",
                        can_step,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.search(true, true, cx))),
                )
                .child(div().flex_1())
                .child(
                    icon_button_enabled("browser-find-close", "Close", "x", true)
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = self.host.view.borrow().is_some();
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .h(px(34.))
            .flex_shrink_0()
            .border_b_1()
            .border_color(content(0.08))
            .child(
                icon_button_enabled("browser-back", "Back", "arrow-left-2", self.can_go_back)
                    .on_click(cx.listener(|this, _, _, _| {
                        this.host.with_view(|view| view.go_back());
                    })),
            )
            .child(
                icon_button_enabled(
                    "browser-forward",
                    "Forward",
                    "arrow-right-2",
                    self.can_go_forward,
                )
                .on_click(cx.listener(|this, _, _, _| {
                    this.host.with_view(|view| view.go_forward());
                })),
            )
            .child(
                icon_button_enabled(
                    "browser-reload",
                    if self.loading { "Stop" } else { "Reload" },
                    if self.loading { "x" } else { "refresh-cw" },
                    ready,
                )
                .on_click(cx.listener(|this, _, _, _| {
                    let loading = this.loading;
                    this.host
                        .with_view(|view| if loading { view.stop() } else { view.reload() });
                })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .bg(content(0.05))
                    .child(self.address.clone()),
            )
            .when(self.zoom != 100, |d| {
                d.child(
                    div()
                        .id("browser-zoom")
                        .role(gpui::Role::Button)
                        .aria_label("Reset Zoom")
                        .flex_shrink_0()
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(px(11.))
                        .text_color(content(0.6))
                        .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                        .child(format!("{}%", self.zoom))
                        .on_click(cx.listener(|this, _, _, cx| this.set_zoom(100, cx))),
                )
            })
            .child(
                icon_button_enabled("browser-devtools", "DevTools", "code", ready)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_dev_tools(cx))),
            )
            .children(self.downloads_button(cx))
    }

    /// Opens the page's DevTools in a window of its own, or closes it.
    pub(crate) fn toggle_dev_tools(&mut self, cx: &mut Context<Self>) {
        self.host.with_view(|view| {
            if view.has_dev_tools() {
                view.close_dev_tools()
            } else {
                view.show_dev_tools()
            }
        });
        cx.notify();
    }

    /// A snapshot once a load has had a moment to paint, so an overlay has
    /// a recent frame to show at once.
    fn snapshot_when_settled(&self, cx: &mut Context<Self>) {
        let host = Rc::downgrade(&self.host);
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            cx.update(|cx| {
                if let Some(host) = host.upgrade()
                    && host.shown.get() == Placement::Shown
                {
                    host.refresh_snapshot(cx);
                }
            });
        })
        .detach();
    }

    /// Instead of the page while Chromium is not running yet.
    fn status(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (headline, detail, retry) = match &self.engine.read(cx).state {
            EngineState::Ready => return None,
            EngineState::Idle => ("Starting Chromium…".to_owned(), String::new(), false),
            EngineState::Failed(error) => {
                ("Chromium could not start".to_owned(), error.clone(), true)
            }
        };
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon("globe", 28.).text_color(content(0.3)))
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(content(0.8))
                        .child(headline),
                )
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(px(420.))
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .child(detail),
                    )
                })
                .when(retry, |d| {
                    d.child(
                        div()
                            .id("browser-retry")
                            .mt_2()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(content(0.08))
                            .hover(|s| s.bg(content(0.14)))
                            .text_size(px(12.))
                            .child("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.start_engine(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn start_engine(&mut self, cx: &mut Context<Self>) {
        let profiles = self.profiles.clone();
        self.engine
            .update(cx, |engine, cx| engine.ensure(profiles, cx));
    }

    /// Where Chromium draws: a canvas that places the native view each frame.
    fn surface(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let host = self.host.clone();
        let url = if self.url.is_empty() {
            "about:blank".to_owned()
        } else {
            self.url.clone()
        };
        let page = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let options = host.options.borrow().clone();
                host.paint(bounds, window, cx, |parent, frame, cx| {
                    match monocode_cef::BrowserView::create_with(parent, frame, &url, &options) {
                        Ok((view, events)) => {
                            let page = page.clone();
                            cx.spawn(async move |cx| {
                                while let Ok(event) = events.recv().await {
                                    if page
                                        .update(cx, |page, cx| page.on_event(event, cx))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            })
                            .detach();
                            Some(view)
                        }
                        Err(e) => {
                            let error = e.to_string();
                            let page = page.clone();
                            cx.defer(move |cx| {
                                let _ = page.update(cx, |page, cx| {
                                    page.failure = Some(error);
                                    cx.notify();
                                });
                            });
                            None
                        }
                    }
                })
            },
        )
        .size_full()
        .into_any_element()
    }
}

impl Render for BrowserPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window = Some(window.window_handle());
        if self.address_keyboard.is_none() {
            let address = self.address.focus_handle(cx);
            self.address_keyboard = Some(cx.on_focus_in(&address, window, |this, window, _| {
                this.take_keyboard(window)
            }));
        }
        if self.engine.read(cx).state == EngineState::Idle {
            self.start_engine(cx);
        }
        self.focus_prompt_now(window, cx);
        let body = match self.status(cx) {
            Some(status) => status,
            None => self.surface(cx),
        };
        div()
            .id("browser-page")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &FocusAddress, window, cx| this.focus_address(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Reload, _, _| this.reload(false)))
            .on_action(cx.listener(|this, _: &ReloadIgnoringCache, _, _| this.reload(true)))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.search(true, true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.search(false, true, cx)))
            .on_action(cx.listener(|this, _: &CloseFind, window, cx| this.close_find(window, cx)))
            .on_action(
                cx.listener(|this, _: &ZoomIn, _, cx| this.set_zoom(zoom_step(this.zoom, 1), cx)),
            )
            .on_action(
                cx.listener(|this, _: &ZoomOut, _, cx| this.set_zoom(zoom_step(this.zoom, -1), cx)),
            )
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.set_zoom(100, cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(cx))
            .children(self.find_bar(cx))
            .when_some(self.failure.clone(), |d, failure| {
                d.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(content(0.6))
                        .child(failure),
                )
            })
            .when(self.loading && self.progress < 1., |d| {
                d.child(
                    div().h(px(2.)).w_full().child(
                        div()
                            .h_full()
                            .w(gpui::relative(self.progress.clamp(0.05, 1.) as f32))
                            .bg(crate::theme::accent_color()),
                    ),
                )
            })
            .child(div().flex_1().min_h_0().min_w_0().child(body))
            .relative()
            .children(self.downloads_popover(cx))
            .children(self.prompt_overlay(cx))
    }
}

// ---- Popups, downloads, dialogs and profiles ----

/// A size for people: `512 B`, `3.4 MB`.
pub(crate) fn human_bytes(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    let units = ["KB", "MB", "GB", "TB"];
    if bytes < 1000. {
        return format!("{bytes} B");
    }
    let mut value = bytes / 1000.;
    let mut unit = 0;
    while value >= 1000. && unit + 1 < units.len() {
        value /= 1000.;
        unit += 1;
    }
    if value < 10. {
        format!("{value:.1} {}", units[unit])
    } else {
        format!("{value:.0} {}", units[unit])
    }
}

/// A download's line under its name.
pub(crate) fn download_status(download: &monocode_cef::Download) -> String {
    use monocode_cef::DownloadState;
    match download.state {
        DownloadState::InProgress => match download.total {
            Some(total) => format!(
                "{} of {}",
                human_bytes(download.received),
                human_bytes(total)
            ),
            None => human_bytes(download.received),
        },
        DownloadState::Complete => match download.total {
            Some(total) => human_bytes(total),
            None => human_bytes(download.received),
        },
        DownloadState::Canceled => "Canceled".into(),
        DownloadState::Failed => "Failed".into(),
    }
}

/// A host for a dialog's title: `example.com says`.
fn origin_host(origin: &str) -> String {
    let rest = origin.split("://").nth(1).unwrap_or(origin);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

impl BrowserPage {
    /// The events beyond the toolbar's: new pages, downloads and dialogs.
    fn on_page_event(&mut self, event: monocode_cef::BrowserEvent, cx: &mut Context<Self>) {
        use monocode_cef::BrowserEvent;
        match event {
            BrowserEvent::PopupRequested {
                url, foreground, ..
            } => cx.emit(PageRequest::Open {
                url,
                foreground,
                popup: None,
            }),
            BrowserEvent::PopupOpened {
                id,
                url,
                foreground,
            } => cx.emit(PageRequest::Open {
                url,
                foreground,
                popup: Some(id),
            }),
            BrowserEvent::CloseRequested => cx.emit(PageRequest::Close),
            BrowserEvent::Download(download) => {
                // The toolbar's indicator shows it; opening the list on its
                // own would hide the page, which no overlay may cover.
                match self.downloads.iter_mut().find(|d| d.id == download.id) {
                    Some(known) => *known = download,
                    None => self.downloads.push(download),
                }
            }
            BrowserEvent::Auth(request) => self.push_prompt(PagePrompt::Auth(request), cx),
            BrowserEvent::Dialog(dialog) => self.push_prompt(PagePrompt::Dialog(dialog), cx),
            BrowserEvent::DialogsReset => {
                let shown = matches!(self.prompts.front(), Some(PagePrompt::Dialog(_)));
                self.prompts.retain(|p| matches!(p, PagePrompt::Auth(_)));
                if shown {
                    self.show_prompt(cx);
                }
            }
            _ => {}
        }
    }

    /// The profile the page browses with. A page that exists already opens
    /// again in the new profile, at the same address.
    pub(crate) fn set_profile(&mut self, profile: monocode_cef::Profile, cx: &mut Context<Self>) {
        if self.host.options.borrow().profile == profile {
            return;
        }
        if self.host.view.borrow().is_some() {
            self.host = Rc::new(Host::default());
            cx.notify();
        }
        self.host.options.borrow_mut().profile = profile;
    }

    /// Takes over a popup this page's opener created (crate::popup in
    /// monocode-cef), so that it keeps `window.opener`. False when it is gone;
    /// the page then loads its address afresh.
    pub(crate) fn adopt_popup(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        #[cfg(target_os = "macos")]
        {
            use objc2::{MainThreadMarker, MainThreadOnly, msg_send, rc::Retained};
            use objc2_app_kit::NSView;
            use objc2_foundation::NSRect;

            if self.host.view.borrow().is_some() {
                return false;
            }
            let Some(mtm) = MainThreadMarker::new() else {
                return false;
            };
            let Some((view, events)) = monocode_cef::take_popup(id) else {
                return false;
            };
            let clip: Retained<mac::BrowserClip> = unsafe {
                let clip = mac::BrowserClip::alloc(mtm).set_ivars(());
                msg_send![super(clip), initWithFrame: NSRect::ZERO]
            };
            let clip: Retained<NSView> = Retained::into_super(clip);
            clip.setWantsLayer(true);
            clip.setClipsToBounds(true);
            view.reparent(Retained::as_ptr(&clip) as *mut std::ffi::c_void);
            // Hidden until its tab paints it.
            view.set_visible(false);
            *self.host.view.borrow_mut() = Some(Hosted { clip, view });
            let page = cx.weak_entity();
            cx.spawn(async move |_, cx| {
                while let Ok(event) = events.recv().await {
                    if page
                        .update(cx, |page, cx| page.on_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (id, cx);
            false
        }
    }

    fn downloads_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use monocode_cef::DownloadState;
        if self.downloads.is_empty() {
            return None;
        }
        // Overall progress of what is still coming, when sizes are known.
        let active: Vec<_> = self
            .downloads
            .iter()
            .filter(|d| d.state == DownloadState::InProgress)
            .collect();
        let progress = (!active.is_empty()).then(|| {
            let fractions: Vec<f64> = active.iter().filter_map(|d| d.fraction()).collect();
            if fractions.is_empty() {
                0.
            } else {
                fractions.iter().sum::<f64>() / fractions.len() as f64
            }
        });
        Some(
            div()
                .relative()
                .flex_shrink_0()
                .child(self.downloads_anchor.probe())
                .child(
                    icon_button("browser-downloads", "Downloads", "arrow-down-circle").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.downloads_open = !this.downloads_open;
                            cx.notify();
                        }),
                    ),
                )
                .when_some(progress, |d, progress| {
                    d.child(
                        div()
                            .absolute()
                            .bottom(px(1.))
                            .left(px(5.))
                            .right(px(5.))
                            .h(px(2.))
                            .rounded_full()
                            .bg(content(0.12))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .w(gpui::relative(progress.clamp(0.08, 1.) as f32))
                                    .bg(crate::theme::accent_color()),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    fn downloads_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.downloads_open || self.downloads.is_empty() {
            return None;
        }
        let weak = cx.weak_entity();
        let rows: Vec<AnyElement> = self
            .downloads
            .iter()
            .rev()
            .map(|download| self.download_row(download, cx))
            .collect();
        Some(
            popover(
                "browser-downloads-popover",
                Anchor::Slot(self.downloads_anchor.clone()),
            )
            .align(Align::End)
            .width(320.)
            .max_height(360.)
            .padding(6.)
            .on_dismiss(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.downloads_open = false;
                    cx.notify();
                });
            })
            .child(
                div()
                    .id("browser-downloads-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element(),
        )
    }

    fn download_row(
        &self,
        download: &monocode_cef::Download,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use monocode_cef::DownloadState;
        let id = download.id;
        let running = download.state == DownloadState::InProgress;
        let path = download.path.clone();
        div()
            .id(("browser-download", id as usize))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|s| s.bg(content(0.05)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(content(
                                if download.state == DownloadState::Complete || running {
                                    0.9
                                } else {
                                    0.5
                                },
                            ))
                            .child(download.name()),
                    )
                    .when(running, |d| {
                        d.child(
                            div()
                                .h(px(3.))
                                .w_full()
                                .rounded_full()
                                .bg(content(0.08))
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .w(gpui::relative(
                                            download.fraction().unwrap_or(0.).clamp(0.03, 1.)
                                                as f32,
                                        ))
                                        .bg(crate::theme::accent_color()),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.45))
                            .child(download_status(download)),
                    ),
            )
            .when(download.state != DownloadState::Canceled, |d| {
                d.child(
                    icon_button("browser-download-reveal", "Show in Finder", "folder-open")
                        .on_click(move |_, _, cx| cx.reveal_path(&path)),
                )
            })
            .when(running, |d| {
                d.child(
                    icon_button("browser-download-cancel", "Cancel download", "x")
                        .on_click(cx.listener(move |_, _, _, _| monocode_cef::cancel_download(id))),
                )
            })
            .into_any_element()
    }

    fn push_prompt(&mut self, prompt: PagePrompt, cx: &mut Context<Self>) {
        self.prompts.push_back(prompt);
        if self.prompts.len() == 1 {
            self.show_prompt(cx);
        }
    }

    /// Makes the fields of the prompt in front, and gives it the keyboard.
    fn show_prompt(&mut self, cx: &mut Context<Self>) {
        let field = |label: &str, cx: &mut Context<Self>| {
            let label = label.to_owned();
            cx.new(|cx| {
                let mut input = Input::field(label.clone(), label, content(0.35), cx);
                input.set_font_size(13.);
                input
            })
        };
        let fields = match self.prompts.front() {
            Some(PagePrompt::Auth(_)) => {
                let user = field("User name", cx);
                let password = field("Password", cx);
                password.update(cx, |input, _| input.set_secret(true));
                vec![user, password]
            }
            Some(PagePrompt::Dialog(dialog)) => match &dialog.kind {
                monocode_cef::DialogKind::Prompt(text) => {
                    let input = field("Answer", cx);
                    let text = text.clone();
                    input.update(cx, |input, cx| input.set_text(text, cx));
                    vec![input]
                }
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        self._prompt_subscriptions = fields
            .iter()
            .map(|field| {
                cx.subscribe(field, |this, _, _: &input::Submitted, cx| {
                    this.answer_prompt(true, cx)
                })
            })
            .collect();
        self.prompt_fields = fields;
        self.focus_prompt = !self.prompts.is_empty();
        if self.prompts.is_empty() {
            // The page takes the keyboard back on the next click into it.
            self.host.with_view(|view| view.set_focus(true));
        }
        cx.notify();
    }

    fn focus_prompt_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.focus_prompt) {
            return;
        }
        self.host.with_view(|view| view.set_focus(false));
        match self.prompt_fields.first() {
            Some(field) => window.focus(&field.focus_handle(cx), cx),
            None => window.focus(&self.prompt_focus, cx),
        }
    }

    /// Answers the prompt in front: OK / Sign In, or Cancel.
    fn answer_prompt(&mut self, accept: bool, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompts.pop_front() else {
            return;
        };
        let text = |ix: usize, cx: &Context<Self>| {
            self.prompt_fields
                .get(ix)
                .map(|f| f.read(cx).content.to_string())
                .unwrap_or_default()
        };
        match &prompt {
            PagePrompt::Auth(request) if accept => {
                request.sign_in(text(0, cx).trim(), &text(1, cx));
            }
            PagePrompt::Auth(request) => request.cancel(),
            PagePrompt::Dialog(dialog) if accept => {
                let answer =
                    matches!(dialog.kind, monocode_cef::DialogKind::Prompt(_)).then(|| text(0, cx));
                dialog.accept(answer.as_deref());
            }
            PagePrompt::Dialog(dialog) => dialog.decline(),
        }
        self.show_prompt(cx);
    }

    fn prompt_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use monocode_cef::DialogKind;
        let prompt = self.prompts.front()?;
        let (title, detail, accept, cancel) = match prompt {
            PagePrompt::Auth(request) => {
                let c = &request.challenge;
                let place = if c.port == 0 || c.port == 80 || c.port == 443 {
                    c.host.clone()
                } else {
                    format!("{}:{}", c.host, c.port)
                };
                let what = if c.proxy { "The proxy" } else { "The site" };
                let mut detail = format!("{what} {place} asks for a user name and password.");
                if !c.realm.is_empty() {
                    detail.push_str(&format!(" It says: “{}”", c.realm));
                }
                (
                    format!("Sign in to {}", c.host),
                    detail,
                    "Sign In",
                    Some("Cancel"),
                )
            }
            PagePrompt::Dialog(dialog) => {
                let says = match origin_host(&dialog.origin) {
                    host if host.is_empty() => "This page says".to_owned(),
                    host => format!("{host} says"),
                };
                match &dialog.kind {
                    DialogKind::Alert => (says, dialog.message.clone(), "OK", None),
                    DialogKind::Confirm | DialogKind::Prompt(_) => {
                        (says, dialog.message.clone(), "OK", Some("Cancel"))
                    }
                    DialogKind::BeforeUnload { reload } => (
                        if *reload {
                            "Reload this page?"
                        } else {
                            "Leave this page?"
                        }
                        .to_owned(),
                        "Changes you made may not be saved.".to_owned(),
                        if *reload { "Reload" } else { "Leave" },
                        Some("Stay"),
                    ),
                }
            }
        };
        let labels: &[&str] = match prompt {
            PagePrompt::Auth(_) => &["User name", "Password"],
            PagePrompt::Dialog(_) => &[""],
        };
        let fields = self.prompt_fields.iter().zip(labels).map(|(field, label)| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .when(!label.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.5))
                            .child(*label),
                    )
                })
                .child(
                    div()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(content(0.12))
                        .bg(content(0.04))
                        .px_2()
                        .py(px(6.))
                        .child(field.clone()),
                )
        });
        let card = modal::card().w(px(380.)).child(
            modal::card_body()
                .child(modal::title(title))
                .when(!detail.is_empty(), |d| {
                    d.child(
                        modal::body(detail)
                            .mt(px(6.))
                            .max_h(px(240.))
                            .overflow_hidden(),
                    )
                })
                .child(
                    div()
                        .mt(px(14.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .children(fields),
                )
                .child(
                    modal::actions()
                        .when_some(cancel, |d, cancel| {
                            d.child(
                                modal::ghost_button("browser-prompt-cancel", cancel)
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.answer_prompt(false, cx)),
                                    ),
                            )
                        })
                        .child(
                            modal::primary_button("browser-prompt-accept", accept)
                                .cursor_pointer()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.answer_prompt(true, cx)),
                                ),
                        ),
                ),
        );
        let has_fields = !self.prompt_fields.is_empty();
        let dismissible = cancel.is_some();
        Some(
            div()
                .absolute()
                .inset_0()
                .track_focus(&self.prompt_focus)
                .key_context("BrowserPrompt")
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.answer_prompt(!dismissible, cx),
                        // Fields submit on Return themselves.
                        "enter" if !has_fields => this.answer_prompt(true, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(modal::overlay("browser-prompt", card, {
                    let weak = cx.weak_entity();
                    move |_, cx| {
                        let _ = weak.update(cx, |this, cx| this.answer_prompt(!dismissible, cx));
                    }
                }))
                .into_any_element(),
        )
    }
}

// ---- The native view and its visibility ----

thread_local! {
    /// Every page's and DevTools' host, for [`sweep`].
    static HOSTS: RefCell<Vec<Weak<dyn Swept>>> = const { RefCell::new(Vec::new()) };
    /// Per window: the frame being painted, and what overlays covered in it.
    static FRAMES: RefCell<HashMap<WindowId, Frame>> = RefCell::new(HashMap::new());
}

/// What overlays covered since the last sweep, taken; tests read it with
/// no page open, when no sweep runs.
#[cfg(test)]
pub(crate) fn take_covers() -> Vec<Bounds<Pixels>> {
    FRAMES.with(|f| {
        (f.borrow_mut().values_mut())
            .flat_map(|frame| std::mem::take(&mut frame.covers))
            .collect()
    })
}

#[derive(Default)]
struct Frame {
    epoch: u64,
    covers: Vec<Bounds<Pixels>>,
}

/// A Chromium view a [`Host`] places.
trait NativeView: 'static {
    fn set_frame(&self, frame: monocode_cef::Frame);
    fn set_visible(&self, visible: bool);
    fn capture(&self, done: Box<dyn FnOnce(Result<Vec<u8>, String>)>);
}

impl NativeView for monocode_cef::BrowserView {
    fn set_frame(&self, frame: monocode_cef::Frame) {
        self.set_frame(frame);
    }
    fn set_visible(&self, visible: bool) {
        self.set_visible(visible);
    }
    fn capture(&self, done: Box<dyn FnOnce(Result<Vec<u8>, String>)>) {
        self.capture(done);
    }
}

/// How a host's view shows, as the last sweep decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Placement {
    /// Not painted (its tab is not in front): hidden from Chromium too, which
    /// throttles it.
    #[default]
    Hidden,
    /// Under an overlay: moved out of the window but still live for
    /// Chromium, so it can be captured; its snapshot is drawn in its place.
    Covered,
    Shown,
}

/// What a host painted in this frame at `bounds` becomes, under `covers`.
fn shown(painted: bool, bounds: Bounds<Pixels>, covers: &[Bounds<Pixels>]) -> Placement {
    if !painted || bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
        Placement::Hidden
    } else if covers.iter().any(|cover| cover.intersects(&bounds)) {
        Placement::Covered
    } else {
        Placement::Shown
    }
}

/// A capture asked for longer ago than this is given up: a page Chromium
/// does not draw may never answer.
const CAPTURE_PATIENCE: Duration = Duration::from_secs(3);

/// A Chromium view inside a clipping view of the window, and the last frame
/// captured from it.
struct Host<V> {
    view: RefCell<Option<Hosted<V>>>,
    /// The window and frame it was last painted in, and where.
    painted: Cell<Option<(WindowId, u64, Bounds<Pixels>)>>,
    shown: Cell<Placement>,
    registered: Cell<bool>,
    /// One recent frame, at the screen's scale. Given back to the GPU atlas
    /// when replaced, when the view hides, and with the page.
    snapshot: RefCell<Option<Arc<RenderImage>>>,
    /// When the capture on its way was asked for.
    capturing: Cell<Option<Instant>>,
    /// The page is gone: captures still on their way are dropped.
    released: Cell<bool>,
    /// The profile the page is created in ([`BrowserPage::set_profile`]).
    options: RefCell<monocode_cef::CreateOptions>,
}

impl<V> Default for Host<V> {
    fn default() -> Self {
        Self {
            view: RefCell::default(),
            options: RefCell::default(),
            painted: Cell::default(),
            shown: Cell::default(),
            registered: Cell::default(),
            snapshot: RefCell::default(),
            capturing: Cell::default(),
            released: Cell::default(),
        }
    }
}

struct Hosted<V> {
    #[cfg(target_os = "macos")]
    clip: objc2::rc::Retained<objc2_app_kit::NSView>,
    view: V,
}

/// Covered views wait this far outside the window (points).
#[cfg(target_os = "macos")]
const AWAY: f64 = -100_000.;

/// Painted last in a window, it settles each host painted there.
trait Swept {
    fn sweep(
        self: Rc<Self>,
        window_id: WindowId,
        epoch: u64,
        covers: &[Bounds<Pixels>],
        window: &mut Window,
        cx: &mut App,
    );
}

impl<V: NativeView> Swept for Host<V> {
    fn sweep(
        self: Rc<Self>,
        window_id: WindowId,
        epoch: u64,
        covers: &[Bounds<Pixels>],
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((painted_in, painted, bounds)) = self.painted.get() else {
            return;
        };
        if painted_in != window_id {
            return;
        }
        let next = shown(painted == epoch, bounds, covers);
        let prev = self.shown.replace(next);
        self.apply(prev, next);
        match next {
            Placement::Hidden if prev != Placement::Hidden => {
                self.set_snapshot(None, Some(window), cx)
            }
            Placement::Covered if prev != Placement::Covered => {
                // Live while covered, so this capture is of the frame just
                // left; until it comes, the last one shows, if any.
                self.refresh_snapshot(cx);
                if self.snapshot.borrow().is_some() {
                    window.on_next_frame(|window, _| window.refresh());
                }
            }
            _ => {}
        }
    }
}

impl<V: NativeView> Host<V> {
    /// Runs `f` on the view, if it exists yet.
    fn with_view(&self, f: impl FnOnce(&V)) -> bool {
        match self.view.borrow().as_ref() {
            Some(hosted) => {
                f(&hosted.view);
                true
            }
            None => false,
        }
    }

    /// Carries the sweep's decision to the native views.
    fn apply(&self, prev: Placement, next: Placement) {
        let slot = self.view.borrow();
        let Some(hosted) = slot.as_ref() else { return };
        match next {
            Placement::Hidden => {
                if prev != Placement::Hidden {
                    #[cfg(target_os = "macos")]
                    hosted.clip.setHidden(true);
                    hosted.view.set_visible(false);
                }
            }
            Placement::Covered | Placement::Shown => {
                if prev == Placement::Hidden {
                    #[cfg(target_os = "macos")]
                    hosted.clip.setHidden(false);
                    hosted.view.set_visible(true);
                }
                // Paint put the clip in place this frame; a covered one moves
                // out of the window instead of hiding, which would stop
                // Chromium drawing and so capturing it.
                #[cfg(target_os = "macos")]
                if next == Placement::Covered {
                    hosted
                        .clip
                        .setFrameOrigin(objc2_foundation::NSPoint::new(AWAY, AWAY));
                }
            }
        }
    }

    /// Replaces the snapshot, giving the old one's texture back.
    fn set_snapshot(
        &self,
        image: Option<Arc<RenderImage>>,
        window: Option<&mut Window>,
        cx: &mut App,
    ) {
        if self.released.get() && image.is_some() {
            return;
        }
        if let Some(old) = self.snapshot.replace(image) {
            cx.drop_image(old, window);
        }
    }

    /// The page is gone: its snapshot goes, and no other comes.
    fn release(&self, cx: &mut App) {
        self.set_snapshot(None, None, cx);
        self.released.set(true);
    }

    /// Asks Chromium for the view's current frame, unless a capture is on
    /// its way. PNG decoding runs off the main thread.
    fn refresh_snapshot(self: &Rc<Self>, cx: &mut App) {
        if self.released.get()
            || self
                .capturing
                .get()
                .is_some_and(|asked| asked.elapsed() < CAPTURE_PATIENCE)
        {
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        let asked = self.with_view(|view| {
            view.capture(Box::new(move |result| {
                let _ = tx.try_send(result);
            }))
        });
        if !asked {
            return;
        }
        self.capturing.set(Some(Instant::now()));
        let host = Rc::downgrade(self);
        let renderer = cx.svg_renderer();
        cx.spawn(async move |cx| {
            // Closed without a result when the view closes first.
            let png = match rx.recv().await {
                Ok(Ok(png)) => Some(png),
                Ok(Err(e)) => {
                    log::debug!("browser: no snapshot: {e}");
                    None
                }
                Err(_) => None,
            };
            let image = match png {
                Some(png) => cx
                    .background_spawn(async move {
                        Image::from_bytes(ImageFormat::Png, png).to_image_data(renderer)
                    })
                    .await
                    .map_err(|e| log::debug!("browser: snapshot did not decode: {e}"))
                    .ok(),
                None => None,
            };
            cx.update(|cx| {
                let Some(host) = host.upgrade() else { return };
                host.capturing.set(None);
                // A hidden view keeps no snapshot: it would only hold memory.
                let Some(image) = image.filter(|_| host.shown.get() != Placement::Hidden) else {
                    return;
                };
                host.set_snapshot(Some(image), None, cx);
                if host.shown.get() == Placement::Covered
                    && let Some((window_id, ..)) = host.painted.get()
                    && let Some(window) = cx
                        .windows()
                        .into_iter()
                        .find(|w| w.window_id() == window_id)
                {
                    let _ = window.update(cx, |_, window, _| window.refresh());
                }
            });
        })
        .detach();
    }

    /// Under an overlay, the snapshot stands in for the view.
    fn paint_snapshot(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.shown.get() != Placement::Covered {
            return;
        }
        if let Some(image) = self.snapshot.borrow().clone() {
            let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn paint(
        self: &Rc<Self>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut App,
        _: impl FnOnce(*mut std::ffi::c_void, monocode_cef::Frame, &mut App) -> Option<V>,
    ) {
    }

    /// Places the view at `bounds` this frame, making it with `create` (in
    /// a parent view and at a frame) the first time.
    #[cfg(target_os = "macos")]
    fn paint(
        self: &Rc<Self>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
        create: impl FnOnce(*mut std::ffi::c_void, monocode_cef::Frame, &mut App) -> Option<V>,
    ) {
        use objc2::{MainThreadMarker, MainThreadOnly, msg_send, rc::Retained};
        use objc2_app_kit::{NSView, NSWindowOrderingMode};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        if !self.registered.replace(true) {
            let swept: Weak<dyn Swept> = Rc::downgrade(self) as Weak<dyn Swept>;
            HOSTS.with(|hosts| hosts.borrow_mut().push(swept));
        }
        self.paint_snapshot(bounds, window);
        let id = Window::window_handle(window).window_id();
        let epoch = FRAMES.with(|f| f.borrow().get(&id).map_or(0, |f| f.epoch));
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // GPUI keeps its AppKit content view alive throughout paint.
        let parent = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let clipped = bounds.intersect(&window.content_mask().bounds);
        // An empty place hides it in the sweep.
        self.painted.set(Some((id, epoch, clipped)));
        if clipped.size.width <= px(0.) || clipped.size.height <= px(0.) {
            return;
        }
        // AppKit frames are in window points; GPUI's are divided by the
        // interface zoom.
        let zoom = window.content_zoom();
        let (outer, inner) = (clipped.map(|v| v * zoom), bounds.map(|v| v * zoom));
        let y = f64::from(outer.origin.y);
        let (width, height) = (f64::from(outer.size.width), f64::from(outer.size.height));
        let y = if parent.isFlipped() {
            y
        } else {
            parent.bounds().size.height - y - height
        };
        let rect = NSRect::new(
            NSPoint::new(f64::from(outer.origin.x), y),
            NSSize::new(width, height),
        );
        // The page keeps its full size while the clip follows GPUI's mask.
        let frame = monocode_cef::Frame {
            x: f64::from(inner.left() - outer.left()),
            y: f64::from(inner.top() - outer.top()),
            width: f64::from(inner.size.width),
            height: f64::from(inner.size.height),
        };

        let mut slot = self.view.borrow_mut();
        if slot.is_none() {
            let clip: Retained<mac::BrowserClip> = unsafe {
                let clip = mac::BrowserClip::alloc(mtm).set_ivars(());
                msg_send![super(clip), initWithFrame: rect]
            };
            let clip: Retained<NSView> = Retained::into_super(clip);
            clip.setWantsLayer(true);
            clip.setClipsToBounds(true);
            parent.addSubview_positioned_relativeTo(&clip, NSWindowOrderingMode::Above, None);
            let raw = Retained::as_ptr(&clip) as *mut std::ffi::c_void;
            match create(raw, frame, cx) {
                Some(view) => {
                    *slot = Some(Hosted { clip, view });
                    // Chromium makes its view shown.
                    self.shown.set(Placement::Shown);
                }
                None => {
                    clip.removeFromSuperview();
                    return;
                }
            }
        }
        let Some(hosted) = slot.as_ref() else { return };
        // SAFETY: main thread, during paint; the clip is ours.
        let attached = unsafe { hosted.clip.superview() };
        if !attached.is_some_and(|s| std::ptr::eq(&*s, parent)) {
            parent.addSubview_positioned_relativeTo(
                &hosted.clip,
                NSWindowOrderingMode::Above,
                None,
            );
        }
        // Shown or not is the sweep's to say, after the overlays painted.
        hosted.clip.setFrame(rect);
        hosted.view.set_frame(frame);
    }
}

impl<V> Drop for Host<V> {
    fn drop(&mut self) {
        // The snapshot's texture went back with the page (`release`).
        if let Some(hosted) = self.view.get_mut().take() {
            drop(hosted.view);
            #[cfg(target_os = "macos")]
            hosted.clip.removeFromSuperview();
        }
    }
}

/// Marks `bounds` (window coordinates) as drawn over by an overlay this
/// frame: a page under it gives way to its snapshot, since GPUI cannot draw
/// above the page.
pub(crate) fn cover(bounds: Bounds<Pixels>, window: &Window) {
    // With no page there is no sweep to take the covers, so they would pile
    // up frame after frame; tests read them without a page.
    if !cfg!(test) && HOSTS.with(|hosts| hosts.borrow().is_empty()) {
        return;
    }
    let id = window.window_handle().window_id();
    FRAMES.with(|f| f.borrow_mut().entry(id).or_default().covers.push(bounds));
}

/// An invisible element that [`cover`]s its own bounds.
pub(crate) fn cover_element() -> impl IntoElement {
    canvas(|_, _, _| {}, |bounds, _, window, _| cover(bounds, window))
        .absolute()
        .inset_0()
}

/// Painted after everything else in a window: hides the views that were not
/// painted in this frame, moves away the ones an overlay covers, and starts
/// the next frame.
pub(crate) fn sweep() -> impl IntoElement {
    gpui::deferred(
        canvas(
            |_, _, _| {},
            |_, _, window, cx| {
                let id = window.window_handle().window_id();
                let (epoch, covers) = FRAMES.with(|f| {
                    let mut frames = f.borrow_mut();
                    let frame = frames.entry(id).or_default();
                    let covers = std::mem::take(&mut frame.covers);
                    let epoch = frame.epoch;
                    frame.epoch += 1;
                    (epoch, covers)
                });
                let live: Vec<Rc<dyn Swept>> = HOSTS.with(|hosts| {
                    let mut hosts = hosts.borrow_mut();
                    hosts.retain(|host| host.strong_count() > 0);
                    hosts.iter().filter_map(Weak::upgrade).collect()
                });
                for host in live {
                    host.sweep(id, epoch, &covers, window, cx);
                }
            },
        )
        .size_0(),
    )
    .with_priority(usize::MAX)
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::{MainThreadOnly, define_class, runtime::NSObjectProtocol};
    use objc2_app_kit::NSView;

    /// Makes GPUI's own view AppKit's first responder, so typed keys reach a
    /// GPUI field instead of the Chromium page that had them.
    pub(super) fn make_gpui_first_responder(window: &gpui::Window) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        // SAFETY: GPUI keeps its content view alive with the window.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        if let Some(native) = view.window() {
            native.makeFirstResponder(Some(view));
        }
    }

    define_class!(
        /// Clips a page to its pane. Flipped, so the page's frame is top-left
        /// based like GPUI's.
        #[unsafe(super(NSView))]
        #[name = "MonocodeBrowserClip"]
        #[thread_kind = MainThreadOnly]
        pub(super) struct BrowserClip;
        unsafe impl NSObjectProtocol for BrowserClip {}
        impl BrowserClip {
            #[unsafe(method(isFlipped))]
            fn flipped(&self) -> bool {
                true
            }
        }
    );
}

/// Windows and Linux get Chromium in phase 6 (context.md §12); until then
/// the pane says so instead of loading.
#[cfg(not(target_os = "macos"))]
mod monocode_cef {
    pub mod runtime {
        pub fn is_started() -> bool {
            false
        }
        pub fn start(_: &super::RuntimeConfig) -> Result<(), &'static str> {
            Err("the browser is not available on this platform yet")
        }
    }

    pub struct RuntimeConfig {
        pub framework_dir: std::path::PathBuf,
        pub helper: std::path::PathBuf,
        pub cache_root: std::path::PathBuf,
        pub mock_keychain: bool,
        pub locale: String,
        pub log_file: Option<std::path::PathBuf>,
    }

    #[allow(dead_code)]
    pub enum BrowserEvent {
        Address(String),
        Title(String),
        Loading {
            loading: bool,
            can_go_back: bool,
            can_go_forward: bool,
        },
        Progress(f64),
        LoadFailed {
            url: String,
            code: i32,
            text: String,
        },
        PopupRequested {
            url: String,
            user_gesture: bool,
            foreground: bool,
        },
        PopupOpened {
            id: u64,
            url: String,
            foreground: bool,
        },
        CloseRequested,
        Download(Download),
        Auth(AuthRequest),
        Dialog(JsDialog),
        DialogsReset,
        Closed,
        FindResult {
            count: i32,
            active_ordinal: i32,
            final_update: bool,
        },
        Focused,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Frame {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    const UNAVAILABLE: &str = "the browser is not available on this platform yet";

    #[derive(Clone, Debug, Default, PartialEq)]
    pub enum Profile {
        #[default]
        Default,
        Isolated(String),
    }
    impl Profile {
        pub fn for_project(path: &str) -> Self {
            Profile::Isolated(path.to_owned())
        }
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct CreateOptions {
        pub profile: Profile,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum DownloadState {
        InProgress,
        Complete,
        Canceled,
        Failed,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Download {
        pub id: u32,
        pub path: std::path::PathBuf,
        pub received: i64,
        pub total: Option<i64>,
        pub state: DownloadState,
    }
    impl Download {
        pub fn name(&self) -> String {
            String::new()
        }
        pub fn fraction(&self) -> Option<f64> {
            None
        }
    }
    pub fn cancel_download(_: u32) {}

    pub struct AuthChallenge {
        pub host: String,
        pub port: u16,
        pub realm: String,
        pub proxy: bool,
    }
    pub struct AuthRequest {
        pub challenge: AuthChallenge,
    }
    impl AuthRequest {
        pub fn sign_in(&self, _: &str, _: &str) {}
        pub fn cancel(&self) {}
    }

    pub enum DialogKind {
        Alert,
        Confirm,
        Prompt(String),
        BeforeUnload { reload: bool },
    }
    pub struct JsDialog {
        pub kind: DialogKind,
        pub origin: String,
        pub message: String,
    }
    impl JsDialog {
        pub fn accept(&self, _: Option<&str>) {}
        pub fn decline(&self) {}
    }

    /// Never created: [`runtime::start`] always fails here.
    pub struct BrowserView;
    impl BrowserView {
        pub fn create(
            _: *mut std::ffi::c_void,
            _: Frame,
            _: &str,
        ) -> Result<(Self, async_channel::Receiver<BrowserEvent>), &'static str> {
            Err(UNAVAILABLE)
        }
        pub fn show_dev_tools(&self) {}
        pub fn close_dev_tools(&self) {}
        pub fn has_dev_tools(&self) -> bool {
            false
        }
        pub fn capture(&self, _: impl FnOnce(Result<Vec<u8>, String>) + 'static) {}
        pub fn set_frame(&self, _: Frame) {}
        pub fn set_visible(&self, _: bool) {}
        pub fn load_url(&self, _: &str) {}
        pub fn go_back(&self) {}
        pub fn go_forward(&self) {}
        pub fn reload(&self) {}
        pub fn stop(&self) {}
        pub fn set_focus(&self, _: bool) {}
        pub fn reload_ignore_cache(&self) {}
        pub fn find(&self, _: &str, _: bool, _: bool, _: bool) {}
        pub fn stop_finding(&self, _: bool) {}
        pub fn set_zoom_level(&self, _: f64) {}
        pub fn zoom_level(&self) -> f64 {
            0.
        }
        pub fn close(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_becomes_an_address_or_a_search() {
        let a = |s| address(s).unwrap();
        assert_eq!(a("https://example.com/x"), "https://example.com/x");
        assert_eq!(a("  example.com "), "https://example.com");
        assert_eq!(a("docs.rs/gpui?x=1"), "https://docs.rs/gpui?x=1");
        assert_eq!(a("localhost:5173"), "http://localhost:5173");
        assert_eq!(a("127.0.0.1:8080/api"), "http://127.0.0.1:8080/api");
        assert_eq!(a("app.localhost"), "http://app.localhost");
        assert_eq!(a("about:blank"), "about:blank");
        assert_eq!(a("rust gpui"), "https://www.google.com/search?q=rust+gpui");
        assert_eq!(a("hello"), "https://www.google.com/search?q=hello");
        assert_eq!(a("a&b"), "https://www.google.com/search?q=a%26b");
        assert_eq!(address("   "), None);
    }

    #[test]
    fn the_helper_is_the_bundle_without_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["X Helper (GPU)", "X Helper (Renderer)", "X Helper"] {
            let macos = dir.path().join(format!("{name}.app/Contents/MacOS"));
            std::fs::create_dir_all(&macos).unwrap();
            std::fs::write(macos.join(name), "").unwrap();
        }
        assert_eq!(
            find_helper(dir.path()),
            Some(dir.path().join("X Helper.app/Contents/MacOS/X Helper"))
        );
        assert_eq!(find_helper(&dir.path().join("missing")), None);
    }

    #[gpui::test]
    fn popups_ask_for_pages_and_downloads_are_listed_once(cx: &mut gpui::TestAppContext) {
        use monocode_cef::{BrowserEvent, Download, DownloadState};
        let page =
            cx.new(|cx| BrowserPage::new(String::new(), String::new(), 100, "/tmp".into(), cx));
        let asked = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let asked = asked.clone();
            cx.subscribe(&page, move |_, request: &PageRequest, _| {
                asked.borrow_mut().push(request.clone())
            })
            .detach();
        });
        let download = |received, state| Download {
            id: 3,
            path: "/d/a.zip".into(),
            received,
            total: Some(10),
            state,
        };
        page.update(cx, |page, cx| {
            page.on_event(
                BrowserEvent::PopupRequested {
                    url: "https://a.test/".into(),
                    user_gesture: false,
                    foreground: false,
                },
                cx,
            );
            page.on_event(BrowserEvent::CloseRequested, cx);
            page.on_event(
                BrowserEvent::Download(download(0, DownloadState::InProgress)),
                cx,
            );
            page.on_event(
                BrowserEvent::Download(download(10, DownloadState::Complete)),
                cx,
            );
            assert_eq!(page.downloads, [download(10, DownloadState::Complete)]);
            assert_eq!(download_status(&page.downloads[0]), "10 B");
        });
        cx.run_until_parked();
        assert_eq!(
            *asked.borrow(),
            [
                PageRequest::Open {
                    url: "https://a.test/".into(),
                    foreground: false,
                    popup: None,
                },
                PageRequest::Close,
            ]
        );
    }

    #[test]
    fn download_sizes_read_like_a_browsers() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(34_000_000), "34 MB");
        assert_eq!(human_bytes(2_100_000_000), "2.1 GB");
        assert_eq!(
            origin_host("https://example.com:8080/a?b"),
            "example.com:8080"
        );
        assert_eq!(origin_host(""), "");
    }

    #[test]
    fn zoom_steps_through_chromes_presets() {
        assert_eq!(zoom_step(100, 1), 110);
        assert_eq!(zoom_step(100, -1), 90);
        assert_eq!(zoom_step(150, 0), 100);
        assert_eq!(zoom_step(500, 1), 500);
        assert_eq!(zoom_step(25, -1), 25);
        // Between presets: another page set the host's level some other way.
        assert_eq!(zoom_step(120, 1), 125);
        assert_eq!(zoom_step(120, -1), 110);
        for percent in ZOOM_PRESETS {
            assert_eq!(zoom_percent(zoom_level(percent)), percent);
        }
        assert_eq!(zoom_level(100), 0.);
        assert!((zoom_level(120) - 1.).abs() < 1e-9);
    }

    #[test]
    fn the_find_bar_counts_matches_once_the_page_answers() {
        assert_eq!(find_status("", Some((3, 1))), None);
        assert_eq!(find_status("rust", None), None);
        assert_eq!(
            find_status("rust", Some((0, 0))).as_deref(),
            Some("No results")
        );
        assert_eq!(
            find_status("rust", Some((12, 3))).as_deref(),
            Some("3 of 12")
        );
        assert_eq!(
            find_status("rust", Some((12, 0))).as_deref(),
            Some("1 of 12")
        );
    }

    #[test]
    fn a_view_hides_unpainted_and_gives_way_under_an_overlay() {
        let at = |x: f32, y: f32, w: f32, h: f32| Bounds {
            origin: gpui::point(px(x), px(y)),
            size: gpui::size(px(w), px(h)),
        };
        let page = at(0., 40., 800., 600.);
        let menu = at(700., 30., 200., 100.);
        let elsewhere = at(900., 0., 50., 50.);
        assert_eq!(shown(true, page, &[]), Placement::Shown);
        assert_eq!(shown(true, page, &[elsewhere]), Placement::Shown);
        assert_eq!(shown(true, page, &[elsewhere, menu]), Placement::Covered);
        assert_eq!(shown(false, page, &[]), Placement::Hidden);
        assert_eq!(shown(false, page, &[menu]), Placement::Hidden);
        assert_eq!(shown(true, at(0., 0., 0., 100.), &[]), Placement::Hidden);
    }

    struct NoView;
    impl NativeView for NoView {
        fn set_frame(&self, _: monocode_cef::Frame) {}
        fn set_visible(&self, _: bool) {}
        fn capture(&self, _: Box<dyn FnOnce(Result<Vec<u8>, String>)>) {}
    }

    #[gpui::test]
    fn a_snapshot_gives_its_texture_back_when_replaced_or_released(cx: &mut gpui::TestAppContext) {
        let image = || {
            let frame = image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba([9; 4])));
            Arc::new(RenderImage::new(vec![frame]))
        };
        let (first, second) = (image(), image());
        let host = Rc::new(Host::<NoView>::default());
        host.shown.set(Placement::Covered);
        let window = cx.add_empty_window();
        let draw = |window: &mut gpui::VisualTestContext| {
            let host = host.clone();
            window.draw(
                gpui::point(px(0.), px(0.)),
                gpui::size(px(20.), px(20.)),
                |_, _| {
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| host.paint_snapshot(bounds, window),
                    )
                    .size_full()
                    .into_any_element()
                },
            );
        };
        // Outside any window's update, as captures land and pages go.
        let app = |window: &mut gpui::VisualTestContext, f: &dyn Fn(&mut App)| {
            let app: &gpui::TestAppContext = window;
            app.update(f);
        };
        let in_atlas = |window: &mut gpui::VisualTestContext, image: &Arc<RenderImage>| {
            window.update(|window, _| window.has_image_atlas_entry(image))
        };

        app(window, &|cx| {
            host.set_snapshot(Some(first.clone()), None, cx)
        });
        draw(window);
        assert!(in_atlas(window, &first));
        // Painting it again reuses the texture.
        draw(window);
        assert!(in_atlas(window, &first));

        app(window, &|cx| {
            host.set_snapshot(Some(second.clone()), None, cx)
        });
        assert!(!in_atlas(window, &first));
        draw(window);
        assert!(in_atlas(window, &second));

        app(window, &|cx| host.release(cx));
        assert!(!in_atlas(window, &second));
        // A capture that lands after the page went is dropped.
        app(window, &|cx| host.set_snapshot(Some(image()), None, cx));
        assert!(host.snapshot.borrow().is_none());
    }

    #[test]
    fn a_page_is_titled_by_its_title_then_its_host() {
        assert_eq!(page_title("https://example.com/a", "Example"), "Example");
        assert_eq!(page_title("https://example.com/a", " "), "example.com");
        assert_eq!(page_title("http://localhost:5173/", ""), "localhost:5173");
        assert_eq!(page_title("", ""), "Browser");
    }
}
