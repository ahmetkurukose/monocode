//! One debug session: a connection to an adapter and what is known about
//! the debuggee, kept as a cache the interface reads without waiting. The
//! adapter's events refresh the cache on a thread of the session and every
//! change is announced to subscribers. Control calls return at once and run
//! in the background; the calls that return data block for the adapter's
//! answer and are for background threads and the agents' tools.
use super::adapter::{Adapter, DebugRequest, RequestKind};
use super::breakpoints::BreakpointStore;
use super::client::{lock, Client, Incoming, ReverseRequest, REQUEST_TIMEOUT};
use super::types::{
    Actor, DapError, EvaluateContext, Evaluation, Frame, FrameLocals, Output, OutputCategory, Page,
    Scope, SessionState, StepKind, Stop, Thread, Variable,
};
use crate::code::spawn::Spawn;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

mod sync;

pub type SessionId = u64;

/// Starting a debuggee can wait on a terminal and on a slow import.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an adapter gets to end the debuggee by itself when a session is dropped.
const DROP_GRACE: Duration = Duration::from_millis(300);
/// The output kept for late readers; older text is dropped first.
const OUTPUT_KEPT: usize = 2 * 1024 * 1024;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub enum SessionEvent {
    /// Every transition; `Stopped` and `Terminated` follow their own.
    StateChanged(SessionState),
    Stopped(Stop),
    Terminated {
        exit_code: Option<i32>,
    },
    /// The cached list named here was replaced; read it from the session.
    Threads,
    Stack {
        thread_id: i64,
    },
    Scopes {
        frame_id: i64,
    },
    Variables {
        reference: i64,
    },
    Output(Output),
    /// The adapter wants the debuggee started in a terminal of the app.
    RunInTerminal(TerminalRequest),
    /// A process of the debuggee is being debugged as a session of its own.
    ChildStarted(SessionId),
    DriverChanged(Actor),
    /// A background call failed; `message` is a sentence for the user.
    Failed {
        action: String,
        message: String,
    },
}

/// The debuggee to start in a terminal, and the way to tell the adapter how
/// that went. Clones share the answer; the first one given counts.
#[derive(Clone)]
pub struct TerminalRequest {
    /// The command with the environment to add to the checkout's own.
    pub spawn: Spawn,
    reply: Arc<Mutex<Option<ReverseRequest>>>,
}

impl TerminalRequest {
    /// The terminal runs the command, as this process when known.
    pub fn started(&self, process_id: Option<u32>) {
        if let Some(request) = lock(&self.reply).take() {
            let body = process_id.map_or_else(|| json!({}), |id| json!({ "processId": id }));
            request.reply(Ok(body));
        }
    }

    pub fn failed(&self, message: &str) {
        if let Some(request) = lock(&self.reply).take() {
            request.reply(Err(message.to_string()));
        }
    }
}

/// A cached list to fetch again in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    Threads,
    Stack(i64),
    Scopes(i64),
    Variables(i64, Option<Page>),
}

pub(crate) struct Setup {
    pub adapter: &'static dyn Adapter,
    pub request: DebugRequest,
    pub store: BreakpointStore,
    pub origin: Actor,
    pub driver: Actor,
    pub parent: Option<SessionId>,
}

enum Job {
    Run(Box<dyn FnOnce(&Session) + Send>),
    Stop,
}

struct Cache {
    state: SessionState,
    /// Counts stops, so a waiter can tell a new stop from the one it left.
    stops: u64,
    configured: bool,
    threads: Vec<Thread>,
    stacks: HashMap<i64, Vec<Frame>>,
    scopes: HashMap<i64, Vec<Scope>>,
    variables: HashMap<(i64, Option<Page>), Vec<Variable>>,
    output: VecDeque<Output>,
    output_bytes: usize,
    exit_code: Option<i32>,
    process_id: Option<u32>,
    failure: Option<String>,
    terminal: Option<TerminalRequest>,
    /// Files the adapter holds breakpoints for, to clear them when the last goes.
    sent_files: HashSet<PathBuf>,
    /// The adapter's breakpoint ids, for its later `breakpoint` events.
    adapter_breakpoints: HashMap<i64, (PathBuf, u64)>,
}

