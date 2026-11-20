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
    collections::HashMap,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity, EventEmitter, FocusHandle,
    Focusable, Global, Image, ImageFormat, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    RenderImage, Subscription, Window, WindowId, actions, canvas, div, prelude::*, px,
};

use crate::{
    input::{self, Input},
    theme::{content, icon, icon_button_enabled},
    workspace::DevToolsDock,
};

actions!(
    workspace,
    [
        /// ⌥⌘I: the focused page's docked DevTools.
        ToggleBrowserDevTools
    ]
);

/// DevTools and the page above it each keep at least this many points.
const DEVTOOLS_MIN: f32 = 120.;
const PAGE_MIN: f32 = 80.;

/// DevTools' height in a body `available` points tall: the saved height
/// (0: 40% of the body), kept clear of both minimums. Unmeasured (0), the
/// body takes the wanted height as it is.
pub(crate) fn devtools_height(saved: u32, available: f32) -> f32 {
    let wanted = if saved == 0 {
        (available * 0.4).max(DEVTOOLS_MIN)
    } else {
        saved as f32
    };
    if available <= 0. {
        return wanted;
    }
    let max = (available - PAGE_MIN).max(0.);
    wanted.clamp(DEVTOOLS_MIN.min(max), max)
}

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

/// A page's address or title changed; the pane saves them.
pub(crate) struct PageChanged;

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
    engine: Entity<Engine>,
    profiles: PathBuf,
    host: Rc<Host<monocode_cef::BrowserView>>,
    /// Saved with the pane; the shell sets it when it makes the page.
    pub devtools_dock: DevToolsDock,
    /// The docked DevTools' view, made when first painted while open.
    devtools: Rc<Host<monocode_cef::DevToolsView>>,
    /// The divider being dragged: where the drag started, and the height then.
    devtools_drag: Option<(f32, f32)>,
    /// The body under the toolbar, as last laid out.
    body_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
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
            cx.on_release(|page, cx| {
                page.host.release(cx);
                page.devtools.release(cx);
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
            engine,
            profiles,
            host: Rc::new(Host::default()),
            devtools_dock: DevToolsDock::default(),
            devtools: Rc::new(Host::default()),
            devtools_drag: None,
            body_bounds: Rc::default(),
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
                    self.snapshot_when_settled(cx);
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
        }
        cx.notify();
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
            .child(
                icon_button_enabled("browser-devtools", "DevTools", "code", ready)
                    .when(self.devtools_dock.open, |b| b.bg(content(0.1)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_dev_tools(cx))),
            )
    }

    /// Opens or closes the docked DevTools.
    pub(crate) fn toggle_dev_tools(&mut self, cx: &mut Context<Self>) {
        self.devtools_dock.open = !self.devtools_dock.open;
        if !self.devtools_dock.open {
            self.drop_dev_tools(cx);
        }
        cx.emit(PageChanged);
        cx.notify();
    }

    /// Closes DevTools' view; a new host makes the next one.
    fn drop_dev_tools(&mut self, cx: &mut App) {
        self.devtools_drag = None;
        let old = std::mem::replace(&mut self.devtools, Rc::new(Host::default()));
        old.release(cx);
        old.close();
    }

    /// DevTools closed on its own (its UI, or a crash).
    fn dev_tools_closed(
        &mut self,
        host: &Weak<Host<monocode_cef::DevToolsView>>,
        cx: &mut Context<Self>,
    ) {
        if std::ptr::eq(host.as_ptr(), Rc::as_ptr(&self.devtools)) {
            self.devtools_dock.open = false;
            self.drop_dev_tools(cx);
            cx.emit(PageChanged);
        }
        // An earlier DevTools finishing its close lets a new one open.
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
                host.paint(bounds, window, cx, |parent, frame, cx| {
                    match monocode_cef::BrowserView::create(parent, frame, &url) {
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

    /// Where DevTools draws, under the page: made once the page is.
    fn devtools_surface(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let host = self.devtools.clone();
        let page_host = self.host.clone();
        let page = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let weak = Rc::downgrade(&host);
                host.paint(bounds, window, cx, |parent, frame, cx| {
                    let opened = page_host
                        .view
                        .borrow()
                        .as_ref()
                        .map(|hosted| hosted.view.show_dev_tools(parent, frame));
                    match opened? {
                        Ok((view, events)) => {
                            let page = page.clone();
                            cx.spawn(async move |cx| {
                                if events.recv().await.is_ok() {
                                    let _ = page
                                        .update(cx, |page, cx| page.dev_tools_closed(&weak, cx));
                                }
                            })
                            .detach();
                            Some(view)
                        }
                        // An earlier DevTools is still closing; its close
                        // repaints, and this tries again.
                        Err(e) => {
                            log::debug!("browser: DevTools did not open: {e}");
                            None
                        }
                    }
                })
            },
        )
        .size_full()
        .into_any_element()
    }

    /// The page, with DevTools under a draggable divider when it is open.
    fn body(&mut self, page: gpui::AnyElement, cx: &mut Context<Self>) -> gpui::AnyElement {
        let open = self.devtools_dock.open && self.engine.read(cx).state == EngineState::Ready;
        if !open {
            return div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(page)
                .into_any_element();
        }
        let available = self
            .body_bounds
            .get()
            .map_or(0., |b| f32::from(b.size.height));
        let height = devtools_height(self.devtools_dock.height, available);
        let slot = self.body_bounds.clone();
        // The next render clamps to a new size, so a resize asks for it.
        let probe = canvas(
            move |bounds, window, _| {
                if slot
                    .replace(Some(bounds))
                    .is_none_or(|old| old.size != bounds.size)
                {
                    window.on_next_frame(|window, _| window.refresh());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let divider = div()
            .id("browser-devtools-divider")
            .role(gpui::Role::Splitter)
            .aria_label("Resize DevTools")
            // Real space, not an overlap: Chromium's views would take the
            // pointer over any part of it they cover.
            .h(px(5.))
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeUpDown)
            .flex()
            .items_center()
            .child(div().h(px(1.)).w_full().bg(content(0.12)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.devtools_drag = Some((f32::from(event.position.y), height));
                    cx.stop_propagation();
                }),
            );
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(probe)
            .child(div().flex_1().min_h_0().min_w_0().child(page))
            .child(divider)
            .child(
                div()
                    .h(px(height))
                    .flex_shrink_0()
                    .min_w_0()
                    .child(self.devtools_surface(cx)),
            )
            .when(self.devtools_drag.is_some(), |d| {
                d.child(self.drag_listener(cx))
            })
            .into_any_element()
    }

    /// While the divider drags: window-wide, since the pointer passes over
    /// Chromium's views, whose drags AppKit still sends to GPUI's view.
    fn drag_listener(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let moved = page.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = moved.update(cx, |page, cx| page.drag_divider(event, cx));
                });
                let released = page.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = released.update(cx, |page, cx| page.end_divider_drag(cx));
                });
            },
        )
        .absolute()
        .size_0()
    }

    fn drag_divider(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((start, height)) = self.devtools_drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_divider_drag(cx);
            return;
        }
        let available = self
            .body_bounds
            .get()
            .map_or(0., |b| f32::from(b.size.height));
        let wanted = (height + start - f32::from(event.position.y)).max(1.);
        let height = devtools_height(wanted.round() as u32, available).round() as u32;
        if height != self.devtools_dock.height {
            self.devtools_dock.height = height;
            cx.notify();
        }
    }

    fn end_divider_drag(&mut self, cx: &mut Context<Self>) {
        if self.devtools_drag.take().is_some() {
            // Saved once, when the drag ends.
            cx.emit(PageChanged);
            cx.notify();
        }
    }
}

