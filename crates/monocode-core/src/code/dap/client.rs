//! The Debug Adapter Protocol on the wire: one connection to an adapter over
//! its standard streams or a TCP socket. A reader thread routes each response
//! to the request that waits for it and hands events and the adapter's own
//! requests to the owner. The adapter process, when this started it, dies
//! with the client.
use super::types::DapError;
use crate::code::{child, wire};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Long enough for a busy interpreter, short enough to notice a hung one.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const STDERR_KEPT: usize = 4096;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How an adapter is reached once its process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The protocol is spoken on the process's standard streams.
    Stdio,
    /// The process listens on this local port.
    Tcp { host: String, port: u16 },
}

/// An adapter process to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterStart {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub transport: Transport,
}

/// What the adapter sends unasked.
pub enum Incoming {
    Event {
        event: String,
        body: Value,
    },
    Request(ReverseRequest),
    /// The connection ended; nothing follows.
    Closed,
}

type Reply = mpsc::Sender<Result<Value, DapError>>;

#[derive(Default)]
struct Routes {
    pending: Mutex<HashMap<i64, Reply>>,
    closed: AtomicBool,
}

struct Shared {
    writer: Mutex<Box<dyn Write + Send>>,
    next_seq: AtomicI64,
    routes: Arc<Routes>,
    process: Mutex<Option<Child>>,
    socket: Option<TcpStream>,
    stderr: Arc<Mutex<String>>,
}

pub struct Client {
    shared: Arc<Shared>,
}

/// A request that was sent and not yet answered.
pub struct Pending {
    seq: i64,
    command: String,
    reply: mpsc::Receiver<Result<Value, DapError>>,
    routes: Arc<Routes>,
}

/// A request from the adapter (`runInTerminal`, `startDebugging`). It stays
/// unanswered until `reply` is called; dropping it leaves the adapter waiting.
pub struct ReverseRequest {
    pub command: String,
    pub arguments: Value,
    seq: i64,
    client: Weak<Shared>,
}

impl Client {
    /// A client over any pair of streams; the adapter process, if given, is
    /// killed when the client is dropped.
    pub fn over(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        process: Option<Child>,
    ) -> (Self, mpsc::Receiver<Incoming>) {
        Self::build(Box::new(reader), Box::new(writer), process, None, None)
    }

    fn build(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        process: Option<Child>,
        socket: Option<TcpStream>,
        stderr: Option<Arc<Mutex<String>>>,
    ) -> (Self, mpsc::Receiver<Incoming>) {
        let routes = Arc::new(Routes::default());
        let shared = Arc::new(Shared {
            writer: Mutex::new(writer),
            next_seq: AtomicI64::new(1),
            routes: routes.clone(),
            process: Mutex::new(process),
            socket,
            stderr: stderr.unwrap_or_default(),
        });
        let (sender, incoming) = mpsc::channel();
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("dap-reader".into())
            .spawn(move || read_loop(reader, &routes, &weak, &sender))
            .ok();
        (Self { shared }, incoming)
    }

