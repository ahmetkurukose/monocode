//! Where opened things go (docs/native/tab-groups.md). A scope (project +
//! worktree) keeps one title tab; whatever opens there joins one of its tab
//! groups, and whatever is open already is shown instead of opened twice.
use super::*;
use crate::review::{ReviewState, ReviewTab};
use crate::workspace::{BrowserPane, TerminalPane};
use monocode_core::files::path_key;

/// The lowest browser pane id handed out in this run, in any window.
static LOWEST_BROWSER_PANE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// A new browser pane's id: `next`, the workspace's next free pane id,
/// unless a page that closed in this run had it or one below. Agents hold
/// page ids, which are pane ids (K21): a closed page's id must not reach
/// another page.
fn browser_pane_id(next: i64) -> i64 {
    use std::sync::atomic::Ordering;
    let pane = next.min(LOWEST_BROWSER_PANE.load(Ordering::Relaxed) - 1);
    LOWEST_BROWSER_PANE.fetch_min(pane, Ordering::Relaxed);
    pane
}

/// How the router places a tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenIn {
    /// A member of the group to open in (a strip's `+`, a drop, a reopen).
    /// It wins over every other rule while that group is in the scope.
    pub group: Option<i64>,
    /// A preview tab, which the group's next preview replaces in place.
    pub preview: bool,
}

impl OpenIn {
    pub(super) const PREVIEW: Self = Self {
        group: None,
        preview: true,
    };
    pub(super) fn preview(preview: bool) -> Self {
        Self {
            group: None,
            preview,
        }
    }
}

/// Where a tab the scope does not hold yet lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// Stacked in the group of this visible tab.
    Group(i64),
    /// A new group split to the right of this visible tab, so a file never
    /// covers the chat or terminal in front.
    Beside(i64),
    /// The scope has no title tab yet: it becomes one.
    NewTab,
}

/// What Reopen Closed Tab brings back.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ClosedTab {
    Session(i64),
    File {
        tab: ReviewTab,
    },
    Terminal {
        owner: i64,
        cwd: String,
    },
    Browser {
        owner: i64,
        url: String,
    },
    /// An app view, Search or a Mono.
    View(TitleTab),
}

#[derive(Clone, Debug)]
pub(super) struct ClosedEntry {
    pub tab: ClosedTab,
    /// Its group mates when it closed: the first still open is where it
    /// reopens.
    near: Vec<i64>,
}

/// Each scope remembers this many closed tabs, newest last.
const CLOSED_TABS: usize = 30;

impl Shell {
    /// The scope key (`tab_scope`'s format) of a project at the checkout it
    /// shows.
    pub(super) fn scope_key(&self, project: &str) -> String {
        let worktree = self
            .worktree_focus
            .get(project)
            .map_or(project, |focus| focus.path.as_str());
        format!("{}\0{}", path_key(project), path_key(worktree))
    }

    /// The scope of a session title tab; `None` when no session scopes it.
    pub(super) fn title_tab_scope(&self, tab: i64, cx: &App) -> Option<String> {
        let pane = self.workspace.scope_pane(tab);
        // A title tab of view panes alone keeps the scope they opened in.
        if let Some(scope) = self.view_pane_scope(pane) {
            return Some(scope);
        }
        let session = self.workspace.session_for_pane(pane);
        if !self.session_summaries.contains_key(&session) {
            return None;
        }
        let project = &self.session_metadata(session, cx).summary.cwd;
        let worktree = (self.workspace.tab_workspaces.get(&tab))
            .map_or_else(|| self.session_working_copy(pane, cx), String::as_str);
        Some(format!("{}\0{}", path_key(project), path_key(worktree)))
    }

    /// The scope a pane opens in: its session's project, at the checkout
    /// that project shows.
    pub(super) fn pane_scope(&self, id: i64, cx: &App) -> Option<String> {
        if let Some(scope) = self.view_pane_scope(id) {
            return Some(scope);
        }
        let session = self.workspace.session_for_pane(id);
        self.session_summaries
            .contains_key(&session)
            .then(|| self.scope_key(&self.session_metadata(session, cx).summary.cwd))
    }

