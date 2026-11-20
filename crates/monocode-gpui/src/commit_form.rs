//! The commit form shared by the Changes panel and the Commit window: the
//! changed files as a tree with a checkbox each (tracked changes apart from
//! unversioned files), the message, amend, and the commit itself. Only the
//! checked files are committed; every other staged entry stays staged.
//!
//! One form per checkout: the panel and the window show the same checks, the
//! same message and the same amend state, and act through the same code.
use crate::{
    change_tree::{Node, TreeRow, basename},
    diff_view::diff_counts,
    file_icons::file_type_icon,
    git_status::{GitChanged, ProjectGit},
    input::{self, Input},
    modal,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover},
    right_panel::{DIFF_ADD, DIFF_DEL, spinner},
    theme::*,
};
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, ElementId, Entity, EventEmitter,
    FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, PromptLevel, Role, Stateful,
    Subscription, Window, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::{
    provider::Kind,
    scm::{self, ChangedFile, DiffIndex},
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// One line of the file tree, in the panel and in the window alike.
pub(crate) const ROW: f32 = 26.;

const MOD: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+"
};

/// Where the form is drawn. Clicks, menus and confirmations belong to the
/// surface they came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Surface {
    Panel,
    Dialog,
}

pub(crate) enum CommitFormEvent {
    /// A file was picked: the panel opens its review, the window its diff.
    Selected(Surface),
    /// "Jump to Source" opened a file in the editor.
    Jumped(Surface),
    /// A commit went through. `pushed` when a push followed it; `error`
    /// carries a failure after the commit (the push or the pull request).
    Committed { pushed: bool, error: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Check {
    All,
    Some,
    None,
}

#[derive(Clone)]
pub(crate) enum Row {
    Group {
        key: &'static str,
        title: &'static str,
        files: Vec<usize>,
        open: bool,
    },
    Tree(TreeRow),
}

pub(crate) fn is_untracked(file: &ChangedFile) -> bool {
    file.status == "untracked"
}

/// What a rollback or a delete would touch, before the user agrees.
pub(crate) struct Pending {
    pub(crate) surface: Surface,
    /// Tracked files: changes are dropped (or an added file leaves the index).
    pub(crate) tracked: Vec<(String, String)>,
    /// Unversioned files, deleted from the disk.
    pub(crate) untracked: Vec<String>,
    /// Also remove newly added files from the disk, not just from the index.
    pub(crate) delete_added: bool,
}

/// The files at `members` split into tracked and unversioned ones.
pub(crate) fn plan(files: &[ChangedFile], members: &[usize]) -> (Vec<usize>, Vec<usize>) {
    members
        .iter()
        .copied()
        .partition(|&ix| !is_untracked(&files[ix]))
}

pub(crate) struct OpenMenu {
    surface: Surface,
    row: usize,
    anchor: gpui::Point<gpui::Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}

enum Outcome {
    /// Committed; a failure after the commit, and the new pull request.
    Committed(Option<String>, Option<String>),
    Failed(String),
}

fn on_default(index: Option<&DiffIndex>) -> bool {
    index.is_some_and(|i| i.branch.is_some() && i.branch == i.default_branch)
}

pub(crate) struct CommitForm {
    git: Entity<ProjectGit>,
    cwd: String,
    pub(crate) text_provider: Kind,
    pub(crate) files: Vec<ChangedFile>,
    pub(crate) checked: HashSet<String>,
    /// Files already seen, so a new tracked change starts checked and a
    /// file the user unchecked stays unchecked across refreshes.
    known: HashSet<String>,
    collapsed: HashSet<String>,
    pub(crate) rows: Vec<Row>,
    pub(crate) selected: Option<String>,
    pub(crate) message: Entity<Input>,
    /// The branch and HEAD the amend was turned on at; it lapses when they move.
    amend: Option<(Option<String>, Option<String>)>,
    generate_cancel: Option<Arc<AtomicBool>>,
    pub(crate) busy: Option<&'static str>,
    pub(crate) error: Option<String>,
    menu: Option<OpenMenu>,
    pub(crate) pending: Option<Pending>,
    /// The Commit window is open: it shows the message box, so the panel
    /// shows the text only.
    pub(crate) dialog_open: bool,
    /// The branch already has an open pull request.
    pub(crate) has_open_pr: bool,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<CommitFormEvent> for CommitForm {}

impl CommitForm {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(Input::commit_message);
        let subscriptions = vec![
            cx.observe(&git, |this, _, cx| this.refresh(cx)),
            cx.subscribe(&git, |this, _, _: &GitChanged, cx| this.refresh(cx)),
            cx.subscribe(&message, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe_in(
                &message,
                window,
                |this, _, _: &input::Submitted, window, cx| this.commit(false, false, window, cx),
            ),
        ];
        let mut this = Self {
            git,
            cwd: String::new(),
            text_provider: Kind::Claude,
            files: Vec::new(),
            checked: HashSet::new(),
            known: HashSet::new(),
            collapsed: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            message,
            amend: None,
            generate_cancel: None,
            busy: None,
            error: None,
            menu: None,
            pending: None,
            dialog_open: false,
            has_open_pr: false,
            _subscriptions: subscriptions,
        };
        this.sync_placeholder(cx);
        this.refresh(cx);
        this
    }

    pub(crate) fn cwd(&self) -> &str {
        &self.cwd
    }
    pub(crate) fn amending(&self) -> bool {
        self.amend.is_some()
    }
    pub(crate) fn generating(&self) -> bool {
        self.generate_cancel.is_some()
    }
    fn index(&self, cx: &App) -> Option<DiffIndex> {
        self.git.read(cx).index.clone()
    }
    fn set_message(&mut self, text: String, cx: &mut Context<Self>) {
        self.message.update(cx, |m, cx| m.set_text(text, cx));
    }
    fn sync_placeholder(&mut self, cx: &mut Context<Self>) {
        let placeholder = if self.amend.is_some() {
            format!("Amend message ({MOD}↩ to amend)")
        } else {
            format!("Commit message ({MOD}↩ to commit)")
        };
        self.message
            .update(cx, |m, cx| m.set_placeholder(placeholder, cx));
    }

    /// Rereads the file list; new tracked changes start checked.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let project = self.git.read(cx).project().to_owned();
        if project != self.cwd {
            // Checkouts don't share drafts, checks or in-flight generation.
            self.cwd = project;
            self.cancel_generate(cx);
            self.files.clear();
            self.rows.clear();
            self.checked.clear();
            self.known.clear();
            self.collapsed.clear();
            self.selected = None;
            self.amend = None;
            self.error = None;
            self.pending = None;
            self.menu = None;
            self.set_message(String::new(), cx);
            self.sync_placeholder(cx);
        }
        let index = self.index(cx);
        if let Some((branch, head)) = &self.amend
            && (index.as_ref().map(|i| &i.branch) != Some(branch)
                || index.as_ref().map(|i| &i.head) != Some(head))
        {
            self.amend = None;
            self.set_message(String::new(), cx);
            self.sync_placeholder(cx);
        }
        let Some(index) = index else {
            cx.notify();
            return;
        };
        let mut files = index.files;
        files.sort_by_key(|f| f.relative.to_lowercase());
        let present: HashSet<String> = files.iter().map(|f| f.relative.clone()).collect();
        self.checked.retain(|path| present.contains(path));
        for file in &files {
            if self.known.insert(file.relative.clone()) && !is_untracked(file) {
                self.checked.insert(file.relative.clone());
            }
        }
        self.known.retain(|path| present.contains(path));
        if self
            .selected
            .as_ref()
            .is_some_and(|path| !present.contains(path))
        {
            self.selected = None;
        }
        self.files = files;
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for (key, title, untracked) in [
            ("tracked", "Changes", false),
            ("untracked", "Unversioned Files", true),
        ] {
            let members: Vec<usize> = (0..self.files.len())
                .filter(|&ix| is_untracked(&self.files[ix]) == untracked)
                .collect();
            if members.is_empty() {
                continue;
            }
            let open = !self.collapsed.contains(&format!("g:{key}"));
            rows.push(Row::Group {
                key,
                title,
                files: members.clone(),
                open,
            });
            if open {
                let mut root = Node::default();
                for &ix in &members {
                    let parts: Vec<&str> = self.files[ix].relative.split('/').collect();
                    root.insert(&parts, ix);
                }
                let mut tree = Vec::new();
                root.flatten(&self.files, key, "", 0, &self.collapsed, &mut tree);
                rows.extend(tree.into_iter().map(Row::Tree));
            }
        }
        self.rows = rows;
    }

    pub(crate) fn state(&self, members: &[usize]) -> Check {
        let on = members
            .iter()
            .filter(|&&ix| self.checked.contains(&self.files[ix].relative))
            .count();
        if on == 0 {
            Check::None
        } else if on == members.len() {
            Check::All
        } else {
            Check::Some
        }
    }
    pub(crate) fn toggle_files(&mut self, members: &[usize], cx: &mut Context<Self>) {
        let all = self.state(members) == Check::All;
        for &ix in members {
            let path = self.files[ix].relative.clone();
            if all {
                self.checked.remove(&path);
            } else {
                self.checked.insert(path);
            }
        }
        cx.notify();
    }
    fn toggle_fold(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild_rows();
        cx.notify();
    }
    /// Picks a file: the surface it came from shows its diff.
    pub(crate) fn select(&mut self, ix: usize, surface: Surface, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(ix) else {
            return;
        };
        self.selected = Some(file.relative.clone());
        cx.emit(CommitFormEvent::Selected(surface));
        cx.notify();
    }
    /// The Commit window always shows a diff: the first file when none is picked.
    pub(crate) fn select_first_if_none(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_none()
            && let Some(file) = self.files.first()
        {
            self.selected = Some(file.relative.clone());
            cx.notify();
        }
    }
    pub(crate) fn selected_file(&self) -> Option<&ChangedFile> {
        let selected = self.selected.as_ref()?;
        self.files.iter().find(|f| &f.relative == selected)
    }

    pub(crate) fn can_commit(&self, cx: &App) -> bool {
        self.busy.is_none()
            && (!self.checked.is_empty() || self.amend.is_some())
            && !self.message.read(cx).content.trim().is_empty()
    }
    /// Whether Commit and Push, and Commit, Push and Create PR, can run.
    pub(crate) fn push_options(&self, cx: &App) -> (bool, bool) {
        let index = self.git.read(cx).index.as_ref();
        let has_remote = index.is_some_and(|i| i.remote.is_some());
        let diverged = index.is_some_and(|i| i.ahead > 0 && i.behind > 0);
        let head_pushed = index.is_some_and(|i| i.head_pushed);
        let can_push = self.can_commit(cx)
            && has_remote
            && !diverged
            && (self.amend.is_none() || !head_pushed);
        (
            can_push,
            can_push && !self.has_open_pr && !on_default(index),
        )
    }
    pub(crate) fn commit_label(&self) -> &'static str {
        match self.busy {
            Some("commit") => "Committing…",
            _ if self.amend.is_some() => "Amend Commit",
            _ => "Commit",
        }
    }
    pub(crate) fn push_label(&self) -> &'static str {
        if self.busy == Some("push") {
            "Pushing…"
        } else {
            "Commit and Push"
        }
    }
    pub(crate) fn pr_label(&self) -> &'static str {
        if self.busy == Some("pr") {
            "Creating Pull Request…"
        } else {
            "Commit, Push and Create PR"
        }
    }

    /// pickTextHarness for this checkout's conversation provider.
    pub(crate) fn text_harness(&self, cx: &App) -> Option<crate::changes::TextHarness> {
        let chosen = cx
            .try_global::<crate::settings::Preferences>()
            .and_then(|p| p.background_text_model());
        crate::changes::pick_text_harness(self.text_provider, chosen)
    }

    /// Commits the checked files; then pushes, then opens a pull request.
    pub(crate) fn commit(
        &mut self,
        push: bool,
        create_pr: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let push = push || create_pr;
        let allowed = if push {
            let (can_push, can_pr) = self.push_options(cx);
            if create_pr { can_pr } else { can_push }
        } else {
            self.can_commit(cx)
        };
        if !allowed {
            return;
        }
        let index = self.index(cx);
        let mut questions: Vec<(String, &'static str)> = Vec::new();
        if push
            && on_default(index.as_ref())
            && let Some(branch) = index.as_ref().and_then(|i| i.branch.clone())
        {
            questions.push((format!("Push to default branch \"{branch}\"?"), "Push"));
        }
        if self.amend.is_some() && index.as_ref().is_some_and(|i| i.head_pushed) {
            questions.push((
                "Amend a commit that is already pushed? You will need a force push from the terminal."
                    .into(),
                "Amend",
            ));
        }
        let message = self.message.read(cx).content.to_string();
        let mut paths: Vec<String> = self.checked.iter().cloned().collect();
        paths.sort();
        let amend = self.amend.is_some();
        let harness = create_pr.then(|| self.text_harness(cx)).flatten();
        self.error = None;
        self.menu = None;
        cx.spawn_in(window, async move |this, cx| {
            for (question, ok) in questions {
                let Ok(answer) = this.update_in(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        &question,
                        None,
                        &[
                            gpui::PromptButton::ok(ok),
                            gpui::PromptButton::cancel("Cancel"),
                        ],
                        cx,
                    )
                }) else {
                    return;
                };
                if !matches!(answer.await, Ok(0)) {
                    return;
                }
            }
            let Ok(work) = this.update(cx, |this, cx| {
                this.busy = Some(if create_pr {
                    "pr"
                } else if push {
                    "push"
                } else {
                    "commit"
                });
                cx.notify();
                let cwd = PathBuf::from(&this.cwd);
                cx.background_executor().spawn(async move {
                    if let Err(error) = scm::commit_paths(&cwd, &message, &paths, amend) {
                        return Outcome::Failed(error);
                    }
                    if !push {
                        return Outcome::Committed(None, None);
                    }
                    if let Err(error) = scm::push(&cwd) {
                        return Outcome::Committed(
                            Some(format!("Committed, but the push failed: {error}")),
                            None,
                        );
                    }
                    if !create_pr {
                        return Outcome::Committed(None, None);
                    }
                    match crate::changes::create_pr_blocking(harness, &cwd) {
                        Ok(url) => Outcome::Committed(None, Some(url)),
                        Err(error) => Outcome::Committed(
                            Some(format!(
                                "Committed and pushed, but the pull request failed: {error}"
                            )),
                            None,
                        ),
                    }
                })
            }) else {
                return;
            };
            let outcome = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Outcome::Committed(error, url) => {
                        this.amend = None;
                        this.set_message(String::new(), cx);
                        this.sync_placeholder(cx);
                        if let Some(url) = url {
                            cx.open_url(url.trim());
                        }
                        let failed = error.is_some();
                        this.error = error;
                        cx.emit(CommitFormEvent::Committed {
                            pushed: push && !failed,
                            error: failed,
                        });
                    }
                    Outcome::Failed(error) => this.error = Some(error),
                }
                this.git.update(cx, |git, cx| git.changed(cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// A message for exactly the checked files, written by a read-only turn
    /// of the background text model.
    pub(crate) fn generate(&mut self, cx: &mut Context<Self>) {
        if self.checked.is_empty() || self.busy.is_some() {
            return;
        }
        let Some(harness) = self.text_harness(cx) else {
            self.error = Some("No provider CLI is installed to write a commit message.".into());
            cx.notify();
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.generate_cancel = Some(cancel.clone());
        self.busy = Some("generate");
        self.error = None;
        cx.notify();
        let cwd = PathBuf::from(&self.cwd);
        let mut paths: Vec<String> = self.checked.iter().cloned().collect();
        paths.sort();
        let flag = cancel.clone();
        let work = cx.background_executor().spawn(async move {
            crate::changes::generate_commit_message(harness, cwd, flag, Some(&paths))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                this.generate_cancel = None;
                this.busy = None;
                match result {
                    // Undoable: the user may want their own words back.
                    Ok(text) => this.message.update(cx, |m, cx| m.replace_all(text, cx)),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn cancel_generate(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.generate_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
            if self.busy == Some("generate") {
                self.busy = None;
            }
            cx.notify();
        }
    }

    /// Amend Last Commit: an empty message takes HEAD's; the amend lapses
    /// when the branch or HEAD moves under it.
    pub(crate) fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        if self.amend.take().is_some() {
            self.sync_placeholder(cx);
            cx.notify();
            return;
        }
        let path = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::head_message(&path) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(head) => {
                        if this.message.read(cx).content.trim().is_empty() {
                            this.message.update(cx, |m, cx| m.replace_all(head, cx));
                        }
                        let index = this.index(cx);
                        this.amend = Some((
                            index.as_ref().and_then(|i| i.branch.clone()),
                            index.and_then(|i| i.head),
                        ));
                        this.sync_placeholder(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The files a row stands for: a group's, a folder's, or the file itself.
    pub(crate) fn members(&self, ix: usize) -> Vec<usize> {
        match self.rows.get(ix) {
            Some(Row::Group { files, .. }) | Some(Row::Tree(TreeRow::Dir { files, .. })) => {
                files.clone()
            }
            Some(Row::Tree(TreeRow::File { ix, .. })) => vec![*ix],
            None => Vec::new(),
        }
    }

    pub(crate) fn open_menu(
        &mut self,
        surface: Surface,
        row: usize,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let members = self.members(row);
        let (tracked, untracked) = plan(&self.files, &members);
        let one_file = matches!(self.rows.get(row), Some(Row::Tree(TreeRow::File { .. })));
        let mut items: Vec<MenuItem> = Vec::new();
        if !tracked.is_empty() {
            items.push(
                MenuAction::new("rollback", "Rollback…")
                    .shortcut("⌘⌥Z")
                    .into(),
            );
        }
        if !untracked.is_empty() {
            items.push(MenuAction::new("add", "Add to Git").shortcut("⌘⌥A").into());
            items.push(MenuAction::new("delete", "Delete…").danger().into());
        }
        if one_file {
            items.push(MenuItem::Separator);
            items.push(MenuAction::new("diff", "Show Diff").into());
            items.push(MenuAction::new("jump", "Jump to Source").into());
            items.push(MenuAction::new("copy-path", "Copy Path").into());
            items.push(MenuAction::new("copy-relative", "Copy Relative Path").into());
        }
        items.push(MenuItem::Separator);
        let all_checked = self.state(&members) == Check::All;
        items.push(MenuAction::new("check", if all_checked { "Uncheck" } else { "Check" }).into());
        self.menu = None;
        let menu = cx.new(|cx| Menu::new("Change actions", items, window, cx));
        let events = cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
            let Some(open) = this.menu.take() else {
                return;
            };
            if let MenuEvent::Picked(id, _) = event {
                this.run_menu_action(id, open.surface, open.row, window, cx);
            }
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            surface,
            row,
            anchor: at,
            menu,
            _events: events,
        });
        cx.notify();
    }

    pub(crate) fn run_menu_action(
        &mut self,
        id: &str,
        surface: Surface,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let members = self.members(row);
        match id {
            "jump" => {
                if let Some(&ix) = members.first() {
                    let path = format!(
                        "{}/{}",
                        self.cwd.trim_end_matches('/'),
                        self.files[ix].relative
                    );
                    window.dispatch_action(Box::new(crate::file_editor::OpenPath(path)), cx);
                    cx.emit(CommitFormEvent::Jumped(surface));
                }
            }
            "rollback" | "delete" => self.ask_rollback(surface, &members, id == "delete", cx),
            "add" => self.add_to_git(&members, cx),
            "diff" => {
                if let Some(&ix) = members.first() {
                    self.select(ix, surface, cx);
                }
            }
            "copy-path" | "copy-relative" => {
                if let Some(&ix) = members.first() {
                    let relative = &self.files[ix].relative;
                    let text = if id == "copy-path" {
                        format!("{}/{relative}", self.cwd.trim_end_matches('/'))
                    } else {
                        relative.clone()
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "check" => self.toggle_files(&members, cx),
            _ => {}
        }
    }

    /// Puts unversioned files under Git (`git add`); they move to Changes, checked.
    pub(crate) fn add_to_git(&mut self, members: &[usize], cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let (_, untracked) = plan(&self.files, members);
        let paths: Vec<String> = untracked
            .iter()
            .map(|&ix| self.files[ix].relative.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.busy = Some("rollback");
        self.error = None;
        let cwd = PathBuf::from(&self.cwd);
        let added = paths.clone();
        let work = cx
            .background_executor()
            .spawn(async move { scm::stage_files(&cwd, &added) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    // What was just added is meant for the commit.
                    Ok(()) => this.checked.extend(paths),
                    Err(error) => this.error = Some(error),
                }
                this.git.update(cx, |git, cx| git.changed(cx));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens the confirmation for rolling back (or deleting) `members`.
    pub(crate) fn ask_rollback(
        &mut self,
        surface: Surface,
        members: &[usize],
        delete: bool,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        let (tracked, untracked) = plan(&self.files, members);
        let pending = Pending {
            surface,
            tracked: if delete {
                Vec::new()
            } else {
                tracked
                    .iter()
                    .map(|&ix| {
                        (
                            self.files[ix].relative.clone(),
                            self.files[ix].status.clone(),
                        )
                    })
                    .collect()
            },
            untracked: if delete {
                untracked
                    .iter()
                    .map(|&ix| self.files[ix].relative.clone())
                    .collect()
            } else {
                Vec::new()
            },
            delete_added: false,
        };
        if pending.tracked.is_empty() && pending.untracked.is_empty() {
            return;
        }
        self.pending = Some(pending);
        cx.notify();
    }

    /// Does what the confirmation described.
    pub(crate) fn confirm_rollback(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.busy = Some("rollback");
        self.error = None;
        let cwd = PathBuf::from(&self.cwd);
        let work = cx.background_executor().spawn(async move {
            if !pending.tracked.is_empty() {
                let paths: Vec<String> = pending.tracked.iter().map(|(p, _)| p.clone()).collect();
                scm::rollback_files(&cwd, &paths, pending.delete_added)?;
            }
            for path in &pending.untracked {
                scm::discard_file(&cwd, path)?;
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.git.update(cx, |git, cx| git.changed(cx));
                if let Err(error) = result {
                    this.error = Some(error);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// ⌘⌥A, ⌘⌥Z and Escape over a confirmation, on the selected file.
    /// Returns whether the key was used.
    pub(crate) fn key(
        &mut self,
        surface: Surface,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let keys = &event.keystroke;
        if keys.key == "escape" && self.pending.is_some() {
            self.pending = None;
            cx.notify();
            return true;
        }
        if !(keys.modifiers.platform && keys.modifiers.alt) {
            return false;
        }
        let Some(ix) = self
            .selected
            .as_ref()
            .and_then(|file| self.files.iter().position(|f| &f.relative == file))
        else {
            return false;
        };
        match keys.key.as_str() {
            "a" => self.add_to_git(&[ix], cx),
            "z" => self.ask_rollback(surface, &[ix], is_untracked(&self.files[ix]), cx),
            _ => return false,
        }
        true
    }
}

// Drawing, shared by both surfaces.
impl CommitForm {
    pub(crate) fn checkbox(id: impl Into<ElementId>, state: Check) -> Stateful<gpui::Div> {
        let ink = background_base();
        div()
            .id(id)
            .role(Role::CheckBox)
            .size(px(14.))
            .flex_shrink_0()
            .rounded(px(3.))
            .border_1()
            .border_color(content(if state == Check::None { 0.35 } else { 0.9 }))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(state != Check::None, |d| d.bg(content(0.9)))
            .when(state == Check::All, |d| {
                d.child(icon("check", 10.).text_color(ink))
            })
            .when(state == Check::Some, |d| {
                d.child(div().w(px(7.)).h(px(2.)).rounded(px(1.)).bg(ink))
            })
    }

    /// The file tree, scrolling.
    pub(crate) fn file_list(form: &Entity<Self>, surface: Surface, cx: &App) -> gpui::UniformList {
        let count = form.read(cx).rows.len();
        let form = form.clone();
        let id = match surface {
            Surface::Panel => "commit-form-rows-panel",
            Surface::Dialog => "commit-form-rows-dialog",
        };
        uniform_list(id, count, move |range, window, cx| {
            let this = form.read(cx);
            range
                .map(|ix| this.row(&form, surface, ix, window))
                .collect::<Vec<_>>()
        })
    }

    fn row(&self, form: &Entity<Self>, surface: Surface, ix: usize, window: &Window) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let base = div()
            .id(("commit-row", ix))
            .h(px(ROW))
            .mx(px(6.))
            .px(px(6.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Right,
                window.listener_for(form, move |this, event: &MouseDownEvent, window, cx| {
                    this.open_menu(surface, ix, event.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        let check = |state: Check, members: Vec<usize>| {
            Self::checkbox(("commit-check", ix), state).on_click(window.listener_for(
                form,
                move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_files(&members, cx)
                },
            ))
        };
        let chevron = |open: bool| {
            icon(
                if open {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                11.,
            )
            .text_color(content(0.45))
        };
        match row {
            Row::Group {
                key,
                title,
                files,
                open,
            } => {
                let count = files.len();
                let fold_key = format!("g:{key}");
                base.text_color(content(0.9))
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|s| s.bg(content(0.05)))
                    .child(chevron(open))
                    .child(check(self.state(&files), files))
                    .child(title)
                    .child(
                        div()
                            .text_color(content(0.4))
                            .child(format!("{count} file{}", if count == 1 { "" } else { "s" })),
                    )
                    .on_click(
                        window.listener_for(form, move |this, _: &ClickEvent, _, cx| {
                            this.toggle_fold(fold_key.clone(), cx)
                        }),
                    )
                    .into_any_element()
            }
            Row::Tree(TreeRow::Dir {
                key,
                name,
                depth,
                files,
                open,
            }) => base
                .pl(px(6. + 14. + depth as f32 * 14.))
                .text_color(content(0.8))
                .hover(|s| s.bg(content(0.05)))
                .child(chevron(open))
                .child(check(self.state(&files), files))
                .child(file_type_icon(&name, true, open, 14.))
                .child(div().min_w_0().truncate().child(name.clone()))
                .on_click(
                    window.listener_for(form, move |this, _: &ClickEvent, _, cx| {
                        this.toggle_fold(key.clone(), cx)
                    }),
                )
                .into_any_element(),
            Row::Tree(TreeRow::File { ix: file_ix, depth }) => {
                let Some(file) = self.files.get(file_ix) else {
                    return div().h(px(ROW)).into_any_element();
                };
                let selected = self.selected.as_ref() == Some(&file.relative);
                let name = basename(&file.relative).to_owned();
                let tint = match file.status.as_str() {
                    "deleted" => Some(rgb(DIFF_DEL)),
                    "added" | "untracked" => Some(rgb(DIFF_ADD)),
                    _ => None,
                };
                base.pl(px(6. + 14. + depth as f32 * 14.))
                    .aria_label(gpui::SharedString::from(file.relative.clone()))
                    .text_color(content(if selected { 1. } else { 0.8 }))
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.06)))
                    .child(div().w(px(11.)).flex_shrink_0())
                    .child(check(self.state(&[file_ix]), vec![file_ix]))
                    .child(file_type_icon(&name, false, false, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when_some(tint, |d, tint| d.text_color(tint))
                            .child(name),
                    )
                    .when(!is_untracked(file), |d| {
                        d.child(diff_counts(
                            file.additions.max(0) as u32,
                            file.deletions.max(0) as u32,
                        ))
                    })
                    .on_click(
                        window.listener_for(form, move |this, _: &ClickEvent, _, cx| {
                            this.select(file_ix, surface, cx)
                        }),
                    )
                    .into_any_element()
            }
        }
    }

    /// The wand in the message box's corner: writes the message, or stops.
    pub(crate) fn generate_button(form: &Entity<Self>, window: &Window, cx: &App) -> AnyElement {
        let this = form.read(cx);
        let generating = this.generating();
        let can_generate = !this.checked.is_empty() && this.busy.is_none();
        div()
            .id("generate-commit-message")
            .role(Role::Button)
            .aria_label(if generating {
                "Cancel commit message generation"
            } else {
                "Generate commit message"
            })
            .group("generate")
            .absolute()
            .top_1()
            .right_1()
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(content(0.1))
            .when(!generating && !can_generate, |d| d.opacity(0.4))
            .when(generating || can_generate, |d| {
                d.hover(|s| s.bg(content(0.2)))
            })
            .child(if generating {
                div()
                    .relative()
                    .size(px(14.))
                    .child(
                        div()
                            .absolute()
                            .group_hover("generate", |s| s.opacity(0.))
                            .child(spinner("generate-spin", "loader", 14.)),
                    )
                    .child(
                        icon("x", 14.)
                            .absolute()
                            .text_color(content(1.))
                            .opacity(0.)
                            .group_hover("generate", |s| s.opacity(1.)),
                    )
                    .into_any_element()
            } else {
                icon("wand", 12.).text_color(content(1.)).into_any_element()
            })
            .when(generating || can_generate, |d| {
                d.on_click(
                    window.listener_for(form, move |this, _: &ClickEvent, _, cx| {
                        if generating {
                            this.cancel_generate(cx);
                        } else {
                            this.generate(cx);
                        }
                    }),
                )
            })
            .into_any_element()
    }

    /// The "Amend last commit" checkbox under the message.
    pub(crate) fn amend_toggle(form: &Entity<Self>, window: &Window, cx: &App) -> AnyElement {
        let this = form.read(cx);
        let amend = this.amending();
        let enabled = this.busy.is_none();
        div()
            .id("commit-amend")
            .role(Role::CheckBox)
            .aria_label("Amend last commit")
            .aria_selected(amend)
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.))
            .text_color(content(0.75))
            .when(!enabled, |d| d.opacity(0.4))
            .child(Self::checkbox(
                "commit-amend-box",
                if amend { Check::All } else { Check::None },
            ))
            .child("Amend last commit")
            .when(enabled, |d| {
                d.cursor_pointer().on_click(
                    window.listener_for(form, |this, _: &ClickEvent, _, cx| this.toggle_amend(cx)),
                )
            })
            .into_any_element()
    }

    /// The last failure, in red.
    pub(crate) fn error_line(&self) -> Option<AnyElement> {
        let error = self.error.clone()?;
        Some(
            div()
                .id("commit-error")
                .role(Role::Status)
                .min_w_0()
                .max_h(px(60.))
                .overflow_hidden()
                .text_size(px(12.))
                .text_color(rgb(0xf87171))
                .child(error)
                .into_any_element(),
        )
    }

    /// The right-click menu, when it was opened on `surface`.
    pub(crate) fn menu_overlay(&self, surface: Surface, cx: &App) -> Option<AnyElement> {
        let open = self.menu.as_ref().filter(|m| m.surface == surface)?;
        Some(
            menu_popover(&open.menu, Anchor::Point(open.anchor), cx)
                .gap(0.)
                .into_any_element(),
        )
    }

    /// The rollback or delete confirmation, when it was asked on `surface`.
    pub(crate) fn pending_overlay(
        form: &Entity<Self>,
        surface: Surface,
        window: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        let pending = form.read(cx).pending.as_ref()?;
        if pending.surface != surface {
            return None;
        }
        let deleting = !pending.untracked.is_empty();
        let added = pending.tracked.iter().any(|(_, status)| status == "added");
        let names: Vec<(String, &str)> = if deleting {
            pending
                .untracked
                .iter()
                .map(|p| (p.clone(), "unversioned"))
                .collect()
        } else {
            pending
                .tracked
                .iter()
                .map(|(p, status)| {
                    (
                        p.clone(),
                        match status.as_str() {
                            "added" => "added",
                            "deleted" => "deleted",
                            _ => "modified",
                        },
                    )
                })
                .collect()
        };
        let count = names.len();
        let title = format!(
            "{} {count} file{}?",
            if deleting { "Delete" } else { "Roll back" },
            if count == 1 { "" } else { "s" }
        );
        let body = if deleting {
            "These files are not under version control. They are removed from the disk and cannot be restored."
        } else {
            "Your changes to these files are discarded and they go back to the last commit. This cannot be undone."
        };
        let delete_added = pending.delete_added;
        let card = modal::card().w(px(480.)).child(
            modal::card_body()
                .gap(px(10.))
                .child(modal::title(title))
                .child(modal::body(body))
                .child(
                    div()
                        .max_h(px(180.))
                        .overflow_hidden()
                        .rounded(px(6.))
                        .bg(content(0.05))
                        .py(px(6.))
                        .children(names.iter().take(8).map(|(path, status)| {
                            div()
                                .px(px(10.))
                                .py(px(2.))
                                .flex()
                                .gap(px(8.))
                                .text_size(px(12.))
                                .child(div().flex_1().min_w_0().truncate().child(path.clone()))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(content(0.4))
                                        .child(*status),
                                )
                        }))
                        .when(count > 8, |d| {
                            d.child(
                                div()
                                    .px(px(10.))
                                    .pt(px(2.))
                                    .text_size(px(11.))
                                    .text_color(content(0.45))
                                    .child(format!("and {} more", count - 8)),
                            )
                        }),
                )
                .when(added && !deleting, |d| {
                    d.child(
                        div()
                            .id("commit-rollback-delete-added")
                            .role(Role::Switch)
                            .aria_label("Also delete the new files from disk")
                            .aria_selected(delete_added)
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(12.))
                            .text_color(content(0.8))
                            .child(Self::checkbox(
                                "commit-rollback-delete-added-box",
                                if delete_added {
                                    Check::All
                                } else {
                                    Check::None
                                },
                            ))
                            .child("Also delete the new files from disk")
                            .on_click(window.listener_for(form, |this, _: &ClickEvent, _, cx| {
                                if let Some(p) = this.pending.as_mut() {
                                    p.delete_added = !p.delete_added;
                                }
                                cx.notify();
                            })),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            crate::project_settings::button("commit-rollback-cancel", "Cancel")
                                .on_click(window.listener_for(
                                    form,
                                    |this, _: &ClickEvent, _, cx| {
                                        this.pending = None;
                                        cx.notify();
                                    },
                                )),
                        )
                        .child(
                            modal::danger_button(
                                "commit-rollback-confirm",
                                if deleting { "Delete" } else { "Roll back" },
                            )
                            .on_click(
                                window.listener_for(form, |this, _: &ClickEvent, _, cx| {
                                    this.confirm_rollback(cx)
                                }),
                            ),
                        ),
                ),
        );
        let weak = form.downgrade();
        Some(
            modal::overlay("commit-rollback", card, move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.pending = None;
                    cx.notify();
                });
            })
            .into_any_element(),
        )
    }
}

#[cfg(test)]
impl Render for CommitForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(relative: &str, status: &str) -> ChangedFile {
        ChangedFile {
            path: format!("/r/{relative}"),
            relative: relative.into(),
            status: status.into(),
            additions: 1,
            deletions: 0,
            staged: false,
            unstaged: true,
        }
    }

    #[test]
    fn tree_merges_single_child_folders_lists_folders_first_and_folds() {
        let files = vec![
            file("src/app/a.rs", "modified"),
            file("src/app/b.rs", "modified"),
            file("README.md", "modified"),
        ];
        let mut root = Node::default();
        for (ix, f) in files.iter().enumerate() {
            root.insert(&f.relative.split('/').collect::<Vec<_>>(), ix);
        }
        let mut rows = vec![];
        root.flatten(&files, "tracked", "", 0, &HashSet::new(), &mut rows);
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                TreeRow::Dir {
                    name, depth, files, ..
                } => format!("dir {depth} {name} {}", files.len()),
                TreeRow::File { ix, depth } => {
                    format!("file {depth} {}", basename(&files[*ix].relative))
                }
            })
            .collect();
        assert_eq!(
            shape,
            [
                "dir 0 src/app 2",
                "file 1 a.rs",
                "file 1 b.rs",
                "file 0 README.md"
            ]
        );
        let mut root = Node::default();
        for (ix, f) in files.iter().enumerate() {
            root.insert(&f.relative.split('/').collect::<Vec<_>>(), ix);
        }
        let mut rows = vec![];
        let folded = HashSet::from(["d:tracked:src/app".to_string()]);
        root.flatten(&files, "tracked", "", 0, &folded, &mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn plan_splits_tracked_from_unversioned() {
        let files = vec![
            file("a.rs", "modified"),
            file("b.rs", "untracked"),
            file("c.rs", "added"),
        ];
        assert_eq!(plan(&files, &[0, 1, 2]), (vec![0, 2], vec![1]));
        assert_eq!(plan(&files, &[1]), (vec![], vec![1]));
    }

    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .env("GIT_AUTHOR_NAME", "t")
                    .env("GIT_AUTHOR_EMAIL", "t@t")
                    .env("GIT_COMMITTER_NAME", "t")
                    .env("GIT_COMMITTER_EMAIL", "t@t")
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bravo\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "first"]);
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        std::fs::remove_file(dir.path().join("b.txt")).unwrap();
        std::fs::write(dir.path().join("loose.txt"), "x\n").unwrap();
        dir
    }

    fn open(cx: &mut gpui::TestAppContext, cwd: String) -> gpui::WindowHandle<CommitForm> {
        cx.update(input::bind);
        cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let git = cx.new(|cx| ProjectGit::new(cwd, cx));
                cx.new(|cx| CommitForm::new(git, window, cx))
            })
            .unwrap()
        })
    }
    fn settle(cx: &mut gpui::TestAppContext) {
        for _ in 0..5 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(300));
            cx.run_until_parked();
        }
    }
    fn row_of(form: &CommitForm, relative: &str) -> usize {
        let file = form
            .files
            .iter()
            .position(|f| f.relative == relative)
            .unwrap();
        form.rows
            .iter()
            .position(|r| matches!(r, Row::Tree(TreeRow::File { ix, .. }) if *ix == file))
            .unwrap()
    }

    #[gpui::test]
    fn add_to_git_moves_an_unversioned_file_to_changes_checked(cx: &mut gpui::TestAppContext) {
        let dir = repo();
        let window = open(cx, dir.path().to_string_lossy().to_string());
        settle(cx);
        window
            .update(cx, |form, window, cx| {
                assert!(!form.checked.contains("loose.txt"));
                // Tracked changes start checked, unversioned files do not.
                assert!(form.checked.contains("a.txt") && form.checked.contains("b.txt"));
                let row = row_of(form, "loose.txt");
                form.open_menu(
                    Surface::Panel,
                    row,
                    gpui::point(px(10.), px(10.)),
                    window,
                    cx,
                );
                form.run_menu_action("add", Surface::Panel, row, window, cx);
                assert_eq!(form.busy, Some("rollback"));
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |form, _, _| {
                assert_eq!(form.busy, None);
                let loose = form
                    .files
                    .iter()
                    .find(|f| f.relative == "loose.txt")
                    .unwrap();
                assert_eq!(loose.status, "added");
                assert!(form.checked.contains("loose.txt"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn rolling_back_asks_first_then_restores_the_file_and_the_list_follows(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = repo();
        let window = open(cx, dir.path().to_string_lossy().to_string());
        settle(cx);
        window
            .update(cx, |form, window, cx| {
                let names: Vec<&str> = form.files.iter().map(|f| f.relative.as_str()).collect();
                assert_eq!(names, ["a.txt", "b.txt", "loose.txt"], "{names:?}");
                // A right click offers Rollback for a tracked file, and asks first.
                let row = row_of(form, "a.txt");
                form.open_menu(
                    Surface::Dialog,
                    row,
                    gpui::point(px(10.), px(10.)),
                    window,
                    cx,
                );
                assert!(form.menu_overlay(Surface::Dialog, cx).is_some());
                assert!(form.menu_overlay(Surface::Panel, cx).is_none());
                form.run_menu_action("rollback", Surface::Dialog, row, window, cx);
                let pending = form.pending.as_ref().expect("a confirmation");
                assert_eq!(pending.surface, Surface::Dialog);
                assert_eq!(
                    pending.tracked,
                    [("a.txt".to_owned(), "modified".to_owned())]
                );
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
                    "changed\n",
                    "nothing happens before the user agrees"
                );
                form.pending = None;
                // A folder or group scope covers its files; an unversioned file is only deleted.
                form.ask_rollback(Surface::Panel, &[0, 1, 2], false, cx);
                let pending = form.pending.as_ref().unwrap();
                assert_eq!(
                    pending.tracked.len(),
                    2,
                    "unversioned files are not rolled back"
                );
                assert!(pending.untracked.is_empty());
                form.ask_rollback(Surface::Panel, &[2], true, cx);
                assert_eq!(form.pending.as_ref().unwrap().untracked, ["loose.txt"]);
                form.pending = None;
                form.ask_rollback(Surface::Panel, &[0, 1], false, cx);
                form.confirm_rollback(cx);
                assert_eq!(form.busy, Some("rollback"));
            })
            .unwrap();
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "bravo\n"
        );
        assert!(dir.path().join("loose.txt").exists());
        window
            .update(cx, |form, _, _| {
                assert_eq!(form.busy, None);
                assert!(form.error.is_none(), "{:?}", form.error);
                let names: Vec<&str> = form.files.iter().map(|f| f.relative.as_str()).collect();
                assert_eq!(
                    names,
                    ["loose.txt"],
                    "the list follows: only the unversioned file is left"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn commit_takes_only_the_checked_files(cx: &mut gpui::TestAppContext) {
        let dir = repo();
        let window = open(cx, dir.path().to_string_lossy().to_string());
        settle(cx);
        window
            .update(cx, |form, window, cx| {
                // Uncheck b.txt (the deletion): only a.txt goes in.
                let b = form
                    .files
                    .iter()
                    .position(|f| f.relative == "b.txt")
                    .unwrap();
                form.toggle_files(&[b], cx);
                assert_eq!(form.commit_label(), "Commit");
                form.set_message("Only a".into(), cx);
                assert!(form.can_commit(cx));
                form.commit(false, false, window, cx);
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |form, _, cx| {
                assert_eq!(form.busy, None);
                assert!(form.error.is_none(), "{:?}", form.error);
                assert!(form.message.read(cx).content.is_empty());
                let names: Vec<&str> = form.files.iter().map(|f| f.relative.as_str()).collect();
                assert_eq!(names, ["b.txt", "loose.txt"], "{names:?}");
                assert!(
                    !form.checked.contains("b.txt"),
                    "an unchecked file stays unchecked"
                );
            })
            .unwrap();
    }
}
