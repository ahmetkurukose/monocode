//! A browser page in a pane: the address row drawn by GPUI over a native
//! web view (docs/native/browser.md). The page keeps its sign-ins between
//! launches; closing or sleeping the pane frees what it used.
use crate::{
    input::{self, Input},
    popover::{
        Align, Anchor, AnchorSlot, Menu, MenuAction, MenuEvent, MenuItem, MenuStyle, menu_popover,
    },
    theme::*,
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, Role, Subscription, Window,
    canvas, div, prelude::*, px,
};
use std::{cell::RefCell, rc::Rc};

pub(crate) mod agent;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod native_ui;

gpui::actions!(
    browser,
    [FocusAddress, Reload, GoBack, GoForward, FindInPage]
);

/// The pane's keys. Copy, paste and the like are Monocode's actions bound
/// per context; here they are bound for a page and passed on to it.
pub(crate) fn bind(cx: &mut gpui::App) {
    use gpui::KeyBinding;
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let key = |keys: &str| format!("{m}-{keys}");
    cx.bind_keys([
        KeyBinding::new(&key("a"), input::SelectAll, Some("Browser")),
        KeyBinding::new(&key("c"), input::Copy, Some("Browser")),
        KeyBinding::new(&key("v"), input::Paste, Some("Browser")),
        KeyBinding::new(&key("x"), input::Cut, Some("Browser")),
        KeyBinding::new(&key("z"), input::Undo, Some("Browser")),
        KeyBinding::new(&key("shift-z"), input::Redo, Some("Browser")),
        KeyBinding::new(&key("l"), FocusAddress, Some("Browser")),
        KeyBinding::new(&key("f"), FindInPage, Some("Browser")),
        KeyBinding::new(&key("r"), Reload, Some("Browser")),
        KeyBinding::new(&key("["), GoBack, Some("Browser")),
        KeyBinding::new(&key("]"), GoForward, Some("Browser")),
    ]);
}

/// Web links clicked somewhere in the app (a reply, a terminal) that are to
/// open in a browser pane. The Shell takes them on its next frame, where it
/// has a window to open a pane in.
#[derive(Default)]
pub(crate) struct LinkQueue(pub Vec<String>);
impl gpui::Global for LinkQueue {}

/// Opens a web link where the setting says: in a browser pane of the
/// project, or in the user's own browser.
pub(crate) fn open_link(url: &str, cx: &mut gpui::App) {
    let in_monocode = cfg!(target_os = "macos")
        && (url.starts_with("http://") || url.starts_with("https://"))
        && cx
            .try_global::<crate::settings::Preferences>()
            .is_some_and(|preferences| preferences.browser_links == "monocode");
    if in_monocode {
        cx.default_global::<LinkQueue>().0.push(url.to_owned());
        cx.refresh_windows();
    } else {
        cx.open_url(url);
    }
}

/// Clears what the browser stored: "cache", "site-data" (cookies and what
/// sites kept) or anything else for both; only what changed in the last
/// `seconds` when given.
pub(crate) fn clear_data(what: &str, seconds: Option<f64>, done: impl FnOnce() + 'static) {
    #[cfg(target_os = "macos")]
    {
        let kind = match what {
            "cache" => host::data::Kind::Cache,
            "site-data" => host::data::Kind::SiteData,
            _ => host::data::Kind::Everything,
        };
        host::data::clear(kind, seconds, done);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (what, seconds);
        done();
    }
}

/// The sites that stored something, each with what kinds of data.
pub(crate) fn data_sites(done: impl FnOnce(Vec<(String, Vec<String>)>) + 'static) {
    #[cfg(target_os = "macos")]
    host::data::sites(done);
    #[cfg(not(target_os = "macos"))]
    done(Vec::new());
}

/// Removes everything one site stored.
pub(crate) fn clear_site(site: String, done: impl FnOnce(bool) + 'static) {
    #[cfg(target_os = "macos")]
    host::data::clear_site(site, done);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = site;
        done(false);
    }
}

pub(crate) enum BrowserEvent {
    /// The address or title changed: what is saved with the pane.
    Changed,
    /// The page took the keyboard, which makes its pane the focused one.
    Focused,
    /// The page asked for a new window at this address.
    OpenNew(String),
    /// Text for the chat of the session the page belongs to.
    AddToChat(String),
    /// The user stopped, or allowed again, a session's agent in the browser.
    StopAgent(i64),
    AllowAgent(i64),
}

#[derive(Clone, Debug, PartialEq)]
struct AgentNote {
    session: i64,
    name: String,
    stopped: bool,
}

/// A line under the address row saying what just happened. It takes room
/// of its own rather than floating: anything floating over the page would
/// be under the native view.
#[derive(Clone, Debug, PartialEq)]
enum Banner {
    Downloaded(std::path::PathBuf),
    Problem(String),
    Info(String),
}

struct OpenMenu {
    menu: Entity<Menu>,
    _events: Subscription,
}

/// A row with one text field under the address row: finding in the page,
/// or typing a user agent of one's own.
struct Entry {
    input: Entity<Input>,
    _events: Subscription,
}