    /// The scope's one title tab: the active tab when it is in the scope,
    /// else the one the scope showed last, else the first.
    pub(super) fn scope_title_tab(&self, scope: &str, cx: &App) -> Option<i64> {
        let tabs: Vec<i64> = (self.workspace.tabs.iter().copied())
            .filter(|tab| self.title_tab_scope(*tab, cx).as_deref() == Some(scope))
            .collect();
        let active = self.workspace.active_tab().filter(|tab| tabs.contains(tab));
        let last = match self.workspace.scope_focus.get(scope) {
            Some(TitleTab::Session(tab)) if tabs.contains(tab) => Some(*tab),
            _ => None,
        };
        active.or(last).or_else(|| tabs.first().copied())
    }

    /// The group a new tab of `scope` joins. Editors (files, diffs, commits)
    /// go to a group showing an editor, else one holding one, else a new
    /// group right of the focused one.
    fn spot(&self, scope: &str, editor: bool, how: OpenIn, cx: &App) -> Spot {
        let Some(tab) = self.scope_title_tab(scope, cx) else {
            return Spot::NewTab;
        };
        let in_tab = |id: &i64| self.workspace.tab_of(*id) == Some(tab);
        if let Some(visible) = (how.group.filter(in_tab)).and_then(|m| self.workspace.visible_of(m))
        {
            return Spot::Group(visible);
        }
        let focused =
            (self.workspace.active.filter(in_tab)).unwrap_or_else(|| self.workspace.tab_focus(tab));
        let focused = self.workspace.visible_of(focused).unwrap_or(focused);
        if !editor {
            return Spot::Group(focused);
        }
        let file = |id: &i64| self.workspace.review_owners.contains_key(id);
        if file(&focused) {
            return Spot::Group(focused);
        }
        let leaves = self.workspace.layout(tab).leaf_ids();
        (leaves.iter().copied().find(file))
            .or_else(|| {
                (leaves.iter().copied())
                    .find(|leaf| self.workspace.group_tabs(*leaf).iter().any(file))
            })
            .map_or(Spot::Beside(focused), Spot::Group)
    }

    /// Put `id`, which no tab holds, where the router says. False when its
    /// scope has no title tab yet, for the caller to open one.
    pub(super) fn place_new_tab(&mut self, id: i64, how: OpenIn, cx: &mut Context<Self>) -> bool {
        if self.workspace.tab_of(id).is_some() {
            return true;
        }
        let Some(scope) = self.pane_scope(id, cx) else {
            return false;
        };
        let editor = self.workspace.review_owners.contains_key(&id);
        match self.spot(&scope, editor, how, cx) {
            Spot::NewTab => return false,
            Spot::Group(visible) => {
                self.workspace.add_to_group(id, visible, None, true);
            }
            Spot::Beside(visible) => {
                let before = self.active_leaves();
                self.workspace.place(id, visible, Edge::Right);
                self.begin_pane_motion(before, cx);
            }
        }
        if how.preview {
            self.workspace.set_preview(id, Some(id));
        }
        true
    }

    /// `place_new_tab`, or a title tab of its own for the scope's first.
    pub(super) fn place_or_open(&mut self, id: i64, how: OpenIn, cx: &mut Context<Self>) {
        if !self.place_new_tab(id, how, cx) {
            self.workspace.open(id);
            if how.preview {
                self.workspace.set_preview(id, Some(id));
            }
        }
    }

    /// The one way a session or pane opens as a tab: an open one is shown
    /// and flashed (opening a preview for real keeps it), a new one joins a
    /// group of its scope's title tab, or starts that title tab.
    pub(super) fn open_in_workspace(
        &mut self,
        id: i64,
        how: OpenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.tab_of(id).is_some() {
            if !how.preview {
                self.keep_preview(id, cx);
            }
            self.flash_tab(id);
        } else {
            // An empty new chat in front gives way to the opened session.
            self.replace_blank_pane(id, cx);
            self.place_new_tab(id, how, cx);
        }
        self.open(id, window, cx);
    }

    /// Stage 3's strip animates the tab an open found already open.
    pub(super) fn flash_tab(&mut self, id: i64) {
        self.tab_flash = Some((id, std::time::Instant::now()));
    }

    /// A preview opened for real, edited or double-clicked stays open.
    pub(super) fn keep_preview(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.workspace.preview_of_group(id) == Some(id) {
            self.workspace.set_preview(id, None);
            if let Some(review) = self.reviews.get(&id) {
                review.update(cx, |r, _| r.keep_preview());
            }
        }
    }

