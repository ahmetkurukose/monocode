//! Test doubles: a scripted CDP peer, a fake host and pages, a fake clock,
//! and a driver that runs many calls at once on one thread.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    future::Future,
    path::PathBuf,
    pin::Pin,
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

use futures_util::future::LocalBoxFuture;
use serde_json::{Value, json};

use crate::browser::{
    Call, CdpClient, DialogInfo, Host, Page, PageId, PageInfo, Scope, ToolOutput, UserControl,
    log::PageLog,
};

type Handler = Rc<dyn Fn(&Value, Option<&str>) -> Result<Value, String>>;

/// Answers CDP calls from a table; unknown methods answer `{}`.
#[derive(Default)]
pub struct Script {
    handlers: RefCell<Vec<(String, Handler)>>,
    sent: RefCell<Vec<Value>>,
    held_methods: RefCell<Vec<String>>,
    held: RefCell<Vec<Value>>,
    client: RefCell<Weak<CdpClient>>,
}

impl Script {
    pub fn client() -> (Rc<CdpClient>, Rc<Script>) {
        let script = Rc::new(Script::default());
        let peer = script.clone();
        let client = CdpClient::new(Box::new(move |bytes| {
            let message: Value = serde_json::from_slice(bytes).unwrap();
            peer.sent.borrow_mut().push(message.clone());
            let method = message["method"].as_str().unwrap_or("").to_owned();
            if peer.held_methods.borrow().contains(&method) {
                peer.held.borrow_mut().push(message);
            } else {
                peer.answer(&message);
            }
            true
        }));
        *script.client.borrow_mut() = Rc::downgrade(&client);
        (client, script)
    }

    /// Answers `method` with `handler` (the newest one wins).
    pub fn on(
        &self,
        method: &str,
        handler: impl Fn(&Value, Option<&str>) -> Result<Value, String> + 'static,
    ) {
        self.handlers
            .borrow_mut()
            .push((method.to_owned(), Rc::new(handler)));
    }

    /// Answers `method` with `result` always.
    pub fn reply(&self, method: &str, result: Value) {
        self.on(method, move |_, _| Ok(result.clone()));
    }

    /// Keeps `method`'s calls unanswered until [`Script::release`].
    pub fn hold(&self, method: &str) {
        self.held_methods.borrow_mut().push(method.to_owned());
    }

    pub fn release(&self, method: &str) {
        self.held_methods.borrow_mut().retain(|m| m != method);
        let held: Vec<Value> = std::mem::take(&mut *self.held.borrow_mut());
        for message in held {
            if message["method"] == method {
                self.answer(&message);
            } else {
                self.held.borrow_mut().push(message);
            }
        }
    }

    fn answer(&self, message: &Value) {
        let method = message["method"].as_str().unwrap_or("");
        let handler = self
            .handlers
            .borrow()
            .iter()
            .rev()
            .find(|(m, _)| m == method)
            .map(|(_, h)| h.clone());
        let session = message["sessionId"].as_str();
        let result = match handler {
            Some(handler) => handler(&message["params"], session),
            None => Ok(json!({})),
        };
        let mut answer = match result {
            Ok(result) => json!({"id": message["id"], "result": result}),
            Err(error) => json!({"id": message["id"], "error": {"code": -32000, "message": error}}),
        };
        if let Some(session) = session {
            answer["sessionId"] = session.into();
        }
        if let Some(client) = self.client.borrow().upgrade() {
            client.on_message(Some(answer.to_string().as_bytes()));
        }
    }

    pub fn sent(&self) -> Vec<Value> {
        self.sent.borrow().clone()
    }

