//! One browser page shown as a native child view of the host window
//! (docs/native/browser/context.md K3, §4.1).
//!
//! CEF creates the page's own native view inside a parent the host gives
//! (macOS: an `NSView`). Chromium composites into it directly, exactly as in
//! Chrome, so scrolling and input never pass through the host. The host only
//! says where the view sits ([`BrowserView::set_frame`]) and whether it shows
//! ([`BrowserView::set_visible`]); page state comes back as [`BrowserEvent`]s.

use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use cef::*;

pub use crate::{
    dialogs::{AuthChallenge, AuthRequest, DialogKind, JsDialog},
    download::{Download, DownloadState, cancel_download},
    popup::take_popup,
    profile::{Profile, profile_id},
};

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
    /// The page asked for a new window (`window.open`, `target=_blank`, a
    /// middle click). No window was created; the host opens the URL as a new
    /// page, and shows it at once when `foreground` (crate::popup).
    PopupRequested {
        url: String,
        user_gesture: bool,
        foreground: bool,
    },
    /// The page opened a popup that keeps `window.opener`; it is ready for a
    /// new page to adopt with [`take_popup`].
    PopupOpened {
        id: u64,
        url: String,
        foreground: bool,
    },
    /// The page closes itself (`window.close()`); the host closes its tab.
    CloseRequested,
    /// A download started or moved on.
    Download(Download),
    /// The page needs a user name and password.
    Auth(AuthRequest),
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    Dialog(JsDialog),
    /// The page's pending dialogs are void (it navigated).
    DialogsReset,
    /// The browser is gone; the view can be dropped.
    Closed,
}

/// How a page is created.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreateOptions {
    pub profile: Profile,
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

/// What a page's handlers share. Thread safe: some callbacks run on CEF's
/// IO thread.
#[derive(Clone)]
pub(crate) struct PageState {
    events: async_channel::Sender<BrowserEvent>,
    /// The host closes the page; only a close of its own is reported.
    closing: Arc<AtomicBool>,
    /// The page's gestureless popups (crate::popup).
    pub(crate) popups: Arc<Mutex<crate::popup::PopupLimiter>>,
    /// Set on a popup CEF creates for the host to adopt.
    popup: Option<crate::popup::PopupKey>,
}

impl PageState {
    pub(crate) fn new(
        popup: Option<crate::popup::PopupKey>,
    ) -> (Self, async_channel::Receiver<BrowserEvent>) {
        let (tx, rx) = async_channel::unbounded();
        let state = PageState {
            events: tx,
            closing: Arc::default(),
            popups: Arc::default(),
            popup,
        };
        (state, rx)
    }

    pub(crate) fn send(&self, event: BrowserEvent) {
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
        // ---- Popups and closing (crate::popup) ----

        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            popup_features: Option<&cef::PopupFeatures>,
            window_info: Option<&mut cef::WindowInfo>,
            client: Option<&mut Option<cef::Client>>,
            _settings: Option<&mut cef::BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // Never lets Alloy open a native window of its own.
            crate::popup::before_popup(
                &self.state,
                browser,
                popup_id,
                target_url,
                target_disposition,
                user_gesture != 0,
                popup_features,
                window_info,
                client,
            )
        }

        fn on_before_popup_aborted(
            &self,
            browser: Option<&mut Browser>,
            popup_id: ::std::os::raw::c_int,
        ) {
            let opener = browser.map(|b| b.identifier()).unwrap_or_default();
            crate::popup::aborted((opener, popup_id));
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let (Some(key), Some(browser)) = (self.state.popup, browser) {
                crate::popup::created(key, browser.clone(), &self.state);
            }
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            // Returning 0 would have CEF close the host's whole window; the
            // host tears the page down instead, when it closes the tab.
            if !self.state.closing.load(Ordering::SeqCst) {
                self.state.send(BrowserEvent::CloseRequested);
            }
            1
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.state.send(BrowserEvent::Closed);
        }
    }
}

wrap_client! {
    struct PageClient {
        display: cef::DisplayHandler,
        load: cef::LoadHandler,
        life_span: cef::LifeSpanHandler,
        // Downloads, authentication and JavaScript dialogs.
        download: cef::DownloadHandler,
        request: cef::RequestHandler,
        dialogs: cef::JsdialogHandler,
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

        fn download_handler(&self) -> Option<cef::DownloadHandler> {
            Some(self.download.clone())
        }

        fn request_handler(&self) -> Option<cef::RequestHandler> {
            Some(self.request.clone())
        }

        fn jsdialog_handler(&self) -> Option<cef::JsdialogHandler> {
            Some(self.dialogs.clone())
        }
    }
}

