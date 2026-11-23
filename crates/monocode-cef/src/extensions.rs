//! The engine half of the browser's own extension layer
//! (docs/native/browser/context.md K69). Nothing here knows an extension:
//!
//! - The `mcext` scheme: registered in every process (standard, secure,
//!   CORS and fetch enabled, bypassing page CSP like Chrome's extension
//!   resources), with a handler factory per profile. Every request goes to
//!   the app's [`ExtensionRequests`] on CEF's IO thread, with what the app
//!   needs to decide (the frame and its parent, the resource type), and is
//!   answered through a [`Responder`], at once or streamed.
//! - [`fetch`]: an extension's cross-origin `fetch`, made by the browser
//!   process (`CefURLRequest`) in the caller's profile, so its cookies and
//!   no CORS, streamed back and abortable. CEF's cross-origin allow list
//!   does not reach the network service's CORS check (spike B finding).
//!
//! Pages the layer hosts (the background, the popup) are ordinary
//! [`crate::BrowserView`]s, in windows of [`crate::mac::ext_window`].

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
};

use cef::*;

/// The scheme extension files are served from (`monocode_core`'s
/// `browser::extensions::SCHEME`).
pub const SCHEME: &str = "mcext";

fn scheme_options() -> i32 {
    (SchemeOptions::STANDARD.get_raw()
        | SchemeOptions::SECURE.get_raw()
        | SchemeOptions::CORS_ENABLED.get_raw()
        | SchemeOptions::CSP_BYPASSING.get_raw()
        | SchemeOptions::FETCH_ENABLED.get_raw()) as i32
}

/// Every process: `OnRegisterCustomSchemes`.
pub(crate) fn register_scheme(registrar: Option<&mut SchemeRegistrar>) {
    if let Some(registrar) = registrar {
        registrar.add_custom_scheme(Some(&SCHEME.into()), scheme_options());
    }
}

wrap_app! {
    struct HelperApp {}

    impl App {
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            register_scheme(registrar);
        }
    }
}

/// The helper processes' app: it registers the scheme, as the browser
/// process does.
pub fn helper_app() -> cef::App {
    HelperApp::new()
}

/// What kind of load a request is, as far as the app cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// A tab's or a window's top document.
    MainFrame,
    /// A frame's document: the parent decides.
    SubFrame,
    /// A worker's script.
    Worker,
    /// Anything else a document loads.
    Resource,
}

/// One request to the scheme.
#[derive(Clone, Debug)]
pub struct ExtRequest {
    /// The profile whose context the request is in: `None` for the
    /// default profile, the isolated profile's id otherwise.
    pub profile: Option<String>,
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub load: Load,
    /// The requesting browser.
    pub browser_id: Option<i32>,
    /// The frame the request is for: for a document, the one loading it
    /// (its URL is still the old document's); else the one asking.
    pub frame_url: Option<String>,
    /// That frame's parent's URL.
    pub parent_url: Option<String>,
    pub referrer: String,
}

impl ExtRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        (self.headers.iter())
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The app's side: called on CEF's IO thread for every request.
pub trait ExtensionRequests: Send + Sync {
    fn handle(&self, request: ExtRequest, responder: Responder);
}

static HANDLER: OnceLock<Arc<dyn ExtensionRequests>> = OnceLock::new();

/// Sets the app's handler, once, before any page loads.
pub fn set_handler(handler: Arc<dyn ExtensionRequests>) {
    let _ = HANDLER.set(handler);
}

