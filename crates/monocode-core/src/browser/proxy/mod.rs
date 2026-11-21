//! The external, Chrome-compatible CDP endpoint (context.md §6.2, K26, K27,
//! K38): one browser-level endpoint per project scope, on the loopback,
//! behind a random token.
//!
//! - this file: the listener, the request guards, HTTP discovery.
//! - [`ws`]: a WebSocket client's thread.
//! - [`hub`]: the main-thread side: the browser target Chrome would be, over
//!   the pages of the client's scope, and the client's sessions on them.
//!
//! One listener serves every scope; the token, the first path segment, says
//! which. A request with an `Origin` header (a web page) is refused, and its
//! `Host` must be the loopback address with our port (DNS rebinding).
//!
//! The app's docked DevTools (K28) use the same listener with tokens of their
//! own ([`devtools_base`]): they work while external connections are off,
//! admit only the DevTools frontend (`Origin: devtools://devtools`), and only
//! its page-level WebSocket. They are not shown anywhere in Settings.
//!
//! Sharing a page (K38): a client gets a DevTools session of its own on each
//! page it attaches to (`Target.attachToTarget` on the page's channel), so
//! what it enables, its scripts, bindings and interception go away when it
//! detaches, and its `*.disable` calls touch nothing of ours. If Chromium
//! refuses that, the client shares the page's root session with our tools;
//! then `*.disable` calls are swallowed so our console and network log keep
//! running. Either way the page is one: `Emulation.*` overrides a client sets
//! (viewport, media, user agent, focus) also change what our tools and the
//! user see, until the client detaches (own session) or resets them. Our
//! tools auto-attach the page's iframes and workers (K42), so a client's
//! `Target.setAutoAttach` never pauses them (`waitForDebuggerOnStart` is
//! forced off), never turns auto-attach off, and child sessions' attach
//! events reach both.

mod hub;
#[cfg(test)]
mod tests;
mod ws;

use std::{
    collections::HashMap,
    io::{self, Read as _, Write as _},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    rc::Rc,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use futures_util::{FutureExt as _, future::LocalBoxFuture};
use serde_json::{Value, json};

use super::{Host, Job, Scope};

/// Connections served at once (HTTP and WebSocket).
const MAX_CONNECTIONS: usize = 32;
/// A request's head must come in this long, and be at most this big.
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
const HEAD_LIMIT: usize = 16 * 1024;
/// A client that does not take our messages for this long is dropped.
const WRITE_DEADLINE: Duration = Duration::from_secs(30);
/// Big enough for a full-page screenshot.
const MAX_MESSAGE: usize = 100 << 20;
/// How long an HTTP request waits for the main thread.
const ASK_TIMEOUT: Duration = Duration::from_secs(15);

static ENABLED: AtomicBool = AtomicBool::new(false);
static SERVER: Mutex<Option<Arc<Server>>> = Mutex::new(None);

/// Whether the endpoint may be handed out (Settings; off by default, K27).
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turns the endpoint on or off. Off closes every external client connection
/// and forgets their tokens: URLs handed out before stop working. The app's
/// DevTools keep theirs.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        return;
    }
    if let Some(server) = SERVER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        server.close_all();
    }
}

/// The endpoint's base URL for `scope` (`http://127.0.0.1:<port>/<token>`),
/// which Playwright `connectOverCDP` and dev-browser `--connect` take as is.
/// Starts the listener on first use. Errors when disabled.
pub fn endpoint(scope: &Scope) -> Result<String, String> {
    if !enabled() {
        return Err("External CDP connections are turned off in Settings".into());
    }
    Ok(server()?.endpoint(scope, false))
}

/// What the app's DevTools frontend takes as its `ws=` parameter for
/// `scope`'s pages, without the target: `127.0.0.1:<port>/<token>`. Append
/// `/devtools/page/<targetId>`. Works whatever the Settings say; the token is
/// the app's own, and only a `devtools://devtools` page may use it.
pub fn devtools_base(scope: &Scope) -> Result<String, String> {
    let server = server()?;
    let base = server.endpoint(scope, true);
    Ok(base.strip_prefix("http://").unwrap_or(&base).to_owned())
}

