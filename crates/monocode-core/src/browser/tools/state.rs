//! What the tools remember between calls, on the main thread: each scope's
//! last page, each page's command queue (K35), refs (K33) and child
//! sessions (K42).

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::{Rc, Weak},
};

use serde_json::{Value, json};

use super::snapshot::Refs;
use crate::browser::{CdpClient, CdpError, PageId, Scope, Subscription};

/// A page's FIFO of commands: one runs at a time.
#[derive(Default)]
pub struct Queue {
    busy: Cell<bool>,
    waiting: RefCell<VecDeque<async_channel::Sender<()>>>,
}

/// The right to run on the page; the next command gets it on drop.
pub struct Turn(Rc<Queue>);

impl Drop for Turn {
    fn drop(&mut self) {
        self.0.pass();
    }
}

/// A command waiting in line; if dropped after being handed the turn, it
/// hands it on.
struct Waiting {
    queue: Rc<Queue>,
    rx: async_channel::Receiver<()>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if self.rx.try_recv().is_ok() {
            self.queue.pass();
        }
    }
}

impl Queue {
    pub async fn turn(self: &Rc<Self>) -> Turn {
        if !self.busy.replace(true) {
            return Turn(self.clone());
        }
        let (tx, rx) = async_channel::bounded(1);
        self.waiting.borrow_mut().push_back(tx);
        let waiting = Waiting {
            queue: self.clone(),
            rx,
        };
        let _ = waiting.rx.recv().await;
        // Ours now: forget the line without passing the turn on.
        let queue = waiting.queue.clone();
        drop(waiting);
        Turn(queue)
    }

    fn pass(&self) {
        loop {
            let next = self.waiting.borrow_mut().pop_front();
            match next {
                Some(tx) => {
                    if tx.try_send(()).is_ok() {
                        return;
                    }
                }
                None => {
                    self.busy.set(false);
                    return;
                }
            }
        }
    }

    pub fn idle(&self) -> bool {
        !self.busy.get()
    }
}

/// An out-of-process iframe attached with `Target.setAutoAttach`.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    pub target_id: String,
    pub session: String,
    pub url: String,
}

/// A snapshot the agent saw: the baseline of the next delta (K45, K46).
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    /// The main frame's loader id: which document it is of.
    pub loader: Option<String>,
    /// Its mode, selector and depth: only snapshots alike compare.
    pub key: String,
    pub lines: Vec<String>,
}

/// The tools' state of one page's current CDP client.
pub struct PageState {
    pub cdp: Weak<CdpClient>,
    pub refs: RefCell<Refs>,
    pub children: Rc<RefCell<Vec<Child>>>,
    /// The page's last snapshot the agent saw.
    pub seen: RefCell<Option<Shot>>,
    /// The isolated world the action effects watch the DOM from, and the
    /// document (loader id) it belongs to.
    pub world: RefCell<Option<(Option<String>, i64)>>,
    attached: Cell<bool>,
    _subscription: Subscription,
}

impl PageState {
    /// Starts following the page's iframe targets.
    fn new(cdp: &Rc<CdpClient>) -> Rc<PageState> {
        let children: Rc<RefCell<Vec<Child>>> = Rc::default();
        let list = children.clone();
        let subscription = cdp.subscribe(move |event| {
            if event.session_id.is_some() {
                return;
            }
            let p = &event.params;
            match event.method.as_str() {
                "Target.attachedToTarget" if p["targetInfo"]["type"] == "iframe" => {
                    let child = Child {
                        target_id: str_of(&p["targetInfo"]["targetId"]),
                        session: str_of(&p["sessionId"]),
                        url: str_of(&p["targetInfo"]["url"]),
                    };
                    let mut list = list.borrow_mut();
                    list.retain(|c| c.target_id != child.target_id);
                    list.push(child);
                }
                "Target.detachedFromTarget" => {
                    let session = str_of(&p["sessionId"]);
                    list.borrow_mut().retain(|c| c.session != session);
                }
                "Target.targetInfoChanged" => {
                    let info = &p["targetInfo"];
                    let id = str_of(&info["targetId"]);
                    for child in list.borrow_mut().iter_mut() {
                        if child.target_id == id {
                            child.url = str_of(&info["url"]);
                        }
                    }
                }
                _ => {}
            }
        });
        Rc::new(PageState {
            cdp: Rc::downgrade(cdp),
            refs: RefCell::default(),
            children,
            seen: RefCell::default(),
            world: RefCell::default(),
            attached: Cell::new(false),
            _subscription: subscription,
        })
    }