wrap_scheme_handler_factory! {
    struct ExtFactory {
        // `None`: the default profile. `Some("")`: a throwaway profile,
        // which runs no extension.
        profile: Option<String>,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut cef::Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let request = request?;
            let load = match request.resource_type() {
                ResourceType::MAIN_FRAME => Load::MainFrame,
                ResourceType::SUB_FRAME => Load::SubFrame,
                ResourceType::WORKER | ResourceType::SHARED_WORKER | ResourceType::SERVICE_WORKER => {
                    Load::Worker
                }
                _ => Load::Resource,
            };
            let frame_url = frame.as_ref().map(|f| CefString::from(&f.url()).to_string());
            let parent_url = (frame.as_ref())
                .and_then(|f| f.parent())
                .map(|p| CefString::from(&p.url()).to_string());
            let mut map = CefStringMultimap::new();
            request.header_map(Some(&mut map));
            let headers = map
                .into_iter()
                .flat_map(|(name, values)| values.into_iter().map(move |v| (name.clone(), v)))
                .collect();
            let request = ExtRequest {
                profile: self.profile.clone(),
                url: CefString::from(&request.url()).to_string(),
                method: CefString::from(&request.method()).to_string(),
                headers,
                body: post_body(request),
                load,
                browser_id: browser.map(|b| b.identifier()),
                frame_url,
                parent_url,
                referrer: CefString::from(&request.referrer_url()).to_string(),
            };
            Some(ExtHandler::new(Arc::new(Mutex::new(Stream::default())), Arc::new(Mutex::new(Some(request)))))
        }
    }
}

/// Browser process, once CEF runs: the default profile's handler.
pub(crate) fn on_context_initialized() {
    let mut factory = ExtFactory::new(None);
    cef::register_scheme_handler_factory(Some(&SCHEME.into()), None, Some(&mut factory));
}

/// A profile's own request context (crate::profile) gets its handler; a
/// throwaway one gets one that refuses everything.
pub(crate) fn register_on(context: &RequestContext, profile: &crate::Profile) {
    let profile = match profile {
        crate::Profile::Default => None,
        crate::Profile::Isolated(id) => Some(id.clone()),
        crate::Profile::Ephemeral(_) => Some(String::new()),
    };
    let mut factory = ExtFactory::new(profile);
    context.register_scheme_handler_factory(Some(&SCHEME.into()), None, Some(&mut factory));
}

fn post_body(request: &Request) -> Vec<u8> {
    let Some(post) = request.post_data() else {
        return Vec::new();
    };
    let mut elements = vec![None; post.element_count()];
    post.elements(Some(&mut elements));
    let mut out = Vec::new();
    for element in elements.into_iter().flatten() {
        let n = element.bytes_count();
        let start = out.len();
        out.resize(start + n, 0);
        element.bytes(n, out[start..].as_mut_ptr());
    }
    out
}

/// A response's head.
#[derive(Clone, Debug)]
pub struct Head {
    pub status: i32,
    pub status_text: String,
    pub mime: String,
    pub headers: Vec<(String, String)>,
    /// The body's length when known.
    pub length: Option<usize>,
}

