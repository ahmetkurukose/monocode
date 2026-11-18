//! One language server process and the conversation with it over its
//! standard streams. A reader thread hands responses to whoever asked, answers
//! the questions servers ask their client, and passes notifications on; a
//! writer thread owns the server's input, so sending never blocks the caller
//! even when the server is slow to read. The process leads its own process
//! group and the whole group is killed when the client is dropped, since some
//! servers are launchers that do their work in a child.
use super::position::Encoding;
use super::types::LspError;
use crate::code::{child, wire};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

const LOG_LINES: usize = 200;
const LOG_LINE_BYTES: usize = 1000;
const METHOD_NOT_FOUND: i64 = -32601;

/// A poisoned lock only means another thread panicked mid-update; the data
/// here stays usable, and a language server must not take the app down.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The process to start for a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Set over the app's own environment.
    pub env: Vec<(String, String)>,
}

/// The streams of a started server, and its process when it is one.
pub struct Transport {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub stderr: Option<Box<dyn Read + Send>>,
    pub process: Option<Child>,
}

/// How servers are started; tests put a scripted server behind this.
pub trait Launcher: Send + Sync {
    fn launch(&self, launch: &Launch) -> io::Result<Transport>;
}

/// Starts the server as a child process speaking over its standard streams.
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn launch(&self, launch: &Launch) -> io::Result<Transport> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(&launch.cwd)
            .envs(launch.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own group, so the launcher's children die with it.
        child::lead_group(&mut command);
        // A language server is background work: it keeps the scheduling
        // hints that runs and debuggers go without.
        crate::process::configure_child(&mut command);
        let mut child = command.spawn()?;
        let streams = (child.stdout.take(), child.stdin.take());
        let (Some(stdout), Some(stdin)) = streams else {
            child::kill_group(&mut child);
            return Err(io::Error::other("the server's streams could not be opened"));
        };
        let stderr = child.stderr.take();
        Ok(Transport {
            reader: Box::new(stdout),
            writer: Box::new(stdin),
            stderr: stderr.map(|stderr| Box::new(stderr) as Box<dyn Read + Send>),
            process: Some(child),
        })
    }
}

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct CancelState {
    cancelled: bool,
    next: u64,
    hooks: Vec<(u64, Hook)>,
}

/// Lets one thread give up a request another thread is waiting on. Make one
/// per question, pass it to the call, and `cancel` it when the answer is no
/// longer wanted: the waiting call returns `LspError::Cancelled` at once and
/// the server is told to stop working on it.
#[derive(Clone, Default)]
pub struct CancelToken {
    state: Arc<Mutex<CancelState>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let hooks = {
            let mut state = lock(&self.state);
            state.cancelled = true;
            std::mem::take(&mut state.hooks)
        };
        for (_, hook) in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        lock(&self.state).cancelled
    }

    /// Runs `hook` when cancelled; right away if that already happened.
    fn on_cancel(&self, hook: Hook) -> Option<u64> {
        let mut state = lock(&self.state);
        if state.cancelled {
            drop(state);
            hook();
            return None;
        }
        state.next += 1;
        let id = state.next;
        state.hooks.push((id, hook));
        Some(id)
    }

    fn forget(&self, id: Option<u64>) {
        if let Some(id) = id {
            lock(&self.state).hooks.retain(|(hook, _)| *hook != id);
        }
    }
}

/// What a server said without being asked.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    Diagnostics {
        uri: String,
        version: Option<i64>,
        items: Vec<Value>,
    },
    ProgressBegin {
        token: String,
        title: Option<String>,
    },
    ProgressEnd {
        token: String,
    },
    /// The server now answers `textDocument/diagnostic`.
    PullDiagnosticsRegistered,
    /// Diagnostics pulled earlier may be out of date.
    DiagnosticsRefresh,
    /// The stream ended: the process exited or sent something unreadable.
    Closed,
}

pub type EventHandler = Box<dyn Fn(ClientEvent) + Send + Sync>;

