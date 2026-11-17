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

/// Ended sessions kept for their output and exit code; older ones go.
const ENDED_KEPT: usize = 8;

struct Entry {
    session: Arc<Session>,
    toolchain: Toolchain,
}

pub struct Manager {
    breakpoints: BreakpointStore,
    sessions: Mutex<Vec<Entry>>,
}

impl Manager {
    /// Every session started here uses `breakpoints`.
    pub fn new(breakpoints: BreakpointStore) -> Self {
        Self {
            breakpoints,
            sessions: Mutex::new(Vec::new()),
        }
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
        let mut sessions = lock(&self.sessions);
        let ended = |entry: &Entry| entry.session.state().is_terminated();
        let mut excess = sessions
            .iter()
            .filter(|entry| ended(entry))
            .count()
            .saturating_sub(ENDED_KEPT);
        sessions.retain(|entry| {
            let dropped = excess > 0 && ended(entry);
            excess -= usize::from(dropped);
            !dropped
        });
        sessions.push(Entry {
            session: session.clone(),
            toolchain: toolchain.clone(),
        });
        Ok(session)
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
        let mut sessions = lock(&self.sessions);
        let entry = Entry {
            session: session.clone(),
            toolchain,
        };
        match sessions.iter().position(|entry| entry.session.id() == id) {
            Some(index) => sessions[index] = entry,
            None => sessions.push(entry),
        }
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
        }
    }

    /// Ends every session: adapters and the debuggees they started.
    pub fn stop_all(&self) {
        let sessions = std::mem::take(&mut *lock(&self.sessions));
        for entry in sessions {
            entry.session.close();
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