    /// The chat a file belongs to: the focused one, else one of the
    /// project's.
    fn file_owner(&self) -> Option<i64> {
        self.terminal_owner()
            .or_else(|| self.project_session_for_terminal())
    }

    /// The file pane of `scope` showing `tab`. A pane closed with its title
    /// tab still counts: it keeps its unsaved edits for reopening.
    fn review_pane_showing(&self, tab: &ReviewTab, scope: &str, cx: &App) -> Option<i64> {
        let mut panes: Vec<i64> = self.workspace.review_owners.keys().copied().collect();
        // Open ones first, then the newest (pane ids count down).
        panes.sort_unstable_by_key(|pane| (self.workspace.tab_of(*pane).is_none(), *pane));
        panes.into_iter().find(|pane| {
            let shows = match self.reviews.get(pane) {
                Some(review) => (review.read(cx).state.tabs.iter()).any(|t| t.same_tab(tab)),
                None => (self.workspace.reviews.get(pane))
                    .is_some_and(|r| r.tabs.iter().any(|t| t.same_tab(tab))),
            };
            shows && self.pane_scope(*pane, cx).as_deref() == Some(scope)
        })
    }

    /// Files, diffs, commits and session changes: each one is a file pane
    /// of its own, found again anywhere in the scope rather than opened
    /// twice. A preview takes over the group's previous preview in place.
    pub(super) fn open_review_tab(
        &mut self,
        tab: ReviewTab,
        how: OpenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let ReviewTab::File { cwd, path } = &tab {
            let recent = self.workspace.recent_files.entry(cwd.clone()).or_default();
            recent.retain(|old| old != path);
            recent.insert(0, path.clone());
            recent.truncate(30);
        }
        let Some(owner) = self.file_owner() else {
            // No chat to own it yet: the project gets a blank one first.
            if self.empty_workspace_showing(cx) {
                self.with_pane_owner("a file", cx, move |this, window, cx| {
                    this.open_review_tab(tab, how, window, cx)
                });
            }
            return;
        };
        let Some(scope) = self.pane_scope(owner, cx) else {
            return;
        };
        if let Some(pane) = self.review_pane_showing(&tab, &scope, cx) {
            if let Some(review) = self.review_for(pane, cx) {
                review.update(cx, |r, cx| r.open_file(tab, !how.preview, cx));
            }
            if !how.preview {
                self.keep_preview(pane, cx);
            }
            if self.workspace.tab_of(pane).is_some() {
                self.flash_tab(pane);
            } else {
                self.place_or_open(pane, how, cx);
            }
            self.open_file_pane(pane, window, cx);
            return;
        }
        if how.preview
            && let Spot::Group(visible) = self.spot(&scope, true, how, cx)
            && let Some(preview) = self.workspace.preview_of_group(visible)
            && let Some(review) = self.review_for(preview, cx)
            && !review.read(cx).unsaved_files(cx)
        {
            review.update(cx, |r, cx| r.replace_with(tab, cx));
            self.open_file_pane(preview, window, cx);
            return;
        }
        let pane = self.workspace.next_pane_id();
        let state = ReviewState {
            chat_hidden: true,
            ..Default::default()
        };
        let review = self.make_review(pane, state, cx);
        review.update(cx, |r, cx| {
            r.open_file(tab, !how.preview, cx);
            r.set_visible(true, cx);
        });
        self.workspace.review_owners.insert(pane, owner);
        (self.workspace.reviews).insert(pane, review.read(cx).state.clone());
        self.place_or_open(pane, how, cx);
        self.open_file_pane(pane, window, cx);
        self.fit_sidebar(cx);
        cx.notify();
    }

    /// Older workspaces (docs/native/tab-groups.md): files attached beside
    /// a chat become file panes stacked right after it, and a scope's extra
    /// title tabs fold into its focused group. Running it again is a no-op.
    pub(super) fn migrate_tab_groups(&mut self, cx: &App) {
        let mut hosts: Vec<i64> = (self.workspace.reviews.keys().copied())
            .filter(|id| !self.workspace.review_owners.contains_key(id))
            .collect();
        hosts.sort_unstable();
        for host in hosts {
            let Some(state) = self.workspace.reviews.remove(&host) else {
                continue;
            };
            self.reviews.remove(&host);
            if self.workspace.tab_of(host).is_none() {
                continue;
            }
            let owner = self.workspace.session_for_pane(host);
            let at = (self.workspace.group_tabs(host).iter())
                .position(|id| *id == host)
                .map_or(0, |ix| ix + 1);
            for (ix, tab) in state.tabs.into_iter().enumerate() {
                let pane = self.workspace.next_pane_id();
                self.workspace.review_owners.insert(pane, owner);
                self.workspace.reviews.insert(
                    pane,
                    ReviewState {
                        tabs: vec![tab],
                        active: 0,
                        ratio: state.ratio,
                        chat_hidden: true,
                    },
                );
                self.workspace
                    .add_to_group(pane, host, Some(at + ix), false);
            }
        }
        self.merge_scope_tabs(cx);
    }