/// The listener, started on first use.
fn server() -> Result<Arc<Server>, String> {
    let mut slot = SERVER.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        let server = Server::start(Arc::new(super::submit))
            .map_err(|e| format!("Could not start the CDP endpoint: {e}"))?;
        *slot = Some(server);
    }
    Ok(slot.as_ref().expect("just started").clone())
}

/// The `Origin` of the DevTools frontend's requests.
const DEVTOOLS_ORIGIN: &str = "devtools://devtools";

/// Hands a job to the main thread; false when it is not taking work.
type Submit = Arc<dyn Fn(Job) -> bool + Send + Sync>;

/// What a WebSocket client's thread gets: its socket's bytes, or messages
/// from the main thread.
enum Input {
    Bytes(Vec<u8>),
    /// The socket closed.
    Eof,
    Send(Value),
    /// The main thread is done with the client.
    Close,
}

/// A scope's token, and the browser context id its pages report.
#[derive(Clone)]
struct Grant {
    token: String,
    scope: Scope,
    context: String,
    /// The app's own DevTools frontend's ([`devtools_base`]).
    internal: bool,
}

struct Server {
    port: u16,
    /// The browser target's id (`/devtools/browser/<id>`).
    browser_id: String,
    submit: Submit,
    inner: Mutex<Inner>,
    active: AtomicUsize,
}

#[derive(Default)]
struct Inner {
    grants: Vec<Grant>,
    /// Open WebSocket clients, to shut down when the endpoint turns off,
    /// and whether they are the app's DevTools.
    clients: HashMap<u64, (TcpStream, bool)>,
}

struct Request {
    method: String,
    path: String,
    query: String,
    /// Names in lower case.
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

impl Server {
    fn start(submit: Submit) -> io::Result<Arc<Server>> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let server = Arc::new(Server {
            port: listener.local_addr()?.port(),
            browser_id: hex_id(),
            submit,
            inner: Mutex::default(),
            active: AtomicUsize::new(0),
        });
        let accepting = server.clone();
        thread::Builder::new()
            .name("cdp-endpoint".into())
            .spawn(move || accepting.accept(listener))?;
        Ok(server)
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The base URL of `scope`'s external grant, or with `internal` its
    /// grant for the app's DevTools.
    fn endpoint(&self, scope: &Scope, internal: bool) -> String {
        let mut inner = self.inner();
        let found = (inner.grants.iter()).find(|g| g.scope == *scope && g.internal == internal);
        let token = match found {
            Some(grant) => grant.token.clone(),
            None => {
                let token = format!(
                    "{:032x}{:032x}",
                    rand::random::<u128>(),
                    rand::random::<u128>()
                );
                inner.grants.push(Grant {
                    token: token.clone(),
                    scope: scope.clone(),
                    context: hex_id(),
                    internal,
                });
                token
            }
        };
        format!("http://127.0.0.1:{}/{token}", self.port)
    }

    /// Ends every external client and grant; the app's DevTools stay.
    fn close_all(&self) {
        let mut inner = self.inner();
        inner.grants.retain(|grant| grant.internal);
        for (client, internal) in inner.clients.values() {
            if !internal {
                let _ = client.shutdown(Shutdown::Both);
            }
        }
    }

    fn add_client(&self, id: u64, stream: TcpStream, internal: bool) {
        self.inner().clients.insert(id, (stream, internal));
    }

    fn remove_client(&self, id: u64) {
        if let Some((stream, _)) = self.inner().clients.remove(&id) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Runs `work` on the main thread; false when it is not taking work.
    fn on_main(
        &self,
        work: impl FnOnce(Rc<dyn Host>) -> LocalBoxFuture<'static, ()> + Send + 'static,
    ) -> bool {
        (self.submit)(Box::new(work))
    }

    /// Runs `work` on the main thread and waits for its answer.
    fn ask<T: Send + 'static>(
        &self,
        work: impl FnOnce(Rc<dyn Host>) -> LocalBoxFuture<'static, T> + Send + 'static,
    ) -> Option<T> {
        let (tx, rx) = mpsc::channel();
        let submitted = self.on_main(move |host| {
            let answer = work(host);
            async move {
                let _ = tx.send(answer.await);
            }
            .boxed_local()
        });
        submitted
            .then(|| rx.recv_timeout(ASK_TIMEOUT).ok())
            .flatten()
    }