/// What a client needs to know before it says hello.
#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    /// The folder the server works in.
    pub root: PathBuf,
    /// Sent as `initializationOptions`; `Null` for none.
    pub initialization_options: Value,
    /// Answers `workspace/configuration`, looked up by dotted section.
    pub settings: Value,
}

/// How a server wants `textDocument/diagnostic` asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullDiagnostics {
    pub identifier: Option<String>,
    /// A change in one file can change another's diagnostics.
    pub inter_file: bool,
}

type Reply = mpsc::Sender<Result<Value, LspError>>;

struct Shared {
    options: ClientOptions,
    outgoing: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    pending: Mutex<HashMap<i64, Reply>>,
    next_id: AtomicI64,
    closed: AtomicBool,
    log: Mutex<VecDeque<String>>,
    capabilities: OnceLock<Value>,
    /// Capabilities registered after the handshake: id to method and options.
    registrations: Mutex<HashMap<String, (String, Value)>>,
    handler: EventHandler,
}

pub struct Client {
    shared: Arc<Shared>,
    process: Mutex<Option<Child>>,
    pid: Option<u32>,
}

pub fn path_to_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(String::from)
        .unwrap_or_else(|()| format!("file://{}", path.to_string_lossy()))
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    (url.scheme() == "file")
        .then(|| url.to_file_path().ok())
        .flatten()
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
}

/// What we tell every server we can do. Snippets are declined because the
/// editor has none; pull diagnostics and UTF-8 positions are offered.
fn client_capabilities() -> Value {
    let markup = json!(["markdown", "plaintext"]);
    json!({
        "general": { "positionEncodings": ["utf-8", "utf-16"] },
        "workspace": {
            "configuration": true,
            "workspaceFolders": true,
            "didChangeConfiguration": { "dynamicRegistration": true },
            "diagnostics": { "refreshSupport": true },
        },
        "window": { "workDoneProgress": true },
        "textDocument": {
            "synchronization": { "dynamicRegistration": false, "didSave": false },
            "publishDiagnostics": {
                "relatedInformation": true,
                "versionSupport": true,
                "codeDescriptionSupport": true,
                "tagSupport": { "valueSet": [1, 2] },
            },
            "diagnostic": { "dynamicRegistration": true, "relatedDocumentSupport": false },
            "definition": { "linkSupport": true },
            "references": {},
            "hover": { "contentFormat": markup },
            "completion": {
                "contextSupport": true,
                "completionItem": {
                    "snippetSupport": false,
                    "documentationFormat": markup,
                    "deprecatedSupport": true,
                    "preselectSupport": true,
                    "insertReplaceSupport": true,
                    "labelDetailsSupport": true,
                    "resolveSupport": {
                        "properties": ["documentation", "detail", "additionalTextEdits"],
                    },
                },
            },
            "signatureHelp": {
                "signatureInformation": {
                    "documentationFormat": markup,
                    "parameterInformation": { "labelOffsetSupport": true },
                    "activeParameterSupport": true,
                },
            },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
        },
    })
}

/// The part of `settings` a dotted `section` names; `Null` when there is none.
fn settings_section(settings: &Value, section: Option<&str>) -> Value {
    let mut at = settings;
    for part in section
        .unwrap_or("")
        .split('.')
        .filter(|part| !part.is_empty())
    {
        match at.get(part) {
            Some(inner) => at = inner,
            None => return Value::Null,
        }
    }
    at.clone()
}

impl Shared {
    fn log(&self, line: &str) {
        let mut log = lock(&self.log);
        for line in line.lines() {
            let mut end = line.len().min(LOG_LINE_BYTES);
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if log.len() == LOG_LINES {
                log.pop_front();
            }
            log.push_back(line[..end].to_string());
        }
    }

