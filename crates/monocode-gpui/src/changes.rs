//! The Changes panel: the header with branch actions, the commit box, sync
//! and pull-request actions, and the changed files as a tree with a checkbox
//! each. The files, the message and the commit are the commit form the Commit
//! window shows too ([`crate::commit_form`]). Every Git call runs off-thread,
//! one mutation at a time per checkout.
use crate::{
    branch_popup::{BranchPopup, PopupEvent},
    commit_dialog::{CommitDialog, CommitDialogEvent},
    commit_form::{CommitForm, CommitFormEvent, Surface},
    git_status::{GitChanged, ProjectGit},
    popover::{
        Align, Anchor, AnchorSlot, Menu, MenuAction, MenuEvent, MenuItem, MenuStyle, Side,
        menu_popover, popover,
    },
    right_panel::*,
    stash_dialog::{Intent as StashIntent, StashDialog, StashDialogEvent},
    theme::*,
    update_dialog::{Source, UpdateDialog, UpdateDialogEvent},
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, FontWeight, Hsla, PromptLevel,
    Role, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use monocode_core::{
    agent::{AccessMode, Settings},
    provider::{self, Event, Kind, Request},
    scm::{self, DiffIndex, Pr, UpdateMethod},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(crate) enum ChangesEvent {
    /// A dialog or the branch switcher opened or closed: the shell redraws.
    Overlays,
    /// A file's review, or all of one side's when `path` is None.
    OpenDiff {
        cwd: String,
        path: Option<String>,
        staged: Option<bool>,
    },
}

/// Where the branch switcher opens.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum BranchPlace {
    /// A popover under the branch name in this panel's header.
    #[default]
    Header,
    /// A centred window, for the menu bar.
    Modal,
    /// A popover above the branch button in the footer.
    Footer,
}

fn sync_status_label(index: &DiffIndex) -> String {
    let plural = |n: i64| if n == 1 { "" } else { "s" };
    if index.ahead > 0 && index.behind > 0 {
        format!(
            "Diverged from {}",
            index.upstream.as_deref().unwrap_or("upstream")
        )
    } else if index.ahead > 0 {
        format!("{} unpushed commit{}", index.ahead, plural(index.ahead))
    } else if index.behind > 0 {
        format!("{} incoming commit{}", index.behind, plural(index.behind))
    } else {
        "No files".into()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MenuKind {
    Branch,
    Commit,
}
struct OpenMenu {
    kind: MenuKind,
    menu: Entity<Menu>,
    _events: Subscription,
}

pub(crate) struct Changes {
    git: Entity<ProjectGit>,
    cwd: String,
    enabled: bool,
    /// The files, the message and the commit, shared with the Commit window.
    form: Entity<CommitForm>,
    /// Takes ⌘⌥Z and ⌘⌥A for the picked file once the list is clicked.
    list_focus: FocusHandle,
    busy: Option<String>,
    status: Option<String>,
    status_clear: Option<Task<()>>,
    pr: Option<Pr>,
    pr_branch: Option<String>,
    pr_request: u64,
    menu: Option<OpenMenu>,
    branch_anchor: AnchorSlot,
    commit_anchor: AnchorSlot,
    commit_dialog: Option<(Entity<CommitDialog>, Subscription)>,
    update_dialog: Option<(Entity<UpdateDialog>, Subscription)>,
    stash_dialog: Option<(Entity<StashDialog>, Subscription)>,
    branch_picker: Option<(Entity<BranchPopup>, Subscription)>,
    /// The switcher opened from the menu bar: a centred window, not a popover.
    branch_place: BranchPlace,
    /// Why branch-changing Git actions are off: an agent is working in this checkout.
    blocked: Option<String>,
    pub(crate) footer_anchor: AnchorSlot,
    /// The shell draws this panel's windows (the right panel's), so they
    /// show from the menu bar; other hosts leave them to the panel.
    pub(crate) overlays_in_shell: bool,
    switch_anchor: AnchorSlot,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<ChangesEvent> for Changes {}

impl Changes {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let form = cx.new(|cx| CommitForm::new(git.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&git, |this, _, cx| this.index_changed(cx)),
            cx.subscribe(&git, |this, _, _: &GitChanged, cx| this.index_changed(cx)),
            cx.observe(&form, |_, _, cx| cx.notify()),
            cx.subscribe(&form, Self::form_event),
        ];
        let mut this = Self {
            git,
            cwd: String::new(),
            enabled: false,
            form,
            list_focus: cx.focus_handle(),
            busy: None,
            status: None,
            status_clear: None,
            pr: None,
            pr_branch: None,
            pr_request: 0,
            menu: None,
            branch_anchor: AnchorSlot::default(),
            commit_anchor: AnchorSlot::default(),
            commit_dialog: None,
            update_dialog: None,
            stash_dialog: None,
            branch_picker: None,
            branch_place: BranchPlace::Header,
            blocked: None,
            footer_anchor: AnchorSlot::default(),
            overlays_in_shell: false,
            switch_anchor: AnchorSlot::default(),
            _subscriptions: subscriptions,
        };
        this.index_changed(cx);
        this
    }
    /// The provider of the conversation in this checkout: it writes commit
    /// messages and pull requests.
    pub(crate) fn set_text_provider(&mut self, provider: Kind, cx: &mut Context<Self>) {
        self.form
            .update(cx, |form, _| form.text_provider = provider);
    }
    fn form_event(
        &mut self,
        _: Entity<CommitForm>,
        event: &CommitFormEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommitFormEvent::Selected(Surface::Panel) => {
                let form = self.form.read(cx);
                let Some(file) = form.selected_file() else {
                    return;
                };
                // A deleted file has nothing left to open.
                if file.status == "deleted" {
                    return;
                }
                cx.emit(ChangesEvent::OpenDiff {
                    cwd: self.cwd.clone(),
                    path: Some(file.path.clone()),
                    staged: None,
                });
            }
            CommitFormEvent::Committed { pushed, error } => {
                self.close_commit_dialog(cx);
                if !error {
                    self.set_status(
                        if *pushed {
                            "Committed and pushed"
                        } else {
                            "Committed"
                        },
                        cx,
                    );
                }
                self.load_pr(cx);
                cx.emit(ChangesEvent::Overlays);
            }
            _ => {}
        }
        cx.notify();
    }
    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            let request = self.pr_request;
            self.index_changed(cx);
            if self.pr_request == request {
                self.load_pr(cx);
            }
        }
    }
    pub(crate) fn resume(&mut self, cx: &mut Context<Self>) {
        if self.enabled {
            self.load_pr(cx);
        }
    }
    fn index(&self, cx: &App) -> Option<DiffIndex> {
        self.git.read(cx).index.clone()
    }
    fn index_changed(&mut self, cx: &mut Context<Self>) {
        let git = self.git.read(cx);
        let project = git.project().to_owned();
        if project != self.cwd {
            // The form resets itself for the new checkout.
            self.cwd = project;
            self.pr = None;
            self.pr_branch = None;
            self.form.update(cx, |form, _| form.has_open_pr = false);
        }
        let branch = self
            .git
            .read(cx)
            .index
            .as_ref()
            .and_then(|i| i.branch.clone());
        if self.enabled && branch != self.pr_branch {
            self.pr_branch = branch;
            self.load_pr(cx);
        }
        cx.notify();
    }
    fn load_pr(&mut self, cx: &mut Context<Self>) {
        self.pr_request += 1;
        let request = self.pr_request;
        let branch = self
            .git
            .read(cx)
            .index
            .as_ref()
            .and_then(|i| i.branch.clone());
        if self.cwd.is_empty() || self.cwd == "~" || branch.is_none() {
            self.pr = None;
            self.form.update(cx, |form, _| form.has_open_pr = false);
            return;
        }
        let path = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::pr_status(&path) });
        cx.spawn(async move |this, cx| {
            let pr = work.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                if this.pr_request == request {
                    let open = pr.as_ref().is_some_and(|p| p.state == "open");
                    this.form.update(cx, |form, _| form.has_open_pr = open);
                    this.pr = pr;
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn set_status(&mut self, text: &str, cx: &mut Context<Self>) {
        self.status = Some(text.into());
        self.status_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.status = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn mutated(&mut self, cx: &mut Context<Self>) {
        self.git.update(cx, |git, cx| git.changed(cx));
    }
    /// `window.alert`.
    fn fail(error: String, window: &mut Window, cx: &mut App) {
        drop(window.prompt(PromptLevel::Critical, &error, None, &["OK"], cx));
    }
    /// confirmNative: Tauri's `ask` with the OK label it was given.
    fn confirm(
        message: String,
        ok: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::Task<bool> {
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            None,
            &[
                gpui::PromptButton::ok(ok),
                gpui::PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.background_executor()
            .spawn(async move { matches!(answer.await, Ok(0)) })
    }
    /// Runs `work` against the checkout with `busy` held, then rereads.
    fn run_git(
        &mut self,
        busy: String,
        work: impl FnOnce(PathBuf) -> Result<(), String> + Send + 'static,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || self.form.read(cx).busy.is_some() {
            return;
        }
        self.busy = Some(busy);
        cx.notify();
        let path = PathBuf::from(&self.cwd);
        let task = cx.background_executor().spawn(async move { work(path) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.mutated(cx);
                match result {
                    Ok(()) => then(this, window, cx),
                    Err(error) => Self::fail(error, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// IntelliJ's Commit window: the same form as this panel, with each
    /// file's diff.
    pub(crate) fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit_dialog.is_some() || self.cwd.is_empty() {
            return;
        }
        // The window may open from a view that never showed this panel.
        self.git.update(cx, |git, cx| git.changed(cx));
        self.menu = None;
        let form = self.form.clone();
        form.update(cx, |form, cx| {
            form.dialog_open = true;
            cx.notify();
        });
        let dialog = cx.new(|cx| CommitDialog::new(form, window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, event: &CommitDialogEvent, cx| {
            let CommitDialogEvent::Close = event;
            this.close_commit_dialog(cx);
            cx.emit(ChangesEvent::Overlays);
            cx.notify();
        });
        self.commit_dialog = Some((dialog, subscription));
        cx.emit(ChangesEvent::Overlays);
        cx.notify();
    }
    /// The window went away, however it closed: the panel takes the message
    /// box back.
    fn close_commit_dialog(&mut self, cx: &mut Context<Self>) {
        self.commit_dialog = None;
        self.form.update(cx, |form, cx| {
            form.dialog_open = false;
            cx.notify();
        });
    }

    /// Fetch and merge (or rebase onto) the upstream, then show what came in.
    pub(crate) fn open_update_dialog(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_dialog.is_some() || self.busy.is_some() || self.cwd.is_empty() {
            return;
        }
        // Pushing leaves the working tree alone, so only the others wait on agents.
        let what = match &source {
            Source::Update(_) => Some("update the project"),
            Source::Merge(_) => Some("merge"),
            Source::RebaseOnto(_) => Some("rebase"),
            Source::Push { .. } => None,
        };
        if let Some(what) = what
            && self.refuse_while_agent_works(what, window, cx)
        {
            return;
        }
        let git = self.git.clone();
        let cwd = self.cwd.clone();
        let dialog = cx.new(|cx| UpdateDialog::new(git, cwd, source, window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, event: &UpdateDialogEvent, cx| {
            this.update_dialog = None;
            if matches!(event, UpdateDialogEvent::Pushed) {
                this.set_status("Pushed", cx);
            }
            this.mutated(cx);
            cx.emit(ChangesEvent::Overlays);
            cx.notify();
        });
        self.update_dialog = Some((dialog, subscription));
        cx.emit(ChangesEvent::Overlays);
        cx.notify();
    }

    /// Stash Changes… and Unstash Changes…: one window to shelve the working
    /// tree and to manage what is shelved.
    pub(crate) fn open_stash_dialog(
        &mut self,
        intent: StashIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stash_dialog.is_some() || self.busy.is_some() || self.cwd.is_empty() {
            return;
        }
        if self.refuse_while_agent_works("stash or unstash", window, cx) {
            return;
        }
        let git = self.git.clone();
        let cwd = self.cwd.clone();
        let dialog = cx.new(|cx| StashDialog::new(git, cwd, intent, window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, event: &StashDialogEvent, cx| {
            match event {
                StashDialogEvent::Changed(what) => {
                    this.mutated(cx);
                    this.set_status(what, cx);
                }
                StashDialogEvent::Close => {
                    this.stash_dialog = None;
                    this.mutated(cx);
                    cx.emit(ChangesEvent::Overlays);
                }
            }
            cx.notify();
        });
        self.stash_dialog = Some((dialog, subscription));
        cx.emit(ChangesEvent::Overlays);
        cx.notify();
    }

    pub(crate) fn footer_picker_open(&self) -> bool {
        self.branch_picker.is_some() && self.branch_place == BranchPlace::Footer
    }
    /// Set by the shell: the title of an agent working in this checkout, if any.
    pub(crate) fn set_blocked(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        if self.blocked != reason {
            self.blocked = reason;
            cx.notify();
        }
    }
    /// Refuses actions that rewrite the working tree while an agent is working
    /// in it. Says why, and returns whether it refused.
    pub(crate) fn refuse_while_agent_works(
        &self,
        what: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(agent) = &self.blocked else {
            return false;
        };
        let message = format!(
            "Can't {what} while \"{agent}\" is working in this checkout. Wait for it to finish or stop it first."
        );
        drop(window.prompt(PromptLevel::Info, &message, None, &["OK"], cx));
        true
    }

    /// The branch switcher: search, check out, create, or merge into this one.
    pub(crate) fn toggle_branch_picker(
        &mut self,
        place: BranchPlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.branch_picker.take().is_some() {
            cx.emit(ChangesEvent::Overlays);
            cx.notify();
            return;
        }
        if self.busy.is_some() || self.cwd.is_empty() {
            return;
        }
        if self.refuse_while_agent_works("switch branches", window, cx) {
            return;
        }
        let cwd = PathBuf::from(&self.cwd);
        let picker = cx.new(|cx| BranchPopup::new(cwd, cx));
        let subscription = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &PopupEvent, window, cx| {
                this.branch_picker = None;
                match event {
                    PopupEvent::Close => {}
                    PopupEvent::Changed(what) => {
                        this.mutated(cx);
                        this.set_status(what, cx);
                    }
                    PopupEvent::Merge(branch) => this.merge_branch(branch.clone(), window, cx),
                    PopupEvent::Rebase(branch) => this.rebase_branch(branch.clone(), window, cx),
                    PopupEvent::Update => {
                        this.open_update_dialog(Source::Update(UpdateMethod::Merge), window, cx)
                    }
                    PopupEvent::Commit => this.open_commit_dialog(window, cx),
                    PopupEvent::Push => this.push(window, cx),
                    PopupEvent::Fetch => this.fetch(window, cx),
                }
                cx.emit(ChangesEvent::Overlays);
                cx.notify();
            },
        );
        window.focus(&picker.read(cx).focus(cx), cx);
        self.branch_place = place;
        self.branch_picker = Some((picker, subscription));
        cx.emit(ChangesEvent::Overlays);
        cx.notify();
    }

    /// Rebase the current branch onto `branch`, after a confirmation.
    fn rebase_branch(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_while_agent_works("rebase", window, cx) {
            return;
        }
        let current = self.index(cx).and_then(|i| i.branch);
        let message = match current {
            Some(current) => format!("Rebase \"{current}\" onto \"{branch}\"?"),
            None => format!("Rebase onto \"{branch}\"?"),
        };
        let ask = Self::confirm(message, "Rebase", window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if ask.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_update_dialog(Source::RebaseOnto(branch), window, cx)
                });
            }
        })
        .detach();
    }

    /// Merge `branch` into the current branch, after a confirmation.
    fn merge_branch(&mut self, branch: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_while_agent_works("merge", window, cx) {
            return;
        }
        let current = self.index(cx).and_then(|i| i.branch);
        let message = match current {
            Some(current) => format!("Merge \"{branch}\" into \"{current}\"?"),
            None => format!("Merge \"{branch}\" into the current checkout?"),
        };
        let ask = Self::confirm(message, "Merge", window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if ask.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_update_dialog(Source::Merge(branch), window, cx)
                });
            }
        })
        .detach();
    }

    pub(crate) fn fetch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cwd.is_empty() {
            return;
        }
        self.status = None;
        self.run_git(
            "fetch".into(),
            |path| scm::fetch(&path),
            |this, _, cx| this.set_status("Fetched", cx),
            window,
            cx,
        );
    }
    /// Opens the Push window: the commits and files a push would send, with
    /// Push to send them.
    pub(crate) fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.index(cx);
        let Some(branch) = index.as_ref().and_then(|i| i.branch.clone()) else {
            return;
        };
        let warn = Self::on_default(index.as_ref())
            .then(|| format!("\"{branch}\" is the default branch."));
        self.open_update_dialog(Source::Push { warn }, window, cx);
    }

    pub(crate) fn pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_while_agent_works("pull", window, cx) {
            return;
        }
        let index = self.index(cx);
        if !index.is_some_and(|i| i.remote.is_some() && i.upstream.is_some()) {
            return;
        }
        self.status = None;
        self.run_git(
            "pull".into(),
            |path| scm::pull(&path),
            |this, _, cx| this.set_status("Pull complete", cx),
            window,
            cx,
        );
    }
    fn on_default(index: Option<&DiffIndex>) -> bool {
        index.is_some_and(|i| i.branch.is_some() && i.branch == i.default_branch)
    }
    /// generatePrContent, `gh pr create`, then the browser.
    fn open_created_pr(
        &mut self,
        harness: Option<TextHarness>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some("pr".into());
        cx.notify();
        let cwd = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { create_pr_blocking(harness, &cwd) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.mutated(cx);
                this.load_pr(cx);
                match result {
                    Ok(url) => cx.open_url(url.trim()),
                    Err(error) => Self::fail(error, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(
            "sync".into(),
            |path| scm::sync(&path),
            |this, _, cx| this.load_pr(cx),
            window,
            cx,
        );
    }
    fn create_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.index(cx);
        let ahead = index.as_ref().is_some_and(|i| i.ahead > 0);
        let question = Self::on_default(index.as_ref()).then(|| {
            format!(
                "Create a pull request from default branch \"{}\"?",
                index
                    .as_ref()
                    .and_then(|i| i.branch.clone())
                    .unwrap_or_default()
            )
        });
        let harness = self.form.read(cx).text_harness(cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Some(question) = question {
                let Ok(ask) = this.update_in(cx, |_, window, cx| {
                    Self::confirm(question, "OK", window, cx)
                }) else {
                    return;
                };
                if !ask.await {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if ahead {
                    this.run_git(
                        "pr".into(),
                        |path| scm::push(&path),
                        move |this, window, cx| this.open_created_pr(harness, window, cx),
                        window,
                        cx,
                    );
                } else {
                    this.open_created_pr(harness, window, cx);
                }
            });
        })
        .detach();
    }

    fn open_menu(&mut self, kind: MenuKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|m| m.kind == kind) {
            self.menu = None;
            cx.notify();
            return;
        }
        self.menu = None;
        let index = self.index(cx);
        let items: Vec<MenuItem> = match kind {
            MenuKind::Branch => {
                let can_pull = index
                    .as_ref()
                    .is_some_and(|i| i.remote.is_some() && i.upstream.is_some());
                let pulling = self.busy.as_deref() == Some("pull");
                vec![
                    MenuAction::new("update", "Update Project")
                        .icon("refresh-cw")
                        .disabled(self.busy.is_some() || !can_pull)
                        .into(),
                    MenuAction::new("update-rebase", "Update Project (Rebase)")
                        .disabled(self.busy.is_some() || !can_pull)
                        .into(),
                    MenuItem::Separator,
                    MenuAction::new("pull", if pulling { "Pulling…" } else { "Pull" })
                        .icon(if pulling { "loader" } else { "refresh-cw" })
                        .disabled(self.busy.is_some() || !can_pull)
                        .into(),
                    MenuItem::Separator,
                    MenuAction::new("stash", "Stash Changes…")
                        .disabled(self.busy.is_some())
                        .into(),
                    MenuAction::new("unstash", "Unstash Changes…")
                        .disabled(self.busy.is_some())
                        .into(),
                ]
            }
            MenuKind::Commit => {
                let (can_push, can_pr) = self.push_options(cx);
                let form = self.form.read(cx);
                vec![
                    MenuAction::new("dialog", "Commit…")
                        .disabled(self.busy.is_some())
                        .into(),
                    MenuItem::Separator,
                    MenuAction::new("push", form.push_label())
                        .disabled(!can_push)
                        .into(),
                    MenuAction::new("push-pr", form.pr_label())
                        .disabled(!can_pr)
                        .into(),
                ]
            }
        };
        let (label, width) = match kind {
            MenuKind::Branch => ("Branch actions", 214.),
            MenuKind::Commit => ("Commit options", 214.),
        };
        let menu = cx.new(|cx| {
            Menu::new(label, items, window, cx)
                .width(width)
                .style(MenuStyle::Git)
        });
        let events = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &MenuEvent, window, cx| {
                this.menu = None;
                if let MenuEvent::Picked(id, _) = event {
                    match id.as_ref() {
                        "pull" => this.pull(window, cx),
                        "update" => {
                            this.open_update_dialog(Source::Update(UpdateMethod::Merge), window, cx)
                        }
                        "update-rebase" => this.open_update_dialog(
                            Source::Update(UpdateMethod::Rebase),
                            window,
                            cx,
                        ),
                        "dialog" => this.open_commit_dialog(window, cx),
                        "stash" => this.open_stash_dialog(StashIntent::Stash, window, cx),
                        "unstash" => this.open_stash_dialog(StashIntent::Manage, window, cx),
                        "push" => this.commit(true, false, window, cx),
                        "push-pr" => this.commit(true, true, window, cx),
                        _ => {}
                    }
                }
                cx.notify();
            },
        );
        self.menu = Some(OpenMenu {
            kind,
            menu,
            _events: events,
        });
        cx.notify();
    }
    /// The form's push options, off while a panel action runs.
    fn push_options(&self, cx: &App) -> (bool, bool) {
        let (can_push, can_pr) = self.form.read(cx).push_options(cx);
        let idle = self.busy.is_none();
        (can_push && idle, can_pr && idle)
    }
    fn can_commit(&self, cx: &App) -> bool {
        self.busy.is_none() && self.form.read(cx).can_commit(cx)
    }
    fn commit(&mut self, push: bool, create_pr: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.menu = None;
        self.form
            .update(cx, |form, cx| form.commit(push, create_pr, window, cx));
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.git.read(cx).index.clone();
        let busy = self.busy.is_some();
        let menu_open = self
            .menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Branch);
        let branch = index.as_ref().and_then(|i| i.branch.clone());
        let index = index.unwrap_or_default();
        pane_header("changes-header", "Changes")
            .when_some(branch.clone(), |d, branch| {
                let picker_open = self.branch_picker.is_some();
                d.child(
                    div()
                        .id("branch-switcher")
                        .role(Role::Button)
                        .aria_label("Switch branch")
                        .aria_expanded(picker_open)
                        .relative()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(24.))
                        .px_1()
                        .rounded_md()
                        .when(picker_open, |d| d.bg(content(0.1)))
                        .when(!busy && !picker_open, |d| d.hover(|s| s.bg(content(0.05))))
                        .when(!busy, |d| {
                            d.cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_branch_picker(BranchPlace::Header, window, cx)
                                }))
                        })
                        .child(self.switch_anchor.probe())
                        .child(icon("git-branch-hi", 12.).text_color(content(0.5)))
                        .child(div().min_w_0().truncate().child(branch))
                        .when(index.ahead > 0, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .font_features(tabular())
                                    .text_size(px(11.))
                                    .text_color(content(0.4))
                                    .child(format!("↑{}", index.ahead)),
                            )
                        })
                        .when(index.behind > 0, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .font_features(tabular())
                                    .text_size(px(11.))
                                    .text_color(content(0.4))
                                    .child(format!("↓{}", index.behind)),
                            )
                        })
                        .child(icon("chevron-down", 11.).text_color(content(0.4))),
                )
            })
            .when(branch.is_none(), |d| {
                d.child(div().flex_shrink_0().child("Changes"))
            })
            .when_some(self.status.clone(), |d, status| {
                d.child(
                    div()
                        .id("changes-status")
                        .role(Role::Status)
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(content(0.5))
                        .child(status),
                )
            })
            .child(div().ml_auto())
            .when(index.upstream.is_some(), |d| {
                d.child(
                    icon_action("open-update-dialog", "Update Project", "refresh-cw", !busy).when(
                        !busy,
                        |d| {
                            d.on_click(cx.listener(|this, _, window, cx| {
                                this.open_update_dialog(
                                    Source::Update(UpdateMethod::Merge),
                                    window,
                                    cx,
                                )
                            }))
                        },
                    ),
                )
            })
            .child(
                icon_action("open-commit-dialog", "Commit…", "git-commit", !busy).when(
                    !busy,
                    |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.open_commit_dialog(window, cx)),
                        )
                    },
                ),
            )
            .when(branch.is_some(), |d| {
                d.child(
                    div()
                        .id("branch-actions")
                        .role(Role::Button)
                        .aria_label("Branch actions")
                        .aria_expanded(menu_open)
                        .group("branch-actions")
                        .relative()
                        .size(px(24.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .child(self.branch_anchor.probe())
                        .when(busy, |d| d.opacity(0.4))
                        .when(menu_open, |d| d.bg(content(0.1)))
                        .when(!busy && !menu_open, |d| d.hover(|s| s.bg(content(0.05))))
                        .child(if self.busy.as_deref() == Some("pull") {
                            spinner("pull-spin", "loader", 14.)
                        } else {
                            icon("more-horizontal", 16.)
                                .text_color(content(if menu_open { 1. } else { 0.5 }))
                                .group_hover("branch-actions", |s| s.text_color(content(1.)))
                                .into_any_element()
                        })
                        .when(!busy, |d| {
                            d.on_click(cx.listener(|this, _, window, cx| {
                                this.open_menu(MenuKind::Branch, window, cx)
                            }))
                        }),
                )
            })
    }

    fn commit_box(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.git.read(cx).index.clone();
        let form = self.form.read(cx);
        let can_commit = self.can_commit(cx);
        let form_busy = form.busy.is_some();
        let commit_label = form.commit_label();
        let dialog_open = form.dialog_open;
        let draft = form.message.read(cx).content.to_string();
        let message = form.message.clone();
        let error = form.error_line();
        let can_open_menu =
            index.as_ref().is_some_and(|i| i.branch.is_some()) && self.busy.is_none() && !form_busy;
        let menu_open = self
            .menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Commit);
        let generate = CommitForm::generate_button(&self.form, window, cx);
        let amend_toggle = CommitForm::amend_toggle(&self.form, window, cx);
        // `bg-content` buttons with `text-background-base`, 40% when unavailable.
        let solid: Hsla = content(1.);
        let face = if can_commit { solid } else { content(0.4) };
        let ink = crate::theme::background_base();
        div()
            .flex_shrink_0()
            .border_b_1()
            .border_color(stroke())
            .p_2()
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .w_full()
                            .rounded_md()
                            .bg(content(0.1))
                            .py_1()
                            .pl_2()
                            .pr_8()
                            .text_size(px(13.))
                            .line_height(px(20.))
                            .text_color(content(1.))
                            .when(self.busy.is_some() || form_busy, |d| d.opacity(0.4))
                            // The Commit window holds the message box while it is open.
                            .map(|d| {
                                if dialog_open {
                                    d.max_h(px(152.))
                                        .overflow_hidden()
                                        .when(draft.trim().is_empty(), |d| {
                                            d.text_color(content(0.35))
                                        })
                                        .child(if draft.trim().is_empty() {
                                            "Commit message".to_owned()
                                        } else {
                                            draft
                                        })
                                } else {
                                    d.child(message)
                                }
                            }),
                    )
                    .when(!dialog_open, |d| d.child(generate)),
            )
            .child(div().mt(px(6.)).child(amend_toggle))
            .child(
                div()
                    .relative()
                    .mt(px(6.))
                    .flex()
                    .child(self.commit_anchor.probe())
                    .child(
                        div()
                            .id("commit")
                            .role(Role::Button)
                            .aria_label(commit_label)
                            .h(px(28.))
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(6.))
                            .rounded_l_md()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(face)
                            .text_color(ink)
                            .child(icon("check-2", 14.).text_color(ink))
                            .child(commit_label)
                            .when(can_commit, |d| {
                                d.on_click(cx.listener(|this, _, window, cx| {
                                    this.commit(false, false, window, cx)
                                }))
                            }),
                    )
                    .child(
                        div()
                            .id("commit-options")
                            .role(Role::Button)
                            .aria_label("Commit options")
                            .aria_expanded(menu_open)
                            .size(px(28.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_r_md()
                            .border_l_1()
                            .border_color(ink.opacity(0.1))
                            .bg(if menu_open { solid } else { face })
                            .when(can_open_menu && !menu_open, |d| {
                                d.hover(move |s| {
                                    s.bg(if can_commit { content(0.8) } else { solid })
                                })
                            })
                            .child(icon("chevron-down-2", 14.).text_color(ink))
                            .when(can_open_menu, |d| {
                                d.on_click(cx.listener(|this, _, window, cx| {
                                    this.open_menu(MenuKind::Commit, window, cx)
                                }))
                            }),
                    ),
            )
            .when_some(error, |d, error| d.child(div().mt(px(6.)).child(error)))
            .when_some(index, |d, index| d.child(self.sync_actions(&index, cx)))
    }

    fn sync_actions(&self, index: &DiffIndex, cx: &mut Context<Self>) -> AnyElement {
        if index.remote.is_none() {
            return div().into_any_element();
        }
        let busy = self.busy.is_some();
        let (ahead, behind) = (index.ahead, index.behind);
        let has_open_pr = self.pr.as_ref().is_some_and(|p| p.state == "open");
        let on_default = Self::on_default(Some(index));
        let diverged = ahead > 0 && behind > 0;
        // Unversioned files never reach a pull request, so only tracked
        // uncommitted changes keep Create PR off.
        let tracked = index
            .files
            .iter()
            .filter(|f| !crate::commit_form::is_untracked(f))
            .count();
        let can_publish = index.upstream.is_none();
        let can_sync = index.upstream.is_some() && (ahead > 0 || behind > 0);
        let can_create_pr = index.branch.is_some()
            && index.default_branch.is_some()
            && !has_open_pr
            && !on_default
            && !diverged
            && tracked == 0
            && index.ahead_of_default > 0
            && behind == 0;
        let can_view_pr = has_open_pr && self.pr.as_ref().is_some_and(|p| !p.url.is_empty());
        let show_create = !has_open_pr && !on_default;
        if !can_publish && !can_sync && !show_create && !has_open_pr {
            return div().into_any_element();
        }
        let dest = index.upstream.clone().unwrap_or_else(|| {
            format!(
                "{}/{}",
                index.remote.as_deref().unwrap_or("origin"),
                index.branch.as_deref().unwrap_or("HEAD")
            )
        });
        let syncing = self.busy.as_deref() == Some("sync");
        let plural = |n: i64| if n == 1 { "" } else { "s" };
        let sync_title = if syncing {
            "Synchronizing Changes...".to_owned()
        } else if can_publish {
            index.branch.as_ref().map_or("Publish Branch".into(), |b| {
                format!("Publish Branch \"{b}\"")
            })
        } else if behind > 0 && ahead > 0 {
            format!("Pull {behind} and push {ahead} commits between {dest}")
        } else if behind > 0 {
            format!("Pull {behind} commit{} from {dest}", plural(behind))
        } else {
            format!("Push {ahead} commit{} to {dest}", plural(ahead))
        };
        let button = |id: &'static str, title: String, enabled: bool| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(SharedString::from(title))
                .h(px(28.))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .px_2()
                .rounded_md()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .bg(content(0.1))
                .text_color(content(1.))
                .when(!enabled, |d| d.opacity(0.4))
                .when(enabled, |d| d.hover(|s| s.bg(content(0.15))))
        };
        let glyph = |path: &'static str| icon(path, 14.).text_color(content(1.)).into_any_element();
        div()
            .mt(px(6.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(can_publish, |d| {
                d.child(
                    button("publish-branch", sync_title.clone(), !busy)
                        .child(if syncing {
                            spinner("sync-spin", "loader", 14.)
                        } else {
                            glyph("cloud-upload")
                        })
                        .child(div().min_w_0().truncate().child("Publish Branch"))
                        .when(!busy, |d| {
                            d.on_click(cx.listener(|this, _, window, cx| this.sync(window, cx)))
                        }),
                )
            })
            .when(!can_publish && can_sync, |d| {
                d.child(
                    button("sync-changes", sync_title.clone(), !busy)
                        .child(if syncing {
                            spinner("sync-spin", "refresh-cw", 14.)
                        } else {
                            glyph("refresh-cw")
                        })
                        .child(div().min_w_0().truncate().child("Sync Changes"))
                        .when(behind > 0, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .font_features(tabular())
                                    .text_color(content(0.55))
                                    .child(format!("↓{behind}")),
                            )
                        })
                        .when(ahead > 0, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .font_features(tabular())
                                    .text_color(content(0.55))
                                    .child(format!("↑{ahead}")),
                            )
                        })
                        .when(!busy, |d| {
                            d.on_click(cx.listener(|this, _, window, cx| this.sync(window, cx)))
                        }),
                )
            })
            .when(show_create, |d| {
                let title = index
                    .pr_base
                    .as_ref()
                    .or(index.default_branch.as_ref())
                    .map_or("Create pull request".to_owned(), |b| {
                        format!("Create a pull request into {b}")
                    });
                let enabled = can_create_pr && !busy;
                d.child(
                    button("create-pr", title, enabled)
                        .child(if self.busy.as_deref() == Some("pr") {
                            spinner("pr-spin", "loader", 14.)
                        } else {
                            glyph("git-pull-request")
                        })
                        .child("Create PR")
                        .when(enabled, |d| {
                            d.on_click(
                                cx.listener(|this, _, window, cx| this.create_pr(window, cx)),
                            )
                        }),
                )
            })
            .when(has_open_pr, |d| {
                let pr = self.pr.clone().unwrap_or(Pr {
                    number: 0,
                    title: String::new(),
                    url: String::new(),
                    state: String::new(),
                });
                let title = if pr.title.is_empty() {
                    "View pull request".to_owned()
                } else {
                    format!("View PR #{}: {}", pr.number, pr.title)
                };
                let enabled = can_view_pr && !busy;
                let url = pr.url.clone();
                d.child(
                    button("view-pr", title, enabled)
                        .child(glyph("external-link"))
                        .child(div().min_w_0().truncate().child(if pr.number > 0 {
                            format!("View PR #{}", pr.number)
                        } else {
                            "View PR".into()
                        }))
                        .when(enabled, |d| d.on_click(move |_, _, cx| cx.open_url(&url))),
                )
            })
            .into_any_element()
    }
}

