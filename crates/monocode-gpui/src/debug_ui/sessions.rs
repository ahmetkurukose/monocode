//! Every debug session of the app, whoever started it. The debug manager
//! is followed for sessions coming and going, so one an agent starts over
//! MCP gets its dock tab like one the user starts; each top-level session
//! has a model and, once the window is up, a tab. Also where the keys find
//! the session they act on.
use super::{
    model::{Control, Model, ModelEvent},
    tab,
};
use crate::code_shell::{
    self,
    context::{self, Checkout, CodeEvent},
};
use gpui::{App, AppContext, Context, Entity, EntityId, Global, Subscription, Task};
use monocode_core::code::{
    dap::{Actor, ManagerEvent, Session, SessionId},
    services::services,
};
use std::sync::Arc;

struct Entry {
    model: Entity<Model>,
    _events: Subscription,
}

pub(super) struct Debugger {
    entries: Vec<Entry>,
    /// The session the keys act on: the last one started here or paused.
    current: Option<EntityId>,
    _follow: Task<()>,
    _context: Subscription,
}

struct Shared(Entity<Debugger>);
impl Global for Shared {}

pub(super) fn init(cx: &mut App) {
    let debugger = cx.new(Debugger::new);
    cx.set_global(Shared(debugger));
}

pub(super) fn debugger(cx: &App) -> Option<Entity<Debugger>> {
    cx.try_global::<Shared>().map(|shared| shared.0.clone())
}

/// The model of the session the keys and commands act on.
pub(super) fn current(cx: &App) -> Option<Entity<Model>> {
    debugger(cx)?.read(cx).current_model(cx)
}

/// Presses a control of the current session. False when there is none or
/// the control does not apply now, so the key can go elsewhere.
pub(super) fn act(control: Control, cx: &mut App) -> bool {
    let Some(model) = current(cx).filter(|model| model.read(cx).can(control)) else {
        return false;
    };
    model.update(cx, |model, cx| model.act(control, cx));
    true
}

/// Where a session's files are: the checkout its debuggee runs in.
fn checkout_of(session: &Session, cx: &App) -> Checkout {
    let folder = &session.request().cwd;
    context::checkout_for_path(&folder.to_string_lossy(), cx)
        .or_else(|| {
            let found = services().context(folder);
            let project = found.project.to_string_lossy().into_owned();
            (context::snapshot(cx).projects.contains(&project)).then(|| Checkout {
                project,
                folder: found.checkout.to_string_lossy().into_owned(),
            })
        })
        .or_else(|| context::focused_checkout(cx))
        .unwrap_or_default()
}