    fn send(&self, message: &Value) {
        if let Some(outgoing) = lock(&self.outgoing).as_ref() {
            let _ = outgoing.send(message.to_string().into_bytes());
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Stops waiting for request `id` and tells the server to stop too.
    fn abandon(&self, id: i64, outcome: LspError) {
        if let Some(reply) = lock(&self.pending).remove(&id) {
            self.notify("$/cancelRequest", json!({ "id": id }));
            let _ = reply.send(Err(outcome));
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        *lock(&self.outgoing) = None;
        let pending = std::mem::take(&mut *lock(&self.pending));
        for (_, reply) in pending {
            let _ = reply.send(Err(LspError::Closed));
        }
        (self.handler)(ClientEvent::Closed);
    }

    fn dispatch(&self, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(method), Some(id)) => {
                let response = match self.answer(method, &params) {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err((code, text)) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": code, "message": text },
                    }),
                };
                self.send(&response);
            }
            (Some(method), None) => self.notified(method, &params),
            (None, Some(id)) => {
                let Some(reply) = id.as_i64().and_then(|id| lock(&self.pending).remove(&id)) else {
                    // Late answers to requests we gave up on end here.
                    return;
                };
                let outcome = match message.get("error").filter(|error| !error.is_null()) {
                    Some(error) => Err(LspError::Server {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    }),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = reply.send(outcome);
            }
            (None, None) => self.log("[client] message with neither method nor id"),
        }
    }

    /// The answer to a request the server makes of its client.
    fn answer(&self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "workspace/configuration" => Ok(Value::Array(
                params
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                let section = item.get("section").and_then(Value::as_str);
                                settings_section(&self.options.settings, section)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            )),
            "client/registerCapability" => {
                let mut pull = false;
                for registration in params
                    .get("registrations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let id = registration.get("id").and_then(Value::as_str);
                    let method = registration.get("method").and_then(Value::as_str);
                    if let (Some(id), Some(method)) = (id, method) {
                        pull |= method == "textDocument/diagnostic";
                        let options = registration
                            .get("registerOptions")
                            .cloned()
                            .unwrap_or(Value::Null);
                        lock(&self.registrations)
                            .insert(id.to_string(), (method.to_string(), options));
                    }
                }
                if pull {
                    (self.handler)(ClientEvent::PullDiagnosticsRegistered);
                }
                Ok(Value::Null)
            }
            "client/unregisterCapability" => {
                // The protocol itself misspells the field; accept both.
                for name in ["unregisterations", "unregistrations"] {
                    for entry in params
                        .get(name)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(id) = entry.get("id").and_then(Value::as_str) {
                            lock(&self.registrations).remove(id);
                        }
                    }
                }
                Ok(Value::Null)
            }
            "window/workDoneProgress/create" => Ok(Value::Null),
            "workspace/workspaceFolders" => Ok(json!([{
                "uri": path_to_uri(&self.options.root),
                "name": folder_name(&self.options.root),
            }])),
            "workspace/diagnostic/refresh" => {
                (self.handler)(ClientEvent::DiagnosticsRefresh);
                Ok(Value::Null)
            }
            "workspace/semanticTokens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/inlineValue/refresh"
            | "workspace/foldingRange/refresh" => Ok(Value::Null),
            "window/showMessageRequest" => {
                if let Some(text) = params.get("message").and_then(Value::as_str) {
                    self.log(&format!("[server] {text}"));
                }
                Ok(Value::Null)
            }
            "window/showDocument" => Ok(json!({ "success": false })),
            "workspace/applyEdit" => Ok(json!({
                "applied": false,
                "failureReason": "Edits from the server are not supported.",
            })),
            _ => Err((METHOD_NOT_FOUND, format!("Method not found: {method}"))),
        }
    }

    fn notified(&self, method: &str, params: &Value) {
        match method {
            "textDocument/publishDiagnostics" => {
                let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                    return;
                };
                (self.handler)(ClientEvent::Diagnostics {
                    uri: uri.to_string(),
                    version: params.get("version").and_then(Value::as_i64),
                    items: params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                });
            }
            "$/progress" => {
                let token = match params.get("token") {
                    Some(Value::String(token)) => token.clone(),
                    Some(Value::Number(token)) => token.to_string(),
                    _ => return,
                };
                let value = params.get("value");
                let field = |name: &str| value.and_then(|value| value.get(name)?.as_str());
                match field("kind") {
                    Some("begin") => (self.handler)(ClientEvent::ProgressBegin {
                        token,
                        title: field("title").map(str::to_string),
                    }),
                    Some("end") => (self.handler)(ClientEvent::ProgressEnd { token }),
                    _ => {}
                }
            }
            "window/logMessage" | "window/showMessage" => {
                if let Some(text) = params.get("message").and_then(Value::as_str) {
                    self.log(&format!("[server] {text}"));
                }
            }
            _ => {}
        }
    }
}