pub(crate) struct BrowserView {
    pub url: String,
    pub title: String,
    pub user_agent: Option<String>,
    /// A fixed size to hold the page at, as a device would; `None` fits the pane.
    pub viewport: Option<(u32, u32)>,
    /// The session whose agent last used the page, and its name.
    agent: Option<AgentNote>,
    /// The user asked for this page to stay loaded.
    pub keep_awake: bool,
    /// When the page was last on screen, or used by an agent.
    last_used: Rc<std::cell::Cell<std::time::Instant>>,
    loading: bool,
    can_back: bool,
    can_forward: bool,
    /// Why the last load failed, shown until the next one starts.
    failure: Option<String>,
    /// No web view: the page was put to sleep, or never shown since launch.
    asleep: bool,
    address: Entity<Input>,
    address_focused: bool,
    focus: FocusHandle,
    /// Set when the native page was clicked; the pane takes GPUI's focus on
    /// its next frame, where there is a window to take it in.
    take_focus: bool,
    #[cfg(target_os = "macos")]
    host: Rc<RefCell<Option<host::Hosted>>>,
    /// Whether this frame's paint put the native view on screen.
    placed: Rc<std::cell::Cell<bool>>,
    /// What the last paint did with the native view, for the self-test.
    paint_note: Rc<std::cell::Cell<&'static str>>,
    banner: Option<Banner>,
    menu: Option<OpenMenu>,
    menu_anchor: AnchorSlot,
    find: Option<Entry>,
    /// The last search found nothing.
    find_missed: bool,
    custom_agent: Option<Entry>,
    /// The Shell is drawing something over the panes (see `set_covered`).
    shell_covered: bool,
    /// Something GPUI draws (a menu, a dialog, a drag) may be over the pane.
    /// The native view would hide it, so a picture of the page is shown in
    /// its place for as long as that lasts.
    covered: bool,
    /// The picture, once it has been taken; the page stays up until then.
    stand_in: Option<std::sync::Arc<gpui::Image>>,
    /// The page had the keyboard when it was covered, and takes it back
    /// when it is put on screen again.
    refocus: Rc<std::cell::Cell<bool>>,
    _subscriptions: Vec<Subscription>,
    _watch: gpui::Task<()>,
}

/// One page's native view, as the Shell's frame sees it: taken off screen
/// after a frame that did not paint it.
///
/// It is not hidden before the frame and shown again by the paint, as the
/// Mono HTML view is: AppKit takes the keyboard away from a view that is
/// hidden, so a page that was being typed in would lose it on every frame.
pub(crate) struct PageFrame {
    #[cfg(target_os = "macos")]
    host: Rc<RefCell<Option<host::Hosted>>>,
    placed: Rc<std::cell::Cell<bool>>,
}

impl PageFrame {
    /// After the frame's panes have painted.
    pub(crate) fn finish(&self) {
        if !self.placed.get() {
            #[cfg(target_os = "macos")]
            if let Some(host) = self.host.borrow().as_ref() {
                host.hide();
            }
        }
    }
}