/// A provider CLI and model for one-off text, `None` for its default model.
pub(crate) type TextHarness = (Kind, PathBuf, Option<String>);

/// pickTextHarness: `chosen` (the background model picked in Settings) when
/// installed, else `preferred`, then Claude Code, then Codex on their default
/// models.
pub(crate) fn pick_text_harness(
    preferred: Kind,
    chosen: Option<(Kind, Option<String>)>,
) -> Option<TextHarness> {
    chosen
        .and_then(|(kind, model)| provider::resolve(kind).map(|binary| (kind, binary, model)))
        .or_else(|| {
            [preferred, Kind::Claude, Kind::Codex]
                .into_iter()
                .find_map(|kind| provider::resolve(kind).map(|binary| (kind, binary, None)))
        })
}

/// generateCommitMessage: a message for the staged changes, written by a
/// read-only turn of `kind`. Blocking.
pub(crate) fn generate_commit_message(
    harness: TextHarness,
    cwd: PathBuf,
    cancel: Arc<AtomicBool>,
    paths: Option<&[String]>,
) -> Result<String, String> {
    let kind = harness.0;
    let context = match paths {
        Some(paths) => scm::selected_context(&cwd, paths)?,
        None => scm::staged_context(&cwd)?,
    };
    let prompt =
        scm::commit_message_prompt(context.branch.as_deref(), &context.summary, &context.patch);
    let output = text_prompt(harness, cwd, prompt, cancel)?;
    scm::parse_commit_message(&output).ok_or_else(|| {
        let snippet: String = output
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(240)
            .collect();
        if snippet.is_empty() {
            format!(
                "Could not generate a commit message. {} returned no text.",
                kind.label()
            )
        } else {
            format!("Could not generate a commit message. Model replied: {snippet}")
        }
    })
}

