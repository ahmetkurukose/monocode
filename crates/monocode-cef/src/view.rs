//! One browser page shown as a native child view of the host window
//! (docs/native/browser/context.md K3, §4.1).
//!
//! CEF creates the page's own native view inside a parent the host gives
//! (macOS: an `NSView`). Chromium composites into it directly, exactly as in
//! Chrome, so scrolling and input never pass through the host. The host only
//! says where the view sits ([`BrowserView::set_frame`]) and whether it shows
//! ([`BrowserView::set_visible`]); page state comes back as [`BrowserEvent`]s.

use std::{cell::RefCell, ffi::c_void, rc::Rc};

use cef::*;

/// What a page reports. Delivered on the main thread, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    /// The main frame's URL changed (navigation, history, fragment).
    Address(String),
    Title(String),
    Loading {
        loading: bool,
        can_go_back: bool,
        can_go_forward: bool,
    },
    /// 0.0 to 1.0.
    Progress(f64),
    /// The main frame failed to load (aborted navigations are not reported).
    LoadFailed {
        url: String,
        code: i32,
        text: String,
    },
    /// The page asked for a new window (`window.open`, `target=_blank`). No
    /// window was created; the host decides what to do with the URL.
    PopupRequested {
        url: String,
        user_gesture: bool,
    },
    /// The browser is gone; the view can be dropped.
    Closed,
    /// A find-in-page result ([`BrowserView::find`]). `active_ordinal` counts
    /// from 1; several updates may arrive for one search, the last one final.
    FindResult {
        count: i32,
        active_ordinal: i32,
        final_update: bool,
    },
    /// The page took the keyboard (a click in it, or [`BrowserView::set_focus`]).
    Focused,
}

/// A rectangle in the parent view's coordinates, in points, origin top-left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug)]
pub enum CreateError {
    NotStarted,
    Unsupported,
    Failed,
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateError::NotStarted => write!(f, "Chromium is not running"),
            CreateError::Unsupported => {
                write!(f, "native browser views are not supported here yet")
            }
            CreateError::Failed => write!(f, "Chromium could not create the page"),
        }
    }
}

impl std::error::Error for CreateError {}

#[derive(Clone)]
struct PageState {
    events: async_channel::Sender<BrowserEvent>,
}

impl PageState {
    fn send(&self, event: BrowserEvent) {
        // Unbounded: never blocks a CEF callback.
        let _ = self.events.try_send(event);
    }
}