pub struct Session {
    id: SessionId,
    parent: Option<SessionId>,
    adapter: &'static dyn Adapter,
    request: DebugRequest,
    client: Client,
    store: BreakpointStore,
    origin: Actor,
    driver: Mutex<Actor>,
    capabilities: Mutex<Value>,
    cache: Mutex<Cache>,
    changed: Condvar,
    subscribers: Mutex<Vec<async_channel::Sender<SessionEvent>>>,
    jobs: Mutex<mpsc::Sender<Job>>,
    /// Held while a file's breakpoints are read and sent, so that two
    /// changes cannot reach the adapter in the wrong order.
    sync: Mutex<()>,
    children: Mutex<Vec<Arc<Session>>>,
    this: Weak<Session>,
}

impl Session {
    /// Takes a connected adapter through `initialize` and sends the launch
    /// or attach request. Blocks for the adapter's first answer.
    pub(crate) fn open(
        client: Client,
        incoming: mpsc::Receiver<Incoming>,
        setup: Setup,
    ) -> Result<Arc<Self>, DapError> {
        let (jobs, queue) = mpsc::channel();
        let session = Arc::new_cyclic(|this| Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            parent: setup.parent,
            adapter: setup.adapter,
            request: setup.request,
            client,
            store: setup.store,
            origin: setup.origin,
            driver: Mutex::new(setup.driver),
            capabilities: Mutex::new(Value::Null),
            cache: Mutex::new(Cache {
                state: SessionState::Starting,
                stops: 0,
                configured: false,
                threads: Vec::new(),
                stacks: HashMap::new(),
                scopes: HashMap::new(),
                variables: HashMap::new(),
                output: VecDeque::new(),
                output_bytes: 0,
                exit_code: None,
                process_id: None,
                failure: None,
                terminal: None,
                sent_files: HashSet::new(),
                adapter_breakpoints: HashMap::new(),
            }),
            changed: Condvar::new(),
            subscribers: Mutex::new(Vec::new()),
            jobs: Mutex::new(jobs),
            sync: Mutex::new(()),
            children: Mutex::new(Vec::new()),
            this: this.clone(),
        });

        // Both threads hold the session weakly: it ends when its owners let go.
        let weak = Arc::downgrade(&session);
        spawn_thread("dap-worker", move || {
            while let Ok(Job::Run(job)) = queue.recv() {
                let Some(session) = weak.upgrade() else { break };
                job(&session);
            }
        });
        let weak = Arc::downgrade(&session);
        spawn_thread("dap-events", move || {
            for message in incoming {
                let Some(session) = weak.upgrade() else { break };
                match message {
                    Incoming::Event { event, body } => session.on_event(&event, &body),
                    Incoming::Request(request) => session.on_request(request),
                    Incoming::Closed => {
                        session.finish();
                        break;
                    }
                }
            }
        });

        let capabilities = session
            .client
            .request(
                "initialize",
                json!({
                    "clientID": "monocode",
                    "clientName": "Monocode",
                    "adapterID": session.adapter.id(),
                    "locale": "en",
                    "pathFormat": "path",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "supportsVariableType": true,
                    "supportsVariablePaging": true,
                    "supportsRunInTerminalRequest": true,
                    "supportsStartDebuggingRequest": true,
                }),
                REQUEST_TIMEOUT,
            )
            .map_err(|error| match error {
                DapError::Closed => {
                    let tail = session.client.stderr_tail();
                    DapError::Start(
                        format!("The debug adapter exited as it started. {tail}")
                            .trim()
                            .to_string(),
                    )
                }
                other => other,
            })?;
        *lock(&session.capabilities) = capabilities;

        let weak = Arc::downgrade(&session);
        session.store.listen(move |change| {
            let Some(session) = weak.upgrade() else {
                return false;
            };
            session.on_store_change(change);
            true
        });

        let command = match session.request.kind {
            RequestKind::Launch => "launch",
            RequestKind::Attach => "attach",
        };
        let started = session
            .client
            .send(command, Value::Object(session.request.arguments.clone()))?;
        // The adapter answers only once the debuggee is configured and
        // running, which needs this session's event thread meanwhile.
        let weak = Arc::downgrade(&session);
        spawn_thread("dap-start", move || {
            let result = started.wait(START_TIMEOUT);
            if let (Err(error), Some(session)) = (result, weak.upgrade()) {
                if !session.state().is_terminated() {
                    session.fail_start(&error.to_string());
                }
            }
        });
        Ok(session)
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// The session whose debuggee started this one's process.
    pub fn parent(&self) -> Option<SessionId> {
        self.parent
    }