    fn accept(self: Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                self.active.fetch_sub(1, Ordering::SeqCst);
                let _ = respond(&mut stream, 503, "text/plain", "Too many connections");
                continue;
            }
            let server = self.clone();
            let spawned = thread::Builder::new()
                .name("cdp-endpoint-client".into())
                .spawn(move || {
                    server.serve(stream);
                    server.active.fetch_sub(1, Ordering::SeqCst);
                });
            if spawned.is_err() {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    /// The request's scope and its path after the token, or the status to
    /// refuse it with: 403 for web pages and rebound hosts, 404 otherwise.
    /// The app's DevTools' grants admit only their frontend's page sockets.
    fn admit(&self, request: &Request) -> Result<(Grant, String), u16> {
        let origin = request.header("origin");
        if origin.is_some_and(|origin| origin != DEVTOOLS_ORIGIN) {
            return Err(403);
        }
        let host = request
            .header("host")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if host != format!("127.0.0.1:{}", self.port) && host != format!("localhost:{}", self.port)
        {
            return Err(403);
        }
        let path = request.path.strip_prefix('/').ok_or(404u16)?;
        let (token, rest) = match path.find('/') {
            Some(end) => (&path[..end], &path[end..]),
            None => (path, "/"),
        };
        let mut found = None;
        for grant in &self.inner().grants {
            // Every token is compared, each in constant time.
            if same(grant.token.as_bytes(), token.as_bytes()) {
                found = Some(grant.clone());
            }
        }
        let grant = found.ok_or(404u16)?;
        if grant.internal != origin.is_some() {
            return Err(403);
        }
        let page_socket = rest.starts_with("/devtools/page/")
            && (request.header("upgrade")).is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        if grant.internal && !page_socket {
            return Err(404);
        }
        Ok((grant, rest.to_owned()))
    }

    fn serve(self: &Arc<Self>, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(HEAD_DEADLINE));
        let _ = stream.set_write_timeout(Some(WRITE_DEADLINE));
        let Ok((head, rest)) = read_head(&mut stream) else {
            return;
        };
        let Some(request) = parse_head(&head) else {
            let _ = respond(&mut stream, 400, "text/plain", "Bad request");
            return;
        };
        let (grant, path) = match self.admit(&request) {
            Ok(admitted) => admitted,
            Err(status) => {
                let _ = respond(&mut stream, status, "text/plain", "");
                return;
            }
        };
        let upgrade = request
            .header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        if upgrade {
            self.upgrade(stream, &request, grant, &path, rest);
            return;
        }
        let (status, body) = self.http(&request, &grant, &path);
        let _ = respond(
            &mut stream,
            status,
            "application/json; charset=UTF-8",
            &body,
        );
    }

    fn ws_base(&self, grant: &Grant) -> String {
        format!("127.0.0.1:{}/{}", self.port, grant.token)
    }

    /// A `/json/list` entry.
    fn listing(&self, grant: &Grant, target: &hub::Listed) -> Value {
        let ws = format!("{}/devtools/page/{}", self.ws_base(grant), target.id);
        json!({
            "description": "",
            "devtoolsFrontendUrl": format!("devtools://devtools/bundled/inspector.html?ws={ws}"),
            "id": target.id,
            "title": target.title,
            "type": "page",
            "url": target.url,
            "webSocketDebuggerUrl": format!("ws://{ws}"),
        })
    }