/// generatePrContent, falling back to the commits, then `gh pr create`; the
/// new pull request's URL. Blocking.
pub(crate) fn create_pr_blocking(
    harness: Option<TextHarness>,
    cwd: &std::path::Path,
) -> Result<String, String> {
    let range = scm::range_context(cwd)?;
    let parsed = harness.and_then(|harness| {
        let output = text_prompt(
            harness,
            cwd.to_path_buf(),
            scm::pr_content_prompt(&range),
            Arc::new(AtomicBool::new(false)),
        )
        .ok()?;
        scm::parse_pr_content(&output)
    });
    let first = range.commit_summary.lines().next().unwrap_or("").trim();
    let title = parsed
        .as_ref()
        .map(|(t, _)| t.clone())
        .filter(|t| !t.is_empty())
        .or_else(|| (!first.is_empty()).then(|| first.to_owned()))
        .unwrap_or_else(|| format!("Update {}", range.head));
    let body = parsed
        .map(|(_, b)| b)
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| range.commit_summary.trim().to_owned());
    scm::pr_create(cwd, &title, &body, &range.base, &range.head)
}

/// A read-only one-shot turn, as the React text harness runs: the final
/// assistant message's text.
fn text_prompt(
    (kind, binary, model): TextHarness,
    cwd: PathBuf,
    prompt: String,
    cancel: Arc<AtomicBool>,
) -> Result<String, String> {
    let request = Request {
        kind,
        binary,
        cwd,
        prompt,
        provider_id: None,
        model,
        settings: Settings {
            access: AccessMode::ReadOnly,
            ..Default::default()
        },
    };
    let mut text = String::new();
    provider::run(request, cancel.clone(), |events| {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        }
        for event in events {
            match event {
                Event::BeginAssistant => text.clear(),
                Event::Text(chunk) => text.push_str(&chunk),
                _ => {}
            }
        }
        Ok(())
    })?;
    Ok(text)
}

