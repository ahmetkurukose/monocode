//! The debug sessions of the app, in one place for the debug pane and the
//! agents' tools to share. Sessions are started here from a run's process
//! and live until they are removed or the manager is dropped, which ends
//! their adapters and debuggees.
use super::adapter::{adapter, derive, DebugRequest, LaunchOptions};
use super::breakpoints::BreakpointStore;
use super::client::{lock, Client};
use super::session::{Session, SessionId, Setup};
use super::types::{Actor, DapError};
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use std::sync::{Arc, Mutex};

/// A top-level session joined or left the manager's list. A restart is a
/// `Removed` for the old session followed by an `Added` for the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagerEvent {
    Added(SessionId),
    Removed(SessionId),
}

/// Ended sessions kept for their output and exit code; older ones go.
const ENDED_KEPT: usize = 8;

struct Entry {
    session: Arc<Session>,
    toolchain: Toolchain,
}

pub struct Manager {
    breakpoints: BreakpointStore,
    sessions: Mutex<Vec<Entry>>,
    subscribers: Mutex<Vec<async_channel::Sender<ManagerEvent>>>,
}

impl Manager {
    /// Every session started here uses `breakpoints`.
    pub fn new(breakpoints: BreakpointStore) -> Self {
        Self {
            breakpoints,
            sessions: Mutex::new(Vec::new()),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Sessions coming and going from now on, whoever starts or ends them.
    /// Nothing is replayed: subscribe, then read `sessions()` once.
    pub fn subscribe(&self) -> async_channel::Receiver<ManagerEvent> {
        let (sender, receiver) = async_channel::unbounded();
        lock(&self.subscribers).push(sender);
        receiver
    }

    fn emit(&self, event: ManagerEvent) {
        lock(&self.subscribers).retain(|subscriber| subscriber.try_send(event).is_ok());
    }

    pub fn breakpoints(&self) -> &BreakpointStore {
        &self.breakpoints
    }

    /// Debugs what `spawn` would run. Blocks while the adapter starts
    /// (well under a second for debugpy): not for the interface thread.
    pub fn start(
        &self,
        spawn: &Spawn,
        toolchain: &Toolchain,
        options: &LaunchOptions,
        origin: Actor,
    ) -> Result<Arc<Session>, DapError> {
        let request = derive(spawn, options).ok_or_else(|| {
            DapError::State("This run cannot be debugged: only Python programs can for now.".into())
        })?;
        self.start_request(request, toolchain, origin)
    }

    /// Starts a prepared request, such as an attach. Blocks like `start`.
    pub fn start_request(
        &self,
        request: DebugRequest,
        toolchain: &Toolchain,
        origin: Actor,
    ) -> Result<Arc<Session>, DapError> {
        let session = self.open(request, toolchain, origin.clone(), origin)?;
        self.adopt(session.clone(), toolchain.clone());
        Ok(session)
    }

    /// Lists a started session, letting the oldest ended ones go.
    fn adopt(&self, session: Arc<Session>, toolchain: Toolchain) {
        let id = session.id();
        let mut dropped = Vec::new();
        {
            let mut sessions = lock(&self.sessions);
            let ended = |entry: &Entry| entry.session.state().is_terminated();
            let mut excess = sessions
                .iter()
                .filter(|entry| ended(entry))
                .count()
                .saturating_sub(ENDED_KEPT);
            sessions.retain(|entry| {
                let goes = excess > 0 && ended(entry);
                excess -= usize::from(goes);
                if goes {
                    dropped.push(entry.session.id());
                }
                !goes
            });
            sessions.push(Entry { session, toolchain });
        }
        for id in dropped {
            self.emit(ManagerEvent::Removed(id));
        }
        self.emit(ManagerEvent::Added(id));
    }

    /// A session over a scripted adapter instead of a process, listed like
    /// any other, for tests of the manager's callers. The handle is the
    /// script's thread: join it so a failed expectation fails the test.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn start_scripted(
        &self,
        request: DebugRequest,
        origin: Actor,
        script: impl FnOnce(&mut super::fake::Peer) + Send + 'static,
    ) -> Result<(Arc<Session>, std::thread::JoinHandle<()>), DapError> {
        let adapter = adapter(request.adapter).ok_or_else(|| {
            DapError::Start(format!(
                "There is no debug adapter named {}.",
                request.adapter
            ))
        })?;
        let (client, incoming, handle) = super::fake::fake(script);
        let session = Session::open(
            client,
            incoming,
            Setup {
                adapter,
                request,
                store: self.breakpoints.clone(),
                origin: origin.clone(),
                driver: origin,
                parent: None,
            },
        )?;
        self.adopt(session.clone(), Toolchain::default());
        Ok((session, handle))
    }

    fn open(
        &self,
        request: DebugRequest,
        toolchain: &Toolchain,
        origin: Actor,
        driver: Actor,
    ) -> Result<Arc<Session>, DapError> {
        let adapter = adapter(request.adapter).ok_or_else(|| {
            DapError::Start(format!(
                "There is no debug adapter named {}.",
                request.adapter
            ))
        })?;
        let start = adapter.locate(toolchain, &request.cwd)?;
        let (client, incoming) = Client::start(&start)?;
        Session::open(
            client,
            incoming,
            Setup {
                adapter,
                request,
                store: self.breakpoints.clone(),
                origin,
                driver,
                parent: None,
            },
        )
    }

    /// The sessions started here, oldest first, ended ones included. A
    /// session's subprocesses are under its `children()`.
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        lock(&self.sessions)
            .iter()
            .map(|entry| entry.session.clone())
            .collect()
    }

