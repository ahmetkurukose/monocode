//! Agents drive the app's browser through an MCP server of Monocode's own
//! (docs/native/browser/context.md §6.1, K41). Built like the code bridge: a
//! process-wide listener on the loopback, a token per session holding its
//! scope, and a stdio subcommand of the Monocode binary that the provider
//! starts and that forwards each tool call here. No user MCP settings are
//! changed.
//!
//! Unlike the code bridge, a call is not answered on its connection's
//! thread: it becomes a [`Call`] that reaches the app's main thread as a
//! [`Job`] (see [`super::submit`]), and the connection waits for the answer
//! for as long as the tool may take, plus a grace.
use super::{Call, Content, Host, Job, Scope, ToolOutput, tools};
use futures_util::{FutureExt, future::LocalBoxFuture};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[cfg(test)]
mod hardening_tests;
#[cfg(test)]
mod tests;

/// A request, in either direction, is never longer than this.
const MAX_WIRE_BYTES: usize = 1_048_576;
/// An answer from the app: room for a few screenshots.
const MAX_ANSWER_BYTES: usize = 16 * 1_048_576;
/// No text item is longer than this; what a tool has beyond it is cut.
const MAX_TEXT_BYTES: usize = 64_000;
/// No image is larger than this, base64 included.
const MAX_IMAGE_BYTES: usize = 8 * 1_048_576;
const ENV_ENDPOINT: &str = "MONOCODE_BROWSER_ENDPOINT";
const ENV_TOKEN: &str = "MONOCODE_BROWSER_TOKEN";
const ENV_SESSION: &str = "MONOCODE_BROWSER_SESSION";
const ENV_EXECUTABLE: &str = "MONOCODE_BROWSER_EXECUTABLE";
const ENV_KEYS: [&str; 4] = [ENV_ENDPOINT, ENV_TOKEN, ENV_SESSION, ENV_EXECUTABLE];
const SUBCOMMAND: &str = "browser-mcp";

/// The external CDP endpoint for the session's project (K27): handed to the
/// agent's process in its environment, never on a command line.
pub const ENV_CDP: &str = "MONOCODE_BROWSER_CDP";

pub const SERVER_NAME: &str = "monocode_browser";

/// How long most tools may take on the main thread (K35).
const TOOL_LIMIT: Duration = Duration::from_secs(30);
/// `browser_wait` may wait this long.
const WAIT_LIMIT: Duration = Duration::from_secs(120);
/// How much longer a connection waits than the tool may take, and the stdio
/// side than the connection.
const GRACE: Duration = Duration::from_secs(15);
/// Calls waiting on the main thread each hold a connection.
const MAX_CONNECTIONS: usize = 32;
/// A connection says what it wants within this, however slowly it writes.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// Writing an answer, screenshots included, takes no longer than this.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);
/// Calls the stdio server has in flight at once.
const MAX_IN_FLIGHT: usize = 16;
/// Sessions whose tokens are kept; the oldest go first beyond it.
const MAX_SESSIONS: usize = 512;
/// How often a waiting connection looks whether its caller gave up.
const POLL: Duration = Duration::from_millis(200);

pub use tools::INSTRUCTIONS;

/// Whether sessions are given the browser tools at all. The app sets it from
/// its settings; a session started while it is off has no capability.
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// How long `tool` may take on the main thread.
fn tool_limit(tool: &str) -> Duration {
    match tool {
        "browser_wait" => WAIT_LIMIT,
        _ => TOOL_LIMIT,
    }
}

/// How long a connection waits for `tool`'s answer.
fn patience(tool: &str) -> Duration {
    tool_limit(tool) + GRACE
}

/// Where calls go once admitted. The app's is the main thread's job queue
/// and the tools; tests give their own.
#[derive(Clone)]
struct Engine {
    /// Whether a tool of this name exists.
    known: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Hands a job to the main thread; false if it takes no more work.
    submit: Arc<dyn Fn(Job) -> bool + Send + Sync>,
    /// Runs a call on the main thread.
    run: fn(Rc<dyn Host>, Call) -> LocalBoxFuture<'static, ()>,
    /// How long a connection waits for a call before it answers for it.
    patience: fn(&str) -> Duration,
}

