//! The native half of a browser pane: a WKWebView kept under a GPUI element.
//!
//! Its own type on purpose. `monos/html.rs` hosts a web view too, but as a
//! sandbox with no network and no stored data; a browser page has both, and
//! the two must never come to share a configuration.
use super::BrowserView;
use gpui::{AsyncApp, WeakEntity};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSView, NSWindowOrderingMode};
use objc2_foundation::{
    NSError, NSObject, NSObjectProtocol, NSRect, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKWebView,
    WKWebViewConfiguration,
};

/// The way back from AppKit to the pane. AppKit may call while GPUI is in
/// the middle of an update, so nothing is done in the call itself.
#[derive(Clone)]
pub(super) struct Link {
    pub app: AsyncApp,
    pub view: WeakEntity<BrowserView>,
}

/// How many calls came from AppKit, and how many reached the pane.
pub(super) static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(super) static REACHED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(super) static FAILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Link {
    pub(super) fn later(
        &self,
        then: impl FnOnce(&mut BrowserView, &mut gpui::Context<BrowserView>) + 'static,
    ) {
        use std::sync::atomic::Ordering::Relaxed;
        CALLS.fetch_add(1, Relaxed);
        let view = self.view.clone();
        self.app
            .spawn(async move |cx| {
                match view.update(cx, then) {
                    Ok(()) => REACHED.fetch_add(1, Relaxed),
                    Err(_) => FAILED.fetch_add(1, Relaxed),
                };
            })
            .detach();
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[name = "MonocodeBrowserClip"]
    #[thread_kind = MainThreadOnly]
    struct Clip;
    unsafe impl NSObjectProtocol for Clip {}
    impl Clip {
        #[unsafe(method(isFlipped))]
        fn flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    #[unsafe(super(WKWebView))]
    #[name = "MonocodeBrowserPage"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Link]
    struct Page;
    unsafe impl NSObjectProtocol for Page {}
    impl Page {
        /// A click in the page makes it first responder without GPUI hearing
        /// of it; the pane has to know to take its own focus.
        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let became: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if became {
                self.ivars().later(|view, cx| view.page_focused(cx));
            }
            became
        }
    }
);

/// What the navigation delegate keeps: the way back to the pane, and the
/// object downloads report to (a download only holds it weakly).
pub(super) struct Nav {
    link: Link,
    downloads: Retained<super::native_ui::Downloads>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MonocodeBrowserNavigation"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Nav]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        unsafe fn decide(
            &self,
            _: &WKWebView,
            _: &WKNavigationAction,
            reply: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // A link meant for a new window is allowed too: WebKit then asks
            // the UI delegate for that window, which opens a page for it.
            reply.call((WKNavigationActionPolicy::Allow,));
        }
        /// What cannot be shown is saved: an archive, an installer, a file
        /// the server marks as an attachment.
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        unsafe fn respond(
            &self,
            _: &WKWebView,
            response: &objc2_web_kit::WKNavigationResponse,
            reply: &block2::DynBlock<dyn Fn(objc2_web_kit::WKNavigationResponsePolicy)>,
        ) {
            reply.call((if unsafe { response.canShowMIMEType() } {
                objc2_web_kit::WKNavigationResponsePolicy::Allow
            } else {
                objc2_web_kit::WKNavigationResponsePolicy::Download
            },));
        }
        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        unsafe fn response_became_download(
            &self,
            _: &WKWebView,
            _: &objc2_web_kit::WKNavigationResponse,
            download: &objc2_web_kit::WKDownload,
        ) {
            unsafe {
                download.setDelegate(Some(ProtocolObject::from_ref(&*self.ivars().downloads)))
            };
        }
        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        unsafe fn action_became_download(
            &self,
            _: &WKWebView,
            _: &WKNavigationAction,
            download: &objc2_web_kit::WKDownload,
        ) {
            unsafe {
                download.setDelegate(Some(ProtocolObject::from_ref(&*self.ivars().downloads)))
            };
        }
        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        unsafe fn challenged(
            &self,
            web: &WKWebView,
            challenge: &objc2_foundation::NSURLAuthenticationChallenge,
            done: &block2::DynBlock<
                dyn Fn(
                    objc2_foundation::NSURLSessionAuthChallengeDisposition,
                    *mut objc2_foundation::NSURLCredential,
                ),
            >,
        ) {
            super::native_ui::answer_challenge(web, challenge, done);
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        unsafe fn stopped(&self, _: &WKWebView) {
            self.ivars().link.later(|view, cx| view.page_stopped(cx));
        }
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        unsafe fn started(&self, _: &WKWebView, _: Option<&WKNavigation>) {
            self.ivars().link.later(|view, cx| view.page_changed(cx));
        }
        #[unsafe(method(webView:didCommitNavigation:))]
        unsafe fn committed(&self, _: &WKWebView, _: Option<&WKNavigation>) {
            self.ivars().link.later(|view, cx| view.page_changed(cx));
        }
        #[unsafe(method(webView:didFinishNavigation:))]
        unsafe fn finished(&self, _: &WKWebView, _: Option<&WKNavigation>) {
            self.ivars().link.later(|view, cx| view.page_changed(cx));
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        unsafe fn failed(&self, _: &WKWebView, _: Option<&WKNavigation>, error: &NSError) {
            let message = error.localizedDescription().to_string();
            self.ivars()
                .link
                .later(|view, cx| view.page_failed(message, cx));
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        unsafe fn failed_early(&self, _: &WKWebView, _: Option<&WKNavigation>, error: &NSError) {
            // Not failures: -999, the load was replaced by another; and
            // WebKit's 102, the load became a download instead of a page.
            let interrupted =
                error.code() == 102 && error.domain().to_string() == "WebKitErrorDomain";
            if error.code() == -999 || interrupted {
                return;
            }
            let message = error.localizedDescription().to_string();
            self.ivars()
                .link
                .later(|view, cx| view.page_failed(message, cx));
        }
    }
);

/// What the page says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PageState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub back: bool,
    pub forward: bool,
}

pub(super) struct Hosted {
    clip: Retained<Clip>,
    web: Retained<Page>,
    _delegate: Retained<Delegate>,
    _ui: Retained<super::native_ui::Ui>,
    _downloads: Retained<super::native_ui::Downloads>,
}

/// The `Version/… Safari/…` part of Safari's user agent. A web view's own
/// user agent stops before it, and some sign-in pages refuse one that does.
fn safari_suffix() -> String {
    let version = unsafe {
        objc2_foundation::NSBundle::bundleWithPath(&NSString::from_str("/Applications/Safari.app"))
            .and_then(|bundle| {
                let key = NSString::from_str("CFBundleShortVersionString");
                let value: Option<Retained<NSString>> =
                    msg_send![&*bundle, objectForInfoDictionaryKey: &*key];
                value
            })
            .map(|version| version.to_string())
    }
    .filter(|version| {
        !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
    })
    .unwrap_or_else(|| "18.0".into());
    format!("Version/{version} Safari/605.1.15")
}

impl Hosted {
    pub(super) fn new(
        mtm: MainThreadMarker,
        link: Link,
        frame: NSRect,
        web_frame: NSRect,
        user_agent: Option<&str>,
        url: &str,
    ) -> Self {
        unsafe {
            let clip: Retained<Clip> = msg_send![Clip::alloc(mtm), initWithFrame: frame];
            clip.setWantsLayer(true);
            clip.setClipsToBounds(true);
            // The default configuration keeps the app's persistent store:
            // cookies and site data stay between launches.
            let config = WKWebViewConfiguration::new(mtm);
            // What the page logs and requests is kept from its first line
            // on, for the agent tools that ask about it later.
            let hooks = objc2_web_kit::WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                objc2_web_kit::WKUserScript::alloc(mtm),
                &NSString::from_str(super::agent::HOOKS),
                objc2_web_kit::WKUserScriptInjectionTime::AtDocumentStart,
                false,
            );
            config.userContentController().addUserScript(&hooks);
            config.setApplicationNameForUserAgent(Some(&NSString::from_str(&safari_suffix())));
            let web = Page::alloc(mtm).set_ivars(link.clone());
            let web: Retained<Page> =
                msg_send![super(web), initWithFrame: web_frame, configuration: &*config];
            web.setInspectable(true);
            if let Some(agent) = user_agent {
                web.setCustomUserAgent(Some(&NSString::from_str(agent)));
            }
            let downloads = super::native_ui::Downloads::new(mtm, link.clone());
            let ui = super::native_ui::Ui::new(mtm, link.clone());
            web.setUIDelegate(Some(ProtocolObject::from_ref(&*ui)));
            let delegate = Delegate::alloc(mtm).set_ivars(Nav {
                link,
                downloads: downloads.clone(),
            });
            let delegate: Retained<Delegate> = msg_send![super(delegate), init];
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            clip.addSubview(&web);
            let hosted = Self {
                clip,
                web,
                _delegate: delegate,
                _ui: ui,
                _downloads: downloads,
            };
            hosted.load(url);
            hosted
        }
    }

    pub(super) fn place(&self, parent: &NSView, frame: NSRect, web_frame: NSRect, zoom: f64) {
        unsafe {
            // Added once. Adding a view again takes it out and puts it back,
            // and a view taken out gives up the keyboard.
            if self.clip.superview().is_none() {
                parent.addSubview_positioned_relativeTo(
                    &self.clip,
                    NSWindowOrderingMode::Above,
                    None,
                );
            }
            self.clip.setFrame(frame);
            self.web.setFrame(web_frame);
            self.web.setPageZoom(zoom);
        }
        self.clip.setHidden(false);
    }

    pub(super) fn hide(&self) {
        self.clip.setHidden(true);
    }

    pub(super) fn load(&self, url: &str) {
        unsafe {
            if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
                self.web.loadRequest(&NSURLRequest::requestWithURL(&url));
            }
        }
    }

    /// `None` goes back to the default. It applies from the next load.
    pub(super) fn set_user_agent(&self, user_agent: Option<&str>) {
        let agent = user_agent.map(NSString::from_str);
        unsafe { self.web.setCustomUserAgent(agent.as_deref()) };
    }

    pub(super) fn back(&self) {
        unsafe { self.web.goBack() };
    }
    pub(super) fn forward(&self) {
        unsafe { self.web.goForward() };
    }
    pub(super) fn reload(&self) {
        unsafe { self.web.reload() };
    }
    pub(super) fn stop(&self) {
        unsafe { self.web.stopLoading() };
    }

    pub(super) fn state(&self) -> PageState {
        unsafe {
            PageState {
                url: (self.web.URL())
                    .and_then(|url| url.absoluteString())
                    .map(|url| url.to_string())
                    .unwrap_or_default(),
                title: (self.web.title())
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
                loading: self.web.isLoading(),
                back: self.web.canGoBack(),
                forward: self.web.canGoForward(),
            }
        }
    }

    /// The keyboard goes to the page.
    pub(super) fn focus(&self) {
        if let Some(window) = self.web.window() {
            window.makeFirstResponder(Some(&self.web));
        }
    }

    /// Copy, paste and the like are the Edit menu's, and Monocode's menu
    /// sends them to GPUI. With the page focused they are passed on to it
    /// the way AppKit's own menu would.
    pub(super) fn edit(&self, action: Sel) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        unsafe {
            NSApplication::sharedApplication(mtm).sendAction_to_from(action, Some(&self.web), None);
        }
    }

    pub(super) fn select_all() -> Sel {
        sel!(selectAll:)
    }

    /// Whether the keyboard is the page's: the web view, or a view inside it.
    pub(super) fn has_keyboard(&self) -> bool {
        let Some(window) = self.web.window() else {
            return false;
        };
        let Some(responder) = window.firstResponder() else {
            return false;
        };
        let web: &NSView = &self.web;
        match responder.downcast::<NSView>() {
            Ok(view) => view.isDescendantOf(web),
            Err(_) => false,
        }
    }

    /// Runs `script` (an expression) in the page and hands back its value as
    /// JSON text, or what went wrong.
    pub(super) fn eval(&self, script: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let wrapped = format!(
            "(()=>{{try{{const v=({script});return JSON.stringify(v===undefined?null:v)}}             catch(e){{return JSON.stringify({{__error:String(e)}})}}}})()"
        );
        let done = std::cell::Cell::new(Some(done));
        let block = block2::RcBlock::new(
            move |value: *mut objc2::runtime::AnyObject, error: *mut NSError| {
                let Some(done) = done.take() else {
                    return;
                };
                if let Some(error) = unsafe { error.as_ref() } {
                    done(Err(error.localizedDescription().to_string()));
                    return;
                }
                let text = unsafe { value.as_ref() }.and_then(|value| {
                    let is_string: bool =
                        unsafe { msg_send![value, isKindOfClass: objc2::class!(NSString)] };
                    is_string
                        .then(|| unsafe { &*(value as *const _ as *const NSString) }.to_string())
                });
                done(text.ok_or_else(|| "The page returned nothing".into()));
            },
        );
        unsafe {
            self.web
                .evaluateJavaScript_completionHandler(&NSString::from_str(&wrapped), Some(&block))
        };
    }

    /// A real mouse click at a point of the page (CSS pixels from its top
    /// left), sent the way AppKit sends one.
    ///
    /// Returned rather than done: AppKit handles an event by calling back
    /// into GPUI, so it must be sent from outside any GPUI update.
    pub(super) fn click(&self, x: f64, y: f64) -> Box<dyn FnOnce()> {
        let web = self.web.clone();
        Box::new(move || Self::send_click(&web, x, y))
    }

    fn send_click(web: &Page, x: f64, y: f64) {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
        let Some(window) = web.window() else {
            return;
        };
        let zoom = unsafe { web.pageZoom() };
        let (x, y) = (x * zoom, y * zoom);
        let y = if web.isFlipped() {
            y
        } else {
            web.bounds().size.height - y
        };
        let at = web.convertPoint_toView(objc2_foundation::NSPoint::new(x, y), None);
        // What a key window does with a click: the view under it is made
        // first responder if it takes the keyboard, then gets the press and
        // the release. Done by hand because a window that is not key (the
        // self-test may run behind a locked screen) spends its first click
        // on becoming key instead.
        let hit = window.contentView().and_then(|content| {
            let point = match unsafe { content.superview() } {
                Some(parent) => parent.convertPoint_fromView(at, None),
                None => at,
            };
            content.hitTest(point)
        });
        let Some(hit) = hit else {
            return;
        };
        if hit.acceptsFirstResponder() {
            window.makeFirstResponder(Some(&hit));
        }
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind,
                at,
                NSEventModifierFlags::empty(),
                0.,
                window.windowNumber(),
                None,
                0,
                1,
                1.,
            );
            let Some(event) = event else {
                continue;
            };
            if kind == NSEventType::LeftMouseDown {
                hit.mouseDown(&event);
            } else {
                hit.mouseUp(&event);
            }
        }
    }

    /// A real key press, with ⌘ held if `command`. Returned, as `click`.
    pub(super) fn key(&self, characters: &str, code: u16, command: bool) -> Box<dyn FnOnce()> {
        let web = self.web.clone();
        let characters = characters.to_owned();
        Box::new(move || Self::send_key(&web, &characters, code, command))
    }

    /// What the application does with a key: a ⌘ chord is first offered to
    /// the window's views and then to the menu as a key equivalent, and
    /// anything not taken goes to the first responder. Done by hand so that
    /// it also works for an app that is not in front.
    fn send_key(web: &Page, characters: &str, code: u16, command: bool) {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
        let (Some(window), Some(mtm)) = (web.window(), MainThreadMarker::new()) else {
            return;
        };
        let flags = if command {
            NSEventModifierFlags::Command
        } else {
            NSEventModifierFlags::empty()
        };
        let keys = NSString::from_str(characters);
        for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                objc2_foundation::NSPoint::new(0., 0.),
                flags,
                0.,
                window.windowNumber(),
                None,
                &keys,
                &keys,
                false,
                code,
            );
            let Some(event) = event else {
                continue;
            };
            if command && kind == NSEventType::KeyDown {
                if window.performKeyEquivalent(&event) {
                    continue;
                }
                let menu = NSApplication::sharedApplication(mtm).mainMenu();
                if menu.is_some_and(|menu| menu.performKeyEquivalent(&event)) {
                    continue;
                }
            }
            window.sendEvent(&event);
        }
    }
}