impl EventEmitter<BrowserEvent> for BrowserView {}
impl Focusable for BrowserView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserView {
    /// `asleep`: a page restored with the workspace loads when first woken,
    /// not at launch.
    pub(crate) fn new(
        url: String,
        title: String,
        user_agent: Option<String>,
        asleep: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| {
            let mut input = Input::field("Address", "Search or enter address", content(0.35), cx);
            input.set_font_size(13.);
            input.set_text(url.clone(), cx);
            input
        });
        let subscriptions =
            vec![
                cx.subscribe(&address, |this, input, _: &input::Submitted, cx| {
                    let text = input.read(cx).content.to_string();
                    this.navigate(&text, cx);
                }),
            ];
        Self {
            url,
            title,
            user_agent,
            viewport: None,
            agent: None,
            keep_awake: false,
            last_used: Rc::new(std::cell::Cell::new(std::time::Instant::now())),
            loading: false,
            can_back: false,
            can_forward: false,
            failure: None,
            asleep,
            address,
            address_focused: false,
            focus: cx.focus_handle(),
            take_focus: false,
            #[cfg(target_os = "macos")]
            host: Rc::new(RefCell::new(None)),
            placed: Rc::new(std::cell::Cell::new(false)),
            paint_note: Rc::new(std::cell::Cell::new("never painted")),
            banner: None,
            menu: None,
            menu_anchor: AnchorSlot::default(),
            find: None,
            find_missed: false,
            custom_agent: None,
            shell_covered: false,
            covered: false,
            stand_in: None,
            refocus: Rc::new(std::cell::Cell::new(false)),
            _subscriptions: subscriptions,
            _watch: cx.spawn(async move |this, cx| {
                // WebKit reports loads as they happen, but a page can also
                // retitle itself or move within itself with no load at all.
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(500))
                        .await;
                    #[cfg(target_os = "macos")]
                    let alive = this.update(cx, |this, cx| this.page_changed(cx)).is_ok();
                    #[cfg(not(target_os = "macos"))]
                    let alive = this.update(cx, |_, _| ()).is_ok();
                    if !alive {
                        break;
                    }
                }
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn asleep(&self) -> bool {
        self.asleep
    }

    /// The page's name for a tab or a row.
    pub(crate) fn label(&self) -> String {
        if self.title.trim().is_empty() {
            monocode_core::browser::label(&self.url)
        } else {
            self.title.clone()
        }
    }

    /// Goes where the typed text says: an address, a host, or a search.
    pub(crate) fn navigate(&mut self, text: &str, cx: &mut Context<Self>) {
        let url = monocode_core::browser::address(text);
        self.url = url.clone();
        self.failure = None;
        self.asleep = false;
        self.address_focused = false;
        self.address
            .update(cx, |input, cx| input.set_text(url.clone(), cx));
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.load(&url);
            host.focus();
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.back();
        }
        cx.notify();
    }
    fn forward(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.forward();
        }
        cx.notify();
    }
    fn reload_or_stop(&mut self, cx: &mut Context<Self>) {
        if self.asleep {
            self.wake(cx);
            return;
        }
        self.failure = None;
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            if self.loading {
                host.stop();
            } else {
                host.reload();
            }
        }
        cx.notify();
    }

    /// Frees the page: its web view and the process behind it go, the
    /// address stays. Waking it loads the address again.
    pub(crate) fn sleep(&mut self, cx: &mut Context<Self>) {
        if self.asleep {
            return;
        }
        self.asleep = true;
        self.loading = false;
        self.can_back = false;
        self.can_forward = false;
        #[cfg(target_os = "macos")]
        self.host.borrow_mut().take();
        cx.notify();
    }

    pub(crate) fn wake(&mut self, cx: &mut Context<Self>) {
        if self.asleep {
            self.asleep = false;
            self.failure = None;
            cx.notify();
        }
    }

    /// Tells the pane whether GPUI is drawing something that may be over it.
    pub(crate) fn set_covered(&mut self, covered: bool, cx: &mut Context<Self>) {
        self.shell_covered = covered;
        self.apply_cover(cx);
    }

    /// The page gives way while the Shell or the pane's own menu is drawn
    /// over it.
    fn apply_cover(&mut self, cx: &mut Context<Self>) {
        let covered = self.shell_covered || self.menu.is_some();
        if self.covered == covered {
            return;
        }
        self.covered = covered;
        if !covered {
            self.stand_in = None;
            cx.notify();
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            self.refocus.set(host.has_keyboard());
            let view = cx.weak_entity();
            let app = cx.to_async();
            host.snapshot(move |bytes| {
                app.spawn(async move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        // Still covered: the picture takes the page's place.
                        if this.covered
                            && let Some(bytes) = bytes
                        {
                            this.stand_in = Some(std::sync::Arc::new(gpui::Image::from_bytes(
                                gpui::ImageFormat::Jpeg,
                                bytes,
                            )));
                            cx.notify();
                        }
                    });
                })
                .detach();
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn covered(&self) -> bool {
        self.covered
    }

    /// Starts a frame: the page counts as off screen until its pane paints.
    pub(crate) fn begin_frame(&self) -> PageFrame {
        self.placed.set(false);
        PageFrame {
            #[cfg(target_os = "macos")]
            host: self.host.clone(),
            placed: self.placed.clone(),
        }
    }

    /// The keyboard goes to the page, or to the address of an empty one.
    pub(crate) fn focus_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            window.focus(&self.focus, cx);
            host.focus();
            return;
        }
        window.focus(&self.address.focus_handle(cx), cx);
    }

    /// Whether the native page is in front of the user right now: real
    /// mouse and key events can only be sent to one that is.
    pub(crate) fn on_screen(&self) -> bool {
        self.placed.get() && !self.covered && self.stand_in.is_none() && !self.asleep
    }

    /// Makes sure the page has a web view even though its pane is not being
    /// drawn, so that an agent can use a page the user is not looking at.
    /// It loads off screen; the pane puts it in place when it is shown.
    pub(crate) fn ensure_host(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSView;
            use objc2_foundation::{NSPoint, NSRect, NSSize};
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if self.asleep {
                self.asleep = false;
                self.failure = None;
                cx.notify();
            }
            if self.host.borrow().is_some() {
                return true;
            }
            let Ok(handle) = HasWindowHandle::window_handle(window) else {
                return false;
            };
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return false;
            };
            let Some(mtm) = objc2::MainThreadMarker::new() else {
                return false;
            };
            let parent = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
            let (width, height) = self.viewport.unwrap_or((1280, 800));
            let frame = NSRect::new(
                NSPoint::new(0., 0.),
                NSSize::new(f64::from(width), f64::from(height)),
            );
            let link = host::Link {
                app: cx.to_async(),
                view: cx.weak_entity(),
            };
            let hosted = host::Hosted::new(
                mtm,
                link,
                frame,
                frame,
                self.user_agent.as_deref(),
                &self.url,
            );
            hosted.place(parent, frame, frame, 1.);
            hosted.hide();
            *self.host.borrow_mut() = Some(hosted);
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (window, cx);
            false
        }
    }

    /// Back, forward or reload, by name.
    pub(crate) fn go(&mut self, action: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match action {
            "back" => self.back(cx),
            "forward" => self.forward(cx),
            "reload" => {
                self.failure = None;
                #[cfg(target_os = "macos")]
                if let Some(host) = self.host.borrow().as_ref() {
                    host.reload();
                }
                cx.notify();
            }
            other => {
                return Err(format!(
                    "Unknown action {other}: use back, forward or reload"
                ));
            }
        }
        Ok(())
    }

    /// Changes the user agent (`None`: the default) and loads the page
    /// again, since a user agent only applies to loads made after it.
    pub(crate) fn set_user_agent(&mut self, user_agent: Option<String>, cx: &mut Context<Self>) {
        if self.user_agent == user_agent {
            return;
        }
        self.user_agent = user_agent;
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.set_user_agent(self.user_agent.as_deref());
            host.reload();
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    /// Holds the page at a size, or (`None`) lets it fit the pane.
    pub(crate) fn set_viewport(&mut self, viewport: Option<(u32, u32)>, cx: &mut Context<Self>) {
        let viewport = viewport
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map(|(width, height)| (width.clamp(200, 4000), height.clamp(200, 4000)));
        if self.viewport != viewport {
            self.viewport = viewport;
            cx.emit(BrowserEvent::Changed);
            cx.notify();
        }
    }

    /// A picture of the page as JPEG bytes, or `None` if it has no view.
    pub(crate) fn snapshot(&self) -> async_channel::Receiver<Option<Vec<u8>>> {
        let (send, receive) = async_channel::bounded(1);
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.snapshot(move |bytes| {
                let _ = send.try_send(bytes);
            });
            return receive;
        }
        let _ = send.try_send(None);
        receive
    }

    /// An agent of `session` is using the page: the pane says so, with a
    /// way to stop it.
    pub(crate) fn note_agent(&mut self, session: i64, name: String, cx: &mut Context<Self>) {
        let note = AgentNote {
            session,
            name,
            stopped: false,
        };
        self.last_used.set(std::time::Instant::now());
        if self.agent.as_ref() != Some(&note) {
            self.agent = Some(note);
            cx.notify();
        }
    }

    /// Shows the page's agent as stopped (or allowed again).
    pub(crate) fn set_agent_stopped(
        &mut self,
        session: i64,
        stopped: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(agent) = self.agent.as_mut().filter(|agent| agent.session == session)
            && agent.stopped != stopped
        {
            agent.stopped = stopped;
            cx.notify();
        }
    }

    /// Runs an expression in the page; the receiver gets its value as JSON
    /// text. A page with no web view answers with an error.
    pub(crate) fn eval(&self, script: &str) -> async_channel::Receiver<Result<String, String>> {
        let (send, receive) = async_channel::bounded(1);
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            host.eval(script, move |result| {
                let _ = send.try_send(result);
            });
            return receive;
        }
        let _ = send.try_send(Err("The page is asleep".into()));
        receive
    }

    pub(crate) fn loading(&self) -> bool {
        self.loading
    }

    /// The page as the rules that free memory see it, if it is loaded.
    pub(crate) fn load(&self, id: i64) -> Option<monocode_core::browser::PageLoad> {
        self.has_page().then(|| monocode_core::browser::PageLoad {
            id,
            idle_secs: self.last_used.get().elapsed().as_secs(),
            on_screen: self.placed.get(),
            keep_awake: self.keep_awake,
        })
    }

    /// Megabytes the page's process holds, when that can be told.
    pub(crate) fn memory_mb(&self) -> Option<u64> {
        #[cfg(target_os = "macos")]
        return (self.host.borrow().as_ref()).and_then(|host| host.memory_mb());
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// Whether the page has a web view (it is not asleep).
    pub(crate) fn has_page(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.host.borrow().is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Loading, as the web view says right now rather than as last heard.
    pub(crate) fn busy(&self) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            return host.state().loading;
        }
        self.loading
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.failure.clone()
    }

    /// Where the page stands, for the self-test's report.
    pub(crate) fn diagnostics(&self) -> serde_json::Value {
        #[cfg(target_os = "macos")]
        let hosted = self.host.borrow().is_some();
        #[cfg(not(target_os = "macos"))]
        let hosted = false;
        serde_json::json!({
            "url": self.url,
            "asleep": self.asleep,
            "hosted": hosted,
            "placed": self.placed.get(),
            "covered": self.covered,
            "stand_in": self.stand_in.is_some(),
            "native": self.native_state(),
            "calls": [
                host::CALLS.load(std::sync::atomic::Ordering::Relaxed),
                host::REACHED.load(std::sync::atomic::Ordering::Relaxed),
                host::FAILED.load(std::sync::atomic::Ordering::Relaxed),
            ],
            "title": self.title,
            "paint": self.paint_note.get(),
            "loading": self.loading,
            "failure": self.failure,
        })
    }

    /// Whether the keyboard is the page's.
    pub(crate) fn has_keyboard(&self) -> bool {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            return host.has_keyboard();
        }
        false
    }

    /// A real click at a point of the page, in its own CSS pixels. The
    /// event is returned to be sent once no GPUI update is running: AppKit
    /// answers it by calling back into GPUI.
    pub(crate) fn click_at(&self, x: f64, y: f64) -> Box<dyn FnOnce()> {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            return host.click(x, y);
        }
        let _ = (x, y);
        Box::new(|| {})
    }

    /// A real key press through the app's event path; sent as `click_at`.
    pub(crate) fn press(&self, characters: &str, code: u16, command: bool) -> Box<dyn FnOnce()> {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            return host.key(characters, code, command);
        }
        let _ = (characters, code, command);
        Box::new(|| {})
    }

    pub(crate) fn probe(&self, x: f64, y: f64) -> serde_json::Value {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            return host.probe(x, y);
        }
        let _ = (x, y);
        serde_json::Value::Null
    }

    /// What the web view itself reports, bypassing what the pane remembers.
    pub(crate) fn native_state(&self) -> serde_json::Value {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            let state = host.state();
            return serde_json::json!({
                "url": state.url, "title": state.title, "loading": state.loading,
            });
        }
        serde_json::Value::Null
    }

    #[cfg(target_os = "macos")]
    pub(super) fn page_changed(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.host.borrow().as_ref().map(|host| host.state()) else {
            return;
        };
        let moved = !state.url.is_empty() && state.url != self.url;
        let retitled = state.title != self.title;
        if !moved
            && !retitled
            && (self.loading, self.can_back, self.can_forward)
                == (state.loading, state.back, state.forward)
        {
            return;
        }
        self.loading = state.loading;
        self.can_back = state.back;
        self.can_forward = state.forward;
        if state.loading {
            self.failure = None;
        }
        if moved {
            self.url = state.url.clone();
            if !self.address_focused {
                self.address
                    .update(cx, |input, cx| input.set_text(state.url, cx));
            }
        }
        if moved || retitled {
            self.title = state.title;
            cx.emit(BrowserEvent::Changed);
        }
        cx.notify();
    }

    /// WebKit ended the page's process: it ran out of memory, or crashed.
    #[cfg(target_os = "macos")]
    pub(super) fn page_stopped(&mut self, cx: &mut Context<Self>) {
        self.sleep(cx);
        self.banner = Some(Banner::Problem(
            "This page stopped (it crashed or was using too much memory) and was put to sleep."
                .into(),
        ));
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    pub(super) fn open_new(&mut self, url: String, cx: &mut Context<Self>) {
        cx.emit(BrowserEvent::OpenNew(url));
    }

    #[cfg(target_os = "macos")]
    pub(super) fn downloaded(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.banner = Some(Banner::Downloaded(path));
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    pub(super) fn download_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.banner = Some(Banner::Problem(format!("The download failed: {message}")));
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    pub(super) fn page_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.loading = false;
        self.failure = Some(message);
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    pub(super) fn page_focused(&mut self, cx: &mut Context<Self>) {
        self.take_focus = true;
        cx.emit(BrowserEvent::Focused);
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn edit(&self, action: objc2::runtime::Sel) {
        if let Some(host) = self.host.borrow().as_ref() {
            host.edit(action);
        }
    }

    fn tool(
        id: &'static str,
        label: &'static str,
        glyph: &'static str,
        enabled: bool,
    ) -> gpui::Stateful<gpui::Div> {
        icon_button(id, label, glyph)
            .debug_selector(move || id.into())
            .flex_shrink_0()
            .when(!enabled, |d| d.opacity(0.3))
    }

    fn address_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (back, forward) = (self.can_back, self.can_forward);
        let url = self.url.clone();
        div()
            .debug_selector(|| "browser-address-row".into())
            .h(px(36.))
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(content(0.07))
            .child(
                Self::tool("browser-back", "Back", "chevron-left", back)
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            )
            .child(
                Self::tool("browser-forward", "Forward", "chevron-right", forward)
                    .on_click(cx.listener(|this, _, _, cx| this.forward(cx))),
            )
            .child(
                Self::tool(
                    "browser-reload",
                    if self.loading { "Stop" } else { "Reload" },
                    if self.loading { "close" } else { "refresh" },
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.reload_or_stop(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .bg(content(0.06))
                    .child(icon("globe", 12.).text_color(content(0.45)))
                    .child(div().flex_1().min_w_0().child(self.address.clone())),
            )
            .child(
                Self::tool(
                    "browser-sleep",
                    "Sleep: free this page's memory",
                    "queue-pause",
                    !self.asleep,
                )
                .on_click(cx.listener(|this, _, _, cx| this.sleep(cx))),
            )
            .child(
                Self::tool(
                    "browser-external",
                    "Open in my browser",
                    "external-link",
                    true,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
            )
            .child(
                Self::tool("browser-menu", "Page options", "more-horizontal", true)
                    .relative()
                    .child(self.menu_anchor.probe())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_menu(window, cx))),
            )
            .into_any_element()
    }

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.apply_cover(cx);
            cx.notify();
            return;
        }
        let agents = monocode_core::browser::USER_AGENTS;
        let current = monocode_core::browser::user_agent_name(self.user_agent.as_deref());
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("find", "Find in Page…")
                .shortcut(if cfg!(target_os = "macos") {
                    "⌘F"
                } else {
                    "Ctrl F"
                })
                .into(),
            MenuAction::new("inspect", "Web Inspector").into(),
            MenuAction::new("errors", "Add Console Errors to Chat").into(),
            MenuAction::new("clear", "Clear This Site's Data").into(),
            MenuAction::new("awake", "Keep This Page Awake")
                .checked(self.keep_awake)
                .into(),
            MenuItem::Separator,
            MenuItem::Header("User agent".into()),
        ];
        for (ix, (name, _)) in agents.iter().enumerate() {
            items.push(
                MenuAction::new(format!("agent:{ix}"), *name)
                    .checked(current == *name)
                    .into(),
            );
        }
        items.push(
            MenuAction::new("agent:custom", "Custom…")
                .checked(current == "Custom")
                .into(),
        );
        items.push(MenuItem::Separator);
        items.push(MenuItem::Header("Size".into()));
        items.push(
            MenuAction::new("size:fit", "Fit pane")
                .checked(self.viewport.is_none())
                .into(),
        );
        for (ix, (name, width, height)) in monocode_core::browser::DEVICE_SIZES.iter().enumerate() {
            items.push(
                MenuAction::new(format!("size:{ix}"), format!("{name}  {width} × {height}"))
                    .checked(self.viewport == Some((*width, *height)))
                    .into(),
            );
        }
        items.push(MenuItem::Note(
            "A user agent changes what servers and scripts are told. The engine stays WebKit."
                .into(),
        ));
        if let Some(memory) = self.memory_mb() {
            items.push(MenuItem::Note(
                format!("This page is using {memory} MB.").into(),
            ));
        }
        let menu = cx.new(|cx| {
            Menu::new("Page options", items, window, cx)
                .width(250.)
                .style(MenuStyle::Explorer)
        });
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            this.menu = None;
            this.apply_cover(cx);
            if let MenuEvent::Picked(id, _) = event {
                this.pick(id.as_ref(), window, cx);
            }
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            menu,
            _events: events,
        });
        self.apply_cover(cx);
        cx.notify();
    }

    fn pick(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "find" => self.open_find(window, cx),
            "inspect" => self.inspect(cx),
            "errors" => self.console_errors_to_chat(cx),
            "clear" => self.clear_site_data(cx),
            "agent:custom" => self.open_custom_agent(window, cx),
            "size:fit" => self.set_viewport(None, cx),
            "awake" => {
                self.keep_awake = !self.keep_awake;
                cx.emit(BrowserEvent::Changed);
            }
            _ => {
                let index = |prefix: &str| {
                    id.strip_prefix(prefix)
                        .and_then(|n| n.parse::<usize>().ok())
                };
                if let Some((_, agent)) =
                    index("agent:").and_then(|ix| monocode_core::browser::USER_AGENTS.get(ix))
                {
                    self.custom_agent = None;
                    self.set_user_agent((!agent.is_empty()).then(|| (*agent).to_owned()), cx);
                } else if let Some((_, width, height)) =
                    index("size:").and_then(|ix| monocode_core::browser::DEVICE_SIZES.get(ix))
                {
                    self.set_viewport(Some((*width, *height)), cx);
                }
            }
        }
    }

    fn entry(
        label: &'static str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        submit: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) -> Entry {
        let input = cx.new(|cx| {
            let mut input = Input::field(label, label, content(0.35), cx);
            input.set_font_size(13.);
            input.set_text(text, cx);
            input
        });
        let events = cx.subscribe(&input, move |this, input, _: &input::Submitted, cx| {
            let text = input.read(cx).content.to_string();
            submit(this, text, cx);
        });
        window.focus(&input.focus_handle(cx), cx);
        Entry {
            input,
            _events: events,
        }
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.find {
            Some(find) => window.focus(&find.input.focus_handle(cx), cx),
            None => {
                self.find_missed = false;
                self.find = Some(Self::entry(
                    "Find in page",
                    String::new(),
                    window,
                    cx,
                    |this, text, cx| this.find_next(&text, false, cx),
                ));
            }
        }
        cx.notify();
    }

    fn find_next(&mut self, text: &str, backwards: bool, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.borrow().as_ref() {
            let (view, app) = (cx.weak_entity(), cx.to_async());
            host.find(text, backwards, move |found| {
                app.spawn(async move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.find_missed = !found;
                        cx.notify();
                    });
                })
                .detach();
            });
        }
        let _ = backwards;
    }

    fn find_again(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if let Some(text) = (self.find.as_ref()).map(|find| find.input.read(cx).content.to_string())
        {
            self.find_next(&text, backwards, cx);
        }
    }

    fn open_custom_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.user_agent.clone().unwrap_or_default();
        self.custom_agent = Some(Self::entry(
            "User agent",
            current,
            window,
            cx,
            |this, text, cx| {
                this.custom_agent = None;
                this.set_user_agent(monocode_core::browser::user_agent(&text), cx);
            },
        ));
        cx.notify();
    }

    fn inspect(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        let opened = (self.host.borrow().as_ref()).is_some_and(|host| host.inspect());
        #[cfg(not(target_os = "macos"))]
        let opened = false;
        if !opened {
            self.banner = Some(Banner::Problem(
                "The inspector could not be opened from here. In Safari: Develop › this Mac › this page."
                    .into(),
            ));
            cx.notify();
        }
    }

    /// Puts what the page logged as errors and warnings into the chat of
    /// the session it belongs to, as Add to Chat does for a selection.
    fn console_errors_to_chat(&mut self, cx: &mut Context<Self>) {
        let script = agent::call(
            "M.console.filter((e) => e.level === 'error' || e.level === 'warn').slice(-40).map((e) => `[${e.level}] ${e.text}`)",
        );
        let receive = self.eval(&script);
        let url = self.url.clone();
        cx.spawn(async move |this, cx| {
            let lines: Vec<String> = (receive.recv().await.ok())
                .and_then(|result| result.ok())
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if lines.is_empty() {
                    this.banner = Some(Banner::Info(
                        "The page has logged no errors or warnings.".into(),
                    ));
                    cx.notify();
                    return;
                }
                let block = lines.join("\n").replace("```", "'''");
                cx.emit(BrowserEvent::AddToChat(format!(
                    "Console of {url}:\n```\n{block}\n```"
                )));
            });
        })
        .detach();
    }

    /// Signs out of the site on screen and forgets what it stored.
    fn clear_site_data(&mut self, cx: &mut Context<Self>) {
        let rest = (self.url.split_once("://")).map_or(self.url.as_str(), |(_, rest)| rest);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = authority
            .rsplit_once(':')
            .map_or(authority, |(host, port)| {
                if port.chars().all(|c| c.is_ascii_digit()) {
                    host
                } else {
                    authority
                }
            });
        if host.is_empty() {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            let site = host::site_of(host);
            let (view, app) = (cx.weak_entity(), cx.to_async());
            let name = site.clone();
            host::data::clear_site(site, move |found| {
                app.spawn(async move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.banner = Some(Banner::Info(if found {
                            format!("Cleared what {name} stored. You are signed out of it.")
                        } else {
                            format!("{name} had nothing stored.")
                        }));
                        let _ = this.go("reload", cx);
                    });
                })
                .detach();
            });
        }
    }

    /// The find bar, or the field for a user agent of one's own.
    fn entry_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let row = |selector: &'static str| {
            div()
                .debug_selector(move || selector.into())
                .h(px(32.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(content(0.07))
                .text_size(px(12.))
        };
        let mut rows = Vec::new();
        if let Some(find) = &self.find {
            rows.push(
                row("browser-find-row")
                    .child(icon("search", 12.).text_color(content(0.45)))
                    .child(div().flex_1().min_w_0().child(find.input.clone()))
                    .when(self.find_missed, |d| {
                        d.child(div().text_color(content(0.5)).child("No matches"))
                    })
                    .child(
                        Self::tool(
                            "browser-find-previous",
                            "Previous match",
                            "chevron-up",
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.find_again(true, cx))),
                    )
                    .child(
                        Self::tool("browser-find-next", "Next match", "chevron-down", true)
                            .on_click(cx.listener(|this, _, _, cx| this.find_again(false, cx))),
                    )
                    .child(
                        Self::tool("browser-find-close", "Close", "close", true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.find = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(agent) = &self.custom_agent {
            rows.push(
                row("browser-agent-entry")
                    .child(div().text_color(content(0.5)).child("User agent"))
                    .child(div().flex_1().min_w_0().child(agent.input.clone()))
                    .child(
                        Self::tool("browser-agent-entry-close", "Cancel", "close", true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.custom_agent = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    /// Who else is acting on the page, with the switch that stops them.
    fn agent_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agent = self.agent.clone()?;
        let session = agent.session;
        let stopped = agent.stopped;
        Some(
            div()
                .debug_selector(|| "browser-agent-row".into())
                .h(px(30.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(content(0.07))
                .bg(content(0.04))
                .text_size(px(12.))
                .text_color(content(0.8))
                .child(icon("bot", 13.).text_color(content(0.6)))
                .child(div().flex_1().min_w_0().truncate().child(if stopped {
                    format!("{} is stopped from using the browser", agent.name)
                } else {
                    format!("{} is using this page", agent.name)
                }))
                .child(
                    crate::project_settings::button(
                        "browser-agent-switch",
                        if stopped { "Allow again" } else { "Stop" },
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(if stopped {
                            BrowserEvent::AllowAgent(session)
                        } else {
                            BrowserEvent::StopAgent(session)
                        });
                    })),
                )
                .into_any_element(),
        )
    }

    fn banner_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.banner.clone()?;
        let (text, path) = match &banner {
            Banner::Downloaded(path) => (
                format!(
                    "Downloaded {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                Some(path.clone()),
            ),
            Banner::Problem(message) | Banner::Info(message) => (message.clone(), None),
        };
        Some(
            div()
                .debug_selector(|| "browser-banner".into())
                .h(px(30.))
                .flex_shrink_0()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(content(0.07))
                .bg(content(0.04))
                .text_size(px(12.))
                .text_color(content(0.8))
                .child(div().flex_1().min_w_0().truncate().child(text))
                .when_some(path, |d, path| {
                    d.child(
                        crate::project_settings::button("browser-banner-reveal", "Show in Finder")
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                    )
                })
                .child(
                    Self::tool("browser-banner-close", "Dismiss", "close", true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.banner = None;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        )
    }

    /// What stands where the page would be while there is none.
    fn card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (headline, detail, action) = if self.asleep {
            (
                "This page is asleep",
                "It uses no memory. Its address is kept.".to_owned(),
                "Wake",
            )
        } else if let Some(failure) = &self.failure {
            ("This page did not load", failure.clone(), "Try again")
        } else {
            return None;
        };
        Some(
            div()
                .debug_selector(|| "browser-card".into())
                .absolute()
                .inset_0()
                .bg(pane_fill())
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_size(px(13.))
                .child(icon("globe", 24.).text_color(content(0.4)))
                .child(div().text_color(content(0.9)).child(headline))
                .child(
                    div()
                        .max_w(px(420.))
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .child(detail),
                )
                .child(
                    crate::project_settings::button("browser-card-action", action)
                        .on_click(cx.listener(|this, _, _, cx| this.reload_or_stop(cx))),
                )
                .into_any_element(),
        )
    }

    #[cfg(target_os = "macos")]
    fn page(&self, cx: &mut Context<Self>) -> AnyElement {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSView;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hosted = self.host.clone();
        let placed = self.placed.clone();
        let note = self.paint_note.clone();
        let refocus = self.refocus.clone();
        let last_used = self.last_used.clone();
        let covered = self.covered;
        let link = host::Link {
            app: cx.to_async(),
            view: cx.weak_entity(),
        };
        // A failed load shows its card over the page; a sleeping page has none.
        let shown = !self.asleep && self.failure.is_none() && self.stand_in.is_none();
        let asleep = self.asleep;
        let (url, user_agent) = (self.url.clone(), self.user_agent.clone());
        let viewport = self.viewport;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let mut hosted = hosted.borrow_mut();
                if !shown {
                    if let Some(host) = hosted.as_ref() {
                        host.hide();
                    }
                    if asleep {
                        return;
                    }
                }
                let Ok(handle) = HasWindowHandle::window_handle(window) else {
                    note.set("no window handle");
                    return;
                };
                let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                    note.set("not an AppKit window");
                    return;
                };
                // GPUI keeps its AppKit content view alive throughout paint.
                let parent = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
                // AppKit frames are in window points; GPUI's are divided by
                // the interface zoom.
                let zoom = window.content_zoom();
                let pane = bounds;
                // Held at a device's size, the page sits at the top of the
                // pane, centred, and the pane clips what does not fit.
                let bounds = match viewport {
                    Some((width, height)) => {
                        let (width, height) = (px(width as f32), px(height as f32));
                        let left = pane.left() + ((pane.size.width - width) / 2.).max(px(0.));
                        gpui::Bounds::new(gpui::point(left, pane.top()), gpui::size(width, height))
                    }
                    None => pane,
                };
                let clipped = bounds
                    .intersect(&pane)
                    .intersect(&window.content_mask().bounds)
                    .map(|v| v * zoom);
                let bounds = bounds.map(|v| v * zoom);
                if clipped.size.width <= px(0.) || clipped.size.height <= px(0.) {
                    if let Some(host) = hosted.as_ref() {
                        host.hide();
                    }
                    note.set("clipped away");
                    return;
                }
                let height = f64::from(clipped.size.height);
                let y = f64::from(clipped.origin.y);
                let y = if parent.isFlipped() {
                    y
                } else {
                    parent.bounds().size.height - y - height
                };
                let frame = NSRect::new(
                    NSPoint::new(f64::from(clipped.origin.x), y),
                    NSSize::new(f64::from(clipped.size.width), height),
                );
                // The page keeps its full size while its clip follows
                // GPUI's content mask.
                let web_frame = NSRect::new(
                    NSPoint::new(
                        f64::from(bounds.left() - clipped.left()),
                        f64::from(bounds.top() - clipped.top()),
                    ),
                    NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
                );
                if hosted.is_none() {
                    let Some(mtm) = MainThreadMarker::new() else {
                        return;
                    };
                    *hosted = Some(host::Hosted::new(
                        mtm,
                        link.clone(),
                        frame,
                        web_frame,
                        user_agent.as_deref(),
                        &url,
                    ));
                }
                if let Some(host) = hosted.as_ref()
                    && shown
                {
                    host.place(parent, frame, web_frame, f64::from(zoom));
                    placed.set(true);
                    last_used.set(std::time::Instant::now());
                    note.set("placed");
                    if !covered && refocus.take() {
                        host.focus();
                    }
                }
            },
        )
        .size_full()
        .into_any_element()
    }

    #[cfg(not(target_os = "macos"))]
    fn page(&self, _: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.))
            .text_color(content(0.5))
            .child("The browser pane needs macOS.")
            .into_any_element()
    }
}

impl gpui::Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.address_focused = self.address.focus_handle(cx).is_focused(window);
        if std::mem::take(&mut self.take_focus) {
            window.focus(&self.focus, cx);
        }
        let pane = div()
            .id("browser-pane")
            .role(Role::Region)
            .aria_label("Browser")
            .size_full()
            .flex()
            .flex_col()
            .bg(pane_fill())
            .track_focus(&self.focus)
            .key_context("Browser");
        #[cfg(target_os = "macos")]
        let pane = {
            use objc2::sel;
            pane.on_action(cx.listener(|this, _: &input::Copy, _, _| this.edit(sel!(copy:))))
                .on_action(cx.listener(|this, _: &input::Cut, _, _| this.edit(sel!(cut:))))
                .on_action(cx.listener(|this, _: &input::Paste, _, _| this.edit(sel!(paste:))))
                .on_action(cx.listener(|this, _: &input::SelectAll, _, _| {
                    this.edit(host::Hosted::select_all())
                }))
                .on_action(cx.listener(|this, _: &input::Undo, _, _| this.edit(sel!(undo:))))
                .on_action(cx.listener(|this, _: &input::Redo, _, _| this.edit(sel!(redo:))))
        };
        let pane = pane
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| {
                window.focus(&this.address.focus_handle(cx), cx);
                window.dispatch_action(Box::new(input::SelectAll), cx);
            }))
            .on_action(cx.listener(|this, _: &FindInPage, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.reload_or_stop(cx)))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.back(cx)))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.forward(cx)));
        pane.child(self.address_row(cx))
            .children(self.entry_rows(cx))
            .children(self.agent_row(cx))
            .children(self.banner_row(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(self.page(cx))
                    .children(self.stand_in.clone().map(|image| {
                        gpui::img(image)
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(gpui::ObjectFit::Fill)
                    }))
                    .children(self.card(cx)),
            )
            .children(self.menu.as_ref().map(|open| {
                menu_popover(&open.menu, Anchor::Slot(self.menu_anchor.clone()), cx)
                    .align(Align::End)
            }))
    }
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        self.host.borrow_mut().take();
    }
}
