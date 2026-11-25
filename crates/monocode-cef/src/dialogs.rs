//! What a page asks the user: HTTP authentication (`CefRequestHandler`)
//! and JavaScript's `alert`, `confirm`, `prompt` and `beforeunload`
//! (`CefJSDialogHandler`). The host shows its own dialogs and answers
//! through the request; one dropped unanswered declines.
//!
//! Alloy's own JavaScript dialogs are Chrome's tab-modal Views dialogs, which
//! need a Views host the native-parent page lacks, and would sit in a window
//! of their own above the app. Our handler replaces them on every platform.

use std::sync::{Arc, Mutex};

use cef::*;

use crate::view::{BrowserEvent, PageState};

/// A CEF callback that may be answered from any thread, as CEF allows for
/// these continuation callbacks.
pub(crate) struct AnyThread<T>(pub(crate) T);
// SAFETY: CEF's continuation callbacks are reference counted atomically and
// may be executed on any thread.
unsafe impl<T> Send for AnyThread<T> {}

/// What an authentication challenge says. The credentials never pass
/// through here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthChallenge {
    pub host: String,
    pub port: u16,
    /// May be empty.
    pub realm: String,
    /// The challenge is a proxy's, not the site's.
    pub proxy: bool,
}

/// A page waits for a user name and password. Answer once; dropping the
/// last clone unanswered cancels the request.
#[derive(Clone)]
pub struct AuthRequest {
    pub challenge: AuthChallenge,
    reply: Arc<AuthReply>,
}

struct AuthReply(Mutex<Option<AnyThread<AuthCallback>>>);

impl AuthReply {
    fn take(&self) -> Option<AuthCallback> {
        self.0.lock().ok()?.take().map(|c| c.0)
    }
}

impl Drop for AuthReply {
    fn drop(&mut self) {
        if let Some(callback) = self.take() {
            if !crate::runtime::is_started() {
                // CEF shut down: no call may follow, not even a release.
                std::mem::forget(callback);
                return;
            }
            callback.cancel();
        }
    }
}

impl AuthRequest {
    pub fn sign_in(&self, user: &str, password: &str) {
        if let Some(callback) = self.reply.take() {
            callback.cont(Some(&user.into()), Some(&password.into()));
        }
    }

    pub fn cancel(&self) {
        if let Some(callback) = self.reply.take() {
            callback.cancel();
        }
    }
}

impl std::fmt::Debug for AuthRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRequest")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl PartialEq for AuthRequest {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    /// `window.prompt`, with its default text.
    Prompt(String),
    /// Leaving or reloading a page whose `beforeunload` objects.
    BeforeUnload {
        reload: bool,
    },
}

/// A JavaScript dialog the page waits on. Answer once; dropping the last
/// clone unanswered declines it (Cancel; leave the page alone).
#[derive(Clone)]
pub struct JsDialog {
    pub kind: DialogKind,
    /// The page's origin, for the dialog's title. May be empty.
    pub origin: String,
    pub message: String,
    reply: Arc<DialogReply>,
}

struct DialogReply(Mutex<Option<AnyThread<JsdialogCallback>>>);

impl DialogReply {
    fn take(&self) -> Option<JsdialogCallback> {
        self.0.lock().ok()?.take().map(|c| c.0)
    }
}

impl Drop for DialogReply {
    fn drop(&mut self) {
        if let Some(callback) = self.take() {
            if !crate::runtime::is_started() {
                std::mem::forget(callback);
                return;
            }
            callback.cont(0, None);
        }
    }
}

impl JsDialog {
    /// OK (or Leave), with the prompt's text.
    pub fn accept(&self, text: Option<&str>) {
        if let Some(callback) = self.reply.take() {
            callback.cont(1, text.map(CefString::from).as_ref());
        }
    }

    pub fn decline(&self) {
        if let Some(callback) = self.reply.take() {
            callback.cont(0, None);
        }
    }
}

impl JsDialog {
    /// A dialog no page waits on, for the host's tests: answering it does
    /// nothing.
    pub fn detached(kind: DialogKind, origin: &str, message: &str) -> Self {
        JsDialog {
            kind,
            origin: origin.to_owned(),
            message: message.to_owned(),
            reply: Arc::new(DialogReply(Mutex::new(None))),
        }
    }
}