    pub fn label(&self) -> &str {
        &self.request.label
    }

    pub fn request(&self) -> &DebugRequest {
        &self.request
    }

    /// Sessions for the processes this debuggee started, ended ones included
    /// until the next one starts.
    pub fn children(&self) -> Vec<Arc<Session>> {
        lock(&self.children).clone()
    }

    pub fn origin(&self) -> &Actor {
        &self.origin
    }

    /// Who steers the session now. Nothing here enforces it: a caller that
    /// acts for an agent checks `driver()` first.
    pub fn driver(&self) -> Actor {
        lock(&self.driver).clone()
    }

    /// Hands the session, and its child sessions, to `actor`.
    pub fn take_over(&self, actor: Actor) {
        let changed = {
            let mut driver = lock(&self.driver);
            let changed = *driver != actor;
            *driver = actor.clone();
            changed
        };
        if changed {
            self.emit(SessionEvent::DriverChanged(actor.clone()));
        }
        for child in self.children() {
            child.take_over(actor.clone());
        }
    }

    /// Events from now on. Each subscriber gets every event.
    pub fn subscribe(&self) -> async_channel::Receiver<SessionEvent> {
        let (sender, receiver) = async_channel::unbounded();
        lock(&self.subscribers).push(sender);
        receiver
    }

    /// Whether the adapter announced the capability (`supportsSetVariable`).
    pub fn supports(&self, capability: &str) -> bool {
        lock(&self.capabilities)
            .get(capability)
            .and_then(Value::as_bool)
            == Some(true)
    }

    // The cache: immediate, for the interface thread.

    pub fn state(&self) -> SessionState {
        lock(&self.cache).state.clone()
    }

    /// Why the session could not start, once it has failed.
    pub fn failure(&self) -> Option<String> {
        lock(&self.cache).failure.clone()
    }

    pub fn cached_threads(&self) -> Vec<Thread> {
        lock(&self.cache).threads.clone()
    }

    pub fn cached_stack(&self, thread_id: i64) -> Option<Vec<Frame>> {
        lock(&self.cache).stacks.get(&thread_id).cloned()
    }

    pub fn cached_scopes(&self, frame_id: i64) -> Option<Vec<Scope>> {
        lock(&self.cache).scopes.get(&frame_id).cloned()
    }

    pub fn cached_variables(&self, reference: i64, page: Option<Page>) -> Option<Vec<Variable>> {
        lock(&self.cache).variables.get(&(reference, page)).cloned()
    }

    /// What the debuggee and the debugger printed, oldest first.
    pub fn output(&self) -> Vec<Output> {
        lock(&self.cache).output.iter().cloned().collect()
    }

    /// The terminal the adapter asked for and nobody has answered yet.
    pub fn pending_terminal(&self) -> Option<TerminalRequest> {
        let terminal = lock(&self.cache).terminal.clone()?;
        let open = lock(&terminal.reply).is_some();
        open.then_some(terminal)
    }

    /// Fetches a list again in the background; its event follows.
    pub fn refresh(&self, what: Refresh) {
        self.background("refresh", move |session| match what {
            Refresh::Threads => session.threads().map(drop),
            Refresh::Stack(thread_id) => session.fetch_stack(thread_id).map(drop),
            Refresh::Scopes(frame_id) => session.fetch_scopes(frame_id).map(drop),
            Refresh::Variables(reference, page) => {
                session.fetch_variables(reference, page).map(drop)
            }
        });
    }

    // Control: returns at once; a failure arrives as `SessionEvent::Failed`.

    /// Lets the debuggee run on. `thread_id` defaults to the stopped thread.
    pub fn resume(&self, thread_id: Option<i64>) {
        self.background("continue", move |session| {
            session.run("continue", thread_id).map(drop)
        });
    }

    pub fn step(&self, kind: StepKind, thread_id: Option<i64>) {
        self.background("step", move |session| {
            session.run(kind.command(), thread_id).map(drop)
        });
    }

    pub fn pause(&self, thread_id: Option<i64>) {
        self.background("pause", move |session| {
            let thread_id = session.pick_thread(thread_id)?;
            session
                .ask("pause", json!({ "threadId": thread_id }))
                .map(drop)
        });
    }