    /// Attaches the page's out-of-process iframes once (K42); Chromium
    /// announces the ones already there before it answers.
    pub async fn attach(&self, cdp: &CdpClient) -> Result<(), CdpError> {
        if self.attached.replace(true) {
            return Ok(());
        }
        let attach = cdp.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            None,
        );
        match attach.await {
            Err(CdpError::Gone) => Err(CdpError::Gone),
            _ => Ok(()),
        }
    }
}

fn str_of(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}

#[derive(Default)]
struct State {
    last_page: HashMap<Scope, PageId>,
    queues: HashMap<PageId, Rc<Queue>>,
    pages: HashMap<PageId, Rc<PageState>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

pub fn last_page(scope: &Scope) -> Option<PageId> {
    STATE.with(|s| s.borrow().last_page.get(scope).copied())
}

pub fn set_last_page(scope: &Scope, page: PageId) {
    STATE.with(|s| s.borrow_mut().last_page.insert(scope.clone(), page));
}

pub fn queue(page: PageId) -> Rc<Queue> {
    STATE.with(|s| s.borrow_mut().queues.entry(page).or_default().clone())
}

/// The page's state for `cdp`; new when the page got a new client.
pub fn page_state(page: PageId, cdp: &Rc<CdpClient>) -> Rc<PageState> {
    let current = STATE.with(|s| {
        s.borrow()
            .pages
            .get(&page)
            .filter(|state| state.cdp.upgrade().is_some_and(|c| Rc::ptr_eq(&c, cdp)))
            .cloned()
    });
    if let Some(state) = current {
        return state;
    }
    let state = PageState::new(cdp);
    STATE.with(|s| s.borrow_mut().pages.insert(page, state.clone()));
    state
}

/// Forgets a closed page.
pub fn forget(page: PageId) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.pages.remove(&page);
        if s.queues.get(&page).is_some_and(|q| q.idle()) {
            s.queues.remove(&page);
        }
        s.last_page.retain(|_, p| *p != page);
    });
}

/// Drops what belongs to pages whose client is gone.
pub fn prune() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let gone: Vec<PageId> = s
            .pages
            .iter()
            .filter(|(_, st)| st.cdp.upgrade().is_none_or(|c| c.is_gone()))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            s.pages.remove(&id);
            if s.queues.get(&id).is_some_and(|q| q.idle()) {
                s.queues.remove(&id);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt as _;

    #[test]
    fn turns_go_in_order_and_skip_the_dropped() {
        let queue = Rc::new(Queue::default());
        let first = queue.turn().now_or_never().expect("free");
        let mut second = queue.turn().boxed_local();
        let third = queue.turn().boxed_local();
        let mut fourth = queue.turn().boxed_local();
        assert!((&mut second).now_or_never().is_none());
        assert!((&mut fourth).now_or_never().is_none());
        drop(third);
        drop(first);
        let second = second.now_or_never().expect("second is next");
        assert!((&mut fourth).now_or_never().is_none());
        drop(second);
        let fourth = fourth.now_or_never().expect("third left the line");
        drop(fourth);
        assert!(queue.idle());
    }

    #[test]
    fn a_turn_handed_to_a_dropped_waiter_moves_on() {
        let queue = Rc::new(Queue::default());
        let first = queue.turn().now_or_never().unwrap();
        let mut second = queue.turn().boxed_local();
        let mut third = queue.turn().boxed_local();
        assert!((&mut second).now_or_never().is_none());
        assert!((&mut third).now_or_never().is_none());
        drop(first); // handed to second, which never runs again
        drop(second);
        assert!(third.now_or_never().is_some());
    }
}
