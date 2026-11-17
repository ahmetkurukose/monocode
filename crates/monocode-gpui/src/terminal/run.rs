//! A terminal that runs one given process instead of the login shell: a run
//! configuration, a test, a debuggee. The process is the terminal's child, so
//! it has a real TTY and its colours, and the tab stays with its scrollback
//! once it ends. `Run` is what the code that started it holds on to: the
//! state, the process id, the output as plain text, and stop and rerun.
// Reached through `code_shell::dock`, whose callers are the feature modules.
#![allow(dead_code)]
use super::{GridSize, Terminal, TerminalEvent, scrollback, spawn_child};
use crate::theme::*;
use alacritty_terminal::{
    event::{OnResize, WindowSize},
    event_loop::Msg,
    tty::{ChildEvent, EventedPty, EventedReadWrite},
};
use gpui::{
    App, Context, Entity, EventEmitter, Role, SharedString, Task, WeakEntity, div, prelude::*, px,
    rgb,
};
use polling::{Event, PollMode, Poller};
use std::{io, sync::Arc, time::Duration};

/// How long a stopped process has to end on its own before it is killed.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(3);
/// The most output `Run::tail` keeps, in bytes.
const TAIL_LIMIT: usize = 64 * 1024;

/// What a run tab starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Program {
    /// The tab's title, and the name lists of running work show.
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    /// The whole environment added to the terminal's own, the project's
    /// setup included.
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunState {
    Running,
    /// It ended by itself; the code is `None` when a signal ended it or it
    /// could not be started.
    Exited(Option<i32>),
    /// It ended because it was stopped from here, or its tab was closed.
    Stopped,
}
impl RunState {
    pub(crate) fn running(self) -> bool {
        self == Self::Running
    }
    /// Ended with exit code 0.
    pub(crate) fn succeeded(self) -> bool {
        self == Self::Exited(Some(0))
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunExit {
    pub code: Option<i32>,
    /// It was stopped from here rather than ending by itself.
    pub stopped: bool,
}

pub(crate) enum RunEvent {
    /// The process started, at first or on a rerun.
    Started {
        pid: u32,
    },
    /// Output as it arrives, without escape sequences.
    Output(SharedString),
    Exited(RunExit),
    /// The tab was closed; the run cannot be rerun any more.
    Closed,
}

/// One run tab, as the feature that started it sees it. Observe it for state
/// changes and subscribe to it for `RunEvent`s; both arrive on the UI thread.
pub(crate) struct Run {
    label: SharedString,
    /// The agent session that started it, as its label, for the tab's marker.
    agent: Option<SharedString>,
    state: RunState,
    pid: Option<u32>,
    tail: String,
    waiting: Vec<async_channel::Sender<RunExit>>,
    last_exit: Option<RunExit>,
    open: bool,
    pub(super) terminal: WeakEntity<super::Terminal>,
}
impl EventEmitter<RunEvent> for Run {}

impl Run {
    pub(super) fn new(label: &str, agent: Option<SharedString>) -> Self {
        Self {
            label: label.to_owned().into(),
            agent,
            state: RunState::Running,
            pid: None,
            tail: String::new(),
            waiting: Vec::new(),
            last_exit: None,
            open: true,
            terminal: WeakEntity::new_invalid(),
        }
    }
    pub(crate) fn label(&self) -> SharedString {
        self.label.clone()
    }
    pub(crate) fn agent(&self) -> Option<SharedString> {
        self.agent.clone()
    }
    pub(crate) fn state(&self) -> RunState {
        self.state
    }
    /// The running process, which leads its own process group. `None` once
    /// it has ended or when it could not be started.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.pid
    }
    /// The end of the output so far, as plain text.
    pub(crate) fn tail(&self) -> &str {
        &self.tail
    }
    /// False once the tab is closed.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
    /// The run's terminal, for revealing its tab.
    pub(crate) fn terminal(&self) -> WeakEntity<super::Terminal> {
        self.terminal.clone()
    }
    /// Resolves when the current process ends, at once when it already has.
    /// A closed channel means the run was dropped first.
    pub(crate) fn exited(&mut self) -> async_channel::Receiver<RunExit> {
        let (tx, rx) = async_channel::bounded(1);
        match self.last_exit.filter(|_| !self.state.running()) {
            Some(exit) => {
                let _ = tx.try_send(exit);
            }
            None => self.waiting.push(tx),
        }
        rx
    }
    /// Asks the process group to terminate, and kills it if it has not ended
    /// after `STOP_GRACE`. Does nothing once it has ended.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let terminal = self.terminal.clone();
        // The terminal reports back into this entity, so it runs after this
        // update is over.
        cx.defer(move |cx| {
            let _ = terminal.update(cx, |terminal, cx| terminal.stop_run(STOP_GRACE, cx));
        });
    }
    /// Starts the same process again in the same tab, ending the current one
    /// first. The scrollback starts afresh.
    pub(crate) fn rerun(&mut self, cx: &mut Context<Self>) {
        let terminal = self.terminal.clone();
        cx.defer(move |cx| {
            let _ = terminal.update(cx, |terminal, cx| terminal.rerun(cx));
        });
    }

