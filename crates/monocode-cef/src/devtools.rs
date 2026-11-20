//! A page's DevTools, docked: a second Chromium view inside a parent the host
//! gives, like the page itself (context.md K18).
//!
//! DevTools is a browser of its own. CEF makes it inside `ShowDevTools`, and
//! hands it over in the client's `OnAfterCreated`; until then the frame and
//! visibility the host sets wait in [`Shared`].

use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

use cef::*;

use crate::view::Frame;

/// What DevTools reports. Delivered on the main thread.
#[derive(Clone, Debug, PartialEq)]
pub enum DevToolsEvent {
    /// DevTools closed, from its own UI or with its page; drop the view.
    Closed,
}

#[derive(Default)]
struct Shared {
    browser: Option<Browser>,
    /// CEF's own view, a child of the parent given to `show_dev_tools`.
    native: usize,
    frame: Option<Frame>,
    visible: bool,
    closed: bool,
}

impl Shared {
    fn native(&self) -> Option<*mut c_void> {
        (self.native != 0 && !self.closed).then_some(self.native as *mut c_void)
    }
}

/// Locks, surviving a panic in another holder: the state stays usable.
fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

wrap_life_span_handler! {
    struct DevToolsLifeSpanHandler {
        shared: Arc<Mutex<Shared>>,
        events: async_channel::Sender<DevToolsEvent>,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            // A pointer on macOS and Windows, an X11 window id on Linux.
            #[allow(clippy::unnecessary_cast)]
            let native = browser
                .host()
                .map(|host| host.window_handle() as *mut c_void)
                .unwrap_or(std::ptr::null_mut());
            let mut shared = lock(&self.shared);
            shared.browser = Some(browser.clone());
            shared.native = native as usize;
            // What the host set while CEF was still making it.
            let (frame, visible) = (shared.frame, shared.visible);
            drop(shared);
            if let Some(frame) = frame {
                place(native, frame);
            }
            show(native, browser, visible);
        }

        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            _popup_id: ::std::os::raw::c_int,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            _window_info: Option<&mut cef::WindowInfo>,
            _client: Option<&mut Option<cef::Client>>,
            _settings: Option<&mut cef::BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // No native windows of Alloy's own, from DevTools either.
            1
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let mut shared = lock(&self.shared);
            shared.closed = true;
            shared.native = 0;
            shared.browser = None;
            drop(shared);
            if let Some(browser) = browser {
                crate::capture::forget(browser.identifier());
            }
            let _ = self.events.try_send(DevToolsEvent::Closed);
        }
    }
}

wrap_client! {
    struct DevToolsClient {
        life_span: cef::LifeSpanHandler,
    }

    impl Client {
        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span.clone())
        }
    }
}

fn place(native: *mut c_void, frame: Frame) {
    #[cfg(target_os = "macos")]
    crate::mac::view::set_frame(native, frame);
    #[cfg(not(target_os = "macos"))]
    let _ = (native, frame);
}

fn show(native: *mut c_void, browser: &Browser, visible: bool) {
    #[cfg(target_os = "macos")]
    crate::mac::view::set_hidden(native, !visible);
    #[cfg(not(target_os = "macos"))]
    let _ = native;
    if let Some(host) = browser.host() {
        host.was_hidden(!visible as _);
    }
}

/// A page's docked DevTools. Dropping it closes DevTools.
pub struct DevToolsView {
    /// The inspected page, which owns DevTools in CEF.
    page: Browser,
    shared: Arc<Mutex<Shared>>,
}

impl DevToolsView {
    /// Opens `page`'s DevTools inside `parent` (macOS: an `NSView*`) at
    /// `frame`. Fails if the page has DevTools open already.
    pub(crate) fn open(
        page: &Browser,
        parent: *mut c_void,
        frame: Frame,
    ) -> Result<(Self, async_channel::Receiver<DevToolsEvent>), crate::CreateError> {
        let host = page.host().ok_or(crate::CreateError::Failed)?;
        if host.has_dev_tools() != 0 {
            return Err(crate::CreateError::Failed);
        }
        let (tx, rx) = async_channel::unbounded();
        let shared = Arc::new(Mutex::new(Shared {
            frame: Some(frame),
            visible: true,
            ..Shared::default()
        }));
        let mut client = DevToolsClient::new(DevToolsLifeSpanHandler::new(shared.clone(), tx));
        let bounds = cef::Rect {
            x: 0,
            y: 0,
            width: frame.width.round().max(1.0) as i32,
            height: frame.height.round().max(1.0) as i32,
        };
        let window_info = cef::WindowInfo {
            // A native parent needs Alloy style on macOS, as for pages.
            runtime_style: cef::RuntimeStyle::ALLOY,
            ..Default::default()
        }
        .set_as_child(parent as _, &bounds);
        let settings = cef::BrowserSettings::default();
        host.show_dev_tools(Some(&window_info), Some(&mut client), Some(&settings), None);
        Ok((
            Self {
                page: page.clone(),
                shared,
            },
            rx,
        ))
    }

    /// Moves and sizes DevTools inside its parent.
    pub fn set_frame(&self, frame: Frame) {
        let mut shared = lock(&self.shared);
        if shared.frame == Some(frame) {
            return;
        }
        shared.frame = Some(frame);
        let native = shared.native();
        drop(shared);
        if let Some(native) = native {
            place(native, frame);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        let mut shared = lock(&self.shared);
        if shared.visible == visible {
            return;
        }
        shared.visible = visible;
        let (native, browser) = (shared.native(), shared.browser.clone());
        drop(shared);
        if let (Some(native), Some(browser)) = (native, browser) {
            show(native, &browser, visible);
        }
    }

    /// Gives DevTools keyboard focus, or takes it away.
    pub fn set_focus(&self, focused: bool) {
        let browser = lock(&self.shared).browser.clone();
        if let Some(host) = browser.and_then(|b| b.host()) {
            host.set_focus(focused as _);
        }
    }

    /// A PNG of what DevTools shows, as [`crate::BrowserView::capture`].
    pub fn capture(&self, done: crate::capture::CaptureDone) {
        let browser = lock(&self.shared).browser.clone();
        match browser {
            Some(browser) => crate::capture::capture(&browser, done),
            None => done(Err("DevTools is not open".into())),
        }
    }

    /// Whether DevTools closed ([`DevToolsEvent::Closed`] follows, or came).
    pub fn is_closed(&self) -> bool {
        lock(&self.shared).closed
    }

    /// Closes DevTools. [`DevToolsEvent::Closed`] follows.
    pub fn close(&self) {
        if self.is_closed() {
            return;
        }
        if let Some(host) = self.page.host() {
            host.close_dev_tools();
        }
    }
}

impl Drop for DevToolsView {
    fn drop(&mut self) {
        self.close();
    }
}
