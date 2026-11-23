//! The endpoint against a fake host whose pages answer CDP from a table, run
//! by a small executor on a "main" thread of its own, with a real WebSocket
//! client on the test's thread.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    io::{Read as _, Write as _},
    net::TcpStream,
    pin::Pin,
    rc::{Rc, Weak},
    sync::{Arc, atomic::AtomicBool, mpsc},
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use futures_executor::LocalPool;
use futures_util::{FutureExt as _, future::LocalBoxFuture, task::LocalSpawnExt as _};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, protocol::Role};

use super::{Server, ws::accept_key};
use crate::browser::{
    CdpClient, DialogInfo, Host, Job, Page, PageId, PageInfo, Scope, log::PageLog,
};

const PROJECT: &str = "/work/shop";

fn scope() -> Scope {
    Scope {
        session: 1,
        project: PROJECT.into(),
    }
}

struct FakePage {
    id: PageId,
    target: String,
    project: String,
    url: RefCell<String>,
    title: RefCell<String>,
    /// Chromium gives clients sessions of their own.
    own_sessions: Cell<bool>,
    sessions: Cell<u32>,
    sent: RefCell<Vec<Value>>,
    cdp: RefCell<Option<Rc<CdpClient>>>,
    /// A restored tab's client, until `ensure_loaded` loads it.
    unloaded: RefCell<Option<Rc<CdpClient>>>,
    loads: Cell<u32>,
    drawn: Rc<Cell<i32>>,
}

struct Drawn(Rc<Cell<i32>>);

impl Drop for Drawn {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl FakePage {
    /// What the page sends back for `message`: events, then the answer.
    fn answer(&self, message: &Value) -> Vec<Value> {
        let method = message["method"].as_str().unwrap_or_default();
        let session = message.get("sessionId").cloned();
        let mut out = Vec::new();
        let result = match method {
            "Test.hang" => return out,
            "Target.getTargetInfo" => json!({"targetInfo": {
                "targetId": self.target, "type": "page", "url": *self.url.borrow(),
                "title": "", "attached": true, "canAccessOpener": false,
            }}),
            "Target.attachToTarget" if !self.own_sessions.get() => {
                let mut error = json!({"id": message["id"], "error": {"code": -32000, "message": "Not allowed"}});
                if let Some(s) = session {
                    error["sessionId"] = s;
                }
                return vec![error];
            }
            "Target.attachToTarget" => {
                self.sessions.set(self.sessions.get() + 1);
                let own = format!("OWN-{}-{}", self.id, self.sessions.get());
                out.push(json!({"method": "Target.attachedToTarget", "params": {
                    "sessionId": own, "targetInfo": {"targetId": self.target, "type": "page"},
                    "waitingForDebugger": false,
                }}));
                json!({"sessionId": own})
            }
            "Browser.getVersion" => json!({
                "protocolVersion": "1.3", "product": "Chrome/140.0.7339.5", "revision": "@abc",
                "userAgent": "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.7339.5 Safari/537.36",
                "jsVersion": "14.0",
            }),
            "Runtime.evaluate" => json!({"result": {"type": "number", "value": 2}}),
            "Network.getAllCookies" => json!({"cookies": [{"name": "a", "value": "1"}]}),
            _ => json!({}),
        };
        let mut reply = json!({"id": message["id"], "result": result});
        if let Some(s) = session {
            reply["sessionId"] = s;
        }
        out.push(reply);
        out
    }

