//! What the interface knows about one top-level debug session and the
//! sessions of its subprocesses: which frame is selected, which variables
//! are open, the console's lines, and the rows the tab draws. It follows the
//! sessions' events on a task and reads only their caches, and it does what
//! a stop asks of the rest of the app: the execution line in the editor,
//! the file opened at it, the debuggee's terminal, the agent's rail status.
use super::{breakpoints, values};
use crate::{
    code_editor::{self, DocumentEvent, InlineNote, LineDecoration, LineStyle, Location},
    code_shell::{
        self,
        context::{self, Checkout},
        dock::{self, DockTabHandle, Run, RunRequest},
        rail,
    },
};
use gpui::{Context, Entity, EventEmitter, FocusHandle, SharedString, Subscription, Task};
use monocode_core::code::dap::{
    Actor, EvaluateContext, Frame, OutputCategory, Page, Refresh, Session, SessionEvent, SessionId,
    SessionState, StepKind, Stop, TerminalRequest, Variable,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

/// The children of a large sequence are fetched and listed this many at a time.
pub(super) const PAGE_SIZE: u64 = 100;
/// The console keeps this many lines; the oldest go first.
const CONSOLE_KEPT: usize = 5000;

/// Names from a scope down to a variable: what stays the same across stops,
/// so that what was open stays open while stepping.
pub(super) type VarPath = Vec<SharedString>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Stdout,
    Stderr,
    /// The debugger's own messages and this module's.
    Info,
    /// An expression the user evaluated.
    Input,
    Result,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConsoleLine {
    pub tone: Tone,
    pub text: SharedString,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum StackRow {
    /// A process, shown once the debuggee has started others.
    Session {
        id: SessionId,
        label: SharedString,
        state: SharedString,
        selected: bool,
    },
    Thread {
        session: SessionId,
        id: i64,
        name: SharedString,
        selected: bool,
    },
    Frame {
        session: SessionId,
        thread: i64,
        frame: Frame,
        selected: bool,
        /// Library code, or a file outside the checkout.
        dim: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum VarKind {
    Scope,
    Variable,
    /// Lists the next page of a large sequence when pressed.
    More,
    Loading,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct VarRow {
    pub kind: VarKind,
    pub depth: usize,
    pub path: VarPath,
    pub name: SharedString,
    pub value: SharedString,
    pub type_name: Option<SharedString>,
    pub expandable: bool,
    pub expanded: bool,
    /// The reference whose child this is, for assigning to it.
    pub parent: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Continue,
    Pause,
    StepOver,
    StepInto,
    StepOut,
    Restart,
    Stop,
}

pub(super) enum ModelEvent {
    /// The debuggee paused; this session is the one the keys now act on.
    Stopped,
    /// The user asked for the session to be started again.
    Restart,
}

pub(super) struct Model {
    root: Arc<Session>,
    children: Vec<Arc<Session>>,
    pub(super) checkout: Checkout,
    /// The session, thread and frame the columns show.
    session: SessionId,
    thread: Option<i64>,
    frame: Option<i64>,
    /// Scopes and variables whose open state differs from the default
    /// (cheap scopes open, everything else closed).
    toggled: HashSet<VarPath>,
    /// Extra pages listed for a large sequence.
    pages: HashMap<VarPath, u64>,
    /// Lists asked of the adapter since the last stop, to ask once.
    requested: HashSet<(SessionId, i64, Option<Page>)>,
    requested_scopes: HashSet<(SessionId, i64)>,
    /// A stop read from a session's state when it was first followed; its
    /// event, if it is still on its way, is the same stop.
    initial: HashMap<SessionId, Stop>,
    pub(super) console: Vec<ConsoleLine>,
    /// The last console line has no line end yet; more of it may follow.
    open_line: Option<Tone>,
    pub(super) history: Vec<String>,
    /// An exception's text or a failure, shown across the top of the tab.
    pub(super) notice: Option<SharedString>,
    /// The agent session that drives, and its title.
    agent: Option<(i64, SharedString)>,
    /// The rail row carrying a status of this session.
    rail: Option<i64>,
    /// The line decorations and inline values this session has in the hub,
    /// to change them only when they differ.
    decorated: HashMap<PathBuf, Vec<LineDecoration>>,
    noted: Option<(PathBuf, Vec<InlineNote>)>,
    /// What the tab was last titled and whether closing it asks first.
    titled: (String, bool),
    source: SharedString,
    terminal: Option<Entity<Run>>,
    terminal_asked: bool,
    pub(super) tab: Option<DockTabHandle>,
    /// What has the keyboard while the user types in the tab's console.
    pub(super) console_focus: Option<FocusHandle>,
    pub(super) stack_rows: Rc<Vec<StackRow>>,
    pub(super) var_rows: Rc<Vec<VarRow>>,
    tasks: Vec<Task<()>>,
    _hub: Subscription,
}
impl EventEmitter<ModelEvent> for Model {}

fn is_system_exit(stop: &Stop) -> bool {
    stop.reason == "exception"
        && (stop.exception.as_deref()).is_some_and(|text| text.starts_with("SystemExit"))
}

fn agent_id(actor: &Actor) -> Option<i64> {
    match actor {
        Actor::Agent(id) => id.parse().ok(),
        Actor::User => None,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
}

impl Model {
    pub(super) fn new(root: Arc<Session>, checkout: Checkout, cx: &mut Context<Self>) -> Self {
        let hub = code_editor::hub(cx);
        let mut this = Self {
            session: root.id(),
            root: root.clone(),
            children: Vec::new(),
            checkout,
            thread: None,
            frame: None,
            toggled: HashSet::new(),
            pages: HashMap::new(),
            requested: HashSet::new(),
            requested_scopes: HashSet::new(),
            initial: HashMap::new(),
            console: Vec::new(),
            open_line: None,
            history: Vec::new(),
            notice: None,
            agent: None,
            rail: None,
            decorated: HashMap::new(),
            noted: None,
            titled: (String::new(), false),
            source: format!("debug-{}", cx.entity_id().as_u64()).into(),
            terminal: None,
            terminal_asked: false,
            tab: None,
            console_focus: None,
            stack_rows: Rc::default(),
            var_rows: Rc::default(),
            tasks: Vec::new(),
            // A file opened at a stop gets its inline values once it is there.
            _hub: cx.subscribe(&hub, |this, _, event: &DocumentEvent, cx| {
                if let DocumentEvent::Opened { doc, .. } = event
                    && this.selected_frame().and_then(|frame| frame.path).as_ref()
                        == Some(&doc.path)
                {
                    this.note_values(cx);
                }
            }),
        };
        this.follow(root, cx);
        this.refresh(cx);
        this
    }

    /// Takes the session a restart made in place of this one.
    pub(super) fn adopt(&mut self, root: Arc<Session>, cx: &mut Context<Self>) {
        self.tasks.clear();
        self.children.clear();
        self.session = root.id();
        self.root = root.clone();
        (self.thread, self.frame, self.notice) = (None, None, None);
        self.requested.clear();
        self.requested_scopes.clear();
        self.initial.clear();
        self.terminal_asked = false;
        self.line(Tone::Info, "Restarted.");
        self.follow(root, cx);
        self.refresh(cx);
    }

    /// Hears the session from now on. Nothing is replayed, so where it is
    /// already is read once.
    fn follow(&mut self, session: Arc<Session>, cx: &mut Context<Self>) {
        let events = session.subscribe();
        let id = session.id();
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let mut batch = vec![event];
                while let Ok(more) = events.try_recv() {
                    batch.push(more);
                }
                let heard = this.update(cx, |this, cx| {
                    for event in batch {
                        this.heard(id, event, cx);
                    }
                    this.refresh(cx);
                });
                if heard.is_err() {
                    break;
                }
            }
        }));
        if session.parent().is_none() {
            for output in session.output() {
                self.print(output.category, &output.text);
            }
        }
        match session.state() {
            SessionState::Stopped(stop) => {
                self.initial.insert(id, stop.clone());
                self.stopped(&session, stop, cx);
            }
            SessionState::Terminated { exit_code } => self.ended(&session, exit_code),
            SessionState::Starting | SessionState::Running => {}
        }
        if let Some(request) = session.pending_terminal() {
            self.run_in_terminal(request, cx);
        }
        for child in session.children() {
            self.add_child(child, cx);
        }
    }

    fn add_child(&mut self, child: Arc<Session>, cx: &mut Context<Self>) {
        if self.sessions().iter().any(|known| known.id() == child.id()) {
            return;
        }
        self.children.retain(|known| !known.state().is_terminated());
        self.children.push(child.clone());
        self.follow(child, cx);
    }

    // Reading.

    pub(super) fn root(&self) -> &Arc<Session> {
        &self.root
    }
    /// The root and its subprocesses.
    pub(super) fn sessions(&self) -> Vec<Arc<Session>> {
        std::iter::once(self.root.clone())
            .chain(self.children.iter().cloned())
            .collect()
    }
    fn find(&self, id: SessionId) -> Option<Arc<Session>> {
        self.sessions()
            .into_iter()
            .find(|session| session.id() == id)
    }
    /// The session the columns show.
    pub(super) fn selected(&self) -> Arc<Session> {
        self.find(self.session).unwrap_or_else(|| self.root.clone())
    }
    pub(super) fn selected_frame_id(&self) -> Option<i64> {
        self.frame
    }
    fn selected_frame(&self) -> Option<Frame> {
        let stack = self.selected().cached_stack(self.thread?)?;
        stack.into_iter().find(|frame| Some(frame.id) == self.frame)
    }
    /// Where the selected frame is, when its session is paused there.
    pub(super) fn selected_location(&self) -> Option<(PathBuf, u32)> {
        self.selected().state().stop()?;
        let frame = self.selected_frame()?;
        Some((frame.path?, frame.line))
    }
    /// The terminal tab the debuggee runs in.
    #[cfg(any(test, feature = "selftest"))]
    pub(super) fn terminal(&self) -> Option<Entity<Run>> {
        self.terminal.clone()
    }
    pub(super) fn user_drives(&self) -> bool {
        self.root.driver() == Actor::User
    }
    /// The title of the agent session that drives, if one does.
    pub(super) fn driven_by(&self) -> Option<SharedString> {
        self.agent.as_ref().map(|(_, label)| label.clone())
    }
    pub(super) fn live(&self) -> bool {
        !self.root.state().is_terminated()
    }
    pub(super) fn state_word(state: &SessionState) -> &'static str {
        match state {
            SessionState::Starting => "starting",
            SessionState::Running => "running",
            SessionState::Stopped(_) => "paused",
            SessionState::Terminated { .. } => "ended",
        }
    }
    pub(super) fn title(&self) -> String {
        format!(
            "{} · {}",
            self.root.label(),
            Self::state_word(&self.root.state())
        )
    }
    /// Whether a control can be used now. While an agent steers, the user's
    /// are Stop and taking over.
    pub(super) fn can(&self, control: Control) -> bool {
        let state = self.selected().state();
        let user = self.user_drives();
        match control {
            Control::Stop => self.live(),
            Control::Restart => user,
            Control::Pause => user && state == SessionState::Running,
            Control::Continue | Control::StepOver | Control::StepInto | Control::StepOut => {
                user && state.stop().is_some()
            }
        }
    }
    pub(super) fn can_set_variables(&self) -> bool {
        self.user_drives() && self.selected().supports("supportsSetVariable")
    }

    // Acting.

    pub(super) fn act(&mut self, control: Control, cx: &mut Context<Self>) {
        if !self.can(control) {
            return;
        }
        let session = self.selected();
        match control {
            Control::Continue => session.resume(self.thread),
            Control::Pause => session.pause(self.thread),
            Control::StepOver => session.step(StepKind::Over, self.thread),
            Control::StepInto => session.step(StepKind::In, self.thread),
            Control::StepOut => session.step(StepKind::Out, self.thread),
            Control::Restart => cx.emit(ModelEvent::Restart),
            // A debuggee that has not come up yet cannot be asked to end.
            Control::Stop if self.root.state() == SessionState::Starting => self.root.disconnect(),
            Control::Stop => self.root.terminate(),
        }
    }

    pub(super) fn take_over(&mut self, cx: &mut Context<Self>) {
        self.root.take_over(Actor::User);
        self.refresh(cx);
    }

    pub(super) fn select_thread(
        &mut self,
        session: SessionId,
        thread: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.find(session) else {
            return;
        };
        (self.session, self.thread) = (session, Some(thread));
        match found.cached_stack(thread) {
            Some(frames) => self.frame = frames.first().map(|frame| frame.id),
            None => {
                self.frame = None;
                if found.state().stop().is_some() {
                    found.refresh(Refresh::Stack(thread));
                }
            }
        }
        self.refresh(cx);
    }

    pub(super) fn select_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(found) = self.find(session) else {
            return;
        };
        let thread = (found.state().stop().and_then(|stop| stop.thread_id))
            .or_else(|| found.cached_threads().first().map(|thread| thread.id));
        match thread {
            Some(thread) => self.select_thread(session, thread, cx),
            None => {
                (self.session, self.thread, self.frame) = (session, None, None);
                self.refresh(cx);
            }
        }
    }

    pub(super) fn select_frame(
        &mut self,
        session: SessionId,
        thread: i64,
        frame: i64,
        cx: &mut Context<Self>,
    ) {
        (self.session, self.thread, self.frame) = (session, Some(thread), Some(frame));
        self.refresh(cx);
    }

    pub(super) fn toggle(&mut self, path: &VarPath, cx: &mut Context<Self>) {
        if !self.toggled.remove(path) {
            self.toggled.insert(path.clone());
        }
        self.refresh(cx);
    }

    pub(super) fn show_more(&mut self, path: &VarPath, cx: &mut Context<Self>) {
        *self.pages.entry(path.clone()).or_default() += 1;
        self.refresh(cx);
    }

    /// Evaluates in the selected frame and prints the answer in the console.
    pub(super) fn evaluate(&mut self, expression: String, cx: &mut Context<Self>) {
        let expression = expression.trim().to_owned();
        if expression.is_empty() {
            return;
        }
        if self.history.last() != Some(&expression) {
            self.history.push(expression.clone());
        }
        self.line(Tone::Input, &expression);
        let (session, frame) = (self.selected(), self.frame);
        let asked = cx
            .background_executor()
            .spawn(async move { session.evaluate(&expression, frame, EvaluateContext::Repl) });
        cx.spawn(async move |this, cx| {
            let answer = asked.await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(value) => this.line(Tone::Result, &value.result),
                    Err(error) => this.line(Tone::Error, &error.to_string()),
                }
                this.refresh(cx);
            });
        })
        .detach();
        self.refresh(cx);
    }

    /// Assigns to a child of `parent`; the lists that showed it are fetched again.
    pub(super) fn set_variable(
        &mut self,
        parent: i64,
        name: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
        let session = self.selected();
        let asked = cx
            .background_executor()
            .spawn(async move { session.set_variable(parent, &name, &value) });
        cx.spawn(async move |this, cx| {
            let answer = asked.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = answer {
                    this.line(Tone::Error, &error.to_string());
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Something asked of the session from here could not be done.
    pub(super) fn fail(&mut self, message: &str, cx: &mut Context<Self>) {
        self.line(Tone::Error, message);
        self.notice = Some(message.to_owned().into());
        self.refresh(cx);
    }

    /// The tab is gone: nothing of the session stays in the editor, the
    /// rail or the dock.
    pub(super) fn closed(&mut self, cx: &mut Context<Self>) {
        self.tasks.clear();
        self.tab = None;
        self.clear_marks(cx);
        if let Some(session) = self.rail.take() {
            rail::clear_status(session, cx);
        }
        if let Some(run) = self.terminal.take() {
            run.update(cx, |run, cx| run.stop(cx));
        }
    }

    // The sessions' events.

    fn heard(&mut self, id: SessionId, event: SessionEvent, cx: &mut Context<Self>) {
        let Some(session) = self.find(id) else {
            return;
        };
        match event {
            SessionEvent::StateChanged(state) => {
                if !matches!(state, SessionState::Stopped(_)) {
                    self.initial.remove(&id);
                }
                if state == SessionState::Running && id == self.session {
                    self.notice = None;
                }
            }
            SessionEvent::Stopped(stop) => {
                if self.initial.remove(&id).as_ref() != Some(&stop) {
                    self.stopped(&session, stop, cx);
                }
            }
            SessionEvent::Terminated { exit_code } => self.ended(&session, exit_code),
            SessionEvent::Stack { thread_id } => {
                if id == self.session && self.thread == Some(thread_id) && self.frame.is_none() {
                    self.frame = (session.cached_stack(thread_id))
                        .and_then(|frames| frames.first().map(|frame| frame.id));
                }
            }
            SessionEvent::Variables { reference } => {
                // A list the session dropped is asked for again when shown.
                self.requested.retain(|(session_id, known, page)| {
                    *session_id != id
                        || *known != reference
                        || session.cached_variables(reference, *page).is_some()
                });
            }
            SessionEvent::Threads | SessionEvent::Scopes { .. } => {}
            SessionEvent::Output(output) => self.print(output.category, &output.text),
            SessionEvent::RunInTerminal(request) => self.run_in_terminal(request, cx),
            SessionEvent::ChildStarted(child) => {
                let child = (session.children().into_iter()).find(|known| known.id() == child);
                if let Some(child) = child {
                    self.add_child(child, cx);
                }
            }
            SessionEvent::DriverChanged(_) => {}
            SessionEvent::Failed { action, message } => {
                self.line(Tone::Error, &message);
                if action == "start" {
                    self.notice = Some(message.into());
                }
            }
        }
    }

    fn stopped(&mut self, session: &Arc<Session>, stop: Stop, cx: &mut Context<Self>) {
        // debugpy reports `sys.exit(n)` as an uncaught exception. Unless the
        // user asked to stop on every raise, that is the program ending.
        if is_system_exit(&stop)
            && !breakpoints::store().exceptions().raised
            && session.driver() == Actor::User
        {
            session.resume(stop.thread_id);
            return;
        }
        (self.session, self.thread, self.frame) = (session.id(), stop.thread_id, stop.frame_id);
        self.requested.clear();
        self.requested_scopes.clear();
        self.notice = stop.exception.clone().map(Into::into);
        if let Some(exception) = &stop.exception {
            self.line(Tone::Error, &format!("Paused on {exception}"));
        }
        cx.emit(ModelEvent::Stopped);
        self.show_stop(&stop, cx);
    }

    fn ended(&mut self, session: &Arc<Session>, exit_code: Option<i32>) {
        if session.parent().is_some() {
            return;
        }
        match (session.failure(), exit_code) {
            (Some(failure), _) => {
                self.notice = Some(failure.into());
            }
            (None, Some(code)) => self.line(
                Tone::Info,
                &format!("The program ended with exit code {code}."),
            ),
            (None, None) => self.line(Tone::Info, "The debug session ended."),
        }
    }

    /// Opens the file at the stop and brings the tab forward. While an
    /// agent steers, the keyboard stays where the user has it and their
    /// project stays in front.
    fn show_stop(&self, stop: &Stop, cx: &mut Context<Self>) {
        let quiet = !self.user_drives();
        let console = self.console_focus.clone();
        let target = (stop.path.clone()).zip(stop.line);
        let (tab, project) = (self.tab.clone(), self.checkout.project.clone());
        cx.defer(move |cx| {
            code_shell::in_window(cx, |window, cx| {
                let keyboard = window.focused(cx);
                let front =
                    context::focused_checkout(cx).is_some_and(|focus| focus.project == project);
                if let Some(tab) = tab.filter(|_| !quiet || front) {
                    tab.reveal(window, cx);
                }
                if let Some((path, line)) = target {
                    let hub = code_editor::hub(cx);
                    let open = hub.read(cx).document(&path).is_some();
                    if quiet && open {
                        let row = line.saturating_sub(1) as usize;
                        hub.update(cx, |hub, cx| hub.reveal(&path, row, cx));
                    } else if !quiet || front {
                        open_at(&path, line, window, cx);
                    }
                }
                // Stepping while typing in the console leaves the keyboard there.
                let typing = keyboard.is_some() && keyboard == console;
                if let Some(keyboard) = keyboard.filter(|_| quiet || typing) {
                    window.focus(&keyboard, cx);
                }
            });
        });
    }

    fn run_in_terminal(&mut self, request: TerminalRequest, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.terminal_asked, true) {
            return;
        }
        let old = self.terminal.take();
        let run = RunRequest {
            agent: self.driven_by(),
            reveal: self.user_drives(),
            ..RunRequest::new(request.spawn.clone(), self.checkout.clone())
        };
        cx.spawn(async move |this, cx| {
            let started = cx.update(|cx| {
                code_shell::in_window(cx, |window, cx| {
                    // The terminal of the run before a restart has had its day.
                    if let Some(old) = old {
                        DockTabHandle::of_run(&old, cx).close(window, cx);
                    }
                    dock::run(run, window, cx)
                })
            });
            let started = started.unwrap_or_else(|| Err("The window is not ready yet.".to_owned()));
            let _ = this.update(cx, |this, cx| match started {
                Ok(run) => {
                    request.started(run.read(cx).pid());
                    this.terminal = Some(run);
                }
                Err(error) => {
                    request.failed(&error);
                    this.line(Tone::Error, &error);
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    // The console.

    fn line(&mut self, tone: Tone, text: &str) {
        self.open_line = None;
        for line in text.trim_end_matches('\n').split('\n') {
            self.console.push(ConsoleLine {
                tone,
                text: line.trim_end_matches('\r').to_owned().into(),
            });
        }
        self.trim_console();
    }

    fn print(&mut self, category: OutputCategory, text: &str) {
        // Nothing was written: the line being written stays open as it was.
        if text.is_empty() {
            return;
        }
        let tone = match category {
            OutputCategory::Stdout => Tone::Stdout,
            OutputCategory::Stderr => Tone::Stderr,
            OutputCategory::Console => Tone::Info,
        };
        let mut pieces = text.split('\n');
        if self.open_line == Some(tone)
            && let (Some(first), Some(last)) = (pieces.next(), self.console.last_mut())
        {
            last.text = format!("{}{}", last.text, first.trim_end_matches('\r')).into();
        }
        let pieces: Vec<&str> = pieces.collect();
        let ends_open = !text.ends_with('\n');
        let count = pieces.len();
        for (n, piece) in pieces.into_iter().enumerate() {
            // What follows the last line end is empty when the text ends with one.
            if n + 1 == count && piece.is_empty() {
                break;
            }
            self.console.push(ConsoleLine {
                tone,
                text: piece.trim_end_matches('\r').to_owned().into(),
            });
        }
        self.open_line = ends_open.then_some(tone);
        self.trim_console();
    }

    fn trim_console(&mut self) {
        if self.console.len() > CONSOLE_KEPT {
            let extra = self.console.len() - CONSOLE_KEPT * 4 / 5;
            self.console.drain(..extra);
        }
    }

    // What is drawn.

    /// Rebuilds the rows from the sessions' caches and brings everything
    /// outside the tab up to date. Called after every change.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.find(self.session).is_none() {
            (self.session, self.thread, self.frame) = (self.root.id(), None, None);
        }
        self.stack_rows = Rc::new(self.build_stack());
        self.var_rows = Rc::new(self.build_variables());
        self.decorate(cx);
        self.note_values(cx);
        self.follow_driver(cx);
        let titled = (self.title(), self.live());
        if let Some(tab) = &self.tab
            && titled != self.titled
        {
            tab.set_title(titled.0.clone(), cx);
            tab.set_busy(titled.1.then(|| "the debug session".to_owned()), cx);
            self.titled = titled;
        }
        cx.notify();
    }

    fn build_stack(&self) -> Vec<StackRow> {
        let mut rows = Vec::new();
        let sessions = self.sessions();
        let several = sessions.len() > 1;
        for session in &sessions {
            let state = session.state();
            let selected = session.id() == self.session;
            if several {
                rows.push(StackRow::Session {
                    id: session.id(),
                    label: session.label().to_owned().into(),
                    state: Self::state_word(&state).into(),
                    selected,
                });
            }
            let Some(stop) = state.stop() else {
                continue;
            };
            let threads = session.cached_threads();
            let shown = if selected {
                self.thread
            } else {
                stop.thread_id
            };
            for thread in &threads {
                if threads.len() > 1 {
                    rows.push(StackRow::Thread {
                        session: session.id(),
                        id: thread.id,
                        name: thread.name.clone().into(),
                        selected: selected && self.thread == Some(thread.id),
                    });
                }
                if shown != Some(thread.id) {
                    continue;
                }
                for frame in session.cached_stack(thread.id).unwrap_or_default() {
                    let inside = (frame.path.as_ref())
                        .is_some_and(|path| path.starts_with(&self.checkout.folder));
                    rows.push(StackRow::Frame {
                        session: session.id(),
                        thread: thread.id,
                        selected: selected && self.frame == Some(frame.id),
                        dim: frame.subtle || !inside,
                        frame,
                    });
                }
            }
        }
        rows
    }

    fn build_variables(&mut self) -> Vec<VarRow> {
        let session = self.selected();
        let mut rows = Vec::new();
        let (Some(frame), true) = (self.frame, session.state().stop().is_some()) else {
            return rows;
        };
        let Some(scopes) = session.cached_scopes(frame) else {
            if self.requested_scopes.insert((session.id(), frame)) {
                session.refresh(Refresh::Scopes(frame));
            }
            return rows;
        };
        for scope in scopes {
            let path: VarPath = vec![scope.name.clone().into()];
            // A cheap scope is open unless it was closed; a costly one
            // (the globals of a large module) only once it is asked for.
            let expanded = self.toggled.contains(&path) == scope.expensive;
            rows.push(VarRow {
                kind: VarKind::Scope,
                depth: 0,
                name: scope.name.into(),
                value: "".into(),
                type_name: None,
                expandable: true,
                expanded,
                parent: 0,
                path: path.clone(),
            });
            if expanded {
                self.push_children(
                    &session,
                    scope.variables_reference,
                    None,
                    &path,
                    1,
                    &mut rows,
                );
            }
        }
        rows
    }

    /// The cached list, or a request for it and nothing yet.
    fn children_of(
        &mut self,
        session: &Arc<Session>,
        reference: i64,
        page: Option<Page>,
    ) -> Option<Vec<Variable>> {
        let cached = session.cached_variables(reference, page);
        if cached.is_none() && self.requested.insert((session.id(), reference, page)) {
            session.refresh(Refresh::Variables(reference, page));
        }
        cached
    }

    fn push_children(
        &mut self,
        session: &Arc<Session>,
        reference: i64,
        indexed: Option<u64>,
        path: &VarPath,
        depth: usize,
        rows: &mut Vec<VarRow>,
    ) {
        let filler = |kind: VarKind, name: String| VarRow {
            kind,
            depth,
            path: path.clone(),
            name: name.into(),
            value: "".into(),
            type_name: None,
            expandable: false,
            expanded: false,
            parent: reference,
        };
        let total = indexed.filter(|count| *count > PAGE_SIZE);
        let pages = match total {
            Some(_) => 1 + self.pages.get(path).copied().unwrap_or(0),
            None => 1,
        };
        for index in 0..pages {
            let page = total.map(|_| Page {
                start: index * PAGE_SIZE,
                count: PAGE_SIZE,
            });
            if total.is_some_and(|total| index * PAGE_SIZE >= total) {
                break;
            }
            let Some(variables) = self.children_of(session, reference, page) else {
                rows.push(filler(VarKind::Loading, "Loading…".into()));
                return;
            };
            for variable in variables {
                let mut child = path.clone();
                child.push(variable.name.clone().into());
                let expandable = variable.variables_reference != 0;
                let expanded = expandable && self.toggled.contains(&child);
                rows.push(VarRow {
                    kind: VarKind::Variable,
                    depth,
                    name: variable.name.into(),
                    value: variable.value.into(),
                    type_name: variable.type_name.map(Into::into),
                    expandable,
                    expanded,
                    parent: reference,
                    path: child.clone(),
                });
                if expanded {
                    self.push_children(
                        session,
                        variable.variables_reference,
                        variable.indexed_children,
                        &child,
                        depth + 1,
                        rows,
                    );
                }
            }
        }
        if let Some(total) = total
            && pages * PAGE_SIZE < total
        {
            rows.push(filler(
                VarKind::More,
                format!("Show more ({} of {total})", pages * PAGE_SIZE),
            ));
        }
    }

    /// The execution line of every paused process, and the line of the
    /// selected frame when it is another.
    fn decorate(&mut self, cx: &mut Context<Self>) {
        let mut lines: HashMap<PathBuf, Vec<LineDecoration>> = HashMap::new();
        for session in self.sessions() {
            let state = session.state();
            let Some((path, line)) =
                (state.stop()).and_then(|stop| stop.path.clone().zip(stop.line))
            else {
                continue;
            };
            lines.entry(path).or_default().push(LineDecoration {
                row: line.saturating_sub(1) as usize,
                style: LineStyle::Execution,
                arrow: true,
            });
        }
        if let Some((path, line)) = self.selected_location() {
            let row = line.saturating_sub(1) as usize;
            let list = lines.entry(path).or_default();
            if !list.iter().any(|known| known.row == row) {
                list.push(LineDecoration {
                    row,
                    style: LineStyle::Frame,
                    arrow: true,
                });
            }
        }
        for list in lines.values_mut() {
            list.sort_by_key(|line| line.row);
        }
        if lines == self.decorated {
            return;
        }
        let hub = code_editor::hub(cx);
        let source = self.source.clone();
        let before = std::mem::replace(&mut self.decorated, lines.clone());
        hub.update(cx, |hub, cx| {
            for path in before.keys().filter(|path| !lines.contains_key(*path)) {
                hub.set_line_decorations(path, source.clone(), Vec::new(), cx);
            }
            for (path, list) in lines {
                if before.get(&path) != Some(&list) {
                    hub.set_line_decorations(&path, source.clone(), list, cx);
                }
            }
        });
    }

    /// The selected frame's simple locals, written after the lines that
    /// name them. Only from what is cached, and only in an open file.
    fn note_values(&mut self, cx: &mut Context<Self>) {
        let hub = code_editor::hub(cx);
        let session = self.selected();
        let notes = self.selected_location().and_then(|(path, line)| {
            let scopes = session.cached_scopes(self.frame?)?;
            let scope = scopes.iter().find(|scope| !scope.expensive)?;
            let locals = session.cached_variables(scope.variables_reference, None)?;
            let text = hub.read(cx).document(&path)?.text.clone();
            Some((path, values::inline_notes(&text, line, &locals)))
        });
        let notes = notes.filter(|(_, notes)| !notes.is_empty());
        if notes == self.noted {
            return;
        }
        let source = self.source.clone();
        let before = std::mem::replace(&mut self.noted, notes.clone());
        hub.update(cx, |hub, cx| {
            if let Some((stale, _)) = &before
                && notes.as_ref().is_none_or(|(path, _)| path != stale)
            {
                hub.set_inline_notes(stale, source.clone(), Vec::new(), cx);
            }
            if let Some((path, notes)) = notes {
                hub.set_inline_notes(&path, source.clone(), notes, cx);
            }
        });
    }

    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        let hub = code_editor::hub(cx);
        let source = self.source.clone();
        let paths: Vec<PathBuf> = (self.decorated.drain().map(|(path, _)| path))
            .chain(self.noted.take().map(|(path, _)| path))
            .collect();
        hub.update(cx, |hub, cx| {
            for path in paths {
                hub.set_line_decorations(&path, source.clone(), Vec::new(), cx);
                hub.set_inline_notes(&path, source.clone(), Vec::new(), cx);
            }
        });
    }

    /// Who steers, and what the agent's rail row says of it.
    fn follow_driver(&mut self, cx: &mut Context<Self>) {
        let driver = agent_id(&self.root.driver());
        if self.agent.as_ref().map(|(id, _)| *id) != driver {
            self.agent = driver.map(|id| {
                let label = rail::session_label(id, cx).unwrap_or_else(|| "an agent".into());
                (id, label)
            });
        }
        let state = self.root.state();
        let status = driver.filter(|_| !state.is_terminated()).map(|id| {
            let paused = (self.sessions().iter())
                .find_map(|session| session.state().stop().cloned())
                .map(|stop| match stop.path.zip(stop.line) {
                    Some((path, line)) => format!("paused at {}:{line}", file_name(&path)),
                    None => "paused".to_owned(),
                });
            (id, paused.unwrap_or_else(|| "debugging".to_owned()))
        });
        if let Some(old) = self.rail
            && status.as_ref().map(|(id, _)| *id) != Some(old)
        {
            rail::clear_status(old, cx);
        }
        self.rail = status.as_ref().map(|(id, _)| *id);
        if let Some((id, text)) = status {
            rail::set_status(id, text, cx);
        }
    }
}

/// Opens a file at a line through the app's file route, and records the
/// jump so Back returns to where the user was.
pub(super) fn open_at(path: &Path, line: u32, window: &mut gpui::Window, cx: &mut gpui::App) {
    let from = context::focused_file(cx).and_then(|focused| {
        let editor = focused.editor.upgrade()?;
        let input = editor.read(cx).input.read(cx);
        Some(Location::of(
            Path::new(&focused.path),
            &input.content,
            input.caret(),
        ))
    });
    let to = Location {
        path: path.to_owned(),
        line: line.max(1) as usize,
        column: 1,
    };
    if let Some(from) = from.filter(|from| from.path != to.path || from.line != to.line) {
        code_editor::hub(cx).update(cx, |hub, cx| hub.jump(from, to.clone(), cx));
    }
    context::open_file(&path.to_string_lossy(), Some((to.line, 1)), window, cx);
}