/// The registrable-looking end of a host: what WebKit files a site's data
/// under (`app.example.com` → `example.com`; an address stays as it is).
pub(super) fn site_of(host: &str) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return host.to_owned();
    }
    let labels: Vec<&str> = host.rsplit('.').take(2).collect();
    labels.into_iter().rev().collect::<Vec<_>>().join(".")
}

/// The stores a page's data lives in, as the app sees them.
pub(crate) mod data {
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_foundation::{NSArray, NSDate, NSSet, NSString};
    use objc2_web_kit::{WKWebsiteDataRecord, WKWebsiteDataStore};

    /// What to clear: what a site stored, what the browser kept of it to
    /// load faster, or both.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Kind {
        Cache,
        SiteData,
        Everything,
    }

    fn types(kind: Kind, mtm: MainThreadMarker) -> Retained<NSSet<NSString>> {
        let all = unsafe { WKWebsiteDataStore::allWebsiteDataTypes(mtm) };
        if kind == Kind::Everything {
            return all;
        }
        let cache = |name: &str| name.contains("Cache") || name.contains("ServiceWorker");
        let picked: Vec<Retained<NSString>> = (all.iter())
            .filter(|name| cache(&name.to_string()) == (kind == Kind::Cache))
            .collect();
        NSSet::from_retained_slice(&picked)
    }

    fn store(mtm: MainThreadMarker) -> Retained<WKWebsiteDataStore> {
        unsafe { WKWebsiteDataStore::defaultDataStore(mtm) }
    }

    /// Clears what changed in the last `seconds` (`None`: ever).
    pub(crate) fn clear(kind: Kind, seconds: Option<f64>, done: impl FnOnce() + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let since = match seconds {
            Some(seconds) => NSDate::dateWithTimeIntervalSinceNow(-seconds),
            None => NSDate::distantPast(),
        };
        let done = std::cell::Cell::new(Some(done));
        let block = block2::RcBlock::new(move || {
            if let Some(done) = done.take() {
                done();
            }
        });
        unsafe {
            store(mtm).removeDataOfTypes_modifiedSince_completionHandler(
                &types(kind, mtm),
                &since,
                &block,
            )
        };
    }

    /// The sites that have data, by name, each with what kinds it holds.
    pub(crate) fn sites(done: impl FnOnce(Vec<(String, Vec<String>)>) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let done = std::cell::Cell::new(Some(done));
        let block = block2::RcBlock::new(
            move |records: std::ptr::NonNull<NSArray<WKWebsiteDataRecord>>| {
                let Some(done) = done.take() else {
                    return;
                };
                let records = unsafe { records.as_ref() };
                let mut sites: Vec<(String, Vec<String>)> = (records.iter())
                    .map(|record| unsafe {
                        let mut kinds: Vec<String> = (record.dataTypes().iter())
                            .map(|kind| {
                                let kind = kind.to_string();
                                kind.trim_start_matches("WKWebsiteDataType").to_owned()
                            })
                            .collect();
                        kinds.sort();
                        (record.displayName().to_string(), kinds)
                    })
                    .collect();
                sites.sort();
                done(sites);
            },
        );
        unsafe {
            store(mtm).fetchDataRecordsOfTypes_completionHandler(
                &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                &block,
            )
        };
    }

    /// Removes everything one site stored: its cookies, storage and cache.
    pub(crate) fn clear_site(site: String, done: impl FnOnce(bool) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let done = std::rc::Rc::new(std::cell::Cell::new(Some(done)));
        let block = block2::RcBlock::new(
            move |records: std::ptr::NonNull<NSArray<WKWebsiteDataRecord>>| {
                let records = unsafe { records.as_ref() };
                let matching: Vec<Retained<WKWebsiteDataRecord>> = (records.iter())
                    .filter(|record| unsafe { record.displayName() }.to_string() == site)
                    .collect();
                let found = !matching.is_empty();
                let done = done.clone();
                let finished = block2::RcBlock::new(move || {
                    if let Some(done) = done.take() {
                        done(found);
                    }
                });
                unsafe {
                    store(mtm).removeDataOfTypes_forDataRecords_completionHandler(
                        &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                        &NSArray::from_retained_slice(&matching),
                        &finished,
                    )
                };
            },
        );
        unsafe {
            store(mtm).fetchDataRecordsOfTypes_completionHandler(
                &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                &block,
            )
        };
    }
}

