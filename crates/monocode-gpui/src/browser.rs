//! The browser pane: a Chromium page (CEF, `monocode-cef`) shown as a native
//! child view of the window (docs/native/browser/context.md §4).
//!
//! Chromium ships in the app bundle and starts in this process the first
//! time a page is shown ([`Engine`]).
//!
//! A page's view sits above GPUI's own view, so GPUI cannot draw over it.
//! Each frame every shown page records that it painted; the [`sweep`] element,
//! painted last, hides the pages that did not (their tab is not in front) and
//! the ones a menu, a modal or a notification covers ([`cover`]).

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
};

use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    KeyBinding, Pixels, Subscription, Window, WindowId, actions, canvas, div, prelude::*, px,
};

use crate::{
    input::{self, Input},
    theme::{content, icon, icon_button_enabled},
};

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
    /// Where the page last drew, to take GPUI focus when Chromium gets it.
    window: Option<AnyWindowHandle>,
    engine: Entity<Engine>,
    profiles: PathBuf,
    host: Rc<Host>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PageChanged> for BrowserPage {}

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
            window: None,
            engine,
            profiles,
            host: Rc::new(Host::default()),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn display_title(&self) -> String {
        page_title(&self.url, &self.title)
    }

    /// The address field takes the keyboard while there is nothing to show;
    /// otherwise the page does.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
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
                }
            }
            BrowserEvent::Progress(progress) => self.progress = progress,
            BrowserEvent::LoadFailed { url, text, .. } => {
                self.failure = Some(format!("{url} could not be loaded: {text}"));
            }
            // New windows open in this page until pages have tabs (phase 1).
            BrowserEvent::PopupRequested { url, .. } => {
                self.host.with_view(|view| view.load_url(&url));
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
            move |bounds, _, window, cx| host.paint(bounds, &url, &page, window, cx),
        )
        .size_full()
        .into_any_element()
    }
}

impl Render for BrowserPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window = Some(window.window_handle());
        if self.engine.read(cx).state == EngineState::Idle {
            self.start_engine(cx);
        }
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
    }
}

// ---- The native view and its visibility ----

thread_local! {
    /// Every page's host, for [`sweep`].
    static HOSTS: RefCell<Vec<Weak<Host>>> = const { RefCell::new(Vec::new()) };
    /// Per window: the frame being painted, and what overlays covered in it.
    static FRAMES: RefCell<HashMap<WindowId, Frame>> = RefCell::new(HashMap::new());
}

#[derive(Default)]
struct Frame {
    epoch: u64,
    covers: Vec<Bounds<Pixels>>,
}

/// A page's Chromium view inside a clipping view of the window.
#[derive(Default)]
struct Host {
    view: RefCell<Option<Hosted>>,
    /// The window and frame it was last painted in, and where.
    painted: Cell<Option<(WindowId, u64, Bounds<Pixels>)>>,
    shown: Cell<bool>,
    registered: Cell<bool>,
}

struct Hosted {
    #[cfg(target_os = "macos")]
    clip: objc2::rc::Retained<objc2_app_kit::NSView>,
    view: monocode_cef::BrowserView,
}

impl Host {
    /// Runs `f` on the page, if it exists yet.
    fn with_view(&self, f: impl FnOnce(&monocode_cef::BrowserView)) -> bool {
        match self.view.borrow().as_ref() {
            Some(hosted) => {
                f(&hosted.view);
                true
            }
            None => false,
        }
    }