    /// One title tab per scope: any other session title tab of a scope
    /// folds into the focused group of the scope's main one, the tab with
    /// the most panes (then the one in front). What was in front stays in
    /// front. True when a tab folded.
    pub(super) fn merge_scope_tabs(&mut self, cx: &App) -> bool {
        if self.workspace.tabs.len() < 2 {
            return false;
        }
        let active = self.workspace.active;
        let mut scopes: Vec<(String, Vec<i64>)> = Vec::new();
        for tab in self.workspace.tabs.clone() {
            let Some(scope) = self.title_tab_scope(tab, cx) else {
                continue;
            };
            match scopes.iter_mut().find(|(s, _)| *s == scope) {
                Some((_, tabs)) => tabs.push(tab),
                None => scopes.push((scope, vec![tab])),
            }
        }
        let mut merged = false;
        for (scope, tabs) in scopes.into_iter().filter(|(_, tabs)| tabs.len() > 1) {
            let front = self.scope_title_tab(&scope, cx);
            let Some(target) = (tabs.iter().copied()).max_by_key(|tab| {
                let panes = self.workspace.layout(*tab).leaf_ids().len();
                // The first of equals wins: `max_by_key` keeps the last.
                let first = tabs.len() - tabs.iter().position(|t| t == tab).unwrap_or(0);
                (panes, front == Some(*tab), first)
            }) else {
                continue;
            };
            let focus = self.workspace.tab_focus(target);
            let into = self.workspace.visible_of(focus).unwrap_or(focus);
            for tab in tabs.into_iter().filter(|tab| *tab != target) {
                merged |= self.workspace.merge_tab_into(tab, into);
            }
            if let Some(tab) = self.workspace.tab_of(into) {
                (self.workspace.scope_focus)
                    .entry(scope)
                    .or_insert(TitleTab::Session(tab));
            }
        }
        if merged
            && let Some(active) = active
            && self.workspace.visible_of(active) != Some(active)
        {
            self.workspace.show(active);
        }
        merged
    }

    /// What reopening `id` would bring back, when it can come back.
    fn closed_tab(&self, id: i64, cx: &App) -> Option<ClosedTab> {
        if let Some(view) = self.view_of(id) {
            return Some(ClosedTab::View(view.clone()));
        }
        if self.workspace.review_owners.contains_key(&id) {
            let tab = match self.reviews.get(&id) {
                Some(review) => {
                    let state = &review.read(cx).state;
                    state.tabs.get(state.active)?.clone()
                }
                None => {
                    let state = self.workspace.reviews.get(&id)?;
                    state.tabs.get(state.active)?.clone()
                }
            };
            return Some(ClosedTab::File { tab });
        }
        if let Some(pane) = self.workspace.terminal_panes.get(&id) {
            let cwd = (self.terminal_pane_views.get(&id))
                .map_or_else(|| pane.cwd.clone(), |t| t.read(cx).cwd.clone());
            return Some(ClosedTab::Terminal {
                owner: pane.owner,
                cwd,
            });
        }
        if let Some(page) = self.workspace.browser_panes.get(&id) {
            return Some(ClosedTab::Browser {
                owner: page.owner,
                url: page.url.clone(),
            });
        }
        (id > 0 && self.session_summaries.contains_key(&id)).then_some(ClosedTab::Session(id))
    }

    /// Remember an open tab that is about to close, for Reopen Closed Tab.
    pub(super) fn remember_closed_tab(&mut self, id: i64, cx: &App) {
        let (Some(tab), Some(scope)) = (self.closed_tab(id, cx), self.pane_scope(id, cx)) else {
            return;
        };
        let near = (self.workspace.group_tabs(id).into_iter())
            .filter(|member| *member != id)
            .collect();
        let stack = self.closed_tabs.entry(scope).or_default();
        stack.retain(|entry| entry.tab != tab);
        stack.push(ClosedEntry { tab, near });
        if stack.len() > CLOSED_TABS {
            stack.remove(0);
        }
    }