impl Engine {
    fn app() -> Self {
        Engine {
            known: Arc::new(|name| {
                tools::definitions()
                    .iter()
                    .any(|tool| tool["name"].as_str() == Some(name))
            }),
            submit: Arc::new(super::submit),
            run: |host, call| tools::run(host, call).boxed_local(),
            patience,
        }
    }
}

/// The job that runs `call` on the main thread. A tool that panics drops
/// its reply unanswered, which the connection reports, and leaves the app
/// running.
fn job(run: fn(Rc<dyn Host>, Call) -> LocalBoxFuture<'static, ()>, call: Call) -> Job {
    Box::new(move |host| {
        AssertUnwindSafe(async move { run(host, call).await })
            .catch_unwind()
            .map(|_| ())
            .boxed_local()
    })
}

struct Hub {
    endpoint: String,
    engine: Engine,
    /// Each session's token and scope. A session keeps its token for the
    /// app's run unless `revoke` ends it; its project follows the latest start.
    sessions: Mutex<HashMap<i64, Issued>>,
    /// False once the thread that accepts connections has ended.
    listening: Arc<AtomicBool>,
    /// Connections being served.
    connections: Arc<AtomicUsize>,
    issued: AtomicU64,
}

struct Issued {
    token: String,
    scope: Scope,
    /// When it was first issued, to drop the oldest first.
    order: u64,
}

/// Marks the accept thread as gone, however it ends.
struct Listening(Arc<AtomicBool>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Frees a connection's place, however its thread ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Hub {
    #[cfg(test)]
    fn start(engine: Engine) -> Result<Arc<Hub>, String> {
        Self::listen(engine, "127.0.0.1:0", None)
    }

    /// A hub on `address` (the loopback), carrying over what `previous` knew.
    fn listen(engine: Engine, address: &str, previous: Option<&Hub>) -> Result<Arc<Hub>, String> {
        let listener = TcpListener::bind(address).map_err(|e| e.to_string())?;
        let endpoint = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .to_string();
        let sessions = previous.map_or_else(HashMap::new, |hub| {
            let sessions = hub.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            sessions
                .iter()
                .map(|(id, issued)| {
                    let issued = Issued {
                        token: issued.token.clone(),
                        scope: issued.scope.clone(),
                        order: issued.order,
                    };
                    (*id, issued)
                })
                .collect()
        });
        let hub = Arc::new(Hub {
            endpoint,
            engine,
            sessions: Mutex::new(sessions),
            listening: Arc::new(AtomicBool::new(true)),
            connections: Arc::new(AtomicUsize::new(0)),
            issued: AtomicU64::new(previous.map_or(0, |hub| hub.issued.load(Ordering::Relaxed))),
        });
        let owner = hub.clone();
        let alive = Listening(hub.listening.clone());
        std::thread::Builder::new()
            .name("browser-bridge".into())
            .spawn(move || accept(listener, owner, alive))
            .map_err(|e| e.to_string())?;
        Ok(hub)
    }

    fn environment(
        &self,
        session: i64,
        project: &Path,
        executable: &Path,
    ) -> Result<HashMap<String, String>, String> {
        // A relative or empty folder names no project's pages.
        if !project.is_absolute() || project.parent().is_none() {
            return Err("The browser tools need an absolute folder for the session".into());
        }
        let scope = Scope {
            session,
            project: project.to_string_lossy().into_owned(),
        };
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "Browser session unavailable")?;
        if !sessions.contains_key(&session) && sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, issued)| issued.order)
                .map(|(id, _)| *id)
            {
                sessions.remove(&oldest);
            }
        }
        let entry = sessions.entry(session).or_insert_with(|| {
            let token = format!(
                "{:032x}{:032x}",
                rand::random::<u128>(),
                rand::random::<u128>()
            );
            Issued {
                token,
                scope: scope.clone(),
                order: self.issued.fetch_add(1, Ordering::Relaxed),
            }
        });
        entry.scope = scope;
        Ok(HashMap::from([
            (ENV_ENDPOINT.into(), self.endpoint.clone()),
            (ENV_TOKEN.into(), entry.token.clone()),
            (ENV_SESSION.into(), session.to_string()),
            (
                ENV_EXECUTABLE.into(),
                executable.to_string_lossy().into_owned(),
            ),
        ]))
    }

    /// Ends a session's capability: its token opens nothing any more.
    fn revoke(&self, session: i64) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&session);
    }

    /// The scope a token opens, if it is that session's token.
    fn admit(&self, session: i64, token: &str) -> Result<Scope, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "Browser session unavailable")?;
        let issued = sessions.get(&session).ok_or("Invalid browser capability")?;
        let secret = &issued.token;
        let equal = token.len() == secret.len()
            && token
                .bytes()
                .zip(secret.bytes())
                .fold(0u8, |diff, (a, b)| diff | a ^ b)
                == 0;
        match equal {
            true => Ok(issued.scope.clone()),
            false => Err("Invalid browser capability".into()),
        }
    }
}