    pub(super) fn started(&mut self, pid: Option<u32>, cx: &mut Context<Self>) {
        self.state = RunState::Running;
        self.pid = pid;
        self.tail.clear();
        self.last_exit = None;
        if let Some(pid) = pid {
            cx.emit(RunEvent::Started { pid });
        }
        cx.notify();
    }
    pub(super) fn output(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.tail.push_str(&text);
        if self.tail.len() > TAIL_LIMIT {
            let mut cut = self.tail.len() - TAIL_LIMIT;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
        cx.emit(RunEvent::Output(text.into()));
    }
    pub(super) fn finished(&mut self, exit: RunExit, cx: &mut Context<Self>) {
        if !self.state.running() {
            return;
        }
        self.state = if exit.stopped {
            RunState::Stopped
        } else {
            RunState::Exited(exit.code)
        };
        self.pid = None;
        self.last_exit = Some(exit);
        for waiting in self.waiting.drain(..) {
            let _ = waiting.try_send(exit);
        }
        cx.emit(RunEvent::Exited(exit));
        cx.notify();
    }
    pub(super) fn closed(&mut self, cx: &mut Context<Self>) {
        self.finished(
            RunExit {
                code: None,
                stopped: true,
            },
            cx,
        );
        self.open = false;
        cx.emit(RunEvent::Closed);
        cx.notify();
    }
}

/// A run tab's process, and what starting it again takes.
pub(super) struct Session {
    pub(super) run: Entity<Run>,
    program: Program,
    output: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
    text: PlainText,
    /// The exit status, held until the last output has been read.
    pub(super) status: Option<std::process::ExitStatus>,
    stopped: bool,
    /// Counts starts, so a kill timer never reaches a later process.
    generation: u64,
    kill: Option<Task<()>>,
}

/// Signals the process group a run leads, and the process itself in case it
/// has left that group.
#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    if pid > 1 {
        unsafe {
            libc::kill(-(pid as i32), signal);
            libc::kill(pid as i32, signal);
        }
    }
}

impl Terminal {
    /// A terminal whose child is `program` rather than a shell, started in
    /// `cwd`. Unlike a shell it is started in tests too, so a test that
    /// makes one must allow parking.
    pub(crate) fn for_run(
        cwd: String,
        program: Program,
        agent: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize {
            columns: 80,
            lines: 24,
        };
        let mut spawned = spawn_child(
            &cwd,
            size,
            scrollback(cx),
            cx.entity_id().as_u64(),
            &program.env,
            Some(&program),
        );
        let output = spawned.output.take();
        let run = cx.new(|_| Run::new(&program.label, agent));
        let terminal = cx.weak_entity();
        run.update(cx, |run, _| run.terminal = terminal);
        let mut this = Self::assemble(cwd, size, spawned, Vec::new(), cx);
        this.title = program.label.clone();
        this.run = Some(Session {
            run,
            program,
            output,
            text: PlainText::default(),
            status: None,
            stopped: false,
            generation: 0,
            kill: None,
        });
        // Closing the tab drops the terminal, which hangs the process up.
        this._release = Some(cx.on_release(|this, cx| {
            if let Some(session) = &this.run {
                session.run.update(cx, |run, cx| run.closed(cx));
            }
        }));
        this.run_started(cx);
        this
    }

    /// The run this terminal hosts, if it is a run tab.
    pub(crate) fn run_handle(&self) -> Option<Entity<Run>> {
        self.run.as_ref().map(|session| session.run.clone())
    }

    fn run_started(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.run else {
            return;
        };
        let (run, label) = (session.run.clone(), session.program.label.clone());
        if self.sender.is_some() {
            self.exited = false;
            let pid = (self.shell_pid > 1).then_some(self.shell_pid);
            run.update(cx, |run, cx| run.started(pid, cx));
            // The footer's running chip and the close prompt name it.
            self.set_foreground(Some(label), cx);
        } else {
            // The reason is in the grid; the run reports that it never ran.
            self.exited = true;
            run.update(cx, |run, cx| {
                run.started(None, cx);
                run.output(format!("Could not start {label}.\n"), cx);
                run.finished(
                    RunExit {
                        code: None,
                        stopped: false,
                    },
                    cx,
                );
            });
        }
        cx.emit(TerminalEvent::Changed);
        cx.notify();
    }

