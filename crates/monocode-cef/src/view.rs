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
    collections::HashMap,
    ffi::c_void,
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use cef::*;

pub use crate::{
    certs::CertificateRequest,
    dialogs::{AuthChallenge, AuthRequest, DialogKind, JsDialog},
    download::{Download, DownloadInto, DownloadState, cancel_download},
    permissions::{PermissionAnswer, PermissionRequest, reset_site_permission},
    popup::take_popup,
    profile::{Profile, forget_profile, profile_id},
};

/// What a page reports. Delivered on the main thread, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserEvent {
    /// The main frame's URL changed (navigation, history, fragment).
    Address(String),
    Title(String),
    /// The page's icon changed: the first icon URL it names.
    Favicon(String),
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
    /// The page asked for a new window without an opener to keep (a middle
    /// or ⌘-click). No window was created; the host opens the URL as a new
    /// page, and shows it at once when `foreground` (crate::popup).
    PopupRequested {
        url: String,
        user_gesture: bool,
        foreground: bool,
    },
    /// The page opened a popup that keeps `window.opener` (`window.open`,
    /// `target=_blank`); it is ready for a new page to adopt with
    /// [`take_popup`]. Not adopted in time, it closes.
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
    /// The page's renderer process ended (a crash, or it was killed). The
    /// page shows nothing until it loads again.
    RenderProcessGone,
    /// A find-in-page result ([`BrowserView::find`]). `active_ordinal` counts
    /// from 1; several updates may arrive for one search, the last one final.
    FindResult {
        count: i32,
        active_ordinal: i32,
        final_update: bool,
    },
    /// The page took the keyboard (a click in it, or [`BrowserView::set_focus`]).
    Focused,
    /// The user chose "Send to Agent" in the page's context menu, right-
    /// clicking at `(x, y)` in the view (points, page zoom not applied).
    GrabAt {
        x: i32,
        y: i32,
    },
    /// The page asks for permissions (crate::permissions).
    PermissionRequested(PermissionRequest),
    /// A permission prompt is answered or void (the page navigated); the
    /// host drops it from its queue.
    PermissionGone(u64),
    /// A navigation waits on a certificate Chromium does not trust
    /// (crate::certs).
    CertificateError(CertificateRequest),
    /// The page's preferred size, while [`BrowserView::set_auto_resize`]
    /// is on (an extension's popup), in points.
    AutoResize {
        width: i32,
        height: i32,
    },
}

/// How a page is created.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreateOptions {
    pub profile: Profile,
    /// Scripts may use the clipboard without a gesture (an extension's
    /// offscreen document, which clears what it copied).
    pub clipboard: bool,
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
    /// A popup's opener: its downloads may be the opener's (crate::download).
    opener: Option<Arc<PageState>>,
    /// In a top-level window CEF owns ([`BrowserView::create_hidden`]):
    /// CEF closes the window itself.
    top_level: bool,
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
            opener: None,
            top_level: false,
        };
        (state, rx)
    }

    /// A popup's state, opened by `opener`.
    pub(crate) fn opened_by(mut self, opener: &PageState) -> Self {
        self.opener = Some(Arc::new(opener.clone()));
        self
    }

    /// The page of browser `browser` with this state, then the pages that
    /// opened it, nearest first: `(browser id, state)`.
    pub(crate) fn lineage(&self, browser: i32) -> Vec<(i32, &PageState)> {
        let mut pages = vec![(browser, self)];
        let mut at = self;
        while let (Some((opener_id, _)), Some(opener)) = (at.popup, at.opener.as_deref()) {
            pages.push((opener_id, opener));
            at = opener;
        }
        pages
    }

    pub(crate) fn send(&self, event: BrowserEvent) {
        // Unbounded: never blocks a CEF callback.
        let _ = self.events.try_send(event);
    }
}