    pub fn methods(&self) -> Vec<String> {
        self.sent
            .borrow()
            .iter()
            .map(|m| m["method"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    /// The params of every call of `method`.
    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.sent
            .borrow()
            .iter()
            .filter(|m| m["method"] == method)
            .map(|m| m["params"].clone())
            .collect()
    }

    pub fn emit(&self, method: &str, params: Value) {
        self.send_event(json!({"method": method, "params": params}));
    }

    pub fn emit_in(&self, session: &str, method: &str, params: Value) {
        self.send_event(json!({"method": method, "params": params, "sessionId": session}));
    }

    fn send_event(&self, event: Value) {
        let client = self.client.borrow().upgrade();
        if let Some(client) = client {
            client.on_message(Some(event.to_string().as_bytes()));
        }
    }

    /// The page's DevTools agent goes away.
    pub fn gone(&self) {
        let client = self.client.borrow().upgrade();
        if let Some(client) = client {
            client.on_message(None);
        }
    }
}

/// Time that moves only when the driver says so.
#[derive(Default)]
pub struct Clock {
    now: Cell<u64>,
    timers: RefCell<Vec<(u64, async_channel::Sender<()>)>>,
}

impl Clock {
    pub fn now(&self) -> u64 {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) -> LocalBoxFuture<'static, ()> {
        let (tx, rx) = async_channel::bounded(1);
        self.timers
            .borrow_mut()
            .push((self.now.get() + duration.as_millis() as u64, tx));
        Box::pin(async move {
            let _ = rx.recv().await;
        })
    }

    /// Moves to the next live timer and fires every timer due; false when
    /// there is none.
    fn advance(&self) -> bool {
        let mut timers = self.timers.borrow_mut();
        timers.retain(|(_, tx)| !tx.is_closed());
        let Some(next) = timers.iter().map(|(at, _)| *at).min() else {
            return false;
        };
        self.now.set(next.max(self.now.get()));
        let now = self.now.get();
        timers.retain(|(at, tx)| {
            if *at <= now {
                let _ = tx.try_send(());
                false
            } else {
                true
            }
        });
        true
    }
}

/// A finished download: a file name and its bytes, or why it failed.
pub type Download = Result<(String, Vec<u8>), String>;

pub struct FakePage {
    pub info: RefCell<PageInfo>,
    pub cdp: RefCell<Option<Rc<CdpClient>>>,
    pub log: RefCell<Option<Rc<PageLog>>>,
    pub script: Rc<Script>,
    pub dialog: RefCell<Option<DialogInfo>>,
    pub answered: RefCell<Vec<(bool, Option<String>)>>,
    pub user_active: Cell<bool>,
    pub user_control: Cell<Option<UserControl>>,
    /// The scopes `agent_used` was called with, in order.
    pub agent_uses: RefCell<Vec<Scope>>,
    /// What `expect_download` resolves with: a file name it writes into
    /// the folder (with these bytes), or an error; `None` never resolves.
    pub download: RefCell<Option<Download>>,
    /// The folders `expect_download` was given.
    pub download_dirs: RefCell<Vec<PathBuf>>,
    pub leases: Rc<Cell<usize>>,
    pub load_error: RefCell<Option<String>>,
    /// The CDP client of a page not loaded yet: `ensure_loaded` gives it.
    pub cold: RefCell<Option<Rc<CdpClient>>>,
}

impl FakePage {
    /// A page on `https://example.com/` answering the calls every tool makes.
    pub fn new(id: PageId) -> Rc<FakePage> {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        script.reply(
            "Page.getFrameTree",
            json!({"frameTree": {"frame": {"id": "F", "loaderId": "L1", "url": "https://example.com/"}}}),
        );
        script.on("Runtime.evaluate", |p, _| {
            let expression = p["expression"].as_str().unwrap_or("");
            Ok(if expression.starts_with("[location.href") {
                json!({"result": {"type": "object", "value": ["https://example.com/", "Example", "complete"]}})
            } else {
                json!({"result": {"type": "undefined"}})
            })
        });
        script.on("DOM.resolveNode", |p, _| {
            Ok(json!({"object": {"objectId": format!("obj-{}", p["backendNodeId"])}}))
        });
        script.on("Runtime.callFunctionOn", |p, _| {
            let function = p["functionDeclaration"].as_str().unwrap_or("");
            Ok(if function.contains("isConnected") {
                json!({"result": {"type": "boolean", "value": true}})
            } else {
                json!({"result": {"type": "undefined"}})
            })
        });
        script.reply(
            "DOM.getBoxModel",
            json!({"model": {"content": [0, 0, 100, 0, 100, 50, 0, 50]}}),
        );
        script.reply(
            "Page.getLayoutMetrics",
            json!({
                "cssVisualViewport": {"pageX": 0, "pageY": 0, "clientWidth": 1280, "clientHeight": 800},
                "cssContentSize": {"width": 1280, "height": 3000},
            }),
        );
        script.reply("Accessibility.getFullAXTree", json!({"nodes": []}));
        Rc::new(FakePage {
            info: RefCell::new(PageInfo {
                id,
                url: "https://example.com/".into(),
                title: "Example".into(),
                loading: false,
                shown: false,
                by_agent: false,
                isolated: false,
                discarded: false,
            }),
            cdp: RefCell::new(Some(cdp)),
            log: RefCell::new(Some(log)),
            script,
            dialog: RefCell::default(),
            answered: RefCell::default(),
            user_active: Cell::new(false),
            user_control: Cell::new(None),
            agent_uses: RefCell::default(),
            download: RefCell::default(),
            download_dirs: RefCell::default(),
            leases: Rc::default(),
            load_error: RefCell::default(),
            cold: RefCell::default(),
        })
    }
}

struct Lease(Rc<Cell<usize>>);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl Page for FakePage {
    fn info(&self) -> PageInfo {
        self.info.borrow().clone()
    }