    /// Hands what the process wrote since the last call to its `Run`.
    pub(super) fn copy_run_output(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &mut self.run else {
            return;
        };
        let Some(output) = &session.output else {
            return;
        };
        let mut text = String::new();
        while let Ok(bytes) = output.try_recv() {
            text.push_str(&session.text.feed(&bytes));
        }
        if !text.is_empty() {
            session.run.update(cx, |run, cx| run.output(text, cx));
        }
    }

    /// The process is gone and its output has been read: the tab stays,
    /// read-only, with its scrollback and status.
    pub(super) fn run_ended(&mut self, cx: &mut Context<Self>) {
        self.copy_run_output(cx);
        let Some(session) = &mut self.run else {
            return;
        };
        let exit = RunExit {
            code: session.status.take().and_then(|status| status.code()),
            stopped: std::mem::take(&mut session.stopped),
        };
        session.kill = None;
        let run = session.run.clone();
        self.exited = true;
        self.sender = None;
        self.set_foreground(None, cx);
        run.update(cx, |run, cx| run.finished(exit, cx));
        cx.emit(TerminalEvent::Changed);
        cx.notify();
    }

    /// Asks the run's process group to terminate, and kills it after `grace`
    /// if it is still there.
    pub(crate) fn stop_run(&mut self, grace: Duration, cx: &mut Context<Self>) {
        let pid = self.shell_pid;
        let Some(session) = &mut self.run else {
            return;
        };
        if self.exited || self.sender.is_none() {
            return;
        }
        session.stopped = true;
        #[cfg(unix)]
        {
            signal_group(pid, libc::SIGTERM);
            let generation = session.generation;
            session.kill = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(grace).await;
                let _ = this.update(cx, |this, _| {
                    let same = (this.run.as_ref()).is_some_and(|s| s.generation == generation);
                    if same && !this.exited {
                        signal_group(this.shell_pid, libc::SIGKILL);
                    }
                });
            }));
        }
        #[cfg(not(unix))]
        {
            // Closing the PTY ends the process and its console.
            let _ = (pid, grace);
            if let Some(sender) = &self.sender {
                let _ = sender.send(Msg::Shutdown);
            }
        }
    }

    /// Starts the run's process again in this terminal, ending the one that
    /// is still running first.
    pub(crate) fn rerun(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &mut self.run else {
            return;
        };
        if !self.exited {
            #[cfg(unix)]
            signal_group(self.shell_pid, libc::SIGKILL);
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(Msg::Shutdown);
            }
            let exit = RunExit {
                code: None,
                stopped: true,
            };
            session.run.update(cx, |run, cx| run.finished(exit, cx));
        }
        session.generation += 1;
        session.kill = None;
        session.status = None;
        session.stopped = false;
        session.text = PlainText::default();
        let mut spawned = spawn_child(
            &self.cwd,
            self.size,
            scrollback(cx),
            cx.entity_id().as_u64(),
            &session.program.env,
            Some(&session.program),
        );
        session.output = spawned.output.take();
        self.stop_selection();
        self.term = spawned.term;
        self.sender = spawned.sender;
        self.master_fd = spawned.master_fd;
        self.shell_pid = spawned.shell_pid;
        // The previous process's events end with its channel.
        self._events = Self::listen(spawned.events, cx);
        self.run_started(cx);
    }

    /// The tab's leading mark: a shell's tile, a run's status dot, or a
    /// hosted view's icon.
    pub(crate) fn tile(&self, cx: &App) -> gpui::Div {
        let slot = || {
            div()
                .w(px(crate::tab_tile::TAB_TILE.w))
                .h(px(crate::tab_tile::TAB_TILE.h))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
        };
        if let Some(hosted) = &self.hosted {
            return slot().child(icon(hosted.icon, 15.).text_color(content(0.7)));
        }
        let Some(session) = &self.run else {
            return crate::tab_tile::terminal_tile(self.agent());
        };
        let (color, label): (gpui::Hsla, &'static str) = match session.run.read(cx).state() {
            RunState::Running => (rgb(0xe39b4a).into(), "Running"),
            RunState::Exited(Some(0)) => (rgb(0x10b981).into(), "Finished"),
            RunState::Exited(_) => (rgb(0xf87171).into(), "Failed"),
            RunState::Stopped => (content(0.35), "Stopped"),
        };
        slot().child(
            div()
                .id("run-status-dot")
                .debug_selector(|| "run-status-dot".into())
                .role(Role::Image)
                .aria_label(label)
                .size(px(8.))
                .rounded_full()
                .bg(color),
        )
    }
}