/// Takes connections until the listener is gone. A failed accept (too many
/// open files, a connection aborted before it was taken) is waited out, not
/// the end of the bridge.
fn accept(listener: TcpListener, hub: Arc<Hub>, _alive: Listening) {
    let connections = hub.connections.clone();
    let mut failures = 0u32;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => {
                failures = 0;
                stream
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                if failures == 1 || failures.is_multiple_of(100) {
                    eprintln!("browser bridge: accepting a connection failed: {error}");
                }
                let pause = 20u64.saturating_mul(1 << failures.min(6));
                std::thread::sleep(Duration::from_millis(pause.min(1000)));
                continue;
            }
        };
        if connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::Relaxed);
            continue;
        }
        let slot = Slot(connections.clone());
        let owner = hub.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-bridge-call".into())
            .spawn(move || {
                let _slot = slot;
                let _ = serve(stream, &owner);
            });
        if spawned.is_err() {
            // The closure, and the slot in it, went with the failed spawn.
            continue;
        }
    }
}

fn hub_slot() -> &'static Mutex<Option<Arc<Hub>>> {
    static HUB: OnceLock<Mutex<Option<Arc<Hub>>>> = OnceLock::new();
    HUB.get_or_init(Default::default)
}

fn hub() -> Result<Arc<Hub>, String> {
    let mut slot = hub_slot()
        .lock()
        .map_err(|_| "Browser bridge unavailable")?;
    live_hub(&mut slot, Engine::app)
}

/// The hub in `slot` if it still listens, else a new one. First use, or the
/// thread that accepted connections has ended: listen again, on the old
/// address if it can be had so that sessions already handed it keep
/// working, and with the sessions already issued.
fn live_hub(slot: &mut Option<Arc<Hub>>, engine: impl Fn() -> Engine) -> Result<Arc<Hub>, String> {
    if let Some(hub) = slot
        .as_ref()
        .filter(|hub| hub.listening.load(Ordering::SeqCst))
    {
        return Ok(hub.clone());
    }
    let previous = slot.take();
    let old = previous.as_ref().map(|hub| hub.endpoint.clone());
    let hub = match old
        .as_deref()
        .map(|address| Hub::listen(engine(), address, previous.as_deref()))
    {
        Some(Ok(hub)) => hub,
        _ => Hub::listen(engine(), "127.0.0.1:0", previous.as_deref())?,
    };
    *slot = Some(hub.clone());
    Ok(hub)
}

/// Ends a session's capability, as when the session is deleted: what it was
/// handed opens nothing afterwards. A session started again is issued a new
/// token.
pub fn revoke(session: i64) {
    let hub = hub_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(hub) = hub {
        hub.revoke(session);
    }
}

/// What a session's agent process needs to reach the bridge. `project` is
/// the session's project folder: the session sees that project's pages and
/// those it opened (K21).
pub fn environment(
    session: i64,
    project: &Path,
    executable: &Path,
) -> Result<HashMap<String, String>, String> {
    hub()?.environment(session, project, executable)
}