/// A page's client, for a new page or a popup CEF creates.
pub(crate) fn page_client(state: PageState) -> cef::Client {
    PageClient::new(
        PageDisplayHandler::new(state.clone()),
        PageLoadHandler::new(state.clone()),
        PageLifeSpanHandler::new(state.clone()),
        crate::download::PageDownloadHandler::new(state.clone()),
        crate::dialogs::PageRequestHandler::new(state.clone()),
        crate::dialogs::PageDialogHandler::new(state),
    )
}

/// A page and its native view.
pub struct BrowserView {
    browser: Browser,
    /// CEF's own view, a child of the parent given to [`BrowserView::create`].
    native: *mut c_void,
    frame: RefCell<Frame>,
    closing: Arc<AtomicBool>,
    /// A popup's first parent, made in `OnBeforePopup`, until
    /// [`BrowserView::reparent`] moves the view out of it.
    #[cfg(target_os = "macos")]
    holder: RefCell<Option<objc2::rc::Retained<objc2_app_kit::NSView>>>,
}

impl BrowserView {
    /// Creates a page inside `parent` (macOS: an `NSView*`) at `frame`, and
    /// starts loading `url`. Events go to the returned receiver.
    pub fn create(
        parent: *mut c_void,
        frame: Frame,
        url: &str,
    ) -> Result<(Self, async_channel::Receiver<BrowserEvent>), CreateError> {
        Self::create_with(parent, frame, url, &CreateOptions::default())
    }

    /// [`BrowserView::create`], in the given profile.
    pub fn create_with(
        parent: *mut c_void,
        frame: Frame,
        url: &str,
        options: &CreateOptions,
    ) -> Result<(Self, async_channel::Receiver<BrowserEvent>), CreateError> {
        if !crate::runtime::is_started() {
            return Err(CreateError::NotStarted);
        }
        if !cfg!(target_os = "macos") {
            return Err(CreateError::Unsupported);
        }
        let (state, rx) = PageState::new(None);
        let mut client = page_client(state.clone());
        let mut context = crate::profile::request_context(&options.profile);

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
            context.as_mut(),
        )
        .ok_or(CreateError::Failed)?;
        let view = Self::from_browser(browser, &state);
        view.set_frame(frame);
        Ok((view, rx))
    }

    /// The view of a browser CEF created, with its page's state.
    pub(crate) fn from_browser(browser: Browser, state: &PageState) -> Self {
        // A pointer on macOS and Windows, an X11 window id on Linux.
        #[allow(clippy::unnecessary_cast)]
        let native = browser
            .host()
            .map(|host| host.window_handle() as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
        Self {
            browser,
            native,
            frame: RefCell::new(Frame::default()),
            closing: state.closing.clone(),
            #[cfg(target_os = "macos")]
            holder: RefCell::default(),
        }
    }

    /// Keeps `parent`, the view's first superview, alive until
    /// [`BrowserView::reparent`].
    #[cfg(target_os = "macos")]
    pub(crate) fn hold(&self, parent: objc2::rc::Retained<objc2_app_kit::NSView>) {
        *self.holder.borrow_mut() = Some(parent);
    }

    /// Moves the page into another parent view (macOS: an `NSView*`), as a
    /// new tab adopts a popup ([`take_popup`]). [`BrowserView::set_frame`]
    /// places it there.
    pub fn reparent(&self, parent: *mut c_void) {
        if self.native.is_null() || parent.is_null() {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            crate::mac::view::reparent(self.native, parent);
            self.holder.borrow_mut().take();
        }
        *self.frame.borrow_mut() = Frame::default();
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

    pub fn show_dev_tools(&self) {
        if let Some(host) = self.browser.host() {
            let window_info = cef::WindowInfo::default();
            let settings = cef::BrowserSettings::default();
            host.show_dev_tools(Some(&window_info), None, Some(&settings), None);
        }
    }

    /// Closes the page. [`BrowserEvent::Closed`] follows.
    pub fn close(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
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
