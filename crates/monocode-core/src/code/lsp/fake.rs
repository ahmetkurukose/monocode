//! A scripted language server for tests: it runs on a thread at the other
//! end of two in-process pipes, answers the handshake, records everything it
//! is sent, and does whatever else a test tells it to.
use super::client::{lock, Launch, Launcher, Transport};
use crate::code::wire;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(10);

pub struct PipeWriter(mpsc::Sender<Vec<u8>>);

pub struct PipeReader {
    chunks: mpsc::Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    at: usize,
}

pub fn pipe() -> (PipeWriter, PipeReader) {
    let (sender, chunks) = mpsc::channel();
    let reader = PipeReader {
        chunks,
        chunk: Vec::new(),
        at: 0,
    };
    (PipeWriter(sender), reader)
}

impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.chunk.len() {
            match self.chunks.recv() {
                Ok(chunk) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                // The writer is gone: end of stream.
                Err(_) => return Ok(0),
            }
        }
        let count = out.len().min(self.chunk.len() - self.at);
        out[..count].copy_from_slice(&self.chunk[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// Polls until `condition` holds; a test that waits forever fails instead.
pub fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting");
        std::thread::sleep(Duration::from_millis(3));
    }
}

type Handler = Arc<dyn Fn(&Value, &FakeOut) + Send + Sync>;

struct Inner {
    capabilities: Value,
    received: Mutex<Vec<Value>>,
    arrived: Condvar,
    handlers: Mutex<HashMap<String, Handler>>,
    out: Mutex<Option<PipeWriter>>,
    transport: Mutex<Option<Transport>>,
    exited: AtomicBool,
}

/// The server's way to talk back, handed to handlers.
pub struct FakeOut<'a>(&'a Inner);

impl FakeOut<'_> {
    pub fn send(&self, message: &Value) {
        self.0.send_raw(&frame(message));
    }

    /// Answers request `message` with `result`.
    pub fn result(&self, message: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }
}

fn frame(message: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    wire::write_message(&mut bytes, message.to_string().as_bytes()).unwrap();
    bytes
}

impl Inner {
    fn send_raw(&self, bytes: &[u8]) {
        if let Some(out) = lock(&self.out).as_mut() {
            let _ = out.write_all(bytes);
        }
    }

    fn handle(&self, message: &Value) -> bool {
        let method = message["method"].as_str().unwrap_or_default();
        let handler = lock(&self.handlers).get(method).cloned();
        let out = FakeOut(self);
        if let Some(handler) = handler {
            handler(message, &out);
        } else if method == "initialize" {
            out.result(message, json!({ "capabilities": self.capabilities }));
        } else if method == "exit" {
            return false;
        } else if !method.is_empty() && !message["id"].is_null() {
            out.result(message, Value::Null);
        }
        true
    }
}

#[derive(Clone)]
pub struct FakeServer {
    inner: Arc<Inner>,
}

impl FakeServer {
    /// A server announcing `capabilities`, already running.
    pub fn new(capabilities: Value) -> Self {
        let (to_server, from_client) = pipe();
        let (to_client, from_server) = pipe();
        let inner = Arc::new(Inner {
            capabilities,
            received: Mutex::new(Vec::new()),
            arrived: Condvar::new(),
            handlers: Mutex::new(HashMap::new()),
            out: Mutex::new(Some(to_client)),
            transport: Mutex::new(Some(Transport {
                reader: Box::new(from_server),
                writer: Box::new(to_server),
                stderr: None,
                process: None,
            })),
            exited: AtomicBool::new(false),
        });
        let server = inner.clone();
        std::thread::spawn(move || {
            let mut input = BufReader::new(from_client);
            while let Ok(Some(body)) = wire::read_message(&mut input) {
                let message: Value = serde_json::from_slice(&body).unwrap();
                lock(&server.received).push(message.clone());
                server.arrived.notify_all();
                if !server.handle(&message) {
                    break;
                }
            }
            *lock(&server.out) = None;
            server.exited.store(true, Ordering::SeqCst);
        });
        Self { inner }
    }

