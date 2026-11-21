//! One page's CDP multiplexer (context.md §6.1, K38).
//!
//! A page has a single DevTools channel (`monocode_cef::BrowserView::
//! devtools_send` / `devtools_attach`). Everyone who speaks CDP to the page,
//! the agent tools and every client of the external endpoint, goes through
//! one [`CdpClient`], which gives each message an id of its own, routes the
//! result back to whoever asked, and hands every event to every subscriber.
//!
//! Main thread only: nothing here is `Send`. The glue feeds the channel's
//! messages to [`CdpClient::on_message`].

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    rc::{Rc, Weak},
};

use serde_json::{Value, json};

/// Our ids stay below this; page captures in `monocode-cef` count from it.
const MAX_ID: i64 = 0x3fff_ffff;

/// A CDP event, from the page itself (`session_id: None`) or from a child
/// target attached with `Target.setAutoAttach {flatten: true}` (an
/// out-of-process iframe, a worker).
#[derive(Clone, Debug, PartialEq)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CdpError {
    /// Chromium answered with an error.
    Protocol { code: i64, message: String },
    /// The page's DevTools agent went away (crash, close) before an answer.
    Gone,
    /// Chromium did not take the message.
    Refused,
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::Protocol { code, message } => write!(f, "{message} ({code})"),
            CdpError::Gone => f.write_str("the page is gone"),
            CdpError::Refused => f.write_str("Chromium did not take the message"),
        }
    }
}

impl std::error::Error for CdpError {}

/// Sends one raw message to the page; false when it was not taken.
pub type Transport = Box<dyn Fn(&[u8]) -> bool>;

enum Waiter {
    /// An agent tool's `call`.
    Call(async_channel::Sender<Result<Value, CdpError>>),
    /// A forwarded client message: its own id goes back on the answer.
    Forward {
        original_id: Value,
        reply: Box<dyn FnOnce(Value)>,
    },
}

type Listener = Rc<dyn Fn(&CdpEvent)>;

pub struct CdpClient {
    transport: Transport,
    next_id: Cell<i64>,
    waiting: RefCell<HashMap<i64, Waiter>>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next_listener: Cell<u64>,
    gone: Cell<bool>,
    weak: Weak<CdpClient>,
}

/// Keeps a subscription; dropping it unsubscribes.
pub struct Subscription {
    client: Weak<CdpClient>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(client) = self.client.upgrade() {
            client
                .listeners
                .borrow_mut()
                .retain(|(id, _)| *id != self.id);
        }
    }
}

impl CdpClient {
    pub fn new(transport: Transport) -> Rc<Self> {
        Rc::new_cyclic(|weak| CdpClient {
            transport,
            next_id: Cell::new(0),
            waiting: RefCell::default(),
            listeners: RefCell::default(),
            next_listener: Cell::new(0),
            gone: Cell::new(false),
            weak: weak.clone(),
        })
    }

    /// True once the page's DevTools agent went away; every call fails.
    pub fn is_gone(&self) -> bool {
        self.gone.get()
    }

    fn take_id(&self) -> i64 {
        let id = self.next_id.get() % MAX_ID + 1;
        self.next_id.set(id);
        id
    }

    fn send(&self, id: i64, waiter: Waiter, message: &Value) -> Result<(), Waiter> {
        if self.gone.get() {
            return Err(waiter);
        }
        // Waiting before sending: an answer may come back inside the send.
        self.waiting.borrow_mut().insert(id, waiter);
        let bytes = serde_json::to_vec(message).unwrap_or_default();
        if (self.transport)(&bytes) {
            Ok(())
        } else {
            Err(self
                .waiting
                .borrow_mut()
                .remove(&id)
                .expect("waiter just inserted"))
        }
    }

    /// Calls `method` on the page, or on a child target's `session`.
    pub fn call(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> impl Future<Output = Result<Value, CdpError>> + 'static {
        let (tx, rx) = async_channel::bounded(1);
        let id = self.take_id();
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let failed = match self.send(id, Waiter::Call(tx), &message) {
            Ok(()) => None,
            Err(_) if self.gone.get() => Some(CdpError::Gone),
            Err(_) => Some(CdpError::Refused),
        };
        async move {
            if let Some(error) = failed {
                return Err(error);
            }
            rx.recv().await.unwrap_or(Err(CdpError::Gone))
        }
    }

    /// Forwards a client's message (with the client's own `id`, any
    /// `sessionId`); `reply` gets Chromium's answer with that id put back.
    /// If the page goes first, `reply` gets a CDP error answer. Returns false
    /// (and does not call `reply`) when the message could not be sent.
    pub fn forward(&self, mut message: Value, reply: Box<dyn FnOnce(Value)>) -> bool {
        let original_id = message.get("id").cloned().unwrap_or(Value::Null);
        let id = self.take_id();
        message["id"] = id.into();
        self.send(id, Waiter::Forward { original_id, reply }, &message)
            .is_ok()
    }

    /// Calls `listener` with every event until the subscription is dropped.
    pub fn subscribe(&self, listener: impl Fn(&CdpEvent) + 'static) -> Subscription {
        let id = self.next_listener.get() + 1;
        self.next_listener.set(id);
        self.listeners.borrow_mut().push((id, Rc::new(listener)));
        Subscription {
            client: self.weak.clone(),
            id,
        }
    }