impl Head {
    pub fn new(status: i32, mime: &str) -> Self {
        Head {
            status,
            status_text: match status {
                200 => "OK",
                204 => "No Content",
                403 => "Forbidden",
                404 => "Not Found",
                410 => "Gone",
                _ => "",
            }
            .into(),
            mime: mime.into(),
            headers: Vec::new(),
            length: None,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct SendCallback(Callback);
// SAFETY: CEF's callbacks are reference counted thread-safely and may be
// executed on any thread.
unsafe impl Send for SendCallback {}

/// A read CEF waits on: its buffer stays valid until the callback runs.
struct PendingRead {
    out: *mut u8,
    len: usize,
    callback: ResourceReadCallback,
}
// SAFETY: as SendCallback; the buffer is only written before `cont`.
unsafe impl Send for PendingRead {}

#[derive(Default)]
struct Stream {
    head: Option<Head>,
    data: VecDeque<u8>,
    done: bool,
    /// The response failed after its head: a network error code.
    error: Option<i32>,
    open: Option<SendCallback>,
    read: Option<PendingRead>,
    cancelled: bool,
    on_cancel: Option<Box<dyn FnOnce() + Send>>,
}

impl Stream {
    /// Gives a waiting read what there is. Returns what to call outside the
    /// lock: the read's callback and its count.
    fn feed(&mut self) -> Option<(ResourceReadCallback, i32)> {
        let read = self.read.as_ref()?;
        if !self.data.is_empty() {
            let n = read.len.min(self.data.len());
            let read = self.read.take().expect("checked");
            for (i, byte) in self.data.drain(..n).enumerate() {
                // SAFETY: CEF keeps `len` bytes at `out` until `cont`.
                unsafe { *read.out.add(i) = byte };
            }
            return Some((read.callback, n as i32));
        }
        if self.done || self.cancelled {
            let read = self.read.take().expect("checked");
            return Some((read.callback, self.error.unwrap_or(0)));
        }
        None
    }
}

/// Answers one request, from any thread: [`Responder::respond`] at once,
/// or [`Responder::start`] and then [`Writer`] chunks.
pub struct Responder(Arc<Mutex<Stream>>);

/// The body of a streamed response.
pub struct Writer(Arc<Mutex<Stream>>);

impl Responder {
    pub fn respond(self, mut head: Head, body: Vec<u8>) {
        head.length = Some(body.len());
        let writer = self.start(head);
        writer.write(&body);
        writer.finish(None);
    }

    pub fn json(self, status: i32, body: &serde_json::Value) {
        let head = Head::new(status, "application/json").header("Cache-Control", "no-store");
        self.respond(head, body.to_string().into_bytes());
    }

    /// Sends the head; the body follows through the writer.
    pub fn start(self, head: Head) -> Writer {
        let open = {
            let mut s = self.0.lock().unwrap();
            s.head = Some(head);
            s.open.take()
        };
        if let Some(open) = open {
            open.0.cont();
        }
        Writer(self.0.clone())
    }

    /// Runs `f` (once, on some CEF thread) if the page gives the request
    /// up (an aborted fetch, a closed page).
    pub fn on_cancel(&self, f: impl FnOnce() + Send + 'static) {
        let mut s = self.0.lock().unwrap();
        if s.cancelled {
            drop(s);
            f();
        } else {
            s.on_cancel = Some(Box::new(f));
        }
    }
}

impl Writer {
    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let ready = {
            let mut s = self.0.lock().unwrap();
            if s.cancelled {
                return;
            }
            s.data.extend(bytes);
            s.feed()
        };
        if let Some((callback, n)) = ready {
            callback.cont(n);
        }
    }

    /// Ends the body; with `error` (a negative network error code, such as
    /// -2 `ERR_FAILED`) the page sees the load fail.
    pub fn finish(&self, error: Option<i32>) {
        let ready = {
            let mut s = self.0.lock().unwrap();
            s.done = true;
            s.error = error;
            s.feed()
        };
        if let Some((callback, n)) = ready {
            callback.cont(n);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().cancelled
    }
}

wrap_resource_handler! {
    struct ExtHandler {
        stream: Arc<Mutex<Stream>>,
        request: Arc<Mutex<Option<ExtRequest>>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            callback: Option<&mut Callback>,
        ) -> i32 {
            let Some(handle_request) = handle_request else { return 0 };
            let request = self.request.lock().unwrap().take();
            let (Some(request), Some(handler)) = (request, HANDLER.get()) else {
                *handle_request = 1;
                let mut s = self.stream.lock().unwrap();
                s.head = Some(Head::new(404, "text/plain"));
                s.done = true;
                return 1;
            };
            if let Some(callback) = callback {
                self.stream.lock().unwrap().open = Some(SendCallback(callback.clone()));
            }
            handler.handle(request, Responder(self.stream.clone()));
            let mut s = self.stream.lock().unwrap();
            if s.head.is_some() {
                // Answered at once.
                s.open = None;
                *handle_request = 1;
            } else {
                *handle_request = 0;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let s = self.stream.lock().unwrap();
            let (Some(response), Some(head)) = (response, s.head.as_ref()) else {
                return;
            };
            response.set_status(head.status);
            response.set_status_text(Some(&head.status_text.as_str().into()));
            response.set_mime_type(Some(&head.mime.as_str().into()));
            for (name, value) in &head.headers {
                response.set_header_by_name(Some(&name.as_str().into()), Some(&value.as_str().into()), 0);
            }
            if let Some(length) = response_length {
                *length = head.length.map_or(-1, |l| l as i64);
            }
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            let Some(bytes_read) = bytes_read else { return 0 };
            *bytes_read = 0;
            let mut s = self.stream.lock().unwrap();
            if bytes_to_read <= 0 {
                return 0;
            }
            if !s.data.is_empty() {
                let n = (bytes_to_read as usize).min(s.data.len());
                for (i, byte) in s.data.drain(..n).enumerate() {
                    // SAFETY: CEF gives a buffer of `bytes_to_read` bytes.
                    unsafe { *data_out.add(i) = byte };
                }
                *bytes_read = n as i32;
                return 1;
            }
            if s.done || s.cancelled {
                if let Some(error) = s.error {
                    *bytes_read = error;
                }
                return 0;
            }
            // Nothing yet: CEF waits for the callback.
            let Some(callback) = callback else { return 0 };
            s.read = Some(PendingRead {
                out: data_out,
                len: bytes_to_read as usize,
                callback: callback.clone(),
            });
            1
        }

        fn cancel(&self) {
            let on_cancel = {
                let mut s = self.stream.lock().unwrap();
                s.cancelled = true;
                s.open = None;
                s.read = None;
                s.on_cancel.take()
            };
            if let Some(f) = on_cancel {
                f();
            }
        }
    }
}

// ---- cross-origin fetch through the browser process ----

/// An extension's `fetch`, made for it.
#[derive(Clone, Debug)]
pub struct FetchSpec {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `credentials: "include"`: the profile's cookies go and come.
    pub credentials: bool,
    /// `redirect: "manual"` or `"error"`: redirects are not followed.
    pub stop_on_redirect: bool,
    /// `cache: "no-store"` / `"reload"`.
    pub skip_cache: bool,
}

/// What a fetch reports, from CEF's UI thread.
pub trait FetchSink: Send {
    /// The response's head, once: status, status text, headers, final URL.
    fn head(
        &mut self,
        status: i32,
        status_text: String,
        headers: Vec<(String, String)>,
        url: String,
    );
    fn data(&mut self, bytes: &[u8]);
    /// The end; `Err` with a network error text when it failed.
    fn done(&mut self, result: Result<(), String>);
}

/// Cancels a fetch [`fetch`] started; dropping it does nothing.
#[derive(Clone)]
pub struct FetchHandle(Arc<Mutex<FetchState>>);

#[derive(Default)]
struct FetchState {
    request: Option<SendUrlrequest>,
    cancelled: bool,
}

struct SendUrlrequest(Urlrequest);
// SAFETY: only touched on CEF's UI thread (tasks post there).
unsafe impl Send for SendUrlrequest {}

impl FetchHandle {
    pub fn cancel(&self) {
        let state = self.0.clone();
        post_ui(move || {
            let mut s = state.lock().unwrap();
            s.cancelled = true;
            if let Some(r) = s.request.take() {
                r.0.cancel();
            }
        });
    }
}

type Job = Box<dyn FnOnce() + Send>;

wrap_task! {
    struct UiTask {
        job: Arc<Mutex<Option<Job>>>,
    }

    impl Task {
        fn execute(&self) {
            if let Some(job) = self.job.lock().unwrap().take() {
                job();
            }
        }
    }
}

fn post_ui(job: impl FnOnce() + Send + 'static) {
    let mut task = UiTask::new(Arc::new(Mutex::new(Some(Box::new(job)))));
    cef::post_task(ThreadId::UI, Some(&mut task));
}

struct SinkBox(Mutex<Option<Box<dyn FetchSink>>>, Mutex<bool>);

wrap_urlrequest_client! {
    struct FetchClient {
        sink: Arc<SinkBox>,
    }

    impl UrlrequestClient {
        fn on_download_data(&self, request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            self.ensure_head(request.as_deref());
            if data.is_null() || data_length == 0 {
                return;
            }
            // SAFETY: CEF gives `data_length` bytes at `data`.
            let slice = unsafe { std::slice::from_raw_parts(data, data_length) };
            if let Some(sink) = self.sink.0.lock().unwrap().as_mut() {
                sink.data(slice);
            }
        }

        fn on_download_progress(&self, request: Option<&mut Urlrequest>, _current: i64, _total: i64) {
            self.ensure_head(request.as_deref());
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let Some(request) = request else { return };
            let status = request.request_status();
            if status == UrlrequestStatus::SUCCESS || request.response().is_some_and(|r| r.status() != 0) {
                self.ensure_head(Some(request));
            }
            let Some(mut sink) = self.sink.0.lock().unwrap().take() else { return };
            if status == UrlrequestStatus::SUCCESS {
                sink.done(Ok(()));
            } else if status == UrlrequestStatus::CANCELED {
                sink.done(Err("net::ERR_ABORTED".into()));
            } else {
                sink.done(Err(format!("{:?}", request.request_error())));
            }
        }

        fn auth_credentials(
            &self,
            _is_proxy: i32,
            _host: Option<&CefString>,
            _port: i32,
            _realm: Option<&CefString>,
            _scheme: Option<&CefString>,
            _callback: Option<&mut AuthCallback>,
        ) -> i32 {
            0
        }
    }
}

impl FetchClient {
    fn ensure_head(&self, request: Option<&Urlrequest>) {
        let mut sent = self.sink.1.lock().unwrap();
        if *sent {
            return;
        }
        let Some(response) = request.and_then(|r| r.response()) else {
            return;
        };
        if response.status() == 0 {
            return;
        }
        *sent = true;
        let mut map = CefStringMultimap::new();
        response.header_map(Some(&mut map));
        let headers = map
            .into_iter()
            .flat_map(|(name, values)| values.into_iter().map(move |v| (name.clone(), v)))
            .collect();
        let url = CefString::from(&response.url()).to_string();
        let text = CefString::from(&response.status_text()).to_string();
        if let Some(sink) = self.sink.0.lock().unwrap().as_mut() {
            sink.head(response.status(), text, headers, url);
        }
    }
}

/// Starts `spec` in `profile`'s request context (`None`: the default
/// profile), from any thread.
pub fn fetch(profile: Option<String>, spec: FetchSpec, sink: Box<dyn FetchSink>) -> FetchHandle {
    let handle = FetchHandle(Arc::default());
    let state = handle.0.clone();
    post_ui(move || {
        let sink = Arc::new(SinkBox(Mutex::new(Some(sink)), Mutex::new(false)));
        let fail = |why: &str| {
            if let Some(mut s) = sink.0.lock().unwrap().take() {
                s.done(Err(why.into()));
            }
        };
        if state.lock().unwrap().cancelled {
            return fail("net::ERR_ABORTED");
        }
        let Some(mut request) = cef::request_create() else {
            return fail("net::ERR_FAILED");
        };
        request.set_url(Some(&spec.url.as_str().into()));
        request.set_method(Some(&spec.method.as_str().into()));
        let mut map = CefStringMultimap::new();
        for (name, value) in &spec.headers {
            map.append(name, value);
        }
        request.set_header_map(Some(&mut map));
        if !spec.body.is_empty() {
            if let (Some(mut post), Some(mut element)) =
                (cef::post_data_create(), cef::post_data_element_create())
            {
                element.set_to_bytes(spec.body.len(), spec.body.as_ptr());
                post.add_element(Some(&mut element));
                request.set_post_data(Some(&mut post));
            }
        }
        let mut flags = 0;
        if spec.credentials {
            flags |= sys::cef_urlrequest_flags_t::UR_FLAG_ALLOW_STORED_CREDENTIALS.0 as i32;
        }
        if spec.stop_on_redirect {
            flags |= sys::cef_urlrequest_flags_t::UR_FLAG_STOP_ON_REDIRECT.0 as i32;
        }
        if spec.skip_cache {
            flags |= sys::cef_urlrequest_flags_t::UR_FLAG_SKIP_CACHE.0 as i32;
        }
        request.set_flags(flags);
        let mut context = match &profile {
            None => None,
            Some(id) => crate::profile::request_context(&crate::Profile::Isolated(id.clone())),
        };
        let mut client = FetchClient::new(sink.clone());
        match cef::urlrequest_create(Some(&mut request), Some(&mut client), context.as_mut()) {
            Some(r) => state.lock().unwrap().request = Some(SendUrlrequest(r)),
            None => fail("net::ERR_FAILED"),
        }
    });
    handle
}