    fn methods(&self) -> Vec<String> {
        self.sent
            .borrow()
            .iter()
            .map(|m| m["method"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

impl Page for FakePage {
    fn info(&self) -> PageInfo {
        PageInfo {
            id: self.id,
            url: self.url.borrow().clone(),
            title: self.title.borrow().clone(),
            loading: false,
            shown: false,
            by_agent: false,
            isolated: false,
        }
    }

    fn cdp(&self) -> Option<Rc<CdpClient>> {
        self.cdp.borrow().clone()
    }

    fn log(&self) -> Option<Rc<PageLog>> {
        None
    }

    fn ensure_loaded(&self) -> LocalBoxFuture<'static, Result<(), String>> {
        if let Some(cdp) = self.unloaded.take() {
            self.loads.set(self.loads.get() + 1);
            *self.cdp.borrow_mut() = Some(cdp);
        }
        async { Ok(()) }.boxed_local()
    }

    fn keep_drawn(&self) -> Box<dyn Any> {
        self.drawn.set(self.drawn.get() + 1);
        Box::new(Drawn(self.drawn.clone()))
    }

    fn dialog(&self) -> Option<DialogInfo> {
        None
    }

    fn answer_dialog(&self, _accept: bool, _text: Option<&str>) -> bool {
        false
    }

    fn user_active(&self) -> bool {
        false
    }
}

type Inbox = Rc<RefCell<VecDeque<(Weak<FakePage>, Value)>>>;

#[derive(Default)]
struct FakeHost {
    pages: RefCell<Vec<Rc<FakePage>>>,
    next: Cell<PageId>,
    inbox: Inbox,
    timers: Rc<RefCell<Vec<Waker>>>,
}

struct Sleep {
    until: Instant,
    timers: Rc<RefCell<Vec<Waker>>>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.until {
            return Poll::Ready(());
        }
        self.timers.borrow_mut().push(cx.waker().clone());
        Poll::Pending
    }
}

impl FakeHost {
    fn add(&self, project: &str, url: &str, title: &str) -> Rc<FakePage> {
        let id = self.next.get() + 1;
        self.next.set(id);
        let page = Rc::new(FakePage {
            id,
            target: format!("{:032X}", 0xA000 + id as u128),
            project: project.into(),
            url: RefCell::new(url.into()),
            title: RefCell::new(title.into()),
            own_sessions: Cell::new(true),
            sessions: Cell::new(0),
            sent: RefCell::default(),
            cdp: RefCell::default(),
            unloaded: RefCell::default(),
            loads: Cell::new(0),
            drawn: Rc::default(),
        });
        let (weak, inbox) = (Rc::downgrade(&page), self.inbox.clone());
        let cdp = CdpClient::new(Box::new(move |bytes| {
            let Some(page) = weak.upgrade() else {
                return false;
            };
            let message: Value = serde_json::from_slice(bytes).unwrap();
            page.sent.borrow_mut().push(message.clone());
            for answer in page.answer(&message) {
                inbox.borrow_mut().push_back((weak.clone(), answer));
            }
            true
        }));
        *page.cdp.borrow_mut() = Some(cdp);
        self.pages.borrow_mut().push(page.clone());
        page
    }

    /// A restored tab not loaded yet (K20): no CDP client until it loads.
    fn add_unloaded(&self, project: &str, url: &str, title: &str) -> Rc<FakePage> {
        let page = self.add(project, url, title);
        let cdp = page.cdp.borrow_mut().take();
        *page.unloaded.borrow_mut() = cdp;
        page
    }

    fn page(&self, id: PageId) -> Rc<FakePage> {
        self.pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .clone()
    }

    /// The page's DevTools agent goes, and the page with it.
    fn kill(&self, id: PageId) {
        let page = self.page(id);
        self.pages.borrow_mut().retain(|p| p.id != id);
        if let Some(cdp) = page.cdp() {
            cdp.on_message(None);
        }
    }

    /// Delivers what the pages sent back; true if there was anything.
    fn deliver(&self) -> bool {
        let mut any = false;
        loop {
            let next = self.inbox.borrow_mut().pop_front();
            let Some((page, message)) = next else {
                return any;
            };
            any = true;
            if let Some(cdp) = page.upgrade().and_then(|p| p.cdp()) {
                cdp.on_message(Some(message.to_string().as_bytes()));
            }
        }
    }

    /// An event from page `id`, on `session` or its root.
    fn emit(&self, id: PageId, method: &str, params: Value, session: Option<&str>) {
        let mut message = json!({"method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let page = Rc::downgrade(&self.page(id));
        self.inbox.borrow_mut().push_back((page, message));
    }
}

impl Host for FakeHost {
    fn engine_started(&self) -> bool {
        true
    }

    fn pages(&self, scope: &Scope) -> Vec<Rc<dyn Page>> {
        self.pages
            .borrow()
            .iter()
            .filter(|p| p.project == scope.project)
            .map(|p| p.clone() as Rc<dyn Page>)
            .collect()
    }

    fn open(
        &self,
        scope: &Scope,
        url: &str,
        _isolated: bool,
    ) -> LocalBoxFuture<'static, Result<Rc<dyn Page>, String>> {
        let page = self.add(&scope.project, url, "");
        async move { Ok(page as Rc<dyn Page>) }.boxed_local()
    }

    fn close(&self, _scope: &Scope, id: PageId) -> bool {
        let found = self.pages.borrow().iter().any(|p| p.id == id);
        if found {
            self.kill(id);
        }
        found
    }

    fn show(&self, _scope: &Scope, _id: PageId) -> bool {
        true
    }

    fn sleep(&self, duration: Duration) -> LocalBoxFuture<'static, ()> {
        Sleep {
            until: Instant::now() + duration,
            timers: self.timers.clone(),
        }
        .boxed_local()
    }
}

type Command = Box<dyn FnOnce(&Rc<FakeHost>) + Send>;

/// The endpoint, and the "main thread" that runs the fake host and the jobs.
struct Harness {
    server: Arc<Server>,
    base: String,
    commands: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
    main: Option<thread::JoinHandle<()>>,
}

impl Harness {
    fn new() -> Harness {
        let (jobs_tx, jobs) = mpsc::channel::<Job>();
        let (commands, commands_rx) = mpsc::channel::<Command>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let main = thread::spawn(move || {
            let host = Rc::new(FakeHost::default());
            let mut pool = LocalPool::new();
            let spawner = pool.spawner();
            while !stopping.load(std::sync::atomic::Ordering::Relaxed) {
                while let Ok(command) = commands_rx.try_recv() {
                    command(&host);
                }
                while let Ok(job) = jobs.try_recv() {
                    spawner
                        .spawn_local(job(host.clone() as Rc<dyn Host>))
                        .unwrap();
                }
                loop {
                    pool.run_until_stalled();
                    if !host.deliver() {
                        break;
                    }
                }
                let timers: Vec<Waker> = host.timers.borrow_mut().drain(..).collect();
                timers.into_iter().for_each(Waker::wake);
                thread::sleep(Duration::from_millis(1));
            }
        });
        let server = Server::start(Arc::new(move |job| jobs_tx.send(job).is_ok())).unwrap();
        let base = server.endpoint(&scope(), false);
        Harness {
            server,
            base,
            commands,
            stop,
            main: Some(main),
        }
    }

    /// Runs `work` on the main thread with the host.
    fn with<T: Send + 'static>(&self, work: impl FnOnce(&Rc<FakeHost>) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.commands
            .send(Box::new(move |host| {
                let _ = tx.send(work(host));
            }))
            .unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn add(
        &self,
        project: &'static str,
        url: &'static str,
        title: &'static str,
    ) -> (PageId, String) {
        self.with(move |host| {
            let page = host.add(project, url, title);
            (page.id, page.target.clone())
        })
    }

    fn add_unloaded(
        &self,
        project: &'static str,
        url: &'static str,
        title: &'static str,
    ) -> (PageId, String) {
        self.with(move |host| {
            let page = host.add_unloaded(project, url, title);
            (page.id, page.target.clone())
        })
    }

    fn loads(&self, id: PageId) -> u32 {
        self.with(move |host| host.page(id).loads.get())
    }

    fn port(&self) -> u16 {
        self.server.port
    }

    /// The path part of the base URL: `/<token>`.
    fn prefix(&self) -> String {
        self.base
            .trim_start_matches(&format!("http://127.0.0.1:{}", self.port()))
            .to_owned()
    }

    fn get(&self, path: &str) -> (u16, String) {
        http(self.port(), &format!("{}{path}", self.prefix()), &[])
    }

    fn browser(&self) -> Client {
        Client::connect(
            self.port(),
            &format!("{}/devtools/browser/x", self.prefix()),
            &[],
        )
        .unwrap()
    }

    /// Sent to page `id` so far.
    fn sent(&self, id: PageId) -> Vec<Value> {
        self.with(move |host| host.page(id).sent.borrow().clone())
    }

    fn methods(&self, id: PageId) -> Vec<String> {
        self.with(move |host| host.page(id).methods())
    }

    fn drawn(&self, id: PageId) -> i32 {
        self.with(move |host| host.page(id).drawn.get())
    }

    fn emit(&self, id: PageId, method: &'static str, params: Value, session: Option<String>) {
        self.with(move |host| host.emit(id, method, params, session.as_deref()));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(main) = self.main.take() {
            let _ = main.join();
        }
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn status_of(head: &str) -> u16 {
    head.split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// One HTTP request; `headers` replace or add to the usual ones.
fn http(port: u16, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        request += &format!("Host: {host}\r\n");
    }
    for (name, value) in headers {
        request += &format!("{name}: {value}\r\n");
    }
    request += "\r\n";
    stream.write_all(request.as_bytes()).unwrap();
    let head = read_head(&mut stream);
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    (status_of(&head), body)
}

/// A CDP client over a real WebSocket.
struct Client {
    socket: WebSocket<TcpStream>,
    /// Events read while waiting for an answer.
    events: VecDeque<Value>,
    next: i64,
}

impl Client {
    fn connect(port: u16, path: &str, headers: &[(&str, &str)]) -> Result<Client, u16> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
        );
        for (name, value) in headers {
            request += &format!("{name}: {value}\r\n");
        }
        request += "\r\n";
        stream.write_all(request.as_bytes()).unwrap();
        let head = read_head(&mut stream);
        if status_of(&head) != 101 {
            return Err(status_of(&head));
        }
        assert!(head.contains(&accept_key(key)), "{head}");
        Ok(Client {
            socket: WebSocket::from_raw_socket(stream, Role::Client, None),
            events: VecDeque::new(),
            next: 0,
        })
    }

    fn read(&mut self) -> Option<Value> {
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    fn send(&mut self, message: Value) {
        self.socket
            .send(Message::text(message.to_string()))
            .unwrap();
    }

    /// Sends a command and returns its answer, keeping the events that came
    /// first.
    fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Value {
        self.next += 1;
        let id = self.next;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        self.send(message);
        loop {
            let message = self
                .read()
                .expect("connection closed while waiting for an answer");
            if message["id"] == id {
                return message;
            }
            self.events.push_back(message);
        }
    }

    /// The first event (read already or to come) that has `method`.
    fn event(&mut self, method: &str) -> Value {
        if let Some(at) = self.events.iter().position(|e| e["method"] == method) {
            return self.events.remove(at).unwrap();
        }
        loop {
            let message = self
                .read()
                .unwrap_or_else(|| panic!("closed while waiting for {method}"));
            if message["method"] == method {
                return message;
            }
            self.events.push_back(message);
        }
    }

    fn no_event(&mut self, method: &str) {
        assert!(
            !self.events.iter().any(|e| e["method"] == method),
            "unexpected {method}: {:?}",
            self.events
        );
    }

    /// Like Playwright's `connectOverCDP`: returns the page sessions'
    /// attach events.
    fn playwright_connect(&mut self) -> Vec<Value> {
        let version = self.call("Browser.getVersion", json!({}), None);
        assert!(version["result"]["userAgent"].is_string(), "{version}");
        let attach = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
        let answer = self.call("Target.setAutoAttach", attach, None);
        assert_eq!(answer["result"], json!({}), "{answer}");
        // Chrome attaches the existing pages before it answers.
        let pages = self
            .events
            .iter()
            .filter(|e| e["method"] == "Target.attachedToTarget")
            .count();
        let download =
            json!({"behavior": "allowAndName", "downloadPath": "/tmp/x", "eventsEnabled": true});
        let answer = self.call("Browser.setDownloadBehavior", download, None);
        assert_eq!(answer["result"], json!({}));
        let answer = self.call("Target.getTargetInfo", json!({}), None);
        assert_eq!(answer["result"]["targetInfo"]["type"], "browser");
        let attached: Vec<Value> = self
            .events
            .iter()
            .filter(|e| e["method"] == "Target.attachedToTarget")
            .cloned()
            .collect();
        assert_eq!(attached.len(), pages);
        self.events
            .retain(|e| e["method"] != "Target.attachedToTarget");
        attached
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn requests_from_web_pages_rebound_hosts_and_without_the_token_are_refused() {
    let h = Harness::new();
    let port = h.port();
    let path = format!("{}/json/version", h.prefix());
    assert_eq!(http(port, &path, &[]).0, 200);
    let localhost = format!("localhost:{port}");
    assert_eq!(http(port, &path, &[("Host", &localhost)]).0, 200);
    assert_eq!(
        http(port, &path, &[("Origin", "https://evil.example")]).0,
        403
    );
    assert_eq!(http(port, &path, &[("Origin", "null")]).0, 403);
    let rebound = format!("evil.example:{port}");
    assert_eq!(http(port, &path, &[("Host", &rebound)]).0, 403);
    assert_eq!(http(port, &path, &[("Host", "127.0.0.1:9222")]).0, 403);
    assert_eq!(http(port, "/json/version", &[]).0, 404);
    let wrong = format!("/{}/json/version", "0".repeat(64));
    assert_eq!(http(port, &wrong, &[]).0, 404);
    let short = format!("{}/json/version", &h.prefix()[..20]);
    assert_eq!(http(port, &short, &[]).0, 404);

    let ws = format!("{}/devtools/browser/x", h.prefix());
    assert_eq!(
        Client::connect(port, &ws, &[("Origin", "https://evil.example")]).err(),
        Some(403)
    );
    assert_eq!(
        Client::connect(port, "/devtools/browser/x", &[]).err(),
        Some(404)
    );
    assert!(Client::connect(port, &ws, &[]).is_ok());
}

#[test]
fn discovery_lists_the_scopes_pages_with_their_target_ids() {
    let h = Harness::new();
    let (_, shop) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    h.add("/work/other", "http://localhost:4000/", "Other");

    for path in ["/json/version", "/json/version/"] {
        let (status, body) = h.get(path);
        assert_eq!(status, 200);
        let version: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(version["Browser"], "Chrome/140.0.7339.5");
        assert_eq!(version["Protocol-Version"], "1.3");
        assert!(
            version["User-Agent"]
                .as_str()
                .unwrap()
                .contains("Chrome/140")
        );
        let ws = version["webSocketDebuggerUrl"].as_str().unwrap();
        let expected = format!(
            "ws://127.0.0.1:{}{}/devtools/browser/",
            h.port(),
            h.prefix()
        );
        assert!(ws.starts_with(&expected), "{ws}");
    }
    for path in ["/json", "/json/", "/json/list", "/json/list/"] {
        let (status, body) = h.get(path);
        assert_eq!(status, 200, "{path}");
        let list: Vec<Value> = serde_json::from_str(&body).unwrap();
        assert_eq!(list.len(), 1, "{body}");
        assert_eq!(list[0]["id"], shop.as_str());
        assert_eq!(list[0]["type"], "page");
        assert_eq!(list[0]["url"], "http://localhost:3000/");
        assert_eq!(list[0]["title"], "Shop");
        let ws = format!(
            "ws://127.0.0.1:{}{}/devtools/page/{shop}",
            h.port(),
            h.prefix()
        );
        assert_eq!(list[0]["webSocketDebuggerUrl"], ws.as_str());
    }
    assert_eq!(h.get("/json/protocol").0, 404);
}

#[test]
fn playwright_connect_attaches_every_page_with_a_session_of_its_own() {
    let h = Harness::new();
    let (page, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let attached = client.playwright_connect();
    assert_eq!(attached.len(), 1, "{attached:?}");
    let params = &attached[0]["params"];
    assert_eq!(params["targetInfo"]["targetId"], target.as_str());
    assert_eq!(params["targetInfo"]["type"], "page");
    assert!(params["targetInfo"]["browserContextId"].is_string());
    assert_eq!(params["waitingForDebugger"], false);
    assert!(attached[0].get("sessionId").is_none());
    let session = params["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(h.drawn(page), 1);

    // What a page session sends goes to the client's own Chromium session.
    for method in [
        "Page.enable",
        "Runtime.enable",
        "Runtime.runIfWaitingForDebugger",
    ] {
        let answer = client.call(method, json!({}), Some(&session));
        assert_eq!(answer["result"], json!({}), "{answer}");
        assert_eq!(answer["sessionId"], session.as_str());
    }
    let answer = client.call(
        "Runtime.evaluate",
        json!({"expression": "1+1"}),
        Some(&session),
    );
    assert_eq!(answer["result"]["result"]["value"], 2);
    assert_eq!(answer["sessionId"], session.as_str());
    let sent = h.sent(page);
    let evaluate = sent
        .iter()
        .find(|m| m["method"] == "Runtime.evaluate")
        .unwrap();
    assert_eq!(evaluate["sessionId"], session.as_str());

    // Its events come tagged; the root session's (our tools') do not come.
    h.emit(
        page,
        "Runtime.consoleAPICalled",
        json!({"type": "root"}),
        None,
    );
    h.emit(
        page,
        "Runtime.consoleAPICalled",
        json!({"type": "own"}),
        Some(session.clone()),
    );
    let event = client.event("Runtime.consoleAPICalled");
    assert_eq!(event["params"]["type"], "own");
    assert_eq!(event["sessionId"], session.as_str());

    // Disabling its own session touches nothing of ours: it goes through.
    client.call("Network.disable", json!({}), Some(&session));
    assert!(h.methods(page).contains(&"Network.disable".to_owned()));

    drop(client);
    wait_until("the session to end", || {
        h.sent(page).iter().any(|m| {
            m["method"] == "Target.detachFromTarget" && m["params"]["sessionId"] == session.as_str()
        })
    });
    wait_until("the page to stop being kept drawn", || h.drawn(page) == 0);
}

#[test]
fn a_shared_root_session_keeps_our_domains_on() {
    let h = Harness::new();
    let (page, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    h.with(move |host| host.page(page).own_sessions.set(false));
    let mut client = h.browser();
    let attached = client.playwright_connect();
    let session = attached[0]["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();

    let answer = client.call(
        "Runtime.evaluate",
        json!({"expression": "1+1"}),
        Some(&session),
    );
    assert_eq!(answer["result"]["result"]["value"], 2);
    assert_eq!(answer["sessionId"], session.as_str());
    let sent = h.sent(page);
    let evaluate = sent
        .iter()
        .find(|m| m["method"] == "Runtime.evaluate")
        .unwrap();
    assert!(evaluate.get("sessionId").is_none(), "{evaluate}");

    for method in [
        "Runtime.disable",
        "Network.disable",
        "Log.disable",
        "Page.disable",
        "DOM.disable",
        "Accessibility.disable",
    ] {
        let answer = client.call(method, json!({}), Some(&session));
        assert_eq!(answer["result"], json!({}));
        assert_eq!(answer["sessionId"], session.as_str());
    }
    let methods = h.methods(page);
    assert!(
        !methods.iter().any(|m| m.ends_with(".disable")),
        "{methods:?}"
    );

    // Enabling Runtime again must report the contexts: off, then on.
    client.call("Runtime.enable", json!({}), Some(&session));
    let methods = h.methods(page);
    let at = methods.iter().position(|m| m == "Runtime.enable").unwrap();
    assert_eq!(methods[at - 1], "Runtime.disable");

    let attach = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    client.call("Target.setAutoAttach", attach, Some(&session));
    let sent = h.sent(page);
    let auto = sent
        .iter()
        .rfind(|m| m["method"] == "Target.setAutoAttach")
        .unwrap();
    assert_eq!(auto["params"]["waitForDebuggerOnStart"], false);
    assert_eq!(auto["params"]["flatten"], true);
    // Our tools auto-attach on the page too: a client never turns it off.
    let off = json!({"autoAttach": false, "waitForDebuggerOnStart": false});
    let answer = client.call("Target.setAutoAttach", off, Some(&session));
    assert_eq!(answer["result"], json!({}));
    let sent = h.sent(page);
    let autos: Vec<_> = sent
        .iter()
        .filter(|m| m["method"] == "Target.setAutoAttach")
        .collect();
    assert_eq!(autos.len(), 1, "{autos:?}");

    // Root events reach it tagged, but not those about our own sessions.
    let about_self = json!({"sessionId": "X", "targetInfo": {"targetId": target}});
    h.emit(page, "Target.attachedToTarget", about_self, None);
    h.emit(page, "Page.frameNavigated", json!({"frame": {}}), None);
    let event = client.event("Page.frameNavigated");
    assert_eq!(event["sessionId"], session.as_str());
    client.no_event("Target.attachedToTarget");
}

#[test]
fn child_sessions_pass_through_with_their_own_ids() {
    let h = Harness::new();
    let (page, _) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let session = client.playwright_connect()[0]["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let iframe = json!({
        "sessionId": "IFRAME-1",
        "targetInfo": {"targetId": "F1", "type": "iframe"},
        "waitingForDebugger": true,
    });
    h.emit(
        page,
        "Target.attachedToTarget",
        iframe,
        Some(session.clone()),
    );
    let event = client.event("Target.attachedToTarget");
    assert_eq!(event["sessionId"], session.as_str());

    // Nothing waits for a debugger: resuming is answered here.
    let answer = client.call(
        "Runtime.runIfWaitingForDebugger",
        json!({}),
        Some("IFRAME-1"),
    );
    assert_eq!(answer["result"], json!({}));
    assert_eq!(answer["sessionId"], "IFRAME-1");
    let attach = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    client.call("Target.setAutoAttach", attach, Some("IFRAME-1"));
    let sent = h.sent(page);
    assert!(
        !sent
            .iter()
            .any(|m| m["method"] == "Runtime.runIfWaitingForDebugger"
                && m["sessionId"] == "IFRAME-1")
    );
    let auto = sent
        .iter()
        .rfind(|m| m["method"] == "Target.setAutoAttach")
        .unwrap();
    assert_eq!(auto["sessionId"], "IFRAME-1");
    assert_eq!(auto["params"]["waitForDebuggerOnStart"], false);

    let answer = client.call("Runtime.evaluate", json!({}), Some("IFRAME-1"));
    assert_eq!(answer["sessionId"], "IFRAME-1");
    let sent = h.sent(page);
    let evaluate = sent
        .iter()
        .rfind(|m| m["method"] == "Runtime.evaluate")
        .unwrap();
    assert_eq!(evaluate["sessionId"], "IFRAME-1");

    h.emit(
        page,
        "Runtime.executionContextCreated",
        json!({}),
        Some("IFRAME-1".into()),
    );
    h.emit(
        page,
        "Runtime.executionContextCreated",
        json!({}),
        Some("SOMEONE-ELSE".into()),
    );
    let event = client.event("Runtime.executionContextCreated");
    assert_eq!(event["sessionId"], "IFRAME-1");

    let answer = client.call("Runtime.evaluate", json!({}), Some("SOMEONE-ELSE"));
    assert_eq!(answer["error"]["code"], -32001);
    client.no_event("Runtime.executionContextCreated");
}

#[test]
fn new_pages_are_announced_before_create_target_answers_and_close_target_closes_them() {
    let h = Harness::new();
    h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    client.playwright_connect();
    let answer = client.call("Target.setDiscoverTargets", json!({"discover": true}), None);
    assert_eq!(answer["result"], json!({}));
    assert_eq!(
        client.event("Target.targetCreated")["params"]["targetInfo"]["type"],
        "page"
    );

    let answer = client.call("Target.createTarget", json!({"url": "about:blank"}), None);
    let target = answer["result"]["targetId"].as_str().unwrap().to_owned();
    // Playwright looks the page up as soon as the answer comes.
    let attached = client.events.iter().find(|e| {
        e["method"] == "Target.attachedToTarget"
            && e["params"]["targetInfo"]["targetId"] == target.as_str()
    });
    let session = attached.expect("attached before the answer")["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = client.event("Target.targetCreated");
    assert_eq!(created["params"]["targetInfo"]["targetId"], target.as_str());
    assert_eq!(created["params"]["targetInfo"]["url"], "about:blank");
    let answer = client.call("Runtime.evaluate", json!({}), Some(&session));
    assert_eq!(answer["result"]["result"]["value"], 2);

    let answer = client.call("Target.closeTarget", json!({"targetId": target}), None);
    assert_eq!(answer["result"]["success"], true);
    let detached = client.event("Target.detachedFromTarget");
    assert_eq!(detached["params"]["sessionId"], session.as_str());
    assert_eq!(detached["params"]["targetId"], target.as_str());
    let destroyed = client.event("Target.targetDestroyed");
    assert_eq!(destroyed["params"]["targetId"], target.as_str());

    let pages = h.with(|host| host.pages.borrow().len());
    assert_eq!(pages, 1);
    let answer = client.call("Target.closeTarget", json!({"targetId": target}), None);
    assert_eq!(answer["error"]["code"], -32602);
}

#[test]
fn focus_stealing_and_browser_wide_commands_are_swallowed_or_refused() {
    let h = Harness::new();
    let (page, _) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let session = client.playwright_connect()[0]["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();

    let answer = client.call("Page.bringToFront", json!({}), Some(&session));
    assert_eq!(answer["result"], json!({}));
    assert!(!h.methods(page).contains(&"Page.bringToFront".to_owned()));
    let answer = client.call("Target.activateTarget", json!({"targetId": "x"}), None);
    assert_eq!(answer["result"], json!({}));

    assert!(client.call("Browser.close", json!({}), None)["error"].is_object());
    assert!(client.call("Browser.close", json!({}), Some(&session))["error"].is_object());
    assert!(client.call("Target.createBrowserContext", json!({}), None)["error"].is_object());
    assert!(client.call("Target.disposeBrowserContext", json!({}), None)["error"].is_object());
    let contexts = client.call("Target.getBrowserContexts", json!({}), None);
    assert_eq!(contexts["result"]["browserContextIds"], json!([]));
    let answer = client.call(
        "Target.createTarget",
        json!({"url": "about:blank"}),
        Some(&session),
    );
    assert!(answer["error"].is_object(), "{answer}");
    let answer = client.call("Target.getTargets", json!({}), Some(&session));
    assert!(answer["error"].is_object(), "{answer}");
    assert_eq!(
        client.call("Unknown.method", json!({}), None)["error"]["code"],
        -32601
    );
    let methods = h.methods(page);
    assert!(
        !methods
            .iter()
            .any(|m| m.starts_with("Browser.close") || m == "Target.createTarget")
    );

    let cookies = client.call("Storage.getCookies", json!({}), None);
    assert_eq!(cookies["result"]["cookies"][0]["name"], "a");
}

#[test]
fn a_page_that_goes_detaches_its_sessions_and_fails_what_it_owed() {
    let h = Harness::new();
    let (page, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let session = client.playwright_connect()[0]["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    client.call("Target.setDiscoverTargets", json!({"discover": true}), None);
    client.send(json!({"id": 99, "method": "Test.hang", "sessionId": session}));
    wait_until("the hanging call to reach the page", || {
        h.methods(page).contains(&"Test.hang".to_owned())
    });
    h.with(move |host| host.kill(page));

    let answer = loop {
        let message = client.read().unwrap();
        if message["id"] == 99 {
            break message;
        }
        client.events.push_back(message);
    };
    assert_eq!(answer["error"]["code"], -32000);
    assert_eq!(answer["sessionId"], session.as_str());
    let detached = client.event("Target.detachedFromTarget");
    assert_eq!(detached["params"]["sessionId"], session.as_str());
    assert_eq!(detached["params"]["targetId"], target.as_str());
    let destroyed = client.event("Target.targetDestroyed");
    assert_eq!(destroyed["params"]["targetId"], target.as_str());
    let answer = client.call("Runtime.evaluate", json!({}), Some(&session));
    assert_eq!(answer["error"]["code"], -32001);
}

#[test]
fn a_page_connection_speaks_without_session_ids() {
    let h = Harness::new();
    let (page, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let path = format!("{}/devtools/page/{target}", h.prefix());
    let mut client = Client::connect(h.port(), &path, &[]).unwrap();
    let answer = client.call("Runtime.evaluate", json!({"expression": "1+1"}), None);
    assert_eq!(answer["result"]["result"]["value"], 2);
    assert!(answer.get("sessionId").is_none(), "{answer}");
    let own = h
        .sent(page)
        .iter()
        .find(|m| m["method"] == "Runtime.evaluate")
        .unwrap()["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    h.emit(page, "Page.loadEventFired", json!({}), Some(own));
    let event = client.event("Page.loadEventFired");
    assert!(event.get("sessionId").is_none());

    let other = format!("{}/devtools/page/{}", h.prefix(), "F".repeat(32));
    assert_eq!(Client::connect(h.port(), &other, &[]).err(), Some(404));

    // Chrome closes a page's socket when the page goes.
    h.with(move |host| host.kill(page));
    assert!(client.read().is_none());
}

#[test]
fn the_apps_devtools_reach_only_page_sockets_and_outlive_the_setting() {
    let h = Harness::new();
    let (id, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let frontend_session = move |h: &Harness| {
        h.with(move |host| {
            let cdp = host.page(id).cdp.borrow().clone().unwrap();
            super::devtools_session(&cdp)
        })
    };
    let port = h.port();
    let base = h.server.endpoint(&scope(), true);
    assert_ne!(base, h.base);
    let prefix = base.trim_start_matches(&format!("http://127.0.0.1:{port}"));
    let page = format!("{prefix}/devtools/page/{target}");
    let frontend = [("Origin", "devtools://devtools")];
    // Only the DevTools frontend, only its page socket.
    assert_eq!(Client::connect(port, &page, &[]).err(), Some(403));
    let web = [("Origin", "https://evil.example")];
    assert_eq!(Client::connect(port, &page, &web).err(), Some(403));
    let browser = format!("{prefix}/devtools/browser/x");
    assert_eq!(Client::connect(port, &browser, &frontend).err(), Some(404));
    let listing = format!("{prefix}/json/list");
    assert_eq!(http(port, &listing, &frontend).0, 404);
    assert_eq!(http(port, &listing, &[]).0, 403);
    // The external token is not the frontend's.
    let external = format!("{}/devtools/page/{target}", h.prefix());
    assert_eq!(Client::connect(port, &external, &frontend).err(), Some(403));

    assert_eq!(frontend_session(&h), None);
    let mut client = Client::connect(port, &page, &frontend).unwrap();
    let answer = client.call("Runtime.evaluate", json!({"expression": "1+1"}), None);
    assert_eq!(answer["result"]["result"]["value"], 2);
    // Its own session on the page, where its `$0` is.
    let own = frontend_session(&h).expect("the frontend's session");
    assert!(own.starts_with("OWN-"), "{own}");
    // External connections turned off: the app's DevTools carry on.
    let mut outside = h.browser();
    h.server.close_all();
    assert!(outside.read().is_none());
    assert_eq!(h.get("/json/version").0, 404);
    let answer = client.call("Runtime.evaluate", json!({"expression": "1+1"}), None);
    assert_eq!(answer["result"]["result"]["value"], 2);
    assert_eq!(h.server.endpoint(&scope(), true), base);
    assert!(Client::connect(port, &page, &frontend).is_ok());
}

#[test]
fn puppeteer_gets_tab_targets_whose_sessions_attach_the_page() {
    let h = Harness::new();
    let (_, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let discover = json!({"discover": true, "filter": [{}]});
    client.call("Target.setDiscoverTargets", discover, None);
    let first = client.event("Target.targetCreated");
    let second = client.event("Target.targetCreated");
    assert_eq!(first["params"]["targetInfo"]["type"], "tab");
    assert_eq!(second["params"]["targetInfo"]["type"], "page");

    let auto = json!({
        "waitForDebuggerOnStart": true, "flatten": true, "autoAttach": true,
        "filter": [{"type": "page", "exclude": true}, {}],
    });
    client.call("Target.setAutoAttach", auto, None);
    let tab = client.event("Target.attachedToTarget");
    assert_eq!(tab["params"]["targetInfo"]["type"], "tab");
    let tab_session = tab["params"]["sessionId"].as_str().unwrap().to_owned();

    let auto = json!({"waitForDebuggerOnStart": true, "flatten": true, "autoAttach": true, "filter": [{}]});
    let answer = client.call("Target.setAutoAttach", auto, Some(&tab_session));
    assert_eq!(answer["result"], json!({}));
    let page = client.event("Target.attachedToTarget");
    assert_eq!(page["sessionId"], tab_session.as_str());
    assert_eq!(page["params"]["targetInfo"]["targetId"], target.as_str());
    let session = page["params"]["sessionId"].as_str().unwrap().to_owned();
    let answer = client.call(
        "Runtime.runIfWaitingForDebugger",
        json!({}),
        Some(&tab_session),
    );
    assert_eq!(answer["result"], json!({}));
    let answer = client.call("Runtime.evaluate", json!({}), Some(&session));
    assert_eq!(answer["result"]["result"]["value"], 2);
}

#[test]
fn browser_sessions_attach_pages_like_new_cdp_session() {
    let h = Harness::new();
    let (page, target) = h.add(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let answer = client.call("Target.attachToBrowserTarget", json!({}), None);
    let browser = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    let attach = json!({"targetId": target, "flatten": true});
    let answer = client.call("Target.attachToTarget", attach, Some(&browser));
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    let event = client.event("Target.attachedToTarget");
    assert_eq!(event["params"]["targetInfo"]["type"], "browser");
    let event = client.event("Target.attachedToTarget");
    assert_eq!(event["sessionId"], browser.as_str());
    assert_eq!(event["params"]["sessionId"], session.as_str());
    assert_eq!(h.drawn(page), 1);

    let detach = json!({"sessionId": session});
    let answer = client.call("Target.detachFromTarget", detach, Some(&browser));
    assert_eq!(answer["result"], json!({}));
    let event = client.event("Target.detachedFromTarget");
    assert_eq!(event["sessionId"], browser.as_str());
    wait_until("the lease to end", || h.drawn(page) == 0);
    let answer = client.call("Runtime.evaluate", json!({}), Some(&session));
    assert_eq!(answer["error"]["code"], -32001);
}

#[test]
fn turning_off_closes_clients_and_forgets_tokens() {
    let h = Harness::new();
    let mut client = h.browser();
    assert_eq!(h.get("/json/version").0, 200);
    h.server.close_all();
    assert!(client.read().is_none());
    assert_eq!(h.get("/json/version").0, 404);
    let fresh = h.server.endpoint(&scope(), false);
    assert_ne!(fresh, h.base);
}

#[test]
fn the_public_endpoint_follows_the_setting() {
    super::set_enabled(false);
    assert!(super::endpoint(&scope()).is_err());
    super::set_enabled(true);
    let first = super::endpoint(&scope()).unwrap();
    assert!(first.starts_with("http://127.0.0.1:"), "{first}");
    assert_eq!(super::endpoint(&scope()).unwrap(), first);
    let token = first.rsplit('/').next().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let other = Scope {
        session: 2,
        project: "/work/other".into(),
    };
    assert_ne!(super::endpoint(&other).unwrap(), first);
    super::set_enabled(false);
    assert!(super::endpoint(&scope()).is_err());
}

#[test]
fn small_pieces() {
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
    assert!(super::same(b"abc", b"abc"));
    assert!(!super::same(b"abc", b"abd"));
    assert!(!super::same(b"abc", b"ab"));
    assert_eq!(
        super::percent_decode("http%3A%2F%2Fa.b%2F%zz"),
        "http://a.b/%zz"
    );
    let request =
        super::parse_head(b"GET /t/json?x=1 HTTP/1.1\r\nHost: a\r\nX-Y:  z \r\n\r\n").unwrap();
    assert_eq!(
        (request.path.as_str(), request.query.as_str()),
        ("/t/json", "x=1")
    );
    assert_eq!(request.header("x-y"), Some("z"));
    assert!(super::parse_head(b"nonsense\r\n\r\n").is_none());
}

#[test]
fn restored_tabs_are_listed_before_they_load_and_load_when_attached() {
    let h = Harness::new();
    let (page, target) = h.add_unloaded(PROJECT, "http://localhost:3000/cart", "Cart");
    let stand_in = format!("MONOCODE-UNLOADED-{page}");

    // Listed with the saved address and title, under a stand-in id.
    let (_, body) = h.get("/json/list");
    let list: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(list.len(), 1, "{body}");
    assert_eq!(list[0]["id"], stand_in.as_str());
    assert_eq!(list[0]["url"], "http://localhost:3000/cart");
    assert_eq!(list[0]["title"], "Cart");
    let mut client = h.browser();
    let answer = client.call("Target.getTargets", json!({}), None);
    let infos = answer["result"]["targetInfos"].as_array().unwrap().clone();
    assert_eq!(infos.len(), 1, "{answer}");
    assert_eq!(infos[0]["targetId"], stand_in.as_str());
    assert_eq!(infos[0]["attached"], false);
    let answer = client.call("Target.setDiscoverTargets", json!({"discover": true}), None);
    assert_eq!(answer["result"], json!({}));
    let created = client.event("Target.targetCreated");
    assert_eq!(
        created["params"]["targetInfo"]["targetId"],
        stand_in.as_str()
    );
    assert_eq!(h.loads(page), 0);

    // Playwright's connect attaches every page: the tab loads, and is
    // attached by Chromium's id.
    let attached = client.playwright_connect();
    assert_eq!(attached.len(), 1, "{attached:?}");
    assert_eq!(
        attached[0]["params"]["targetInfo"]["targetId"],
        target.as_str()
    );
    assert_eq!(h.loads(page), 1);
    let session = attached[0]["params"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let answer = client.call(
        "Runtime.evaluate",
        json!({"expression": "1+1"}),
        Some(&session),
    );
    assert_eq!(answer["result"]["result"]["value"], 2);
    // The stand-in goes for the real target.
    let destroyed = client.event("Target.targetDestroyed");
    assert_eq!(destroyed["params"]["targetId"], stand_in.as_str());
    let (_, body) = h.get("/json/list");
    let list: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(list[0]["id"], target.as_str(), "{body}");

    // A page the client saw loaded that loses its browser (a crashed
    // renderer) is not loaded again behind the user's back.
    h.with(move |host| {
        let page = host.page(page);
        let cdp = page.cdp.borrow_mut().take();
        *page.unloaded.borrow_mut() = cdp;
    });
    thread::sleep(Duration::from_millis(700));
    assert_eq!(h.loads(page), 1);
}

#[test]
fn a_restored_tab_loads_for_attach_to_target_and_its_own_socket() {
    let h = Harness::new();
    let (first, target) = h.add_unloaded(PROJECT, "http://localhost:3000/", "Shop");
    let mut client = h.browser();
    let stand_in = format!("MONOCODE-UNLOADED-{first}");
    let answer = client.call(
        "Target.attachToTarget",
        json!({"targetId": stand_in, "flatten": true}),
        None,
    );
    let session = answer["result"]["sessionId"].as_str().unwrap().to_owned();
    let event = client.event("Target.attachedToTarget");
    assert_eq!(event["params"]["sessionId"], session.as_str());
    assert_eq!(event["params"]["targetInfo"]["targetId"], target.as_str());
    assert_eq!(h.loads(first), 1);

    let (second, _) = h.add_unloaded(PROJECT, "http://localhost:3000/b", "B");
    let path = format!("{}/devtools/page/MONOCODE-UNLOADED-{second}", h.prefix());
    let mut page = Client::connect(h.port(), &path, &[]).unwrap();
    let answer = page.call("Runtime.evaluate", json!({"expression": "1+1"}), None);
    assert_eq!(answer["result"]["result"]["value"], 2, "{answer}");
    assert_eq!(h.loads(second), 1);
    // The socket stays open after the stand-in id is gone.
    thread::sleep(Duration::from_millis(700));
    let answer = page.call("Runtime.evaluate", json!({"expression": "1+1"}), None);
    assert_eq!(answer["result"]["result"]["value"], 2, "{answer}");

    // Closing by the stand-in id closes the page without loading it.
    let (third, _) = h.add_unloaded(PROJECT, "http://localhost:3000/c", "C");
    let answer = client.call(
        "Target.closeTarget",
        json!({"targetId": format!("MONOCODE-UNLOADED-{third}")}),
        None,
    );
    assert_eq!(answer["result"]["success"], true, "{answer}");
}