impl Render for BrowserPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.engine.read(cx).state == EngineState::Idle {
            self.start_engine(cx);
        }
        let body = match self.status(cx) {
            Some(status) => status,
            None => self.surface(cx),
        };
        div()
            .id("browser-page")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(cx))
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
            .child(self.body(body, cx))
    }
}

// ---- The native view and its visibility ----

thread_local! {
    /// Every page's and DevTools' host, for [`sweep`].
    static HOSTS: RefCell<Vec<Weak<dyn Swept>>> = const { RefCell::new(Vec::new()) };
    /// Per window: the frame being painted, and what overlays covered in it.
    static FRAMES: RefCell<HashMap<WindowId, Frame>> = RefCell::new(HashMap::new());
}

#[derive(Default)]
struct Frame {
    epoch: u64,
    covers: Vec<Bounds<Pixels>>,
}

/// A Chromium view a [`Host`] places: a page or its DevTools.
trait NativeView: 'static {
    fn set_frame(&self, frame: monocode_cef::Frame);
    fn set_visible(&self, visible: bool);
    fn capture(&self, done: Box<dyn FnOnce(Result<Vec<u8>, String>)>);
    fn close(&self);
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
    fn close(&self) {
        self.close();
    }
}

impl NativeView for monocode_cef::DevToolsView {
    fn set_frame(&self, frame: monocode_cef::Frame) {
        self.set_frame(frame);
    }
    fn set_visible(&self, visible: bool) {
        self.set_visible(visible);
    }
    fn capture(&self, done: Box<dyn FnOnce(Result<Vec<u8>, String>)>) {
        self.capture(done);
    }
    fn close(&self) {
        self.close();
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
}

impl<V> Default for Host<V> {
    fn default() -> Self {
        Self {
            view: RefCell::default(),
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

    /// Closes the view now, rather than when the last frame lets go of it.
    fn close(&self) {
        if let Some(hosted) = self.view.borrow_mut().take() {
            hosted.view.close();
            #[cfg(target_os = "macos")]
            hosted.clip.removeFromSuperview();
        }
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
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Frame {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    const UNAVAILABLE: &str = "the browser is not available on this platform yet";

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
        pub fn show_dev_tools(
            &self,
            _: *mut std::ffi::c_void,
            _: Frame,
        ) -> Result<(DevToolsView, async_channel::Receiver<DevToolsEvent>), &'static str> {
            Err(UNAVAILABLE)
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
        pub fn close(&self) {}
    }

    pub enum DevToolsEvent {
        #[allow(dead_code)]
        Closed,
    }

    pub struct DevToolsView;
    impl DevToolsView {
        pub fn set_frame(&self, _: Frame) {}
        pub fn set_visible(&self, _: bool) {}
        pub fn capture(&self, _: Box<dyn FnOnce(Result<Vec<u8>, String>)>) {}
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
    fn devtools_keeps_its_height_clear_of_both_minimums() {
        // Unsaved: 40% of the body, never under the minimum.
        assert_eq!(devtools_height(0, 1000.), 400.);
        assert_eq!(devtools_height(0, 250.), DEVTOOLS_MIN);
        // Saved: as it was, while it fits.
        assert_eq!(devtools_height(300, 1000.), 300.);
        assert_eq!(devtools_height(10, 1000.), DEVTOOLS_MIN);
        assert_eq!(devtools_height(990, 1000.), 1000. - PAGE_MIN);
        // A body too short for both: the page keeps its minimum.
        assert_eq!(devtools_height(300, 150.), 150. - PAGE_MIN);
        assert_eq!(devtools_height(300, 50.), 0.);
        // Not measured yet: the wanted height.
        assert_eq!(devtools_height(300, 0.), 300.);
        assert_eq!(devtools_height(0, 0.), DEVTOOLS_MIN);
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
        fn close(&self) {}
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