    /// ⌘⇧T: the scope's last closed tab, in the group it left when that
    /// group is still there, else where the router puts it.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = self.tab_scope();
        while let Some(entry) = self.closed_tabs.get_mut(&scope).and_then(Vec::pop) {
            let how = OpenIn {
                group: (entry.near.iter().copied()).find(|m| self.workspace.tab_of(*m).is_some()),
                preview: false,
            };
            let known = |this: &Self, id: i64| {
                this.session_summaries.get(&id).is_some_and(|s| !s.archived)
                    && !this.deleting.contains(&id)
            };
            match entry.tab {
                ClosedTab::Session(id) => {
                    if !known(self, id) {
                        continue;
                    }
                    self.open_in_workspace(id, how, window, cx);
                }
                ClosedTab::File { tab } => self.open_review_tab(tab, how, window, cx),
                ClosedTab::Terminal { owner, cwd } => {
                    if !known(self, owner) || self.refuse_terminal_in_removed_worktree(&cwd, cx) {
                        continue;
                    }
                    let terminal = self.spawn_terminal(cwd.clone(), cx);
                    let pane = self.workspace.next_pane_id();
                    (self.workspace.terminal_panes).insert(pane, TerminalPane { owner, cwd });
                    self.terminal_pane_views.insert(pane, terminal);
                    self.place_or_open(pane, how, cx);
                    self.open_terminal_pane(pane, window, cx);
                }
                ClosedTab::View(view) => {
                    let view = match view {
                        TitleTab::Mono { id, .. } => {
                            // Through `Monos`, so its page opens with it.
                            self.open_mono_tab(&id, window, cx);
                            return;
                        }
                        TitleTab::App(app) => {
                            self.ready_app_view(app, window, cx);
                            TitleTab::App(app)
                        }
                        view => view,
                    };
                    self.open_view_tab(view, how, window, cx);
                }
                ClosedTab::Browser { owner, url } => {
                    if !known(self, owner) {
                        continue;
                    }
                    let pane = self.create_browser_pane(owner, url);
                    self.place_or_open(pane, how, cx);
                    self.fit_sidebar(cx);
                    self.open_browser_pane(pane, window, cx);
                }
            }
            return;
        }
    }

    /// A new browser page, not yet in any tab. Its page starts the first
    /// time its pane draws (`browser_page`); an empty `url` focuses the
    /// address field.
    pub(super) fn create_browser_pane(&mut self, owner: i64, url: String) -> i64 {
        let pane = browser_pane_id(self.workspace.next_pane_id());
        self.workspace.browser_panes.insert(
            pane,
            BrowserPane {
                owner,
                url,
                ..Default::default()
            },
        );
        pane
    }

    /// The tabs of the focused group, when a session title tab is in front
    /// and that group has more than one. Tab keys walk these, else the
    /// title strip as before.
    pub(super) fn focused_group_tabs(&self, cx: &App) -> Option<Vec<i64>> {
        if !matches!(self.active_title_tab(cx), Some(TitleTab::Session(_))) {
            return None;
        }
        let tabs = self.workspace.group_tabs(self.workspace.active?);
        (tabs.len() > 1).then_some(tabs)
    }

    /// Close several tabs of one group: one question for their sessions, one
    /// for their running terminals, and each dirty file still asks.
    pub(super) fn close_group_tabs(
        &mut self,
        ids: Vec<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let asked = ids.clone();
        self.ask_close_sessions(
            &asked,
            window,
            cx,
            Box::new(move |this, archive, window, cx| {
                let panes = ids.clone();
                this.confirm_close_terminal_panes(&panes, window, cx, move |this, window, cx| {
                    for id in ids {
                        if this.workspace.tab_of(id).is_none() {
                            continue;
                        }
                        match this.review_for(id, cx) {
                            Some(review) if this.workspace.review_owners.contains_key(&id) => {
                                review.update(cx, |r, cx| r.close_active(cx));
                            }
                            _ => this.finish_close_pane(id, window, cx),
                        }
                    }
                    this.archive_closed_sessions(&archive, cx);
                });
            }),
        );
    }
}

#[cfg(test)]
#[path = "tab_router_tests.rs"]
mod tests;