    fn cdp(&self) -> Option<Rc<CdpClient>> {
        self.cdp.borrow().clone()
    }

    fn log(&self) -> Option<Rc<PageLog>> {
        self.log.borrow().clone()
    }

    fn ensure_loaded(&self) -> LocalBoxFuture<'static, Result<(), String>> {
        let result = match self.load_error.borrow().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        };
        if result.is_ok() {
            if let Some(cdp) = self.cold.borrow_mut().take() {
                *self.cdp.borrow_mut() = Some(cdp);
            }
        }
        Box::pin(async move { result })
    }

    fn keep_drawn(&self) -> Box<dyn Any> {
        self.leases.set(self.leases.get() + 1);
        Box::new(Lease(self.leases.clone()))
    }

    fn dialog(&self) -> Option<DialogInfo> {
        self.dialog.borrow().clone()
    }

    fn answer_dialog(&self, accept: bool, text: Option<&str>) -> bool {
        if self.dialog.borrow_mut().take().is_none() {
            return false;
        }
        self.answered
            .borrow_mut()
            .push((accept, text.map(str::to_owned)));
        true
    }

    fn user_active(&self) -> bool {
        self.user_active.get()
    }

    fn user_control(&self) -> Option<UserControl> {
        self.user_control.get()
    }

    fn agent_used(&self, scope: &Scope) {
        self.agent_uses.borrow_mut().push(scope.clone());
    }

    fn expect_download(&self, dir: PathBuf) -> LocalBoxFuture<'static, Result<PathBuf, String>> {
        self.download_dirs.borrow_mut().push(dir.clone());
        match self.download.borrow().clone() {
            None => Box::pin(futures_util::future::pending()),
            Some(Err(error)) => Box::pin(async move { Err(error) }),
            Some(Ok((name, bytes))) => Box::pin(async move {
                let path = dir.join(name);
                std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                Ok(path)
            }),
        }
    }
}

#[derive(Default)]
pub struct FakeHost {
    pub pages: RefCell<Vec<Rc<FakePage>>>,
    pub clock: Rc<Clock>,
    pub opened: RefCell<Vec<(String, bool)>>,
    pub closed: RefCell<Vec<PageId>>,
    pub shown: RefCell<Vec<PageId>>,
    /// The project folder of the calls' scope; default [`scope`]'s.
    pub project: RefCell<Option<String>>,
}

impl FakeHost {
    pub fn with_pages(ids: &[PageId]) -> Rc<FakeHost> {
        let host = FakeHost::default();
        for &id in ids {
            host.pages.borrow_mut().push(FakePage::new(id));
        }
        Rc::new(host)
    }

    pub fn page(&self, id: PageId) -> Rc<FakePage> {
        self.pages
            .borrow()
            .iter()
            .find(|p| p.info.borrow().id == id)
            .cloned()
            .expect("no such fake page")
    }
}

impl Host for FakeHost {
    fn engine_started(&self) -> bool {
        true
    }

    fn pages(&self, _scope: &Scope) -> Vec<Rc<dyn Page>> {
        self.pages
            .borrow()
            .iter()
            .map(|p| p.clone() as Rc<dyn Page>)
            .collect()
    }