    fn set_shown(&self, shown: bool) {
        if self.shown.replace(shown) == shown {
            return;
        }
        if let Some(hosted) = self.view.borrow().as_ref() {
            #[cfg(target_os = "macos")]
            hosted.clip.setHidden(!shown);
            hosted.view.set_visible(shown);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn paint(
        self: &Rc<Self>,
        _: Bounds<Pixels>,
        _: &str,
        _: &gpui::WeakEntity<BrowserPage>,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    #[cfg(target_os = "macos")]
    fn paint(
        self: &Rc<Self>,
        bounds: Bounds<Pixels>,
        url: &str,
        page: &gpui::WeakEntity<BrowserPage>,
        window: &mut Window,
        cx: &mut App,
    ) {
        use objc2::{MainThreadMarker, MainThreadOnly, msg_send, rc::Retained};
        use objc2_app_kit::{NSView, NSWindowOrderingMode};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        if !self.registered.replace(true) {
            HOSTS.with(|hosts| hosts.borrow_mut().push(Rc::downgrade(self)));
        }
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
        self.painted.set(Some((id, epoch, clipped)));
        if clipped.size.width <= px(0.) || clipped.size.height <= px(0.) {
            self.set_shown(false);
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
            match monocode_cef::BrowserView::create(raw, frame, url) {
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
                    *slot = Some(Hosted { clip, view });
                    self.shown.set(true);
                }
                Err(e) => {
                    clip.removeFromSuperview();
                    let error = e.to_string();
                    let page = page.clone();
                    cx.defer(move |cx| {
                        let _ = page.update(cx, |page, cx| {
                            page.failure = Some(error);
                            cx.notify();
                        });
                    });
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
        hosted.clip.setFrame(rect);
        hosted.view.set_frame(frame);
        drop(slot);
        self.set_shown(true);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(hosted) = self.view.get_mut().take() {
            hosted.view.close();
            #[cfg(target_os = "macos")]
            hosted.clip.removeFromSuperview();
        }
    }
}

/// Marks `bounds` (window coordinates) as drawn over by an overlay this
/// frame: a page under it hides, since GPUI cannot draw above the page.
pub(crate) fn cover(bounds: Bounds<Pixels>, window: &Window) {
    let id = window.window_handle().window_id();
    FRAMES.with(|f| f.borrow_mut().entry(id).or_default().covers.push(bounds));
}

/// An invisible element that [`cover`]s its own bounds.
pub(crate) fn cover_element() -> impl IntoElement {
    canvas(|_, _, _| {}, |bounds, _, window, _| cover(bounds, window))
        .absolute()
        .inset_0()
}

/// Painted after everything else in a window: hides the pages that were not
/// painted in this frame or that an overlay covers, and starts the next frame.
pub(crate) fn sweep() -> impl IntoElement {
    gpui::deferred(
        canvas(
            |_, _, _| {},
            |_, _, window, _| {
                let id = window.window_handle().window_id();
                let (epoch, covers) = FRAMES.with(|f| {
                    let mut frames = f.borrow_mut();
                    let frame = frames.entry(id).or_default();
                    let covers = std::mem::take(&mut frame.covers);
                    let epoch = frame.epoch;
                    frame.epoch += 1;
                    (epoch, covers)
                });
                HOSTS.with(|hosts| {
                    hosts.borrow_mut().retain(|host| {
                        let Some(host) = host.upgrade() else {
                            return false;
                        };
                        match host.painted.get() {
                            Some((window, painted, bounds)) if window == id => {
                                let covered = covers.iter().any(|c| c.intersects(&bounds));
                                if painted != epoch || covered {
                                    host.set_shown(false);
                                }
                            }
                            _ => {}
                        }
                        true
                    });
                });
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
        },
        Closed,
        FindResult {
            count: i32,
            active_ordinal: i32,
            final_update: bool,
        },
        Focused,
    }

    /// Never created: [`runtime::start`] always fails here.
    pub struct BrowserView;
    impl BrowserView {
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
    fn a_page_is_titled_by_its_title_then_its_host() {
        assert_eq!(page_title("https://example.com/a", "Example"), "Example");
        assert_eq!(page_title("https://example.com/a", " "), "example.com");
        assert_eq!(page_title("http://localhost:5173/", ""), "localhost:5173");
        assert_eq!(page_title("", ""), "Browser");
    }
}
