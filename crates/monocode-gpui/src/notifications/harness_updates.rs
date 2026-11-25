//! Native counterpart of HarnessUpdateNotice.tsx. The footer shows a small
//! chip while updates wait; its menu lists them (`shell/harness_footer.rs`).
use gpui::{Context, EventEmitter};
use monocode_core::{
    harness_updates::{self, Update},
    provider::Kind,
};
use std::collections::HashMap;

pub(crate) enum HarnessUpdatesEvent {
    Changed,
    Updated(Kind),
}
#[derive(Clone)]
enum RowState {
    Updating,
    Updated(String),
    Failed(String),
}
pub(crate) struct HarnessUpdates {
    updates: Vec<Update>,
    rows: HashMap<Kind, RowState>,
    checking: bool,
    generation: u64,
}
impl EventEmitter<HarnessUpdatesEvent> for HarnessUpdates {}
impl HarnessUpdates {
    pub(crate) fn new(hidden: Vec<String>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            updates: Vec::new(),
            rows: HashMap::new(),
            checking: false,
            generation: 0,
        };
        if harness_updates::claim_launch_check() {
            this.check_now(hidden, cx);
        }
        this
    }
    pub(crate) fn is_visible(&self) -> bool {
        !self.updates.is_empty()
    }
    fn busy(&self) -> bool {
        self.rows.values().any(|s| matches!(s, RowState::Updating))
    }
    /// Explicit refresh is permitted after the launch claim or a dismissal.
    pub(crate) fn check_now(&mut self, hidden: Vec<String>, cx: &mut Context<Self>) {
        if self.checking || self.busy() {
            return;
        }
        self.checking = true;
        self.generation += 1;
        let generation = self.generation;
        let work = cx
            .background_executor()
            .spawn(async move { harness_updates::find_updates(&hidden) });
        cx.spawn(async move |this, cx| {
            let updates = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.checking = false;
                this.updates = updates;
                this.rows.clear();
                cx.emit(HarnessUpdatesEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.generation += 1;
        self.checking = false;
        self.updates.clear();
        self.rows.clear();
        cx.emit(HarnessUpdatesEvent::Changed);
        cx.notify();
    }
    fn start(&mut self, targets: Vec<Update>, cx: &mut Context<Self>) {
        for update in targets {
            let kind = update.harness;
            if matches!(
                self.rows.get(&kind),
                Some(RowState::Updating | RowState::Updated(_))
            ) {
                continue;
            }
            self.rows.insert(kind, RowState::Updating);
            let work = cx
                .background_executor()
                .spawn(async move { harness_updates::run_update(&update) });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |this, cx| {
                    let state = match result {
                        Ok(version) => {
                            cx.emit(HarnessUpdatesEvent::Updated(kind));
                            RowState::Updated(version)
                        }
                        Err(error) => RowState::Failed(error),
                    };
                    this.rows.insert(kind, state);
                    cx.emit(HarnessUpdatesEvent::Changed);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
}
/// One harness row as the footer's menu shows it.
pub(crate) enum RowView {
    Pending,
    Updating,
    Updated(String),
    Failed(String),
}
impl HarnessUpdates {
    pub(crate) fn rows(&self) -> Vec<(Update, RowView)> {
        self.updates
            .iter()
            .map(|u| {
                let view = match self.rows.get(&u.harness) {
                    None => RowView::Pending,
                    Some(RowState::Updating) => RowView::Updating,
                    Some(RowState::Updated(v)) => RowView::Updated(v.clone()),
                    Some(RowState::Failed(e)) => RowView::Failed(e.clone()),
                };
                (u.clone(), view)
            })
            .collect()
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.busy()
    }
    fn pending(&self) -> Vec<Update> {
        self.updates
            .iter()
            .filter(|u| {
                !matches!(
                    self.rows.get(&u.harness),
                    Some(RowState::Updating | RowState::Updated(_))
                )
            })
            .cloned()
            .collect()
    }
    pub(crate) fn pending_count(&self) -> usize {
        self.pending().len()
    }
    /// Starts one harness's update, by its id.
    pub(crate) fn update_one(&mut self, id: &str, cx: &mut Context<Self>) {
        let targets: Vec<Update> = self
            .pending()
            .into_iter()
            .filter(|u| u.harness.id() == id)
            .collect();
        self.start(targets, cx);
    }
    pub(crate) fn update_all(&mut self, cx: &mut Context<Self>) {
        let targets = self.pending();
        self.start(targets, cx);
    }
}