impl Hosted {
    /// Looks for text in the page, forwards or backwards from the last
    /// match, and says whether there was one.
    pub(super) fn find(&self, text: &str, backwards: bool, done: impl FnOnce(bool) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let done = std::cell::Cell::new(Some(done));
        let block = block2::RcBlock::new(
            move |result: std::ptr::NonNull<objc2_web_kit::WKFindResult>| {
                if let Some(done) = done.take() {
                    done(unsafe { result.as_ref().matchFound() });
                }
            },
        );
        unsafe {
            let how = objc2_web_kit::WKFindConfiguration::new(mtm);
            how.setBackwards(backwards);
            how.setWraps(true);
            self.web.findString_withConfiguration_completionHandler(
                &NSString::from_str(text),
                Some(&how),
                &block,
            );
        }
    }

    /// Opens WebKit's Web Inspector on the page. It is the same inspector
    /// Safari's Develop menu opens; this is the direct way to it, which is
    /// not public API, so it is asked for and may not be there.
    pub(super) fn inspect(&self) -> bool {
        unsafe {
            let web: &objc2::runtime::AnyObject = &self.web;
            if !msg_send![web, respondsToSelector: sel!(_inspector)] {
                return false;
            }
            let inspector: *mut objc2::runtime::AnyObject = msg_send![web, _inspector];
            let Some(inspector) = inspector.as_ref() else {
                return false;
            };
            if !msg_send![inspector, respondsToSelector: sel!(show)] {
                return false;
            }
            let _: () = msg_send![inspector, show];
            true
        }
    }