    fn open(
        &self,
        _scope: &Scope,
        url: &str,
        isolated: bool,
    ) -> LocalBoxFuture<'static, Result<Rc<dyn Page>, String>> {
        self.opened.borrow_mut().push((url.to_owned(), isolated));
        let id = 100 + self.pages.borrow().len() as PageId;
        let page = FakePage::new(id);
        {
            let mut info = page.info.borrow_mut();
            info.by_agent = true;
            info.isolated = isolated;
            info.url = url.to_owned();
        }
        self.pages.borrow_mut().push(page.clone());
        Box::pin(async move { Ok(page as Rc<dyn Page>) })
    }

    fn close(&self, _scope: &Scope, id: PageId) -> bool {
        let mut pages = self.pages.borrow_mut();
        let Some(at) = pages.iter().position(|p| p.info.borrow().id == id) else {
            return false;
        };
        let page = pages.remove(at);
        drop(pages);
        page.script.gone();
        self.closed.borrow_mut().push(id);
        true
    }

    fn show(&self, _scope: &Scope, id: PageId) -> bool {
        self.shown.borrow_mut().push(id);
        self.pages.borrow().iter().any(|p| p.info.borrow().id == id)
    }

    fn sleep(&self, duration: Duration) -> LocalBoxFuture<'static, ()> {
        self.clock.sleep(duration)
    }
}

pub fn scope() -> Scope {
    Scope {
        session: 1,
        project: "/tmp/project".into(),
    }
}

struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Runs tool calls side by side, like the app's foreground executor.
pub struct Driver {
    host: Rc<FakeHost>,
    tasks: Vec<Option<LocalBoxFuture<'static, ()>>>,
    flag: Arc<Flag>,
}

/// A call's answer, once it came.
pub type Answer = Rc<RefCell<Option<ToolOutput>>>;

impl Driver {
    pub fn new(host: &Rc<FakeHost>) -> Driver {
        Driver {
            host: host.clone(),
            tasks: Vec::new(),
            flag: Arc::new(Flag(AtomicBool::new(true))),
        }
    }

    /// Starts `tool` with `arguments`; the answer lands in the result.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Answer {
        let answer: Answer = Rc::default();
        let out = answer.clone();
        let mut scope = scope();
        if let Some(project) = self.host.project.borrow().clone() {
            scope.project = project;
        }
        let call = Call {
            scope,
            tool: tool.to_owned(),
            arguments,
            reply: Box::new(move |output| {
                // Send is required of the reply; this one stays on its thread.
                SEND_BACK.with(|slot| slot.borrow_mut().push(output));
            }),
        };
        let host: Rc<dyn Host> = self.host.clone();
        self.tasks.push(Some(Box::pin(async move {
            super::run(host, call).await;
            let mut outputs = SEND_BACK.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
            assert_eq!(outputs.len(), 1, "a call answers exactly once");
            *out.borrow_mut() = outputs.pop();
        })));
        self.flag.0.store(true, Ordering::SeqCst);
        answer
    }

    /// Polls until nothing moves, without moving the clock.
    pub fn idle(&mut self) {
        let waker = Waker::from(self.flag.clone());
        let mut cx = Context::from_waker(&waker);
        let mut rounds = 0;
        while self.flag.0.swap(false, Ordering::SeqCst) {
            rounds += 1;
            assert!(rounds < 100_000, "the calls never settle");
            for task in &mut self.tasks {
                if let Some(future) = task {
                    if Pin::new(future).poll(&mut cx) == Poll::Ready(()) {
                        *task = None;
                    }
                }
            }
        }
    }

    pub fn done(&self) -> bool {
        self.tasks.iter().all(Option::is_none)
    }

    /// Runs every call to its end, moving the clock when all wait on it.
    pub fn finish(&mut self) {
        let mut advances = 0;
        loop {
            self.idle();
            if self.done() {
                return;
            }
            advances += 1;
            assert!(advances < 1_000_000, "the clock runs away");
            if !self.host.clock.advance() {
                panic!("calls wait on nothing");
            }
            self.flag.0.store(true, Ordering::SeqCst);
        }
    }

    /// One call run to its end.
    pub fn run(&mut self, tool: &str, arguments: Value) -> ToolOutput {
        let answer = self.call(tool, arguments);
        self.finish();
        let output = answer.borrow_mut().take();
        output.expect("no answer")
    }
}

thread_local! {
    static SEND_BACK: RefCell<Vec<ToolOutput>> = const { RefCell::new(Vec::new()) };
}

/// The text of an output's text items.
pub fn text(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|c| match c {
            crate::browser::Content::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
