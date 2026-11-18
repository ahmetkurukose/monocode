//! What a client has queued for its server's input. A server that stops
//! reading must not make the app queue without limit while the user types,
//! so the queue is bounded, a document's pending `didChange` is replaced by
//! its newest (the server is sent the whole text each time, so only that
//! one matters), and one write that has not finished for too long is told
//! to the caller, which then gives the server up as hung.
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Most bytes waiting to be written. Coalescing keeps one text per open
/// document, so this is far above what a working server ever has queued.
pub(super) const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_QUEUED_MESSAGES: usize = 4096;

/// Why a message could not be queued.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refused {
    /// The queue is closed; nothing more is sent.
    Closed,
    /// The server is not reading: the queue is full, or a write has been
    /// stuck for longer than allowed.
    Stuck(String),
}

enum Kind {
    /// A `didChange` of this document; a newer one replaces it.
    Change(String),
    /// A `didOpen` or `didClose` of this document: a `didChange` queued
    /// before it must not be replaced by one queued after it.
    Barrier(String),
    Other,
}

struct Message {
    kind: Kind,
    body: Vec<u8>,
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<Message>,
    bytes: usize,
    closed: bool,
    /// When the write now in progress began.
    writing_since: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Outgoing {
    inner: Mutex<Inner>,
    ready: Condvar,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn kind_of(method: Option<&str>, params: Option<&Value>) -> Kind {
    let uri = || {
        params?
            .get("textDocument")?
            .get("uri")?
            .as_str()
            .map(str::to_owned)
    };
    match (method, uri()) {
        (Some("textDocument/didChange"), Some(uri)) => Kind::Change(uri),
        (Some("textDocument/didOpen" | "textDocument/didClose"), Some(uri)) => Kind::Barrier(uri),
        _ => Kind::Other,
    }
}

impl Outgoing {
    /// Queues a message. `patience` is how long a write may take before the
    /// server counts as not reading.
    pub(super) fn push(&self, message: &Value, patience: Duration) -> Result<(), Refused> {
        let kind = kind_of(
            message.get("method").and_then(Value::as_str),
            message.get("params"),
        );
        let body = message.to_string().into_bytes();
        let mut inner = lock(&self.inner);
        if inner.closed {
            return Err(Refused::Closed);
        }
        if let Some(since) = inner
            .writing_since
            .filter(|since| since.elapsed() > patience)
        {
            return Err(Refused::Stuck(format!(
                "has not read its input for {} s",
                since.elapsed().as_secs()
            )));
        }
        let replaced = match &kind {
            Kind::Change(uri) => Self::replace(&mut inner, uri, &body),
            _ => false,
        };
        if !replaced {
            inner.bytes += body.len();
            inner.queue.push_back(Message { kind, body });
        }
        if inner.bytes > MAX_QUEUED_BYTES || inner.queue.len() > MAX_QUEUED_MESSAGES {
            return Err(Refused::Stuck(format!(
                "has {} messages ({} bytes) waiting to be read",
                inner.queue.len(),
                inner.bytes
            )));
        }
        drop(inner);
        self.ready.notify_one();
        Ok(())
    }

    /// Puts `body` in place of the newest queued `didChange` of `uri` that
    /// no open or close of it follows.
    fn replace(inner: &mut Inner, uri: &str, body: &[u8]) -> bool {
        let found = inner.queue.iter().rposition(|message| match &message.kind {
            Kind::Change(other) | Kind::Barrier(other) => other == uri,
            Kind::Other => false,
        });
        let Some(at) = found.filter(|at| matches!(inner.queue[*at].kind, Kind::Change(_))) else {
            return false;
        };
        let old = std::mem::replace(&mut inner.queue[at].body, body.to_vec());
        inner.bytes = inner.bytes - old.len() + body.len();
        true
    }

    /// No more messages are taken; what is queued is still written.
    pub(super) fn close(&self) {
        lock(&self.inner).closed = true;
        self.ready.notify_all();
    }

    /// The writer's side: the next message, waiting for one; `None` once
    /// closed and drained. A message taken counts as being written until
    /// `written`.
    pub(super) fn next(&self) -> Option<Vec<u8>> {
        let mut inner = lock(&self.inner);
        loop {
            if let Some(message) = inner.queue.pop_front() {
                inner.bytes -= message.body.len();
                inner.writing_since = Some(Instant::now());
                return Some(message.body);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .ready
                .wait(inner)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub(super) fn written(&self) {
        lock(&self.inner).writing_since = None;
    }

    #[cfg(test)]
    pub(super) fn queued(&self) -> usize {
        lock(&self.inner).queue.len()
    }
}