    /// Ends the debuggee (or, for an attached one, lets it go).
    pub fn terminate(&self) {
        self.background("terminate", |session| session.end(true));
    }

    /// Leaves the debuggee. One this session launched ends with it when the
    /// adapter cannot let it run on, as debugpy cannot.
    pub fn disconnect(&self) {
        self.background("disconnect", |session| session.end(false));
    }

    // Data and waiting: these block the calling thread for the adapter.

    pub fn threads(&self) -> Result<Vec<Thread>, DapError> {
        let threads = Thread::list(&self.ask("threads", Value::Null)?);
        lock(&self.cache).threads = threads.clone();
        self.emit(SessionEvent::Threads);
        Ok(threads)
    }

    /// The frames of a stopped thread, innermost first; cached until the
    /// debuggee runs again, like scopes and variables.
    pub fn stack(&self, thread_id: i64) -> Result<Vec<Frame>, DapError> {
        match self.cached_stack(thread_id) {
            Some(frames) => Ok(frames),
            None => self.fetch_stack(thread_id),
        }
    }

    pub fn scopes(&self, frame_id: i64) -> Result<Vec<Scope>, DapError> {
        match self.cached_scopes(frame_id) {
            Some(scopes) => Ok(scopes),
            None => self.fetch_scopes(frame_id),
        }
    }

    /// The children of a scope or of a variable with a reference. `page`
    /// selects part of a large sequence's indexed children.
    pub fn variables(&self, reference: i64, page: Option<Page>) -> Result<Vec<Variable>, DapError> {
        match self.cached_variables(reference, page) {
            Some(variables) => Ok(variables),
            None => self.fetch_variables(reference, page),
        }
    }

    /// Evaluates in a frame, or globally without one. `Repl` may have side
    /// effects in the debuggee; `Hover` and `Watch` are meant not to.
    pub fn evaluate(
        &self,
        expression: &str,
        frame_id: Option<i64>,
        context: EvaluateContext,
    ) -> Result<Evaluation, DapError> {
        if context == EvaluateContext::Hover && !self.supports("supportsEvaluateForHovers") {
            return Err(DapError::Unsupported("evaluating on hover".into()));
        }
        let mut arguments = json!({ "expression": expression, "context": context.name() });
        if let Some(frame_id) = frame_id {
            arguments["frameId"] = json!(frame_id);
        }
        let result = Evaluation::from_body(&self.ask("evaluate", arguments)?);
        if context == EvaluateContext::Repl {
            // It may have assigned to anything.
            self.forget_variables(None);
        }
        Ok(result)
    }

    /// Assigns to a child of `reference` and returns the value as it now reads.
    pub fn set_variable(
        &self,
        reference: i64,
        name: &str,
        value: &str,
    ) -> Result<Evaluation, DapError> {
        if !self.supports("supportsSetVariable") {
            return Err(DapError::Unsupported("changing a variable".into()));
        }
        let body = self.ask(
            "setVariable",
            json!({ "variablesReference": reference, "name": name, "value": value }),
        )?;
        self.forget_variables(Some(reference));
        Ok(Evaluation::from_body(&body))
    }

    /// The stack at the stop with the local variables of its first `frames` frames.
    pub fn stack_with_locals(&self, frames: usize) -> Result<Vec<FrameLocals>, DapError> {
        let thread_id = self.pick_thread(None)?;
        let mut result = Vec::new();
        for (index, frame) in self.stack(thread_id)?.into_iter().enumerate() {
            let mut locals = Vec::new();
            if index < frames {
                let scopes = self.scopes(frame.id)?;
                let scope = scopes
                    .iter()
                    .find(|scope| !scope.expensive)
                    .or(scopes.first());
                if let Some(scope) = scope {
                    locals = self.variables(scope.variables_reference, None)?;
                }
            }
            result.push(FrameLocals { frame, locals });
        }
        Ok(result)
    }

    /// Waits until the debuggee is stopped or over and returns that state;
    /// at once when it already is.
    pub fn wait_for_stop(&self, timeout: Duration) -> Result<SessionState, DapError> {
        self.wait(timeout, |cache| {
            matches!(
                cache.state,
                SessionState::Stopped(_) | SessionState::Terminated { .. }
            )
        })
    }