fn string(value: Option<&CefString>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

/// A browser between `OnAfterCreated` and `OnBeforeClose`.
struct Live {
    browser: Browser,
    closing: Arc<AtomicBool>,
}

thread_local! {
    /// Every browser that exists, pages and popups, by id: what must close
    /// before CEF shuts down.
    static LIVE: RefCell<HashMap<i32, Live>> = RefCell::new(HashMap::new());
}

/// How many browsers exist ([`close_all`]).
pub(crate) fn live() -> usize {
    LIVE.with(|l| l.borrow().len())
}

/// Closes every browser, for CEF to shut down: `OnBeforeClose` follows for
/// each as the message loop runs. A page's close waits for its view to go
/// (`DoClose` keeps the host's window), so the view leaves its parent here.
pub(crate) fn close_all() {
    let all: Vec<(i32, Browser, bool)> = LIVE.with(|l| {
        (l.borrow().iter())
            .map(|(id, live)| {
                let asked = live.closing.swap(true, Ordering::SeqCst);
                (*id, live.browser.clone(), asked)
            })
            .collect()
    });
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| {
        for (id, browser, asked) in all {
            crate::capture::forget(id);
            let Some(host) = browser.host() else { continue };
            if !asked {
                host.close_browser(1);
            }
            crate::mac::view::detach(host.window_handle());
        }
    });
    #[cfg(not(target_os = "macos"))]
    for (id, browser, asked) in all {
        crate::capture::forget(id);
        if let Some(host) = browser.host().filter(|_| !asked) {
            host.close_browser(1);
        }
    }
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

        fn on_favicon_urlchange(
            &self,
            _browser: Option<&mut Browser>,
            icon_urls: Option<&mut CefStringList>,
        ) {
            // The list is CEF's; a borrowed copy of the handle reads it.
            let first = icon_urls.and_then(|list| {
                let raw: *mut sys::_cef_string_list_t = list.into();
                CefStringList::from(raw).into_iter().next()
            });
            if let Some(url) = first.filter(|url| !url.is_empty()) {
                self.state.send(BrowserEvent::Favicon(url));
            }
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.state.send(BrowserEvent::Progress(progress));
        }

        fn on_auto_resize(
            &self,
            _browser: Option<&mut Browser>,
            new_size: Option<&cef::Size>,
        ) -> ::std::os::raw::c_int {
            let Some(size) = new_size else { return 0 };
            self.state.send(BrowserEvent::AutoResize {
                width: size.width,
                height: size.height,
            });
            1
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
            no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let scriptable = no_javascript_access.is_none_or(|no| *no == 0);
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
                scriptable,
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
            let Some(browser) = browser else { return };
            let live = Live {
                browser: browser.clone(),
                closing: self.state.closing.clone(),
            };
            LIVE.with(|l| l.borrow_mut().insert(browser.identifier(), live));
            if let Some(key) = self.state.popup {
                crate::popup::created(key, browser.clone(), &self.state);
            }
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            // Returning 0 would have CEF close the host's whole window; the
            // host tears the page down instead, when it closes the tab.
            if !self.state.closing.load(Ordering::SeqCst) {
                self.state.send(BrowserEvent::CloseRequested);
            }
            // CEF's own window: CEF closes it, and the browser with it.
            if self.state.top_level {
                return 0;
            }
            1
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let id = browser.identifier();
                // Dropped outside the borrow.
                let live = LIVE.with(|l| l.borrow_mut().remove(&id));
                drop(live);
                crate::popup::opener_closed(id);
            }
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

// One handler per concern: CEF's client takes them all at once, so its
// generated constructor has a parameter for each.
mod client {
    #![allow(clippy::too_many_arguments)]
    use super::*;

    wrap_client! {
        pub(super) struct PageClient {
            display: cef::DisplayHandler,
            load: cef::LoadHandler,
            life_span: cef::LifeSpanHandler,
            find: cef::FindHandler,
            focus: cef::FocusHandler,
            // Downloads, authentication and JavaScript dialogs.
            download: cef::DownloadHandler,
            request: cef::RequestHandler,
            dialogs: cef::JsdialogHandler,
            // "Send to Agent" (context.md K49).
            context_menu: cef::ContextMenuHandler,
            // Site permissions (K63), and an agent's downloads in a row (K50).
            permission: cef::PermissionHandler,
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

            fn download_handler(&self) -> Option<cef::DownloadHandler> {
                Some(self.download.clone())
            }

            fn request_handler(&self) -> Option<cef::RequestHandler> {
                Some(self.request.clone())
            }

            fn jsdialog_handler(&self) -> Option<cef::JsdialogHandler> {
                Some(self.dialogs.clone())
            }

            fn context_menu_handler(&self) -> Option<cef::ContextMenuHandler> {
                Some(self.context_menu.clone())
            }

            fn permission_handler(&self) -> Option<cef::PermissionHandler> {
                Some(self.permission.clone())
            }
        }
    }
}
use client::PageClient;

fn browser_settings(options: &CreateOptions) -> cef::BrowserSettings {
    let mut settings = cef::BrowserSettings::default();
    if options.clipboard {
        settings.javascript_access_clipboard = cef::State::ENABLED;
    }
    settings
}

/// A page's client, for a new page or a popup CEF creates.
pub(crate) fn page_client(state: PageState) -> cef::Client {
    PageClient::new(
        PageDisplayHandler::new(state.clone()),
        PageLoadHandler::new(state.clone()),
        PageLifeSpanHandler::new(state.clone()),
        PageFindHandler::new(state.clone()),
        PageFocusHandler::new(state.clone()),
        crate::download::PageDownloadHandler::new(state.clone()),
        crate::dialogs::PageRequestHandler::new(state.clone()),
        crate::dialogs::PageDialogHandler::new(state.clone()),
        crate::context_menu::PageContextMenuHandler::new(state.clone()),
        crate::permissions::PagePermissionHandler::new(state),
    )
}

/// A page and its native view.
/// An Edit menu command, for [`BrowserView::edit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

pub struct BrowserView {
    /// Let go of only while CEF runs ([`Drop`]).
    browser: ManuallyDrop<Browser>,
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
        let settings = browser_settings(options);
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

    /// A page no one sees: in a top-level window of CEF's own that never
    /// shows (an extension's background or offscreen document, K69). It
    /// runs like any page, with CDP; [`BrowserView::set_frame`] and
    /// visibility do nothing.
    pub fn create_hidden(
        url: &str,
        options: &CreateOptions,
    ) -> Result<(Self, async_channel::Receiver<BrowserEvent>), CreateError> {
        if !crate::runtime::is_started() {
            return Err(CreateError::NotStarted);
        }
        let (mut state, rx) = PageState::new(None);
        state.top_level = true;
        let mut client = page_client(state.clone());
        let mut context = crate::profile::request_context(&options.profile);
        let window_info = cef::WindowInfo {
            runtime_style: cef::RuntimeStyle::ALLOY,
            bounds: cef::Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            hidden: 1,
            window_name: "Monocode extension".into(),
            ..Default::default()
        };
        let settings = browser_settings(options);
        let browser = cef::browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&url.into()),
            Some(&settings),
            None,
            context.as_mut(),
        )
        .ok_or(CreateError::Failed)?;
        let mut view = Self::from_browser(browser, &state);
        // Not ours to place: CEF owns the window.
        view.native = std::ptr::null_mut();
        Ok((view, rx))
    }

    /// Reports the page's preferred size between `min` and `max` points as
    /// [`BrowserEvent::AutoResize`] (Chrome's popups size to their page).
    pub fn set_auto_resize(&self, min: (i32, i32), max: (i32, i32)) {
        if let Some(host) = self.browser.host() {
            let min = cef::Size {
                width: min.0,
                height: min.1,
            };
            let max = cef::Size {
                width: max.0,
                height: max.1,
            };
            host.set_auto_resize_enabled(1, Some(&min), Some(&max));
        }
    }

    /// The URL of the page's main frame now.
    pub fn url(&self) -> String {
        self.browser
            .main_frame()
            .map(|f| CefString::from(&f.url()).to_string())
            .unwrap_or_default()
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
            browser: ManuallyDrop::new(browser),
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

    /// Opens the page's DevTools in a window of its own, or brings it to the
    /// front. CEF makes DevTools Chrome-style, which cannot sit inside a host
    /// view on macOS (context.md K18).
    pub fn show_dev_tools(&self) {
        if let Some(host) = self.browser.host() {
            let window_info = cef::WindowInfo::default();
            let settings = cef::BrowserSettings::default();
            host.show_dev_tools(Some(&window_info), None, Some(&settings), None);
        }
    }

    pub fn close_dev_tools(&self) {
        if let Some(host) = self.browser.host() {
            host.close_dev_tools();
        }
    }

    pub fn has_dev_tools(&self) -> bool {
        self.browser
            .host()
            .is_some_and(|host| host.has_dev_tools() != 0)
    }

    /// Mutes the page's sound, or lets it play again. Chromium keeps it for
    /// the page's life, across navigations.
    pub fn set_audio_muted(&self, muted: bool) {
        if let Some(host) = self.browser.host() {
            host.set_audio_muted(muted as _);
        }
    }

    pub fn is_audio_muted(&self) -> bool {
        self.browser
            .host()
            .is_some_and(|host| host.is_audio_muted() != 0)
    }

    /// Runs an Edit menu command in the frame that has the keyboard (the
    /// main frame when none has). On macOS Chromium sends ⌘C/V/X/A/Z the
    /// page leaves alone to the app's menu, as Chrome's Edit menu would
    /// send `copy:` and the like to the page; the app's menu runs its own
    /// actions instead, so the pane hands them back here.
    pub fn edit(&self, command: EditCommand) {
        let Some(frame) = self
            .browser
            .focused_frame()
            .or_else(|| self.browser.main_frame())
        else {
            return;
        };
        match command {
            EditCommand::Undo => frame.undo(),
            EditCommand::Redo => frame.redo(),
            EditCommand::Cut => frame.cut(),
            EditCommand::Copy => frame.copy(),
            EditCommand::Paste => frame.paste(),
            EditCommand::SelectAll => frame.select_all(),
        }
    }

    /// Prints the page: Chromium's print, which on macOS shows the system's
    /// print panel as a sheet on the page's window.
    pub fn print(&self) {
        if let Some(host) = self.browser.host() {
            host.print();
        }
    }

    /// Asks for a PNG of what the page shows now, at the screen's scale.
    /// `done` runs on the main thread; it is dropped without running if the
    /// page closes first.
    pub fn capture(&self, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
        crate::capture::capture(&self.browser, Box::new(done));
    }

    /// The page's next download goes into `dir` (made if missing) instead
    /// of `~/Downloads`, while the returned value lives.
    pub fn download_next_into(&self, dir: std::path::PathBuf) -> DownloadInto {
        crate::download::download_into(self.browser.identifier(), dir)
    }

    /// Chromium's id of the page's browser, unique for the app's run.
    pub fn browser_id(&self) -> i32 {
        self.browser.identifier()
    }

    /// Attaches `sink` to the page's raw DevTools messages (crate::devtools).
    /// One attachment per page is expected; dropping it detaches.
    pub fn devtools_attach(&self, sink: crate::DevToolsSink) -> Option<crate::DevToolsAttachment> {
        crate::devtools::attach(&self.browser, sink)
    }

    /// Sends one raw CDP message to the page. False when Chromium refused it.
    pub fn devtools_send(&self, message: &[u8]) -> bool {
        crate::devtools::send(&self.browser, message)
    }

    /// Closes the page. [`BrowserEvent::Closed`] follows.
    pub fn close(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
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
        // After CEF shut down (the app quits) no CEF call may follow, not
        // even a release: the reference is left to the exit.
        if !crate::runtime::is_started() {
            return;
        }
        self.close();
        // SAFETY: dropped once, here; the field is not used again.
        unsafe { ManuallyDrop::drop(&mut self.browser) };
    }
}