    /// The page's memory, in megabytes, if it can be told: the resident
    /// size of the WebKit process drawing it. WebKit does not publish which
    /// process that is; the selector asked for here is its own.
    pub(super) fn memory_mb(&self) -> Option<u64> {
        let pid: i32 = unsafe {
            let web: &objc2::runtime::AnyObject = &self.web;
            if !msg_send![web, respondsToSelector: sel!(_webProcessIdentifier)] {
                return None;
            }
            msg_send![web, _webProcessIdentifier]
        };
        if pid <= 0 {
            return None;
        }
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let got = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size)
        };
        (got == size).then_some(info.pti_resident_size / (1024 * 1024))
    }

    /// A picture of the page as it looks now, as JPEG bytes. It stands in
    /// for the page while something GPUI draws has to be on top of it.
    pub(super) fn snapshot(&self, done: impl FnOnce(Option<Vec<u8>>) + 'static) {
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
        let done = std::cell::Cell::new(Some(done));
        let block = block2::RcBlock::new(move |image: *mut NSImage, _: *mut NSError| {
            let Some(done) = done.take() else {
                return;
            };
            let bytes = unsafe { image.as_ref() }
                .and_then(|image| image.TIFFRepresentation())
                .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
                .and_then(|bitmap| unsafe {
                    bitmap.representationUsingType_properties(
                        NSBitmapImageFileType::JPEG,
                        &objc2_foundation::NSDictionary::new(),
                    )
                })
                .map(|data| data.to_vec());
            done(bytes);
        });
        unsafe {
            self.web
                .takeSnapshotWithConfiguration_completionHandler(None, &block)
        };
    }

    /// For the self-test: which view a click at a point of the page lands
    /// on, and who has the keyboard.
    pub(super) fn probe(&self, x: f64, y: f64) -> serde_json::Value {
        let name = |object: &objc2::runtime::AnyObject| {
            object.class().name().to_string_lossy().into_owned()
        };
        let Some(window) = self.web.window() else {
            return serde_json::json!({"window": null});
        };
        let zoom = unsafe { self.web.pageZoom() };
        let flipped = self.web.isFlipped();
        let local = objc2_foundation::NSPoint::new(
            x * zoom,
            if flipped {
                y * zoom
            } else {
                self.web.bounds().size.height - y * zoom
            },
        );
        let at = self.web.convertPoint_toView(local, None);
        let content = window.contentView();
        let hit = content.as_ref().and_then(|content| {
            // hitTest: takes a point in the receiver's superview's system.
            let point = match unsafe { content.superview() } {
                Some(parent) => parent.convertPoint_fromView(at, None),
                None => at,
            };
            content.hitTest(point)
        });
        let frame = self.web.convertRect_toView(self.web.bounds(), None);
        let clip = self.clip.convertRect_toView(self.clip.bounds(), None);
        serde_json::json!({
            "zoom": zoom,
            "flipped": flipped,
            "window_point": [at.x, at.y],
            "web_in_window": [frame.origin.x, frame.origin.y, frame.size.width, frame.size.height],
            "clip_in_window": [clip.origin.x, clip.origin.y, clip.size.width, clip.size.height],
            "clip_hidden": self.clip.isHidden(),
            "hit": hit.as_deref().map(|view| name(view)),
            "hit_is_in_web": hit.as_deref().map(|view| view.isDescendantOf(&self.web)),
            "first_responder": window.firstResponder().as_deref().map(|r| name(r)),
            "content_view": content.as_deref().map(|view| name(view)),
            "clip_parent": unsafe { self.clip.superview() }.as_deref().map(|view| name(view)),
            "key_window": window.isKeyWindow(),
        })
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        unsafe { self.web.stopLoading() };
        self.clip.removeFromSuperview();
    }
}