#[derive(Deserialize)]
struct Wire {
    session: i64,
    token: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

/// Cuts a text to `limit` bytes, on a character.
fn capped(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let total = text.len();
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(&format!(
        "\n[Cut here: {} more bytes. Ask for less at a time.]",
        total - cut
    ));
    text
}

/// A tool's answer as an MCP `tools/call` result, cut to what an answer may
/// carry.
fn call_result(output: ToolOutput) -> Value {
    let content: Vec<Value> = output
        .content
        .into_iter()
        .map(|item| match item {
            Content::Text(text) => json!({"type":"text","text":capped(text, MAX_TEXT_BYTES)}),
            Content::Image { mime, data } if data.len() <= MAX_IMAGE_BYTES => {
                json!({"type":"image","data":data,"mimeType":mime})
            }
            Content::Image { data, .. } => json!({"type":"text","text":format!(
                "[An image of {} bytes was left out: it is too large. Take a smaller one.]",
                data.len()
            )}),
        })
        .collect();
    json!({"content": content, "isError": output.is_error})
}

/// A failure the bridge itself reports, without a code.
fn failure(text: impl Into<String>) -> Value {
    json!({"content":[{"type":"text","text":text.into()}],"isError":true})
}

/// A stream read under one deadline for all of it: a peer that writes a
/// byte at a time cannot hold the connection beyond it.
struct ByDeadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for ByDeadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buffer)
    }
}

fn serve(mut stream: TcpStream, hub: &Hub) -> Result<(), String> {
    let mut line = String::new();
    let reader = ByDeadline {
        stream: &stream,
        until: Instant::now() + REQUEST_DEADLINE,
    };
    BufReader::new(reader.take((MAX_WIRE_BYTES + 1) as u64))
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    // The caller going away (it gave up on the call) ends the wait; the
    // call itself still runs to its end on the main thread.
    let hung_up = Arc::new(AtomicBool::new(false));
    watch(&stream, &hung_up);
    let answer = catch_unwind(AssertUnwindSafe(|| answer(&line, hub, &hung_up)))
        .unwrap_or_else(|_| failure("The tool failed unexpectedly. Try something else."));
    let mut body = answer.to_string();
    if body.len() > MAX_ANSWER_BYTES {
        body = failure("The answer was too large. Ask for less at a time.").to_string();
    }
    stream
        .set_write_timeout(Some(ANSWER_DEADLINE))
        .map_err(|e| e.to_string())?;
    let written = writeln!(stream, "{body}").map_err(|e| e.to_string());
    // Ends the watcher's read.
    let _ = stream.shutdown(Shutdown::Both);
    written
}

/// What one request line is answered with: an MCP `tools/call` result.
fn answer(line: &str, hub: &Hub, hung_up: &AtomicBool) -> Value {
    if line.len() > MAX_WIRE_BYTES || !line.ends_with('\n') {
        return failure("Invalid browser request");
    }
    let Ok(wire) = serde_json::from_str::<Wire>(line) else {
        return failure("Invalid browser request");
    };
    let scope = match hub.admit(wire.session, &wire.token) {
        Ok(scope) => scope,
        Err(error) => return failure(error),
    };
    if !(hub.engine.known)(&wire.tool) {
        return failure("Unknown browser tool");
    }
    let patience = (hub.engine.patience)(&wire.tool);
    let (send, receive) = mpsc::sync_channel::<ToolOutput>(1);
    let call = Call {
        scope,
        tool: wire.tool,
        arguments: wire.arguments,
        reply: Box::new(move |output| {
            // The connection may have stopped waiting.
            let _ = send.try_send(output);
        }),
    };
    if !(hub.engine.submit)(job(hub.engine.run, call)) {
        return call_result(ToolOutput::error(
            "browser_not_started",
            "Monocode's browser is not taking work. Is the app still running?",
        ));
    }
    let until = Instant::now() + patience;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() || hung_up.load(Ordering::Relaxed) {
            return call_result(ToolOutput::error(
                "browser_timeout",
                format!(
                    "Monocode's browser did not answer within {} s. The page may be busy; \
                     try again, or ask for less.",
                    patience.as_secs_f32().ceil()
                ),
            ));
        }
        match receive.recv_timeout(left.min(POLL)) {
            Ok(output) => return call_result(output),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The job ended, or was dropped, without answering.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return call_result(ToolOutput::error(
                    "browser_cdp_error",
                    "The tool failed unexpectedly. Try something else.",
                ));
            }
        }
    }
}