/// A PTY that hands a copy of everything read from it to `tap`, so the
/// output can be followed outside the grid.
pub(super) struct Tee<P> {
    pub(super) pty: P,
    pub(super) tap: std::sync::mpsc::Sender<Vec<u8>>,
}
impl<P: EventedReadWrite> io::Read for Tee<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.pty.reader().read(buf)?;
        if read > 0 {
            let _ = self.tap.send(buf[..read].to_vec());
        }
        Ok(read)
    }
}
impl<P: EventedReadWrite> EventedReadWrite for Tee<P> {
    type Reader = Self;
    type Writer = P::Writer;
    unsafe fn register(
        &mut self,
        poller: &Arc<Poller>,
        event: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // The wrapped PTY outlives its registration exactly as it would alone.
        unsafe { self.pty.register(poller, event, mode) }
    }
    fn reregister(&mut self, poller: &Arc<Poller>, event: Event, mode: PollMode) -> io::Result<()> {
        self.pty.reregister(poller, event, mode)
    }
    fn deregister(&mut self, poller: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poller)
    }
    fn reader(&mut self) -> &mut Self {
        self
    }
    fn writer(&mut self) -> &mut P::Writer {
        self.pty.writer()
    }
}
impl<P: EventedPty> EventedPty for Tee<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}
impl<P: OnResize> OnResize for Tee<P> {
    fn on_resize(&mut self, size: WindowSize) {
        self.pty.on_resize(size)
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Escape {
    #[default]
    Text,
    /// After ESC.
    Start,
    /// Inside `ESC [ ... final`.
    Control,
    /// Inside a string sequence (`ESC ]`, `ESC P`, ...), until BEL or ST.
    String,
    /// After ESC inside a string sequence.
    StringEnd,
    /// One more byte belongs to the sequence (`ESC ( B`).
    One,
}

/// Terminal output as plain text: escape sequences and control characters
/// are dropped, and a carriage return ends a line. Sequences and characters
/// split across reads are carried over.
#[derive(Default)]
pub(super) struct PlainText {
    escape: Escape,
    carriage: bool,
    partial: Vec<u8>,
}
impl PlainText {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> String {
        let mut out = std::mem::take(&mut self.partial);
        for &byte in bytes {
            match self.escape {
                Escape::Text => {
                    if std::mem::take(&mut self.carriage) {
                        out.push(b'\n');
                        if byte == b'\n' {
                            continue;
                        }
                    }
                    match byte {
                        0x1b => self.escape = Escape::Start,
                        b'\r' => self.carriage = true,
                        b'\n' | b'\t' => out.push(byte),
                        0..0x20 | 0x7f => {}
                        _ => out.push(byte),
                    }
                }
                Escape::Start => {
                    self.escape = match byte {
                        b'[' => Escape::Control,
                        b']' | b'P' | b'X' | b'^' | b'_' => Escape::String,
                        b'(' | b')' | b'*' | b'+' | b'#' | b' ' => Escape::One,
                        _ => Escape::Text,
                    }
                }
                Escape::Control => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = Escape::Text;
                    }
                }
                Escape::String => match byte {
                    0x07 => self.escape = Escape::Text,
                    0x1b => self.escape = Escape::StringEnd,
                    _ => {}
                },
                Escape::StringEnd | Escape::One => self.escape = Escape::Text,
            }
        }
        // A character cut in two by the read waits for its other half.
        let whole = match std::str::from_utf8(&out) {
            Ok(_) => out.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => out.len(),
        };
        self.partial = out.split_off(whole);
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_plain_text_across_split_reads() {
        let mut text = PlainText::default();
        assert_eq!(text.feed(b"\x1b[1;32mok\x1b[0m\r\n"), "ok\n");
        assert_eq!(text.feed(b"\x1b]0;title\x07a\x1b"), "a");
        assert_eq!(text.feed(b"[2Kb\x1b(Bc"), "bc");
        // A progress line rewritten in place becomes lines.
        assert_eq!(text.feed(b"1%\r2%\r\n"), "1%\n2%\n");
        // A carriage return ends the line once what follows it is known.
        assert_eq!(text.feed(b"a\r"), "a");
        assert_eq!(text.feed(b"b"), "\nb");
        let snow = "\u{2603}".as_bytes();
        assert_eq!(text.feed(&snow[..1]), "");
        assert_eq!(text.feed(&snow[1..]), "\u{2603}");
        assert_eq!(text.feed(b"\x08\x00tab\there"), "tab\there");
    }
}