    /// Starts the adapter process and connects to it.
    pub fn start(start: &AdapterStart) -> Result<(Self, mpsc::Receiver<Incoming>), DapError> {
        let mut command = Command::new(&start.program);
        command
            .args(&start.args)
            .current_dir(&start.cwd)
            .envs(start.env.iter().map(|(key, value)| (key, value)))
            .stderr(Stdio::piped());
        match start.transport {
            Transport::Stdio => command.stdin(Stdio::piped()).stdout(Stdio::piped()),
            Transport::Tcp { .. } => command.stdin(Stdio::null()).stdout(Stdio::null()),
        };
        // The adapter starts the debuggee, which inherits how it is
        // scheduled: the user's program runs at the user's priority.
        crate::process::configure_user_child(&mut command);
        // Its own group, so that helpers the adapter starts die with it.
        child::lead_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            DapError::Start(format!(
                "Could not start {}: {error}.",
                start.program.display()
            ))
        })?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let kept = stderr.clone();
            std::thread::Builder::new()
                .name("dap-stderr".into())
                .spawn(move || {
                    let mut chunk = [0u8; 1024];
                    while let Ok(read @ 1..) = pipe.read(&mut chunk) {
                        let mut kept = lock(&kept);
                        kept.push_str(&String::from_utf8_lossy(&chunk[..read]));
                        if kept.len() > STDERR_KEPT {
                            let mut cut = kept.len() - STDERR_KEPT;
                            while !kept.is_char_boundary(cut) {
                                cut += 1;
                            }
                            kept.drain(..cut);
                        }
                    }
                })
                .ok();
        }
        match &start.transport {
            Transport::Stdio => {
                let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                    child::kill_group(&mut child);
                    return Err(DapError::Start(
                        "The debug adapter has no standard streams.".into(),
                    ));
                };
                Ok(Self::build(
                    Box::new(stdout),
                    Box::new(stdin),
                    Some(child),
                    None,
                    Some(stderr),
                ))
            }
            Transport::Tcp { host, port } => {
                // The adapter needs a moment before it listens.
                let deadline = Instant::now() + REQUEST_TIMEOUT;
                loop {
                    match open_socket(host, *port, Duration::from_millis(500)) {
                        Ok(socket) => {
                            return Self::over_socket(socket, Some(child), Some(stderr));
                        }
                        Err(error) => {
                            if child::exited(&mut child) || Instant::now() >= deadline {
                                child::kill_group(&mut child);
                                return Err(error);
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        }
    }

    /// Connects to an adapter that already listens, as a child session does.
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<(Self, mpsc::Receiver<Incoming>), DapError> {
        Self::over_socket(open_socket(host, port, timeout)?, None, None)
    }

    fn over_socket(
        socket: TcpStream,
        process: Option<Child>,
        stderr: Option<Arc<Mutex<String>>>,
    ) -> Result<(Self, mpsc::Receiver<Incoming>), DapError> {
        let failed = |error: std::io::Error| {
            DapError::Start(format!("Could not use the debugger's socket: {error}."))
        };
        socket.set_nodelay(true).ok();
        let reader = socket.try_clone().map_err(failed)?;
        let writer = socket.try_clone().map_err(failed)?;
        Ok(Self::build(
            Box::new(reader),
            Box::new(writer),
            process,
            Some(socket),
            stderr,
        ))
    }

    /// Sends a request without waiting for its answer.
    pub fn send(&self, command: &str, arguments: Value) -> Result<Pending, DapError> {
        let shared = &self.shared;
        let seq = shared.next_seq.fetch_add(1, Ordering::Relaxed);
        let (sender, reply) = mpsc::channel();
        lock(&shared.routes.pending).insert(seq, sender);
        let mut message = json!({ "seq": seq, "type": "request", "command": command });
        if !arguments.is_null() {
            message["arguments"] = arguments;
        }
        let pending = Pending {
            seq,
            command: command.to_string(),
            reply,
            routes: shared.routes.clone(),
        };
        // Checked after registering: a reader that ended before this point
        // has already failed everything it knew of, and would miss this one.
        if shared.routes.closed.load(Ordering::SeqCst) || shared.write(&message).is_err() {
            lock(&shared.routes.pending).remove(&seq);
            return Err(DapError::Closed);
        }
        Ok(pending)
    }

    /// Sends a request and waits for its answer: the response's `body`, or
    /// the adapter's message when it refused. Blocks the calling thread.
    pub fn request(
        &self,
        command: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<Value, DapError> {
        self.send(command, arguments)?.wait(timeout)
    }

    pub fn is_closed(&self) -> bool {
        self.shared.routes.closed.load(Ordering::SeqCst)
    }

    /// The end of what the adapter process wrote to its error stream.
    pub fn stderr_tail(&self) -> String {
        lock(&self.shared.stderr).trim().to_string()
    }

    /// Ends the connection, giving the adapter up to `grace` to exit by
    /// itself (it may be finishing a `disconnect`) before it is killed.
    pub fn shutdown(&self, grace: Duration) {
        self.shared.shutdown(grace);
    }
}

impl Pending {
    pub fn wait(self, timeout: Duration) -> Result<Value, DapError> {
        match self.reply.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.routes.pending).remove(&self.seq);
                Err(DapError::Timeout {
                    command: self.command,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(DapError::Closed),
        }
    }
}

impl ReverseRequest {
    /// Answers the adapter: `Ok` with the response body, `Err` with the
    /// reason shown to the user by the adapter.
    pub fn reply(self, result: Result<Value, String>) {
        let Some(shared) = self.client.upgrade() else {
            return;
        };
        let seq = shared.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({
            "seq": seq,
            "type": "response",
            "request_seq": self.seq,
            "command": self.command,
            "success": result.is_ok(),
        });
        match result {
            Ok(body) => message["body"] = body,
            Err(reason) => message["message"] = Value::String(reason),
        }
        shared.write(&message).ok();
    }
}

impl Shared {
    fn write(&self, message: &Value) -> std::io::Result<()> {
        let body = serde_json::to_vec(message)?;
        wire::write_message(&mut *lock(&self.writer), &body)
    }

    fn shutdown(&self, grace: Duration) {
        if let Some(mut child) = lock(&self.process).take() {
            let deadline = Instant::now() + grace;
            // Looked at without reaping, so the group is still the
            // adapter's own when it is killed.
            while Instant::now() < deadline && !child::exited(&mut child) {
                std::thread::sleep(Duration::from_millis(10));
            }
            child::kill_group(&mut child);
        }
        if let Some(socket) = &self.socket {
            socket.shutdown(Shutdown::Both).ok();
        }
        self.routes.close();
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.shutdown(Duration::ZERO);
    }
}

impl Routes {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, reply) in lock(&self.pending).drain() {
            reply.send(Err(DapError::Closed)).ok();
        }
    }
}

fn open_socket(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, DapError> {
    let failed = |detail: String| {
        DapError::Start(format!(
            "Could not reach the debugger at {host}:{port}: {detail}."
        ))
    };
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| failed(error.to_string()))?;
    let mut last = "no address".to_string();
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(socket) => return Ok(socket),
            Err(error) => last = error.to_string(),
        }
    }
    Err(failed(last))
}