impl Render for Changes {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let available = !self.cwd.is_empty() && self.cwd != "~";
        if !available {
            return div()
                .px_3()
                .py_2()
                .text_size(px(12.))
                .text_color(content(0.5))
                .child("No project folder")
                .into_any_element();
        }
        let index = self.git.read(cx).index.clone();
        let empty = match &index {
            None => Some("Loading changes…".to_owned()),
            Some(i) if i.files.is_empty() => Some(if i.ahead > 0 || i.behind > 0 {
                sync_status_label(i)
            } else {
                "No uncommitted changes".into()
            }),
            _ => None,
        };
        let row_menu = self.form.read(cx).menu_overlay(Surface::Panel, cx);
        let menu = self.menu.as_ref().map(|open| {
            let anchor = match open.kind {
                MenuKind::Branch => self.branch_anchor.clone(),
                MenuKind::Commit => self.commit_anchor.clone(),
            };
            menu_popover(&open.menu, Anchor::Slot(anchor), cx).align(Align::End)
        });
        div()
            .id("changes")
            .role(Role::TabPanel)
            .aria_label("Changes")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.header(cx))
            .child(self.commit_box(window, cx))
            .map(|d| match empty {
                Some(text) => d.child(
                    div().py_1().child(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(px(12.))
                            .text_color(content(0.45))
                            .child(text),
                    ),
                ),
                None => d.child(
                    div()
                        .id("change-files")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .track_focus(&self.list_focus)
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, window, cx| window.focus(&this.list_focus, cx)),
                        )
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            if this
                                .form
                                .update(cx, |form, cx| form.key(Surface::Panel, event, cx))
                            {
                                cx.stop_propagation();
                            }
                        }))
                        .when(self.form.read(cx).busy.is_some(), |d| d.opacity(0.6))
                        .child(
                            CommitForm::file_list(&self.form, Surface::Panel, cx)
                                .py_1()
                                .flex_1()
                                .min_h_0(),
                        ),
                ),
            })
            .children(row_menu)
            .children(menu)
            .children(if self.overlays_in_shell {
                Vec::new()
            } else {
                self.overlays(window, cx)
            })
            .children(
                self.branch_picker
                    .as_ref()
                    .filter(|_| self.branch_place == BranchPlace::Header)
                    .map(|(picker, _)| {
                        let weak = cx.weak_entity();
                        popover(
                            "changes-branch-picker",
                            Anchor::Slot(self.switch_anchor.clone()),
                        )
                        .side(Side::Bottom)
                        .align(Align::Start)
                        .width(320.)
                        .height(360.)
                        .on_dismiss(move |_, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.branch_picker = None;
                                cx.notify();
                            });
                        })
                        .child(picker.clone())
                    }),
            )
            .into_any_element()
    }
}