    /// Waits until the session is over and returns the exit code, if any.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<Option<i32>, DapError> {
        match self.wait(timeout, |cache| cache.state.is_terminated())? {
            SessionState::Terminated { exit_code } => Ok(exit_code),
            _ => Ok(None),
        }
    }

    /// Continues and waits for the next stop or the end.
    pub fn continue_and_wait(&self, timeout: Duration) -> Result<SessionState, DapError> {
        let stops = self.run("continue", None)?;
        self.wait_after(stops, timeout)
    }

    pub fn step_and_wait(
        &self,
        kind: StepKind,
        timeout: Duration,
    ) -> Result<SessionState, DapError> {
        let stops = self.run(kind.command(), None)?;
        self.wait_after(stops, timeout)
    }

    fn wait_after(&self, stops: u64, timeout: Duration) -> Result<SessionState, DapError> {
        self.wait(timeout, |cache| {
            cache.stops > stops || cache.state.is_terminated()
        })
    }

    fn wait(
        &self,
        timeout: Duration,
        done: impl Fn(&Cache) -> bool,
    ) -> Result<SessionState, DapError> {
        let deadline = Instant::now() + timeout;
        let mut cache = lock(&self.cache);
        loop {
            if done(&cache) {
                return Ok(cache.state.clone());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(DapError::StillRunning);
            };
            cache = self
                .changed
                .wait_timeout(cache, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    // Requests.

    fn ask(&self, command: &str, arguments: Value) -> Result<Value, DapError> {
        self.client.request(command, arguments, REQUEST_TIMEOUT)
    }

    fn fetch_stack(&self, thread_id: i64) -> Result<Vec<Frame>, DapError> {
        let epoch = lock(&self.cache).stops;
        let body = self.ask(
            "stackTrace",
            json!({ "threadId": thread_id, "startFrame": 0 }),
        )?;
        let frames = Frame::list(&body);
        self.keep(epoch, |cache| {
            cache.stacks.insert(thread_id, frames.clone())
        });
        self.emit(SessionEvent::Stack { thread_id });
        Ok(frames)
    }

    fn fetch_scopes(&self, frame_id: i64) -> Result<Vec<Scope>, DapError> {
        let epoch = lock(&self.cache).stops;
        let scopes = Scope::list(&self.ask("scopes", json!({ "frameId": frame_id }))?);
        self.keep(epoch, |cache| cache.scopes.insert(frame_id, scopes.clone()));
        self.emit(SessionEvent::Scopes { frame_id });
        Ok(scopes)
    }

    fn fetch_variables(
        &self,
        reference: i64,
        page: Option<Page>,
    ) -> Result<Vec<Variable>, DapError> {
        let epoch = lock(&self.cache).stops;
        let mut arguments = json!({ "variablesReference": reference });
        let paged = page.filter(|_| self.supports("supportsVariablePaging"));
        if let Some(page) = paged {
            arguments["filter"] = json!("indexed");
            arguments["start"] = json!(page.start);
            arguments["count"] = json!(page.count);
        }
        let mut variables = Variable::list(&self.ask("variables", arguments)?);
        if let (Some(page), None) = (page, paged) {
            // The adapter sent everything; the page is cut here.
            let start = usize::try_from(page.start).unwrap_or(usize::MAX);
            let count = usize::try_from(page.count).unwrap_or(usize::MAX);
            variables = variables.into_iter().skip(start).take(count).collect();
        }
        self.keep(epoch, |cache| {
            cache.variables.insert((reference, page), variables.clone())
        });
        self.emit(SessionEvent::Variables { reference });
        Ok(variables)
    }

    /// Caches an answer unless the debuggee has moved on since it was asked for.
    fn keep<T>(&self, epoch: u64, insert: impl FnOnce(&mut Cache) -> T) {
        let mut cache = lock(&self.cache);
        if cache.stops == epoch && !cache.state.is_terminated() {
            insert(&mut cache);
        }
    }

    fn forget_variables(&self, reference: Option<i64>) {
        let references: HashSet<i64> = {
            let mut cache = lock(&self.cache);
            cache
                .variables
                .drain()
                .map(|((reference, _), _)| reference)
                .collect()
        };
        for reference in references.into_iter().chain(reference) {
            self.emit(SessionEvent::Variables { reference });
        }
    }

    fn pick_thread(&self, thread_id: Option<i64>) -> Result<i64, DapError> {
        let cache = lock(&self.cache);
        thread_id
            .or_else(|| cache.state.stop().and_then(|stop| stop.thread_id))
            .or_else(|| cache.threads.first().map(|thread| thread.id))
            .ok_or_else(|| DapError::State("The debugger has not reported a thread yet.".into()))
    }

    /// Sends `continue` or a step for a stopped debuggee and returns the
    /// number of stops so far, for a caller that waits for the next.
    fn run(&self, command: &str, thread_id: Option<i64>) -> Result<u64, DapError> {
        let (before, stops) = {
            let cache = lock(&self.cache);
            (cache.state.clone(), cache.stops)
        };
        if before.stop().is_none() {
            return Err(DapError::State("The program is not paused.".into()));
        }
        let thread_id = self.pick_thread(thread_id)?;
        // Shown as running before the adapter confirms: its answer and the
        // next stop can arrive in either order.
        self.set_state(SessionState::Running);
        if let Err(error) = self.ask(command, json!({ "threadId": thread_id })) {
            let mut cache = lock(&self.cache);
            if cache.stops == stops && cache.state == SessionState::Running {
                cache.state = before.clone();
                // Announced like every other transition, under the lock
                // and with the waiters woken: the debuggee is stopped
                // again, and who waits for a stop must not sleep on.
                self.emit(SessionEvent::StateChanged(before));
                self.changed.notify_all();
            }
            return Err(error);
        }
        Ok(stops)
    }

    fn end(&self, terminate: bool) -> Result<(), DapError> {
        if self.state().is_terminated() {
            return Ok(());
        }
        let launched = self.request.kind == RequestKind::Launch;
        if terminate && launched && self.supports("supportsTerminateRequest") {
            self.ask("terminate", json!({})).map(drop)
        } else {
            let result = self.ask(
                "disconnect",
                json!({ "terminateDebuggee": terminate && launched }),
            );
            self.finish();
            match result {
                // An adapter may hang up instead of answering its last request.
                Ok(_) | Err(DapError::Closed) => Ok(()),
                Err(error) => Err(error),
            }
        }
    }

    // The adapter's events.

    fn on_event(&self, event: &str, body: &Value) {
        match event {
            "initialized" => self.configure(),
            "stopped" => self.on_stopped(body),
            "continued" => {
                if self.state().stop().is_some() {
                    self.set_state(SessionState::Running);
                }
            }
            "thread" => {
                if lock(&self.cache).configured {
                    self.refresh(Refresh::Threads);
                }
            }
            "output" => self.on_output(body),
            "process" => {
                let id = body.get("systemProcessId").and_then(Value::as_u64);
                lock(&self.cache).process_id = id.and_then(|id| u32::try_from(id).ok());
            }
            "exited" => {
                let code = body.get("exitCode").and_then(Value::as_i64);
                lock(&self.cache).exit_code = code.and_then(|code| i32::try_from(code).ok());
            }
            "terminated" => self.finish(),
            "breakpoint" => self.on_breakpoint(body.get("breakpoint").unwrap_or(&Value::Null)),
            "capabilities" => {
                let update = body.get("capabilities").and_then(Value::as_object);
                let mut capabilities = lock(&self.capabilities);
                if let (Some(update), Some(known)) = (update, capabilities.as_object_mut()) {
                    known.extend(update.clone());
                }
            }
            other => {
                if let Some(configuration) = self.adapter.child_from_event(other, body) {
                    self.start_child(configuration);
                }
            }
        }
    }

    fn on_request(&self, request: ReverseRequest) {
        match request.command.as_str() {
            "runInTerminal" => {
                let arguments = &request.arguments;
                let mut words = arguments
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|word| word.as_str().map(str::to_string));
                let Some(command) = words.next() else {
                    request.reply(Err("The request names no command.".into()));
                    return;
                };
                let text = |key: &str| arguments.get(key).and_then(Value::as_str);
                let env = arguments.get("env").and_then(Value::as_object);
                let spawn = Spawn {
                    label: text("title").unwrap_or(self.label()).to_string(),
                    command,
                    args: words.collect(),
                    cwd: text("cwd").map_or_else(|| self.request.cwd.clone(), PathBuf::from),
                    // A null value asks for the variable to be unset; the
                    // terminal's environment is the checkout's, so it is skipped.
                    env: env
                        .into_iter()
                        .flatten()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect(),
                };
                let terminal = TerminalRequest {
                    spawn,
                    reply: Arc::new(Mutex::new(Some(request))),
                };
                lock(&self.cache).terminal = Some(terminal.clone());
                self.emit(SessionEvent::RunInTerminal(terminal));
            }
            "startDebugging" => {
                let configuration = request
                    .arguments
                    .get("configuration")
                    .and_then(Value::as_object)
                    .cloned();
                match configuration {
                    Some(configuration) => {
                        request.reply(Ok(json!({})));
                        self.start_child(configuration);
                    }
                    None => request.reply(Err("The request has no configuration.".into())),
                }
            }
            _ => request.reply(Err("Monocode does not handle this request.".into())),
        }
    }

    /// The adapter is ready for breakpoints; after them the debuggee runs.
    fn configure(&self) {
        lock(&self.cache).configured = true;
        for path in self.store.files() {
            self.sync_file(&path);
        }
        self.sync_exceptions();
        if self.supports("supportsConfigurationDoneRequest") {
            if let Err(error) = self.ask("configurationDone", json!({})) {
                self.fail_start(&error.to_string());
                return;
            }
        }
        if self.state() == SessionState::Starting {
            self.set_state(SessionState::Running);
        }
    }

    fn on_stopped(&self, body: &Value) {
        {
            // Frame ids and variable references of the last stop are void.
            let mut cache = lock(&self.cache);
            cache.stacks.clear();
            cache.scopes.clear();
            cache.variables.clear();
        }
        let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
        let reason = text("reason").unwrap_or_default();
        let threads = self.threads().unwrap_or_default();
        let thread_id = body
            .get("threadId")
            .and_then(Value::as_i64)
            .or_else(|| threads.first().map(|thread| thread.id));
        let frames = thread_id
            .and_then(|id| self.fetch_stack(id).ok())
            .unwrap_or_default();
        let top = frames.first();
        let mut exception = None;
        if reason == "exception" {
            if let (Some(id), true) = (thread_id, self.supports("supportsExceptionInfoRequest")) {
                if let Ok(info) = self.ask("exceptionInfo", json!({ "threadId": id })) {
                    let part =
                        |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default();
                    let (name, description) = (part("exceptionId"), part("description"));
                    exception = Some(match (name.is_empty(), description.is_empty()) {
                        (false, false) => format!("{name}: {description}"),
                        _ => format!("{name}{description}"),
                    });
                }
            }
            exception = exception
                .filter(|text| !text.is_empty())
                .or_else(|| text("text"))
                .or_else(|| text("description"));
        }
        self.set_state(SessionState::Stopped(Stop {
            reason,
            thread_id,
            frame_id: top.map(|frame| frame.id),
            path: top.and_then(|frame| frame.path.clone()),
            line: top.map(|frame| frame.line),
            exception,
        }));
    }

    fn on_output(&self, body: &Value) {
        let category = match body.get("category").and_then(Value::as_str) {
            Some("stdout") => OutputCategory::Stdout,
            Some("stderr") => OutputCategory::Stderr,
            Some("telemetry") => return,
            _ => OutputCategory::Console,
        };
        let text = body
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let output = Output {
            category,
            text: text.to_string(),
        };
        {
            let mut cache = lock(&self.cache);
            cache.output_bytes += output.text.len();
            cache.output.push_back(output.clone());
            while cache.output_bytes > OUTPUT_KEPT && cache.output.len() > 1 {
                if let Some(old) = cache.output.pop_front() {
                    cache.output_bytes -= old.text.len();
                }
            }
        }
        self.emit(SessionEvent::Output(output));
    }

    fn set_state(&self, state: SessionState) {
        {
            let mut cache = lock(&self.cache);
            // Over is over, whatever arrives late.
            if cache.state.is_terminated() {
                return;
            }
            match &state {
                SessionState::Stopped(_) => cache.stops += 1,
                _ if cache.state == state => return,
                _ => {
                    cache.stacks.clear();
                    cache.scopes.clear();
                    cache.variables.clear();
                }
            }
            if state.is_terminated() {
                cache.terminal = None;
            }
            cache.state = state.clone();
            // Announced before the lock is let go, so that who waits for
            // the state finds its events already sent.
            self.emit(SessionEvent::StateChanged(state.clone()));
            match &state {
                SessionState::Stopped(stop) => self.emit(SessionEvent::Stopped(stop.clone())),
                SessionState::Terminated { exit_code } => {
                    self.emit(SessionEvent::Terminated {
                        exit_code: *exit_code,
                    });
                }
                _ => {}
            }
            self.changed.notify_all();
        }
        if state.is_terminated() && self.parent.is_none() {
            self.store.reset_status();
        }
    }

    fn finish(&self) {
        let exit_code = lock(&self.cache).exit_code;
        self.set_state(SessionState::Terminated { exit_code });
    }

    fn fail_start(&self, message: &str) {
        lock(&self.cache).failure = Some(message.to_string());
        self.emit(SessionEvent::Failed {
            action: "start".into(),
            message: message.to_string(),
        });
        self.finish();
    }

    fn emit(&self, event: SessionEvent) {
        lock(&self.subscribers).retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }

    fn background(
        &self,
        action: &'static str,
        job: impl FnOnce(&Session) -> Result<(), DapError> + Send + 'static,
    ) {
        let job = Job::Run(Box::new(move |session: &Session| {
            if let Err(error) = job(session) {
                // A request cut off by the end of the session is not news.
                if !session.state().is_terminated() {
                    session.emit(SessionEvent::Failed {
                        action: action.into(),
                        message: error.to_string(),
                    });
                }
            }
        }));
        lock(&self.jobs).send(job).ok();
    }

    // Child sessions.

    fn start_child(&self, configuration: Map<String, Value>) {
        let weak = self.this.clone();
        spawn_thread("dap-child", move || {
            let Some(parent) = weak.upgrade() else { return };
            match parent.open_child(configuration) {
                Ok(child) => {
                    let id = child.id();
                    {
                        let mut children = lock(&parent.children);
                        children.retain(|other| !other.state().is_terminated());
                        children.push(child);
                    }
                    parent.emit(SessionEvent::ChildStarted(id));
                }
                Err(error) => parent.emit(SessionEvent::Failed {
                    action: "debug a subprocess".into(),
                    message: error.to_string(),
                }),
            }
        });
    }

    fn open_child(&self, mut configuration: Map<String, Value>) -> Result<Arc<Session>, DapError> {
        let (host, port) = self.adapter.child_endpoint(&configuration).ok_or_else(|| {
            DapError::Start("The debugger did not say where its subprocess listens.".into())
        })?;
        let (client, incoming) = Client::connect(&host, port, REQUEST_TIMEOUT)?;
        configuration.insert("request".into(), json!("attach"));
        let label = configuration
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Subprocess")
            .to_string();
        let request = DebugRequest {
            adapter: self.request.adapter,
            kind: RequestKind::Attach,
            label,
            cwd: self.request.cwd.clone(),
            arguments: configuration,
        };
        Session::open(
            client,
            incoming,
            Setup {
                adapter: self.adapter,
                request,
                store: self.store.clone(),
                origin: self.origin.clone(),
                driver: self.driver(),
                parent: Some(self.id),
            },
        )
    }
}

impl Session {
    /// Ends the session now, whoever else still holds it: the adapter is
    /// told to disconnect and is killed, and so is a debuggee it started.
    pub(crate) fn close(&self) {
        for child in std::mem::take(&mut *lock(&self.children)) {
            child.close();
        }
        let (terminated, process_id) = {
            let cache = lock(&self.cache);
            (cache.state.is_terminated(), cache.process_id)
        };
        let launched = self.request.kind == RequestKind::Launch;
        if !terminated {
            self.client
                .send("disconnect", json!({ "terminateDebuggee": launched }))
                .ok();
        }
        self.client.shutdown(DROP_GRACE);
        // A debuggee in a terminal belongs to the terminal that started it.
        if launched && !terminated && !self.request.uses_terminal() {
            kill_debuggee(process_id);
        }
        self.finish();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        lock(&self.jobs).send(Job::Stop).ok();
        self.close();
    }
}

/// In case the adapter died before it could end the debuggee itself.
fn kill_debuggee(process_id: Option<u32>) {
    #[cfg(unix)]
    if let Some(id) = process_id.and_then(|id| libc::pid_t::try_from(id).ok()) {
        if id > 1 {
            // The debuggee leads a group of its own under debugpy; the
            // group first, so that what it started goes too.
            unsafe {
                libc::kill(-id, libc::SIGKILL);
                libc::kill(id, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = process_id;
}

fn spawn_thread(name: &str, body: impl FnOnce() + Send + 'static) {
    // Without a thread the session stays as it is; nothing to unwind.
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .ok();
}