fn read_loop(
    reader: Box<dyn Read + Send>,
    routes: &Routes,
    client: &Weak<Shared>,
    incoming: &mpsc::Sender<Incoming>,
) {
    let mut reader = BufReader::new(reader);
    while let Ok(Some(body)) = wire::read_message(&mut reader) {
        // A message that is not JSON is skipped: the framing is still sound.
        let Ok(message) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        let field = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default();
        match field("type") {
            "response" => {
                let seq = message.get("request_seq").and_then(Value::as_i64);
                let Some(reply) = seq.and_then(|seq| lock(&routes.pending).remove(&seq)) else {
                    // Answered after its timeout, or never asked.
                    continue;
                };
                let body = message.get("body").cloned().unwrap_or(Value::Null);
                let result = if message.get("success").and_then(Value::as_bool) == Some(true) {
                    Ok(body)
                } else {
                    Err(DapError::Adapter {
                        command: field("command").to_string(),
                        message: failure_text(&message, &body),
                    })
                };
                reply.send(result).ok();
            }
            "event" => {
                let event = Incoming::Event {
                    event: field("event").to_string(),
                    body: message.get("body").cloned().unwrap_or(Value::Null),
                };
                if incoming.send(event).is_err() {
                    break;
                }
            }
            "request" => {
                let request = ReverseRequest {
                    command: field("command").to_string(),
                    arguments: message.get("arguments").cloned().unwrap_or(Value::Null),
                    seq: message.get("seq").and_then(Value::as_i64).unwrap_or(0),
                    client: client.clone(),
                };
                if incoming.send(Incoming::Request(request)).is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
    routes.close();
    incoming.send(Incoming::Closed).ok();
}

/// The sentence an adapter gives for a failed request: its `message`, else
/// the structured error's format string.
fn failure_text(message: &Value, body: &Value) -> String {
    let short = message.get("message").and_then(Value::as_str);
    let long = body
        .get("error")
        .and_then(|error| error.get("format"))
        .and_then(Value::as_str);
    short.or(long).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::fake::{fake, fake_listening};
    use super::*;

    const SOON: Duration = Duration::from_secs(5);

    #[test]
    fn responses_find_their_requests_in_any_order() {
        let (client, _incoming, adapter) = fake(|peer| {
            let first = peer.expect("first");
            let second = peer.expect("second");
            assert_eq!(first["type"], json!("request"));
            assert_eq!(second["arguments"], json!({ "n": 2 }));
            assert!(first.get("arguments").is_none());
            peer.respond(&second, json!({ "answer": "second" }));
            peer.respond(&first, json!({ "answer": "first" }));
            let third = peer.expect("third");
            peer.refuse(&third, "No such thing.");
            let fourth = peer.expect("fourth");
            peer.write(json!({
                "type": "response", "request_seq": fourth["seq"], "success": false,
                "command": "fourth", "body": { "error": { "id": 1, "format": "Long form." } },
            }));
        });
        let first = client.send("first", Value::Null).unwrap();
        let second = client.send("second", json!({ "n": 2 })).unwrap();
        assert_eq!(first.wait(SOON).unwrap(), json!({ "answer": "first" }));
        assert_eq!(second.wait(SOON).unwrap(), json!({ "answer": "second" }));
        assert_eq!(
            client.request("third", Value::Null, SOON),
            Err(DapError::Adapter {
                command: "third".into(),
                message: "No such thing.".into()
            })
        );
        let error = client.request("fourth", Value::Null, SOON).unwrap_err();
        assert_eq!(error.to_string(), "Long form.");
        drop(client);
        adapter.join().unwrap();
    }

    #[test]
    fn a_silent_adapter_times_out_and_a_late_answer_is_dropped() {
        let (client, _incoming, adapter) = fake(|peer| {
            let slow = peer.expect("slow");
            let next = peer.expect("next");
            peer.respond(&slow, json!("late"));
            peer.respond(&next, json!("on time"));
        });
        assert_eq!(
            client.request("slow", Value::Null, Duration::from_millis(30)),
            Err(DapError::Timeout {
                command: "slow".into()
            })
        );
        assert_eq!(
            client.request("next", Value::Null, SOON),
            Ok(json!("on time"))
        );
        drop(client);
        adapter.join().unwrap();
    }

    #[test]
    fn events_and_reverse_requests_reach_the_owner() {
        let (client, incoming, adapter) = fake(|peer| {
            peer.event("output", json!({ "output": "hi" }));
            let asked = peer.request("runInTerminal", json!({ "args": ["python"] }));
            let answer = peer.next().unwrap();
            assert_eq!(answer["type"], json!("response"));
            assert_eq!(answer["request_seq"], json!(asked));
            assert_eq!(answer["command"], json!("runInTerminal"));
            assert_eq!(answer["success"], json!(true));
            assert_eq!(answer["body"], json!({ "processId": 7 }));
            let asked = peer.request("startDebugging", json!({}));
            let answer = peer.next().unwrap();
            assert_eq!(answer["request_seq"], json!(asked));
            assert_eq!(answer["success"], json!(false));
            assert_eq!(answer["message"], json!("Not now."));
        });
        match incoming.recv_timeout(SOON).unwrap() {
            Incoming::Event { event, body } => {
                assert_eq!(event, "output");
                assert_eq!(body, json!({ "output": "hi" }));
            }
            _ => panic!("expected the event first"),
        }
        for answer in [Ok(json!({ "processId": 7 })), Err("Not now.".to_string())] {
            match incoming.recv_timeout(SOON).unwrap() {
                Incoming::Request(request) => request.reply(answer),
                _ => panic!("expected a request"),
            }
        }
        adapter.join().unwrap();
        // The adapter is gone: the owner is told, and requests fail at once.
        assert!(matches!(
            incoming.recv_timeout(SOON).unwrap(),
            Incoming::Closed
        ));
        assert!(client.is_closed());
        assert_eq!(
            client.request("late", Value::Null, SOON),
            Err(DapError::Closed)
        );
    }

    #[test]
    fn a_request_in_flight_fails_when_the_adapter_goes() {
        let (client, _incoming, adapter) = fake(|peer| {
            peer.expect("doomed");
        });
        assert_eq!(
            client.request("doomed", Value::Null, SOON),
            Err(DapError::Closed)
        );
        adapter.join().unwrap();
    }

    #[test]
    fn a_listening_adapter_is_reached_over_tcp() {
        let (port, adapter) = fake_listening(|peer| {
            let request = peer.expect("initialize");
            peer.respond(&request, json!({ "ok": true }));
            assert!(peer.next().is_none());
        });
        let (client, _incoming) = Client::connect("127.0.0.1", port, SOON).unwrap();
        assert_eq!(
            client.request("initialize", json!({}), SOON),
            Ok(json!({ "ok": true }))
        );
        // Dropping the client closes the socket, which ends the script.
        drop(client);
        adapter.join().unwrap();
    }

    #[test]
    fn a_program_that_cannot_start_is_reported() {
        let start = AdapterStart {
            program: PathBuf::from("/nonexistent/debug-adapter"),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            transport: Transport::Stdio,
        };
        assert!(matches!(Client::start(&start), Err(DapError::Start(_))));
    }
}
