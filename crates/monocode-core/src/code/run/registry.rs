//! What the app is running: every run the user or an agent started, its
//! state and the end of its output. The registry does not own a terminal
//! run's process (the terminal does); it records what the terminal tells
//! it and carries a stop request the other way. Runs started with no
//! terminal are in `headless.rs`.
use super::output::{RunOutput, Tail};
use crate::code::spawn::Spawn;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;

/// How many finished runs are kept for their output before the oldest go.
const FINISHED_KEPT: usize = 40;
/// A subscriber this far behind is not sent more output notices: it will
/// read the whole tail when it catches up anyway.
const BACKLOG: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartedBy {
    User,
    /// The session id of the agent that asked for the run.
    Agent(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// Ended by itself; the code is absent when a signal ended it.
    Exited(Option<i32>),
    /// Ended after a stop was asked for.
    Stopped,
}

impl RunState {
    pub fn is_running(self) -> bool {
        self == RunState::Running
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunInfo {
    pub id: RunId,
    pub label: String,
    pub spawn: Spawn,
    pub checkout: PathBuf,
    pub started_by: StartedBy,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub state: RunState,
    pub pid: Option<u32>,
    /// Started by the registry itself, with no terminal.
    pub headless: bool,
    /// A stop was asked for; a terminal run's owner must end the process.
    pub stop_requested: bool,
    /// The first local address the output announced (`ports::local_urls`).
    pub url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEvent {
    Started(RunId),
    /// There is new output to read. Sent per chunk, so read with an offset.
    Output(RunId),
    /// The state, process id or address changed.
    Changed(RunId),
    /// `stop` was called: the terminal that owns the run ends its process
    /// and then reports `finish`.
    StopRequested(RunId),
    Removed(RunId),
}

struct Entry {
    info: RunInfo,
    tail: Tail,
}

#[derive(Default)]
struct Inner {
    last_id: u64,
    runs: Vec<Entry>,
    subscribers: Vec<async_channel::Sender<RunEvent>>,
}

impl Inner {
    fn emit(&mut self, event: RunEvent) {
        let output = matches!(event, RunEvent::Output(_));
        self.subscribers.retain(|subscriber| {
            if subscriber.is_closed() {
                return false;
            }
            if !(output && subscriber.len() > BACKLOG) {
                let _ = subscriber.try_send(event);
            }
            true
        });
    }

    fn entry(&mut self, id: RunId) -> Option<&mut Entry> {
        self.runs.iter_mut().find(|entry| entry.info.id == id)
    }
}

/// Cheap to clone; every clone is the same registry. All methods may be
/// called from any thread and none blocks beyond a short lock.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Inner>>,
}

/// The registry of the app. Tests make their own with `Registry::new`.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every change from now on. Each subscriber has its own queue, which
    /// is dropped from the registry once the receiver is.
    pub fn subscribe(&self) -> async_channel::Receiver<RunEvent> {
        let (sender, receiver) = async_channel::unbounded();
        self.lock().subscribers.push(sender);
        receiver
    }

    /// Records a run whose process a terminal is about to start. The
    /// terminal then reports `set_pid`, `append_output` and `finish`, and
    /// ends the process when it sees `RunEvent::StopRequested` for it.
    pub fn register(&self, spawn: Spawn, checkout: &Path, started_by: StartedBy) -> RunId {
        self.insert(spawn, checkout, started_by, false, None)
    }

    pub(super) fn insert(
        &self,
        spawn: Spawn,
        checkout: &Path,
        started_by: StartedBy,
        headless: bool,
        pid: Option<u32>,
    ) -> RunId {
        let mut inner = self.lock();
        inner.last_id += 1;
        let id = RunId(inner.last_id);
        inner.runs.push(Entry {
            info: RunInfo {
                id,
                label: spawn.label.clone(),
                spawn,
                checkout: checkout.to_path_buf(),
                started_by,
                started_at: SystemTime::now(),
                ended_at: None,
                state: RunState::Running,
                pid,
                headless,
                stop_requested: false,
                url: None,
            },
            tail: Tail::default(),
        });
        inner.emit(RunEvent::Started(id));
        id
    }

    pub fn set_pid(&self, id: RunId, pid: u32) {
        let mut inner = self.lock();
        if let Some(entry) = inner.entry(id) {
            entry.info.pid = Some(pid);
            inner.emit(RunEvent::Changed(id));
        }
    }

    /// Adds output as the process wrote it; terminal escape codes are
    /// taken out here. Output may still arrive after `finish`.
    pub fn append_output(&self, id: RunId, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let Some(entry) = inner.entry(id) else {
            return;
        };
        if entry.tail.push(text) {
            entry.info.url = entry.tail.url().map(str::to_owned);
            inner.emit(RunEvent::Changed(id));
        }
        inner.emit(RunEvent::Output(id));
    }

    /// The process ended with `code` (absent when a signal ended it). The
    /// run becomes `Stopped` if a stop was asked for, `Exited` otherwise.
    /// A second call for the same run changes nothing.
    pub fn finish(&self, id: RunId, code: Option<i32>) {
        let mut inner = self.lock();
        let Some(entry) = inner.entry(id) else {
            return;
        };
        if !entry.info.state.is_running() {
            return;
        }
        entry.info.state = match entry.info.stop_requested {
            true => RunState::Stopped,
            false => RunState::Exited(code),
        };
        entry.info.ended_at = Some(SystemTime::now());
        inner.emit(RunEvent::Changed(id));
        let finished = |entry: &Entry| !entry.info.state.is_running();
        let mut excess = inner
            .runs
            .iter()
            .filter(|entry| finished(entry))
            .count()
            .saturating_sub(FINISHED_KEPT);
        while excess > 0 {
            let Some(oldest) = inner.runs.iter().position(finished) else {
                break;
            };
            let removed = inner.runs.remove(oldest).info.id;
            inner.emit(RunEvent::Removed(removed));
            excess -= 1;
        }
    }

    /// Asks a running run to stop; false when it is not running. A
    /// headless run's process group is ended by its watcher. A terminal
    /// run is ended by its terminal, which sees
    /// `RunEvent::StopRequested`. Either way the run stays `Running` until
    /// its process is gone.
    pub fn stop(&self, id: RunId) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.entry(id) else {
            return false;
        };
        if !entry.info.state.is_running() {
            return false;
        }
        if !entry.info.stop_requested {
            entry.info.stop_requested = true;
            inner.emit(RunEvent::StopRequested(id));
        }
        true
    }

    /// Whether a stop is waiting to be honoured; what a headless run's
    /// watcher polls.
    pub(super) fn stop_requested(&self, id: RunId) -> bool {
        self.lock()
            .entry(id)
            .is_some_and(|entry| entry.info.stop_requested && entry.info.state.is_running())
    }

    /// Forgets a finished run and its output. False for a running one.
    pub fn remove(&self, id: RunId) -> bool {
        let mut inner = self.lock();
        let Some(index) = inner
            .runs
            .iter()
            .position(|entry| entry.info.id == id && !entry.info.state.is_running())
        else {
            return false;
        };
        inner.runs.remove(index);
        inner.emit(RunEvent::Removed(id));
        true
    }

    pub fn get(&self, id: RunId) -> Option<RunInfo> {
        self.lock().entry(id).map(|entry| entry.info.clone())
    }

    /// Every run, the oldest first.
    pub fn list(&self) -> Vec<RunInfo> {
        (self.lock().runs.iter())
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// The run's output after offset `from` (0 for all that is kept), as
    /// plain text. Pass the result's `next` to read only what follows.
    pub fn output(&self, id: RunId, from: u64) -> Option<RunOutput> {
        self.lock().entry(id).map(|entry| entry.tail.read(from))
    }

    /// Ends every headless run at once, for when the app quits and their
    /// watchers may not get to run again. Terminal runs end with their
    /// terminals.
    pub fn shutdown(&self) {
        let inner = self.lock();
        for entry in &inner.runs {
            if let (true, true, Some(pid)) = (
                entry.info.headless,
                entry.info.state.is_running(),
                entry.info.pid,
            ) {
                super::headless::kill(pid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(label: &str) -> Spawn {
        Spawn {
            label: label.into(),
            command: "npm".into(),
            args: vec!["start".into()],
            cwd: "/work/tree".into(),
            env: Vec::new(),
        }
    }

    fn drain(events: &async_channel::Receiver<RunEvent>) -> Vec<RunEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    #[test]
    fn a_terminal_run_is_followed_from_start_to_exit() {
        let registry = Registry::new();
        let events = registry.subscribe();
        let id = registry.register(spawn("Serve"), Path::new("/work/tree"), StartedBy::User);
        let info = registry.get(id).unwrap();
        assert_eq!(info.label, "Serve");
        assert_eq!(info.spawn, spawn("Serve"));
        assert_eq!(info.checkout, PathBuf::from("/work/tree"));
        assert_eq!(info.started_by, StartedBy::User);
        assert_eq!(info.state, RunState::Running);
        assert_eq!(
            (info.pid, info.headless, info.stop_requested),
            (None, false, false)
        );
        assert_eq!((info.url.clone(), info.ended_at), (None, None));

        registry.set_pid(id, 4242);
        registry.append_output(id, "\u{1b}[32mcompiled\u{1b}[0m\r\n");
        registry.append_output(id, "");
        registry.append_output(id, "Local: http://localhost:5173/\r\n");
        registry.finish(id, Some(3));
        assert_eq!(
            drain(&events),
            [
                RunEvent::Started(id),
                RunEvent::Changed(id),
                RunEvent::Output(id),
                RunEvent::Changed(id),
                RunEvent::Output(id),
                RunEvent::Changed(id),
            ]
        );
        let info = registry.get(id).unwrap();
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.state, RunState::Exited(Some(3)));
        assert_eq!(info.url.as_deref(), Some("http://localhost:5173/"));
        assert!(info.ended_at.is_some_and(|ended| ended >= info.started_at));

        let output = registry.output(id, 0).unwrap();
        assert_eq!(output.text, "compiled\nLocal: http://localhost:5173/\n");
        // Late output is still kept; a second finish is not a new state.
        registry.append_output(id, "bye\n");
        registry.finish(id, Some(0));
        assert_eq!(registry.output(id, output.next).unwrap().text, "bye\n");
        assert_eq!(registry.get(id).unwrap().state, RunState::Exited(Some(3)));
        assert_eq!(drain(&events), [RunEvent::Output(id)]);
    }

    #[test]
    fn a_stop_is_a_request_the_terminal_honours() {
        let registry = Registry::new();
        let events = registry.subscribe();
        let agent = StartedBy::Agent(17);
        let id = registry.register(spawn("Tests"), Path::new("/work/tree"), agent);
        assert!(registry.stop(id));
        let info = registry.get(id).unwrap();
        assert_eq!(info.started_by, agent);
        // Still running until the terminal says the process is gone.
        assert_eq!((info.state, info.stop_requested), (RunState::Running, true));
        assert!(!registry.remove(id));
        registry.finish(id, None);
        assert_eq!(registry.get(id).unwrap().state, RunState::Stopped);
        assert!(!registry.stop(id), "nothing to stop any more");
        assert!(!registry.stop(RunId(999)));
        assert!(registry.remove(id));
        assert!(!registry.remove(id));
        assert_eq!(registry.get(id), None);
        assert_eq!(registry.output(id, 0), None);
        assert_eq!(
            drain(&events),
            [
                RunEvent::Started(id),
                RunEvent::StopRequested(id),
                RunEvent::Changed(id),
                RunEvent::Removed(id),
            ]
        );
        // Calls for a run that is gone are ignored.
        registry.set_pid(id, 1);
        registry.append_output(id, "x");
        registry.finish(id, Some(0));
        assert!(drain(&events).is_empty());
    }

    #[test]
    fn runs_are_listed_in_order_and_old_finished_ones_go() {
        let registry = Registry::new();
        let keeper = registry.register(spawn("Server"), Path::new("/work/tree"), StartedBy::User);
        let events = registry.subscribe();
        let mut ids = Vec::new();
        for index in 0..FINISHED_KEPT + 2 {
            let id = registry.register(
                spawn(&format!("Run {index}")),
                Path::new("/work/tree"),
                StartedBy::User,
            );
            registry.finish(id, Some(0));
            ids.push(id);
        }
        let listed: Vec<RunId> = registry.list().iter().map(|info| info.id).collect();
        assert_eq!(listed.len(), FINISHED_KEPT + 1);
        assert_eq!(listed[0], keeper, "a running run is never dropped");
        assert_eq!(listed[1..], ids[2..]);
        let removed: Vec<RunEvent> = drain(&events)
            .into_iter()
            .filter(|event| matches!(event, RunEvent::Removed(_)))
            .collect();
        assert_eq!(
            removed,
            [RunEvent::Removed(ids[0]), RunEvent::Removed(ids[1])]
        );
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn every_subscriber_hears_and_a_dropped_one_is_forgotten() {
        let registry = Registry::new();
        let first = registry.subscribe();
        let second = registry.subscribe();
        let id = registry.register(spawn("Serve"), Path::new("/work/tree"), StartedBy::User);
        assert_eq!(drain(&first), [RunEvent::Started(id)]);
        assert_eq!(drain(&second), [RunEvent::Started(id)]);
        drop(first);
        registry.set_pid(id, 7);
        assert_eq!(registry.lock().subscribers.len(), 1);
        assert_eq!(drain(&second), [RunEvent::Changed(id)]);

        // A subscriber that stopped reading is not flooded with output
        // notices, but never misses a state change.
        for _ in 0..BACKLOG * 2 {
            registry.append_output(id, "line\n");
        }
        registry.finish(id, Some(0));
        let heard = drain(&second);
        assert!(heard.len() <= BACKLOG + 2, "{}", heard.len());
        assert_eq!(heard.last(), Some(&RunEvent::Changed(id)));
        // The clone is the same registry.
        assert_eq!(registry.clone().list().len(), 1);
    }
}