impl Debugger {
    fn new(cx: &mut Context<Self>) -> Self {
        let manager = services().dap();
        let events = manager.subscribe();
        let follow = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let heard = this.update(cx, |this, cx| match event {
                    ManagerEvent::Added(id) => this.added(id, cx),
                    ManagerEvent::Removed(id) => this.removed(id, cx),
                });
                if heard.is_err() {
                    break;
                }
            }
        });
        let context = context::events(cx);
        let mut this = Self {
            entries: Vec::new(),
            current: None,
            _follow: follow,
            // A session that began before the window was up gets its tab
            // once there is a dock to put it in.
            _context: cx.subscribe(&context, |this, _, _: &CodeEvent, cx| this.place_tabs(cx)),
        };
        for session in manager.sessions() {
            this.show(session, cx);
        }
        this
    }

    fn added(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let listed = services().dap().sessions();
        if let Some(session) = listed.into_iter().find(|session| session.id() == id) {
            self.show(session, cx);
        }
    }

    /// A session without a tab leaves with the manager's entry; one with a
    /// tab stays to be read until its tab is closed.
    fn removed(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let gone = |entry: &Entry, cx: &App| {
            let model = entry.model.read(cx);
            model.root().id() == id && model.tab.is_none()
        };
        let (leaving, staying): (Vec<Entry>, Vec<Entry>) =
            (self.entries.drain(..)).partition(|entry| gone(entry, cx));
        self.entries = staying;
        for entry in leaving {
            entry.model.update(cx, |model, cx| model.closed(cx));
        }
    }

    fn show(&mut self, session: Arc<Session>, cx: &mut Context<Self>) {
        let known = |entry: &Entry| entry.model.read(cx).root().id() == session.id();
        if self.entries.iter().any(known) {
            return;
        }
        let user = session.origin() == &Actor::User;
        // The same program debugged again, by a restart or a new start,
        // takes the tab its ended run left.
        let ended = self.entries.iter().find(|entry| {
            let root = entry.model.read(cx).root();
            root.state().is_terminated() && root.request() == session.request()
        });
        if let Some(entry) = ended {
            let model = entry.model.clone();
            model.update(cx, |model, cx| model.adopt(session, cx));
            if user {
                self.current = Some(model.entity_id());
                self.reveal(&model, cx);
            }
            return;
        }
        let checkout = checkout_of(&session, cx);
        let model = cx.new(|cx| Model::new(session, checkout, cx));
        let events = cx.subscribe(&model, |this, model, event: &ModelEvent, cx| match event {
            ModelEvent::Stopped => this.current = Some(model.entity_id()),
            ModelEvent::Restart => this.restart(&model, cx),
        });
        if user || self.current.is_none() {
            self.current = Some(model.entity_id());
        }
        self.entries.push(Entry {
            model,
            _events: events,
        });
        self.place_tabs(cx);
    }

    fn reveal(&self, model: &Entity<Model>, cx: &mut Context<Self>) {
        let Some(tab) = model.read(cx).tab.clone() else {
            return;
        };
        cx.defer(move |cx| {
            code_shell::in_window(cx, |window, cx| tab.reveal(window, cx));
        });
    }

    /// Opens the dock tab of every session that has none yet. An agent's
    /// session gets its tab without the dock being raised or focused.
    fn place_tabs(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<Entity<Model>> = (self.entries.iter())
            .filter(|entry| entry.model.read(cx).tab.is_none())
            .map(|entry| entry.model.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let this = cx.weak_entity();
        // After this update: the dock belongs to the shell, which may be
        // the one telling us of the change.
        cx.defer(move |cx| {
            for model in waiting {
                if model.read(cx).tab.is_some() {
                    continue;
                }
                if model.read(cx).checkout.project.is_empty() {
                    let checkout = checkout_of(model.read(cx).root(), cx);
                    model.update(cx, |model, _| model.checkout = checkout);
                }
                let reveal = model.read(cx).root().origin() == &Actor::User;
                let id = model.entity_id();
                let closed = this.clone();
                let opened = code_shell::in_window(cx, |window, cx| {
                    tab::open(
                        &model,
                        reveal,
                        Box::new(move |cx| {
                            let _ = closed.update(cx, |this, cx| this.tab_closed(id, cx));
                        }),
                        window,
                        cx,
                    )
                });
                if let Some(Ok(tab)) = opened {
                    model.update(cx, |model, cx| {
                        model.tab = Some(tab);
                        model.refresh(cx);
                    });
                }
            }
        });
    }

    /// Closing the tab ends the session: the adapter, the debuggee and the
    /// terminal it ran in.
    fn tab_closed(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(at) = (self.entries.iter()).position(|entry| entry.model.entity_id() == id) else {
            return;
        };
        let entry = self.entries.remove(at);
        let session = entry.model.read(cx).root().id();
        entry.model.update(cx, |model, cx| model.closed(cx));
        if self.current == Some(id) {
            self.current = None;
        }
        // Ending an adapter waits a moment for it to hang up.
        cx.background_executor()
            .spawn(async move { services().dap().remove(session) })
            .detach();
    }

    fn restart(&mut self, model: &Entity<Model>, cx: &mut Context<Self>) {
        let id = model.read(cx).root().id();
        self.current = Some(model.entity_id());
        let model = model.downgrade();
        let started = cx
            .background_executor()
            .spawn(async move { services().dap().restart(id).map(|_| ()) });
        cx.spawn(async move |_, cx| {
            if let Err(error) = started.await {
                let _ = model.update(cx, |model, cx| model.fail(&error.to_string(), cx));
            }
        })
        .detach();
    }

    fn current_model(&self, cx: &App) -> Option<Entity<Model>> {
        let find = |id: EntityId| {
            (self.entries.iter())
                .find(|entry| entry.model.entity_id() == id)
                .map(|entry| entry.model.clone())
        };
        let live = |model: &Entity<Model>| model.read(cx).live();
        let chosen = self.current.and_then(find);
        (chosen.clone().filter(live))
            .or_else(|| {
                let mut models = self.entries.iter().rev().map(|entry| entry.model.clone());
                models.find(live)
            })
            .or(chosen)
    }

    /// The models of the sessions shown, oldest first.
    #[cfg(test)]
    pub(super) fn models(&self) -> Vec<Entity<Model>> {
        (self.entries.iter())
            .map(|entry| entry.model.clone())
            .collect()
    }

    /// Makes `model` the session the keys act on and brings its tab forward.
    pub(super) fn focus(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let found = (self.entries.iter())
            .find(|entry| entry.model.read(cx).root().id() == session)
            .map(|entry| entry.model.clone());
        if let Some(model) = found {
            self.current = Some(model.entity_id());
            self.reveal(&model, cx);
        }
    }
}