/// Sets `hung_up` once the peer has closed its end of `stream`.
fn watch(stream: &TcpStream, hung_up: &Arc<AtomicBool>) {
    let Ok(mut watched) = stream.try_clone() else {
        return;
    };
    let hung_up = hung_up.clone();
    let _ = std::thread::Builder::new()
        .name("browser-bridge-watch".into())
        .spawn(move || {
            let _ = watched.set_read_timeout(None);
            let mut scratch = [0u8; 256];
            // Anything more the peer writes is not part of the protocol.
            while matches!(watched.read(&mut scratch), Ok(count) if count > 0) {}
            hung_up.store(true, Ordering::Relaxed);
        });
}

fn current_environment() -> HashMap<String, String> {
    ENV_KEYS
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect()
}

pub fn configured(environment: &HashMap<String, String>) -> bool {
    ENV_KEYS.iter().all(|key| environment.contains_key(*key))
}

/// A call the stdio server can give up on: the model interrupted it.
#[derive(Default)]
struct Cancel {
    done: AtomicBool,
    stream: Mutex<Option<TcpStream>>,
}

impl Cancel {
    /// Abandons the wait: the connection to the app is closed, which also
    /// tells the app to stop waiting for the answer.
    fn cancel(&self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(stream) = &*self.stream.lock().unwrap_or_else(PoisonError::into_inner) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn cancelled(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn attach(&self, stream: &TcpStream) {
        *self.stream.lock().unwrap_or_else(PoisonError::into_inner) = stream.try_clone().ok();
        if self.cancelled() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Sends one call to the app and waits for its answer: an MCP `tools/call`
/// result, or why there is none.
#[cfg(test)]
fn forward(
    environment: &HashMap<String, String>,
    tool: &str,
    arguments: Value,
) -> Result<Value, String> {
    forward_cancellable(environment, tool, arguments, &Cancel::default())
}

fn forward_cancellable(
    environment: &HashMap<String, String>,
    tool: &str,
    arguments: Value,
    cancel: &Cancel,
) -> Result<Value, String> {
    if !configured(environment) {
        return Err("The browser tools are only available inside a Monocode session".into());
    }
    let address: SocketAddr = environment[ENV_ENDPOINT]
        .parse()
        .map_err(|_| "Invalid browser endpoint")?;
    let session: i64 = environment[ENV_SESSION]
        .parse()
        .map_err(|_| "Invalid browser session")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
        .map_err(|_| "Monocode's browser tools are not reachable. Is the app still running?")?;
    cancel.attach(&stream);
    let request = json!({
        "session": session, "token": environment[ENV_TOKEN], "tool": tool, "arguments": arguments,
    });
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(patience(tool) + GRACE))
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream.take((MAX_ANSWER_BYTES + 1) as u64))
        .read_line(&mut line)
        .map_err(|_| "Monocode did not answer in time".to_string())?;
    if cancel.cancelled() {
        return Err("The call was cancelled.".into());
    }
    if line.len() > MAX_ANSWER_BYTES || !line.ends_with('\n') {
        return Err("Invalid browser answer".into());
    }
    let answer: Value = serde_json::from_str(&line).map_err(|_| "Invalid browser answer")?;
    match answer["content"].is_array() {
        true => Ok(answer),
        false => Err("Invalid browser answer".into()),
    }
}

/// Provider-specific adapters use this stdio server definition.
pub fn mcp_server(environment: &HashMap<String, String>) -> Option<Value> {
    configured(environment).then(|| {
        json!({
            "type":"stdio", "command":environment[ENV_EXECUTABLE], "args":[SUBCOMMAND]
        })
    })
}

pub fn acp_server(environment: &HashMap<String, String>) -> Option<Value> {
    let mut server = mcp_server(environment)?;
    // ACP's stdio server shape omits the transport discriminator.
    server.as_object_mut()?.remove("type");
    server["name"] = SERVER_NAME.into();
    server["env"] = ENV_KEYS
        .into_iter()
        .map(|key| json!({"name":key,"value":environment[key]}))
        .collect::<Vec<_>>()
        .into();
    Some(server)
}

pub fn configure_codex(command: &mut std::process::Command, environment: &HashMap<String, String>) {
    if !configured(environment) {
        return;
    }
    for (key, value) in [
        ("command", json!(environment[ENV_EXECUTABLE])),
        ("args", json!([SUBCOMMAND])),
        ("env_vars", json!(ENV_KEYS)),
    ] {
        command.args(["-c", &format!("mcp_servers.{SERVER_NAME}.{key}={value}")]);
    }
}

/// The tools Claude may call without asking: all but those the user approves
/// each time (K25, `browser_eval`). Empty while there are no tools.
pub fn claude_allowed_tools() -> &'static str {
    static ALLOWED: OnceLock<String> = OnceLock::new();
    ALLOWED.get_or_init(|| allowed_tools(&tools::definitions()))
}

fn allowed_tools(definitions: &[Value]) -> String {
    definitions
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| !tools::NEEDS_APPROVAL.contains(name))
        .map(|name| format!("mcp__{SERVER_NAME}__{name}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn mcp_response(request: Value, environment: &HashMap<String, String>) -> Option<Value> {
    mcp_answer(request, environment, &Cancel::default())
}

fn mcp_answer(
    request: Value,
    environment: &HashMap<String, String>,
    cancel: &Cancel,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request["method"].as_str().unwrap_or("") {
        "initialize" => Ok(json!({
            "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools::definitions()})),
        "tools/call" => {
            let tool = request["params"]["name"].as_str().unwrap_or("");
            let arguments = request["params"]["arguments"].clone();
            Ok(forward_cancellable(environment, tool, arguments, cancel).unwrap_or_else(failure))
        }
        _ => Err(json!({"code":-32601,"message":"Method not found"})),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
    })
}

/// The MCP server over a pair of streams: one JSON-RPC frame per line.
/// A tool call is answered on a thread of its own, so a call that waits
/// (`browser_wait`) does not hold up the others, and
/// `notifications/cancelled` abandons the wait of the call it names, which
/// then gets no answer.
fn mcp_serve(
    mut input: impl BufRead,
    output: impl Write + Send,
    environment: &HashMap<String, String>,
) -> Result<(), String> {
    let output = Mutex::new(output);
    let in_flight: Mutex<HashMap<String, Arc<Cancel>>> = Mutex::new(HashMap::new());
    let write = |frame: &Value| -> Result<(), String> {
        let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
        writeln!(output, "{frame}")
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())
    };
    std::thread::scope(|scope| {
        loop {
            let mut line = String::new();
            (&mut input)
                .take((MAX_WIRE_BYTES + 1) as u64)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if line.is_empty() {
                return Ok(());
            }
            if line.len() > MAX_WIRE_BYTES || !line.ends_with('\n') {
                return Err("Oversized or incomplete MCP frame".into());
            }
            let request: Value = match serde_json::from_str(&line) {
                Ok(request) => request,
                Err(_) => {
                    write(
                        &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                    )?;
                    continue;
                }
            };
            match request["method"].as_str() {
                Some("notifications/cancelled") => {
                    let key = request["params"]["requestId"].to_string();
                    let found = in_flight
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get(&key)
                        .cloned();
                    if let Some(cancel) = found {
                        cancel.cancel();
                    }
                }
                Some("tools/call") if request.get("id").is_some() => {
                    let key = request["id"].to_string();
                    let cancel = Arc::new(Cancel::default());
                    let crowded = {
                        let mut calls = in_flight.lock().unwrap_or_else(PoisonError::into_inner);
                        let crowded = calls.len() >= MAX_IN_FLIGHT;
                        if !crowded {
                            calls.insert(key.clone(), cancel.clone());
                        }
                        crowded
                    };
                    if crowded {
                        let busy =
                            failure("Too many calls are waiting at once. Let some finish first.");
                        write(&json!({"jsonrpc":"2.0","id":request["id"],"result":busy}))?;
                        continue;
                    }
                    let (write, in_flight) = (&write, &in_flight);
                    scope.spawn(move || {
                        let response = mcp_answer(request, environment, &cancel);
                        in_flight
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&key);
                        // A cancelled call is not answered.
                        if let (Some(response), false) = (response, cancel.cancelled()) {
                            let _ = write(&response);
                        }
                    });
                }
                _ => {
                    if let Some(response) = mcp_response(request, environment) {
                        write(&response)?;
                    }
                }
            }
        }
    })
}

/// Runs before GUI initialization. stdout contains MCP JSON-RPC frames only.
pub fn mcp_stdio() -> Result<(), String> {
    mcp_serve(
        std::io::stdin().lock(),
        std::io::stdout(),
        &current_environment(),
    )
}