impl std::fmt::Debug for JsDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsDialog")
            .field("kind", &self.kind)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl PartialEq for JsDialog {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

fn string(value: Option<&CefString>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

// The page's request handler. Other request callbacks join this one: a
// client has a single request handler.
wrap_request_handler! {
    pub(crate) struct PageRequestHandler {
        state: PageState,
    }

    impl RequestHandler {
        // A click to another site in a page that peeks stops here; the
        // host shows it in Peek (BrowserView::set_peek_host).
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            user_gesture: ::std::os::raw::c_int,
            is_redirect: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let (Some(browser), Some(frame), Some(request)) = (browser, frame, request) else {
                return 0;
            };
            if user_gesture == 0 || is_redirect != 0 || frame.is_main() == 0 {
                return 0;
            }
            let Some(home) = peek_host(browser.identifier()) else {
                return 0;
            };
            let url = CefString::from(&request.url()).to_string();
            let method = CefString::from(&request.method()).to_string();
            if !method.eq_ignore_ascii_case("GET") || !peeks(&home, &url) {
                return 0;
            }
            self.state.send(BrowserEvent::PeekRequested(url));
            1
        }

        // A middle or ⌘-click on a link opens a new page (crate::popup).
        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            crate::popup::open_url_from_tab(
                &self.state,
                target_url,
                target_disposition,
                user_gesture != 0,
            )
        }

        // Called on CEF's IO thread; the event channel is thread safe.
        fn auth_credentials(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            is_proxy: ::std::os::raw::c_int,
            host: Option<&CefString>,
            port: ::std::os::raw::c_int,
            realm: Option<&CefString>,
            _scheme: Option<&CefString>,
            callback: Option<&mut AuthCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let request = AuthRequest {
                challenge: AuthChallenge {
                    host: string(host),
                    port: u16::try_from(port).unwrap_or_default(),
                    realm: string(realm),
                    proxy: is_proxy != 0,
                },
                reply: Arc::new(AuthReply(Mutex::new(Some(AnyThread(callback.clone()))))),
            };
            self.state.send(BrowserEvent::Auth(request));
            1
        }

        // A certificate the network stack would not trust (crate::certs).
        fn on_certificate_error(
            &self,
            _browser: Option<&mut Browser>,
            cert_error: Errorcode,
            request_url: Option<&CefString>,
            ssl_info: Option<&mut Sslinfo>,
            callback: Option<&mut cef::Callback>,
        ) -> ::std::os::raw::c_int {
            crate::certs::certificate_error(
                &self.state,
                cert_error.get_raw(),
                &string(request_url),
                ssl_info,
                callback,
            )
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: ::std::os::raw::c_int,
            _error_string: Option<&CefString>,
        ) {
            self.state.send(BrowserEvent::RenderProcessGone);
        }
    }
}

wrap_jsdialog_handler! {
    pub(crate) struct PageDialogHandler {
        state: PageState,
    }

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let kind = if dialog_type == JsdialogType::CONFIRM {
                DialogKind::Confirm
            } else if dialog_type == JsdialogType::PROMPT {
                DialogKind::Prompt(string(default_prompt_text))
            } else {
                DialogKind::Alert
            };
            self.state.send(BrowserEvent::Dialog(JsDialog {
                kind,
                origin: string(origin_url),
                message: string(message_text),
                reply: Arc::new(DialogReply(Mutex::new(Some(AnyThread(callback.clone()))))),
            }));
            1
        }

        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            message_text: Option<&CefString>,
            is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            self.state.send(BrowserEvent::Dialog(JsDialog {
                kind: DialogKind::BeforeUnload {
                    reload: is_reload != 0,
                },
                origin: String::new(),
                message: string(message_text),
                reply: Arc::new(DialogReply(Mutex::new(Some(AnyThread(callback.clone()))))),
            }));
            1
        }

        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            // A navigation voided the page's pending dialogs.
            self.state.send(BrowserEvent::DialogsReset);
        }
    }
}

/// The site each peeking page keeps to, by browser id.
static PEEK_HOSTS: std::sync::LazyLock<Mutex<std::collections::HashMap<i32, String>>> =
    std::sync::LazyLock::new(Default::default);

pub(crate) fn set_peek_host(browser: i32, host: Option<String>) {
    if let Ok(mut hosts) = PEEK_HOSTS.lock() {
        match host {
            Some(host) => hosts.insert(browser, host),
            None => hosts.remove(&browser),
        };
    }
}

fn peek_host(browser: i32) -> Option<String> {
    PEEK_HOSTS.lock().ok()?.get(&browser).cloned()
}

/// Whether `url` is a web page on another site than `home`; `www.` and the
/// scheme do not count.
pub(crate) fn peeks(home: &str, url: &str) -> bool {
    fn host(url: &str) -> Option<String> {
        let (scheme, rest) = url.split_once("://")?;
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return None;
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .to_ascii_lowercase();
        Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
    }
    let home = home.trim().to_ascii_lowercase();
    let home = home.strip_prefix("www.").unwrap_or(&home);
    host(url).is_some_and(|host| !host.is_empty() && host != home)
}

#[cfg(test)]
mod peek_tests {
    #[test]
    fn only_other_web_sites_peek() {
        assert!(super::peeks("github.com", "https://example.com/a"));
        assert!(!super::peeks("github.com", "https://www.github.com/x"));
        assert!(!super::peeks("www.github.com", "http://github.com:443/"));
        assert!(!super::peeks("github.com", "mailto:a@b.c"));
        assert!(super::peeks("github.com", "https://gist.github.com/"));
    }
}