/// Whether a session working in `cwd` shares files with the project checkout:
/// the same folder, or one inside the other. Linked worktrees live apart.
pub(crate) fn checkouts_overlap(project: &str, cwd: &str) -> bool {
    let (project, cwd) = (
        monocode_core::files::path_key(project),
        monocode_core::files::path_key(cwd),
    );
    let inside = |outer: &str, inner: &str| {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    inside(&project, &cwd) || inside(&cwd, &project)
}

impl Changes {
    /// The windows this panel opens. The shell draws them, so they appear
    /// from the menu bar even where the panel itself is not on screen.
    pub(crate) fn overlays(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(confirm) = CommitForm::pending_overlay(&self.form, Surface::Panel, window, cx) {
            out.push(crate::modal::window_overlay(window, confirm).into_any_element());
        }
        if let Some((dialog, _)) = &self.commit_dialog {
            out.push(crate::modal::window_overlay(window, dialog.clone()).into_any_element());
        }
        if let Some((dialog, _)) = &self.update_dialog {
            out.push(crate::modal::window_overlay(window, dialog.clone()).into_any_element());
        }
        if let Some((dialog, _)) = &self.stash_dialog {
            out.push(crate::modal::window_overlay(window, dialog.clone()).into_any_element());
        }
        let Some(picker) = self
            .branch_picker
            .as_ref()
            .map(|(picker, _)| picker.clone())
        else {
            return out;
        };
        match self.branch_place {
            BranchPlace::Header => {}
            BranchPlace::Modal => {
                let card = crate::modal::card()
                    .w(px(560.))
                    .h(px(560.))
                    .overflow_hidden()
                    .child(picker.clone());
                let weak = cx.weak_entity();
                let overlay = crate::modal::overlay("branch-switcher", card, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.branch_picker = None;
                        cx.emit(ChangesEvent::Overlays);
                        cx.notify();
                    });
                });
                out.push(crate::modal::window_overlay(window, overlay).into_any_element());
            }
            BranchPlace::Footer => {
                let weak = cx.weak_entity();
                out.push(
                    popover(
                        "footer-branch-picker",
                        Anchor::Slot(self.footer_anchor.clone()),
                    )
                    .side(Side::Top)
                    .align(Align::Start)
                    .width(560.)
                    .height(560.)
                    .on_dismiss(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.branch_picker = None;
                            cx.emit(ChangesEvent::Overlays);
                            cx.notify();
                        });
                    })
                    .child(picker)
                    .into_any_element(),
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_sessions_sharing_the_checkout_block_git_changes() {
        assert!(checkouts_overlap("/work/app", "/work/app"));
        assert!(checkouts_overlap("/work/app", "/work/app/packages/web"));
        assert!(checkouts_overlap("/work/app/packages/web", "/work/app"));
        // A sibling with a common prefix, and a linked worktree, are separate.
        assert!(!checkouts_overlap("/work/app", "/work/app-two"));
        assert!(!checkouts_overlap(
            "/work/app",
            "/work/.worktrees/app-feature"
        ));
    }
    #[gpui::test]
    fn enabling_an_enabled_changes_panel_does_not_reload(cx: &mut gpui::TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let git = cx.new(|cx| ProjectGit::new("~".into(), cx));
                cx.new(|cx| Changes::new(git, window, cx))
            })
            .unwrap()
        });
        window
            .update(cx, |changes, _, cx| {
                changes.set_enabled(true, cx);
                let request = changes.pr_request;
                changes.set_enabled(true, cx);
                assert_eq!(request, changes.pr_request);
                changes.set_enabled(false, cx);
                changes.set_enabled(false, cx);
                assert_eq!(request, changes.pr_request);
            })
            .unwrap();
    }
}