fn spawn_thread(name: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
        .map(|_| ())
}

impl Client {
    /// Takes over a started server's streams. Nothing is sent yet; call
    /// `initialize` next. `handler` runs on the reader thread and must not
    /// call back into a blocking method of this client.
    pub fn start(
        transport: Transport,
        options: ClientOptions,
        handler: EventHandler,
    ) -> io::Result<Self> {
        let Transport {
            reader,
            mut writer,
            stderr,
            process,
        } = transport;
        let (outgoing, queue) = mpsc::channel::<Vec<u8>>();
        let shared = Arc::new(Shared {
            options,
            outgoing: Mutex::new(Some(outgoing)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            closed: AtomicBool::new(false),
            log: Mutex::new(VecDeque::new()),
            capabilities: OnceLock::new(),
            registrations: Mutex::new(HashMap::new()),
            handler,
        });
        let pid = process.as_ref().map(Child::id);
        let client = Self {
            shared: shared.clone(),
            process: Mutex::new(process),
            pid,
        };
        spawn_thread("lsp-writer", move || {
            for body in queue {
                if wire::write_message(&mut writer, &body).is_err() {
                    break;
                }
            }
        })?;
        if let Some(stderr) = stderr {
            let shared = Arc::downgrade(&shared);
            spawn_thread("lsp-stderr", move || read_stderr(stderr, &shared))?;
        }
        spawn_thread("lsp-reader", move || {
            let mut reader = BufReader::new(reader);
            loop {
                match wire::read_message(&mut reader) {
                    Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                        Ok(message) => shared.dispatch(message),
                        Err(error) => shared.log(&format!("[client] unreadable message: {error}")),
                    },
                    Ok(None) => break,
                    Err(error) => {
                        // Without a length there is no finding the next message.
                        shared.log(&format!("[client] broken stream: {error}"));
                        break;
                    }
                }
            }
            shared.close();
        })?;
        Ok(client)
    }