    /// A session or child session by id.
    pub fn get(&self, id: SessionId) -> Option<Arc<Session>> {
        fn find(sessions: Vec<Arc<Session>>, id: SessionId) -> Option<Arc<Session>> {
            sessions.into_iter().find_map(|session| {
                if session.id() == id {
                    return Some(session);
                }
                find(session.children(), id)
            })
        }
        find(self.sessions(), id)
    }

    /// Ends a session and starts its request again, in its place and with
    /// the same origin and driver. The new session has a new id. Blocks.
    pub fn restart(&self, id: SessionId) -> Result<Arc<Session>, DapError> {
        let (old, toolchain) = lock(&self.sessions)
            .iter()
            .find(|entry| entry.session.id() == id)
            .map(|entry| (entry.session.clone(), entry.toolchain.clone()))
            .ok_or_else(|| DapError::State("That debug session is gone.".into()))?;
        old.close();
        let session = self.open(
            old.request().clone(),
            &toolchain,
            old.origin().clone(),
            old.driver(),
        )?;
        {
            let mut sessions = lock(&self.sessions);
            let entry = Entry {
                session: session.clone(),
                toolchain,
            };
            match sessions.iter().position(|entry| entry.session.id() == id) {
                Some(index) => sessions[index] = entry,
                None => sessions.push(entry),
            }
        }
        self.emit(ManagerEvent::Removed(id));
        self.emit(ManagerEvent::Added(session.id()));
        Ok(session)
    }

    /// Ends a session and forgets it.
    pub fn remove(&self, id: SessionId) {
        let removed: Vec<Entry> = {
            let mut sessions = lock(&self.sessions);
            let (gone, kept) = sessions
                .drain(..)
                .partition(|entry| entry.session.id() == id);
            *sessions = kept;
            gone
        };
        for entry in removed {
            entry.session.close();
            self.emit(ManagerEvent::Removed(entry.session.id()));
        }
    }

    /// Ends every session: adapters and the debuggees they started.
    pub fn stop_all(&self) {
        let sessions = std::mem::take(&mut *lock(&self.sessions));
        for entry in sessions {
            entry.session.close();
            self.emit(ManagerEvent::Removed(entry.session.id()));
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::dap::adapter::{RequestKind, DEBUGPY};
    use serde_json::json;

    #[test]
    fn sessions_coming_and_going_are_announced() {
        let manager = Manager::new(BreakpointStore::new());
        let events = manager.subscribe();
        let request = DebugRequest {
            adapter: DEBUGPY,
            kind: RequestKind::Launch,
            label: "app.py".into(),
            cwd: "/p".into(),
            arguments: serde_json::Map::new(),
        };
        // An adapter that agrees to everything until the client hangs up.
        let (session, adapter) = manager
            .start_scripted(request, Actor::User, |peer| {
                while let Some(message) = peer.next() {
                    if message["type"] == json!("request") {
                        peer.respond(&message, json!({}));
                    }
                }
            })
            .unwrap();
        assert_eq!(events.try_recv(), Ok(ManagerEvent::Added(session.id())));
        assert_eq!(manager.sessions().len(), 1);
        assert!(manager.get(session.id()).is_some());

        manager.remove(session.id());
        assert_eq!(events.try_recv(), Ok(ManagerEvent::Removed(session.id())));
        assert!(manager.sessions().is_empty());
        assert!(events.try_recv().is_err());
        drop(session);
        adapter.join().unwrap();
    }
}