fn string(value: Option<&CefString>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

wrap_display_handler! {
    struct PageDisplayHandler {
        state: PageState,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut cef::Frame>,
            url: Option<&CefString>,
        ) {
            if frame.is_some_and(|f| f.is_main() != 0) {
                self.state.send(BrowserEvent::Address(string(url)));
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.state.send(BrowserEvent::Title(string(title)));
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.state.send(BrowserEvent::Progress(progress));
        }
    }
}

wrap_load_handler! {
    struct PageLoadHandler {
        state: PageState,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            self.state.send(BrowserEvent::Loading {
                loading: is_loading != 0,
                can_go_back: can_go_back != 0,
                can_go_forward: can_go_forward != 0,
            });
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut cef::Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // ABORTED: replaced by another navigation, nothing to show.
            if error_code == Errorcode::ABORTED || !frame.is_some_and(|f| f.is_main() != 0) {
                return;
            }
            self.state.send(BrowserEvent::LoadFailed {
                url: string(failed_url),
                code: error_code.get_raw(),
                text: string(error_text),
            });
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpanHandler {
        state: PageState,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            _window_info: Option<&mut cef::WindowInfo>,
            _client: Option<&mut Option<cef::Client>>,
            _settings: Option<&mut cef::BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // Never let Alloy open a native window of its own.
            self.state.send(BrowserEvent::PopupRequested {
                url: string(target_url),
                user_gesture: user_gesture != 0,
            });
            1
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.state.send(BrowserEvent::Closed);
        }
    }
}

wrap_find_handler! {
    struct PageFindHandler {
        state: PageState,
    }

    impl FindHandler {
        fn on_find_result(
            &self,
            _browser: Option<&mut Browser>,
            _identifier: ::std::os::raw::c_int,
            count: ::std::os::raw::c_int,
            _selection_rect: Option<&cef::Rect>,
            active_match_ordinal: ::std::os::raw::c_int,
            final_update: ::std::os::raw::c_int,
        ) {
            self.state.send(BrowserEvent::FindResult {
                count,
                active_ordinal: active_match_ordinal,
                final_update: final_update != 0,
            });
        }
    }
}

wrap_focus_handler! {
    struct PageFocusHandler {
        state: PageState,
    }

    impl FocusHandler {
        fn on_got_focus(&self, _browser: Option<&mut Browser>) {
            self.state.send(BrowserEvent::Focused);
        }
    }
}

wrap_client! {
    struct PageClient {
        display: cef::DisplayHandler,
        load: cef::LoadHandler,
        life_span: cef::LifeSpanHandler,
        find: cef::FindHandler,
        focus: cef::FocusHandler,
    }

    impl Client {
        fn display_handler(&self) -> Option<cef::DisplayHandler> {
            Some(self.display.clone())
        }

        fn load_handler(&self) -> Option<cef::LoadHandler> {
            Some(self.load.clone())
        }

        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn find_handler(&self) -> Option<cef::FindHandler> {
            Some(self.find.clone())
        }

        fn focus_handler(&self) -> Option<cef::FocusHandler> {
            Some(self.focus.clone())
        }
    }
}

/// A page and its native view.
pub struct BrowserView {
    browser: Browser,
    /// CEF's own view, a child of the parent given to [`BrowserView::create`].
    native: *mut c_void,
    frame: RefCell<Frame>,
    closed: Rc<std::cell::Cell<bool>>,
}

impl BrowserView {
    /// Creates a page inside `parent` (macOS: an `NSView*`) at `frame`, and
    /// starts loading `url`. Events go to the returned receiver.
    pub fn create(
        parent: *mut c_void,
        frame: Frame,
        url: &str,
    ) -> Result<(Self, async_channel::Receiver<BrowserEvent>), CreateError> {
        if !crate::runtime::is_started() {
            return Err(CreateError::NotStarted);
        }
        if !cfg!(target_os = "macos") {
            return Err(CreateError::Unsupported);
        }
        let (tx, rx) = async_channel::unbounded();
        let state = PageState { events: tx };
        let mut client = PageClient::new(
            PageDisplayHandler::new(state.clone()),
            PageLoadHandler::new(state.clone()),
            PageLifeSpanHandler::new(state.clone()),
            PageFindHandler::new(state.clone()),
            PageFocusHandler::new(state),
        );

        let bounds = cef::Rect {
            x: 0,
            y: 0,
            width: frame.width.round().max(1.0) as i32,
            height: frame.height.round().max(1.0) as i32,
        };
        let window_info = cef::WindowInfo {
            // Native parent requires Alloy style on macOS (context.md §4.2).
            runtime_style: cef::RuntimeStyle::ALLOY,
            ..Default::default()
        }
        .set_as_child(parent as _, &bounds);
        let settings = cef::BrowserSettings::default();
        let browser = cef::browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&url.into()),
            Some(&settings),
            None,
            None,
        )
        .ok_or(CreateError::Failed)?;
        // A pointer on macOS and Windows, an X11 window id on Linux.
        #[allow(clippy::unnecessary_cast)]
        let native = browser
            .host()
            .map(|host| host.window_handle() as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
        let view = Self {
            browser,
            native,
            frame: RefCell::new(Frame::default()),
            closed: Rc::default(),
        };
        view.set_frame(frame);
        Ok((view, rx))
    }

    /// Moves and sizes the page inside its parent.
    pub fn set_frame(&self, frame: Frame) {
        if *self.frame.borrow() == frame || self.native.is_null() {
            return;
        }
        *self.frame.borrow_mut() = frame;
        #[cfg(target_os = "macos")]
        crate::mac::view::set_frame(self.native, frame);
    }

    pub fn set_visible(&self, visible: bool) {
        if self.native.is_null() {
            return;
        }
        #[cfg(target_os = "macos")]
        crate::mac::view::set_hidden(self.native, !visible);
        if let Some(host) = self.browser.host() {
            host.was_hidden(!visible as _);
        }
    }

    pub fn load_url(&self, url: &str) {
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&url.into()));
        }
    }

    pub fn go_back(&self) {
        self.browser.go_back();
    }

    pub fn go_forward(&self) {
        self.browser.go_forward();
    }

    pub fn reload(&self) {
        self.browser.reload();
    }

    pub fn stop(&self) {
        self.browser.stop_load();
    }

    /// Reloads without the cache (⇧⌘R).
    pub fn reload_ignore_cache(&self) {
        self.browser.reload_ignore_cache();
    }

    /// Finds `text` in the page; results arrive as
    /// [`BrowserEvent::FindResult`]. `find_next` continues the current
    /// search (next or previous match) instead of starting a new one.
    pub fn find(&self, text: &str, forward: bool, match_case: bool, find_next: bool) {
        if let Some(host) = self.browser.host() {
            host.find(
                Some(&text.into()),
                forward as _,
                match_case as _,
                find_next as _,
            );
        }
    }

    /// Ends the search; `clear_selection` also drops the active match's
    /// selection.
    pub fn stop_finding(&self, clear_selection: bool) {
        if let Some(host) = self.browser.host() {
            host.stop_finding(clear_selection as _);
        }
    }

    /// Gives the page keyboard focus, or takes it away.
    pub fn set_focus(&self, focused: bool) {
        if let Some(host) = self.browser.host() {
            host.set_focus(focused as _);
        }
    }

    /// Zoom level as Chromium counts it: 0 is 100%, each step is ×1.2.
    pub fn set_zoom_level(&self, level: f64) {
        if let Some(host) = self.browser.host() {
            host.set_zoom_level(level);
        }
    }

    /// The page's zoom level now. Chromium keeps one level per host, so it
    /// changes when the page navigates to another host.
    pub fn zoom_level(&self) -> f64 {
        self.browser.host().map_or(0., |host| host.zoom_level())
    }

    /// Opens the page's DevTools docked inside `parent` (macOS: an
    /// `NSView*`) at `frame`, as a second child view rather than a window.
    /// Fails if DevTools is open already.
    pub fn show_dev_tools(
        &self,
        parent: *mut c_void,
        frame: Frame,
    ) -> Result<
        (
            crate::DevToolsView,
            async_channel::Receiver<crate::DevToolsEvent>,
        ),
        CreateError,
    > {
        crate::DevToolsView::open(&self.browser, parent, frame)
    }

    /// Asks for a PNG of what the page shows now, at the screen's scale.
    /// `done` runs on the main thread; it is dropped without running if the
    /// page closes first.
    pub fn capture(&self, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
        crate::capture::capture(&self.browser, Box::new(done));
    }

    /// Closes the page. [`BrowserEvent::Closed`] follows.
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        crate::capture::forget(self.browser.identifier());
        if let Some(host) = self.browser.host() {
            host.close_browser(1);
        }
    }
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        self.close();
    }
}