    /// One message from the page's DevTools channel; `None` when the agent
    /// went away.
    pub fn on_message(&self, raw: Option<&[u8]>) {
        let Some(raw) = raw else {
            self.go();
            return;
        };
        let Ok(message) = serde_json::from_slice::<Value>(raw) else {
            eprintln!("browser: unreadable CDP message ({} bytes)", raw.len());
            return;
        };
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            let waiter = self.waiting.borrow_mut().remove(&id);
            match waiter {
                Some(Waiter::Call(tx)) => {
                    let _ = tx.try_send(result_of(&message));
                }
                Some(Waiter::Forward { original_id, reply }) => {
                    let mut message = message;
                    message["id"] = original_id;
                    reply(message);
                }
                // An id of another DevTools client of the page.
                None => {}
            }
            return;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        let event = CdpEvent {
            method: method.to_owned(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
            session_id: message
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        // Listeners may subscribe or unsubscribe while being called.
        let listeners: Vec<Listener> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            listener(&event);
        }
    }

    /// The page is gone: every waiter learns it now.
    fn go(&self) {
        self.gone.set(true);
        let waiting: Vec<(i64, Waiter)> = self.waiting.borrow_mut().drain().collect();
        for (_, waiter) in waiting {
            match waiter {
                Waiter::Call(tx) => {
                    let _ = tx.try_send(Err(CdpError::Gone));
                }
                Waiter::Forward { original_id, reply } => reply(json!({
                    "id": original_id,
                    "error": {"code": -32000, "message": "Target closed"},
                })),
            }
        }
    }
}

fn result_of(message: &Value) -> Result<Value, CdpError> {
    if let Some(error) = message.get("error") {
        return Err(CdpError::Protocol {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("CDP error")
                .to_owned(),
        });
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt as _;

    fn client() -> (Rc<CdpClient>, Rc<RefCell<Vec<Value>>>) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let log = sent.clone();
        let client = CdpClient::new(Box::new(move |bytes| {
            log.borrow_mut()
                .push(serde_json::from_slice(bytes).unwrap());
            true
        }));
        (client, sent)
    }

    #[test]
    fn a_call_gets_its_own_result() {
        let (client, sent) = client();
        let first = client.call("Page.enable", json!({}), None);
        let second = client.call("DOM.getDocument", json!({}), Some("S1"));
        let ids: Vec<i64> = sent
            .borrow()
            .iter()
            .map(|m| m["id"].as_i64().unwrap())
            .collect();
        assert_ne!(ids[0], ids[1]);
        assert_eq!(sent.borrow()[1]["sessionId"], "S1");
        client.on_message(Some(
            json!({"id": ids[1], "result": {"root": 1}})
                .to_string()
                .as_bytes(),
        ));
        client.on_message(Some(
            json!({"id": ids[0], "error": {"code": -1, "message": "no"}})
                .to_string()
                .as_bytes(),
        ));
        assert_eq!(second.now_or_never(), Some(Ok(json!({"root": 1}))));
        assert_eq!(
            first.now_or_never(),
            Some(Err(CdpError::Protocol {
                code: -1,
                message: "no".into()
            }))
        );
    }

    #[test]
    fn a_forwarded_message_gets_the_clients_id_back() {
        let (client, sent) = client();
        let answer = Rc::new(RefCell::new(None));
        let out = answer.clone();
        assert!(client.forward(
            json!({"id": 7, "method": "Runtime.evaluate", "params": {}}),
            Box::new(move |m| *out.borrow_mut() = Some(m)),
        ));
        let ours = sent.borrow()[0]["id"].as_i64().unwrap();
        client.on_message(Some(
            json!({"id": ours, "result": {}}).to_string().as_bytes(),
        ));
        assert_eq!(answer.borrow().as_ref().unwrap()["id"], 7);
    }

    #[test]
    fn events_reach_every_subscriber_until_dropped() {
        let (client, _) = client();
        let seen = Rc::new(Cell::new(0));
        let a = seen.clone();
        let first = client.subscribe(move |_| a.set(a.get() + 1));
        let b = seen.clone();
        let _second = client.subscribe(move |e| {
            assert_eq!(e.session_id.as_deref(), Some("S"));
            b.set(b.get() + 10)
        });
        let event = json!({"method": "Log.entryAdded", "params": {}, "sessionId": "S"});
        client.on_message(Some(event.to_string().as_bytes()));
        drop(first);
        client.on_message(Some(event.to_string().as_bytes()));
        assert_eq!(seen.get(), 21);
    }

    #[test]
    fn waiters_learn_at_once_that_the_page_is_gone() {
        let (client, _) = client();
        let call = client.call("Page.navigate", json!({}), None);
        let answer = Rc::new(RefCell::new(None));
        let out = answer.clone();
        client.forward(
            json!({"id": 3, "method": "X"}),
            Box::new(move |m| *out.borrow_mut() = Some(m)),
        );
        client.on_message(None);
        assert_eq!(call.now_or_never(), Some(Err(CdpError::Gone)));
        assert_eq!(answer.borrow().as_ref().unwrap()["id"], 3);
        assert!(answer.borrow().as_ref().unwrap().get("error").is_some());
        assert_eq!(
            client.call("Page.reload", json!({}), None).now_or_never(),
            Some(Err(CdpError::Gone))
        );
    }

    #[test]
    fn a_refused_message_fails_the_call() {
        let client = CdpClient::new(Box::new(|_| false));
        assert_eq!(
            client.call("Page.enable", json!({}), None).now_or_never(),
            Some(Err(CdpError::Refused))
        );
        assert!(!client.forward(json!({"id": 1}), Box::new(|_| panic!("no reply"))));
    }
}