    /// The client's end of the pipes; there is only one.
    pub fn transport(&self) -> Transport {
        lock(&self.inner.transport).take().expect("transport taken")
    }

    /// What to do when `method` arrives, instead of the default (answer the
    /// handshake, answer any other request with null).
    pub fn on(&self, method: &str, handler: impl Fn(&Value, &FakeOut) + Send + Sync + 'static) {
        lock(&self.inner.handlers).insert(method.to_string(), Arc::new(handler));
    }

    pub fn send(&self, message: &Value) {
        self.inner.send_raw(&frame(message));
    }

    pub fn notify(&self, method: &str, params: Value) {
        FakeOut(&self.inner).notify(method, params);
    }

    pub fn send_raw(&self, bytes: &[u8]) {
        self.inner.send_raw(bytes);
    }

    /// Ends the stream without a word, as a crashed process does.
    pub fn crash(&self) {
        *lock(&self.inner.out) = None;
    }

    pub fn exited(&self) -> bool {
        self.inner.exited.load(Ordering::SeqCst)
    }

    pub fn received(&self, method: &str) -> Vec<Value> {
        lock(&self.inner.received)
            .iter()
            .filter(|message| message["method"] == method)
            .cloned()
            .collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.received(method).len()
    }

    fn wait_matching(&self, what: &str, pick: impl Fn(&[Value]) -> Option<Value>) -> Value {
        let deadline = Instant::now() + PATIENCE;
        let mut received = lock(&self.inner.received);
        loop {
            if let Some(found) = pick(&received) {
                return found;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the server never received {what}");
            received = self
                .inner
                .arrived
                .wait_timeout(received, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// The first `method` message, waiting for it to arrive.
    pub fn wait_for(&self, method: &str) -> Value {
        self.wait_nth(method, 0)
    }

    /// The `index`-th (from zero) `method` message, waiting for it.
    pub fn wait_nth(&self, method: &str, index: usize) -> Value {
        self.wait_matching(method, |received| {
            received
                .iter()
                .filter(|message| message["method"] == method)
                .nth(index)
                .cloned()
        })
    }

    /// The client's answer to the server's request `id`.
    pub fn wait_for_response(&self, id: i64) -> Value {
        self.wait_matching("a response", |received| {
            received
                .iter()
                .find(|message| message["id"] == id && message.get("method").is_none())
                .cloned()
        })
    }
}

type Factory = Box<dyn Fn(&Launch) -> io::Result<FakeServer> + Send + Sync>;

/// Starts a `FakeServer` wherever the manager would start a process, and
/// keeps each one so a test can script and inspect it.
pub struct FakeLauncher {
    factory: Factory,
    pub launched: Mutex<Vec<(Launch, FakeServer)>>,
}

impl FakeLauncher {
    pub fn new(
        factory: impl Fn(&Launch) -> io::Result<FakeServer> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            factory: Box::new(factory),
            launched: Mutex::new(Vec::new()),
        })
    }

    pub fn count(&self) -> usize {
        lock(&self.launched).len()
    }

    /// The `index`-th server started, waiting for it to be.
    pub fn server(&self, index: usize) -> FakeServer {
        wait_until(|| self.count() > index);
        lock(&self.launched)[index].1.clone()
    }

    /// The most recently started server whose program is named `name`.
    pub fn named(&self, name: &str) -> FakeServer {
        let find = || {
            lock(&self.launched)
                .iter()
                .rev()
                .find(|(launch, _)| launch.program.file_name().is_some_and(|file| file == name))
                .map(|(_, server)| server.clone())
        };
        wait_until(|| find().is_some());
        find().expect("just found")
    }
}

impl Launcher for FakeLauncher {
    fn launch(&self, launch: &Launch) -> io::Result<Transport> {
        let server = (self.factory)(launch)?;
        let transport = server.transport();
        lock(&self.launched).push((launch.clone(), server));
        Ok(transport)
    }
}