    fn http(&self, request: &Request, grant: &Grant, path: &str) -> (u16, String) {
        let scope = grant.scope.clone();
        let path = path
            .strip_suffix('/')
            .filter(|p| !p.is_empty())
            .unwrap_or(path);
        let unavailable = (503, json!({"error": "Monocode did not answer"}).to_string());
        match path {
            "/json/version" => {
                let Some(version) = self.ask(move |host| hub::version_of(host, scope)) else {
                    return unavailable;
                };
                let body = json!({
                    "Browser": version["product"],
                    "Protocol-Version": version["protocolVersion"],
                    "User-Agent": version["userAgent"],
                    "V8-Version": version["jsVersion"],
                    "WebKit-Version": "537.36",
                    "webSocketDebuggerUrl": format!(
                        "ws://{}/devtools/browser/{}",
                        self.ws_base(grant),
                        self.browser_id
                    ),
                });
                (200, body.to_string())
            }
            "/json" | "/json/list" => {
                let Some(targets) = self.ask(move |host| hub::list(host, scope)) else {
                    return unavailable;
                };
                let list: Vec<Value> = targets.iter().map(|t| self.listing(grant, t)).collect();
                (200, Value::Array(list).to_string())
            }
            "/json/new" => {
                let url = percent_decode(&request.query);
                let url = if url.is_empty() {
                    "about:blank".to_owned()
                } else {
                    url
                };
                match self.ask(move |host| hub::open_page(host, scope, url)) {
                    Some(Ok(target)) => (200, self.listing(grant, &target).to_string()),
                    Some(Err(error)) => (500, json!({"error": error}).to_string()),
                    None => unavailable,
                }
            }
            _ => {
                if let Some(id) = path.strip_prefix("/json/close/") {
                    let id = id.to_owned();
                    return match self.ask(move |host| hub::close_page(host, scope, id)) {
                        Some(true) => (200, "\"Target is closing\"".into()),
                        Some(false) => (404, "\"No such target id\"".into()),
                        None => unavailable,
                    };
                }
                if path.starts_with("/json/activate/") {
                    // Never steals the user's focus.
                    return (200, "\"Target activated\"".into());
                }
                (404, json!({"error": "Not found"}).to_string())
            }
        }
    }

    fn upgrade(
        self: &Arc<Self>,
        mut stream: TcpStream,
        request: &Request,
        grant: Grant,
        path: &str,
        rest: Vec<u8>,
    ) {
        let key = request.header("sec-websocket-key").unwrap_or_default();
        if request.method != "GET"
            || key.is_empty()
            || request.header("sec-websocket-version") != Some("13")
        {
            let _ = respond(&mut stream, 400, "text/plain", "Bad WebSocket request");
            return;
        }
        let kind = if path == "/devtools/browser" || path.starts_with("/devtools/browser/") {
            hub::Kind::Browser
        } else if let Some(id) = path
            .strip_prefix("/devtools/page/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
        {
            let (scope, wanted) = (grant.scope.clone(), id.to_owned());
            match self.ask(move |host| hub::has_target(host, scope, wanted)) {
                Some(true) => hub::Kind::Page(id.to_owned()),
                Some(false) => {
                    let _ = respond(&mut stream, 404, "text/plain", "No such target id");
                    return;
                }
                None => {
                    let _ = respond(&mut stream, 503, "text/plain", "Monocode did not answer");
                    return;
                }
            }
        } else {
            let _ = respond(&mut stream, 404, "text/plain", "");
            return;
        };
        let key = key.to_owned();
        ws::run(self.clone(), stream, &key, rest, grant, kind);
    }
}

/// A random 32-digit hex id, the shape of Chromium's target ids.
fn hex_id() -> String {
    format!("{:032X}", rand::random::<u128>())
}

/// Equal bytes, in time that depends only on the lengths.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Reads up to the end of the request head; returns the head and any bytes
/// after it.
fn read_head(stream: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let deadline = Instant::now() + HEAD_DEADLINE;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            return Ok((buffer, rest));
        }
        if buffer.len() > HEAD_LIMIT || Instant::now() > deadline {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
}

fn parse_head(head: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?;
    first.next()?.starts_with("HTTP/1.").then_some(())?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Some(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