    /// The handshake. Blocks until the server answers or `timeout` passes.
    pub fn initialize(&self, timeout: Duration) -> Result<(), LspError> {
        let options = &self.shared.options;
        let uri = path_to_uri(&options.root);
        let result = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "Monocode" },
                "rootPath": options.root.to_string_lossy(),
                "rootUri": uri,
                "workspaceFolders": [{ "uri": uri, "name": folder_name(&options.root) }],
                "capabilities": client_capabilities(),
                "initializationOptions": options.initialization_options,
            }),
            timeout,
            &CancelToken::new(),
        )?;
        let capabilities = result.get("capabilities").cloned().unwrap_or(Value::Null);
        let _ = self.shared.capabilities.set(capabilities);
        self.notify("initialized", json!({}));
        if !options.settings.is_null() {
            // Servers that do not ask for their settings are told them.
            self.notify(
                "workspace/didChangeConfiguration",
                json!({ "settings": options.settings }),
            );
        }
        Ok(())
    }

    /// Asks and waits for the answer, for at most `timeout`. Giving up, by
    /// timeout or through `cancel`, also tells the server to stop.
    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<Value, LspError> {
        if cancel.is_cancelled() {
            return Err(LspError::Cancelled);
        }
        let shared = &self.shared;
        let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
        let (reply, answer) = mpsc::channel();
        lock(&shared.pending).insert(id, reply);
        // Checked after registering, so a close in between cannot strand us.
        if shared.closed.load(Ordering::SeqCst) {
            lock(&shared.pending).remove(&id);
            return Err(LspError::Closed);
        }
        shared.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let hook = {
            let shared: Weak<Shared> = Arc::downgrade(shared);
            cancel.on_cancel(Box::new(move || {
                if let Some(shared) = shared.upgrade() {
                    shared.abandon(id, LspError::Cancelled);
                }
            }))
        };
        let outcome = answer.recv_timeout(timeout);
        cancel.forget(hook);
        match outcome {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                shared.abandon(id, LspError::Timeout);
                Err(LspError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(LspError::Closed),
        }
    }

    /// Queues a notification; never blocks.
    pub fn notify(&self, method: &str, params: Value) {
        self.shared.notify(method, params);
    }

    /// What the server said it can do; `Null` before `initialize`.
    pub fn capabilities(&self) -> &Value {
        static NONE: Value = Value::Null;
        self.shared.capabilities.get().unwrap_or(&NONE)
    }

    /// Whether the server declared `provider`, e.g. `hoverProvider`.
    pub fn provides(&self, provider: &str) -> bool {
        !matches!(
            self.capabilities().get(provider),
            None | Some(Value::Null | Value::Bool(false))
        )
    }

    pub fn encoding(&self) -> Encoding {
        Encoding::from_name(
            self.capabilities()
                .get("positionEncoding")
                .and_then(Value::as_str),
        )
    }

    /// Whether the server wants to hear about edits at all.
    pub fn syncs_changes(&self) -> bool {
        let sync = self.capabilities().get("textDocumentSync");
        let kind = match sync {
            Some(Value::Number(kind)) => kind.as_u64(),
            Some(Value::Object(options)) => options.get("change").and_then(Value::as_u64),
            _ => None,
        };
        // 0 is "none"; full and incremental both accept the whole text.
        kind.is_some_and(|kind| kind != 0)
    }

    /// How to pull diagnostics, when the server offers that, statically or
    /// by a later registration.
    pub fn pull_diagnostics(&self) -> Option<PullDiagnostics> {
        let registrations = lock(&self.shared.registrations);
        let options = match self.capabilities().get("diagnosticProvider") {
            Some(options) if !options.is_null() && options != &Value::Bool(false) => options,
            _ => registrations
                .values()
                .find(|(method, _)| method == "textDocument/diagnostic")
                .map(|(_, options)| options)?,
        };
        Some(PullDiagnostics {
            identifier: options
                .get("identifier")
                .and_then(Value::as_str)
                .map(str::to_string),
            inter_file: options
                .get("interFileDependencies")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// The last `lines` lines the server wrote to standard error or logged.
    pub fn log_tail(&self, lines: usize) -> String {
        let log = lock(&self.shared.log);
        let skip = log.len().saturating_sub(lines);
        log.iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Asks the server to leave, then makes sure it has. Blocks for up to
    /// about three seconds, so call it off the UI thread.
    pub fn shutdown(&self) {
        if !self.is_closed() {
            let asked = self.request(
                "shutdown",
                Value::Null,
                Duration::from_secs(2),
                &CancelToken::new(),
            );
            if asked.is_ok() {
                self.notify("exit", Value::Null);
            }
        }
        let mut process = lock(&self.process);
        if let Some(child) = process.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(1);
            // Looked at without reaping: a launcher that has left may have
            // workers still in its group, and the kill must reach them.
            while Instant::now() < deadline && !child::exited(child) {
                std::thread::sleep(Duration::from_millis(15));
            }
            child::kill_group(child);
        }
        *process = None;
        drop(process);
        *lock(&self.shared.outgoing) = None;
    }
}

impl Client {
    /// Ends the server without asking. Requests still waiting fail with
    /// `LspError::Closed` once its stream ends.
    pub fn kill(&self) {
        // Closing its input lets a well-behaved server leave by itself; the
        // kill is for the rest.
        *lock(&self.shared.outgoing) = None;
        if let Some(mut child) = lock(&self.process).take() {
            child::kill_group(&mut child);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read_stderr(stderr: Box<dyn Read + Send>, shared: &Weak<Shared>) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        // Capped: a server that writes on without a line end is logged in
        // pieces instead of being buffered whole.
        match wire::read_line_capped(&mut reader, &mut line, wire::MAX_LINE_BYTES) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Some(shared) = shared.upgrade() else {
                    break;
                };
                shared.log(String::from_utf8_lossy(&line).trim_end());
            }
        }
    }
}

#[cfg(test)]
mod tests;
