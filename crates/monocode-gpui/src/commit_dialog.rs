//! The Commit window: the changed files as a tree with a checkbox each,
//! tracked changes apart from unversioned files, the message beside it, and
//! the checked file's diff below. Only the checked files are committed; every
//! other staged entry stays staged.
use crate::{
    change_tree::{Node, TreeRow, basename},
    diff_view::{DiffFile, DiffView, diff_counts},
    file_icons::file_type_icon,
    git_status::ProjectGit,
    input::{self, Input},
    modal,
    popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, menu_popover},
    right_panel::{DIFF_ADD, DIFF_DEL, stroke},
    theme::*,
};
use gpui::{
    AnyElement, ClipboardItem, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyDownEvent, PromptLevel, Role, SharedString, Stateful, Subscription, Task,
    Window, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::provider::Kind;
use monocode_core::{
    diff,
    scm::{self, ChangedFile, FileDiff},
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const ROW: f32 = 26.;

pub(crate) enum CommitDialogEvent {
    Close,
    /// The commit went through; `Some` carries a failure of the push after it.
    Finished(Option<String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    All,
    Some,
    None,
}

#[derive(Clone)]
enum Row {
    Group {
        key: &'static str,
        title: &'static str,
        files: Vec<usize>,
        open: bool,
    },
    Tree(TreeRow),
}

fn is_untracked(file: &ChangedFile) -> bool {
    file.status == "untracked"
}

enum Outcome {
    Committed(Option<String>),
    Failed(String),
}

/// What a rollback or a delete would touch, before the user agrees.
struct Pending {
    /// Tracked files: changes are dropped (or an added file leaves the index).
    tracked: Vec<(String, String)>,
    /// Unversioned files, deleted from the disk.
    untracked: Vec<String>,
    /// Also remove newly added files from the disk, not just from the index.
    delete_added: bool,
}

/// The files at `members` split into tracked and unversioned ones.
fn plan(files: &[ChangedFile], members: &[usize]) -> (Vec<usize>, Vec<usize>) {
    members
        .iter()
        .copied()
        .partition(|&ix| !is_untracked(&files[ix]))
}

struct OpenMenu {
    row: usize,
    anchor: gpui::Point<gpui::Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}

pub(crate) struct CommitDialog {
    git: Entity<ProjectGit>,
    cwd: String,
    files: Vec<ChangedFile>,
    checked: HashSet<String>,
    /// Files already seen, so a new tracked change starts checked and a
    /// file the user unchecked stays unchecked across refreshes.
    known: HashSet<String>,
    collapsed: HashSet<String>,
    rows: Vec<Row>,
    selected: Option<String>,
    message: Entity<Input>,
    diff_view: Entity<DiffView>,
    diff_request: u64,
    _diff_task: Option<Task<()>>,
    amend: bool,
    text_provider: Kind,
    generate_cancel: Option<Arc<AtomicBool>>,
    busy: Option<&'static str>,
    error: Option<String>,
    menu: Option<OpenMenu>,
    pending: Option<Pending>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<CommitDialogEvent> for CommitDialog {}
impl Focusable for CommitDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CommitDialog {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        cwd: String,
        message: String,
        text_provider: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(Input::commit_message);
        input.update(cx, |m, cx| {
            m.set_placeholder("Commit message", cx);
            if !message.is_empty() {
                m.set_text(message, cx);
            }
        });
        let diff_view = cx.new(DiffView::new);
        diff_view.update(cx, |view, cx| {
            view.allow_split = true;
            view.set_split(true, cx);
        });
        let subscriptions = vec![
            cx.observe(&git, |this, _, cx| this.refresh(cx)),
            cx.subscribe(&input, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe_in(
                &input,
                window,
                |this, _, _: &input::AlternateSubmitted, window, cx| this.commit(false, window, cx),
            ),
        ];
        window.focus(&input.focus_handle(cx), cx);
        let mut this = Self {
            git,
            cwd,
            files: vec![],
            checked: HashSet::new(),
            known: HashSet::new(),
            collapsed: HashSet::new(),
            rows: vec![],
            selected: None,
            message: input,
            diff_view,
            diff_request: 0,
            _diff_task: None,
            amend: false,
            text_provider,
            generate_cancel: None,
            busy: None,
            error: None,
            menu: None,
            pending: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    /// Rereads the file list; new tracked changes start checked.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.git.read(cx).index.clone() else {
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
        let reload = self.files != files;
        self.files = files;
        self.rebuild_rows();
        let keep = self
            .selected
            .as_ref()
            .is_some_and(|path| present.contains(path));
        if !keep {
            self.selected = self.files.first().map(|f| f.relative.clone());
        }
        if reload || !keep {
            self.load_diff(cx);
        }
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

    fn state(&self, members: &[usize]) -> Check {
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
    fn toggle_files(&mut self, members: &[usize], cx: &mut Context<Self>) {
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
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let path = self.files[ix].relative.clone();
        if self.selected.as_ref() == Some(&path) {
            return;
        }
        self.selected = Some(path);
        self.load_diff(cx);
        cx.notify();
    }

    /// The selected file against HEAD, in the diff below.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(relative) = self.selected.clone() else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        self.diff_request += 1;
        let request = self.diff_request;
        let cwd = PathBuf::from(&self.cwd);
        let id: SharedString = format!("commit-{relative}").into();
        let pending = model(&id, &relative, None);
        self.diff_view
            .update(cx, |view, cx| view.set_files(vec![pending], cx));
        let path = relative.clone();
        let work = cx.background_executor().spawn(async move {
            scm::file_diff_head(&cwd, &path).map(|file| {
                let unified = (!file.binary && !file.too_large).then(|| {
                    diff::build_unified_file(&file.original, &file.current, diff::CONTEXT_DEFAULT)
                });
                (file, unified)
            })
        });
        self._diff_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_request != request {
                    return;
                }
                let file = match result {
                    Ok((file, unified)) => model(&id, &relative, Some((file, unified))),
                    Err(error) => DiffFile {
                        empty_message: Some(format!("Couldn’t load diff: {error}")),
                        ..model(&id, &relative, None)
                    },
                };
                this.diff_view.update(cx, |view, cx| {
                    view.set_files(vec![file], cx);
                    view.reveal_file(id.clone(), cx);
                });
                cx.notify();
            });
        }));
    }

    fn can_commit(&self, cx: &gpui::App) -> bool {
        self.busy.is_none()
            && (!self.checked.is_empty() || self.amend)
            && !self.message.read(cx).content.trim().is_empty()
    }
    fn can_push(&self, cx: &gpui::App) -> bool {
        let index = self.git.read(cx).index.as_ref();
        let has_remote = index.is_some_and(|i| i.remote.is_some());
        let diverged = index.is_some_and(|i| i.ahead > 0 && i.behind > 0);
        let head_pushed = index.is_some_and(|i| i.head_pushed);
        self.can_commit(cx) && has_remote && !diverged && (!self.amend || !head_pushed)
    }

    /// A message for exactly the checked files, written by a read-only turn
    /// of the background text model.
    fn generate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checked.is_empty() || self.busy.is_some() {
            return;
        }
        let chosen = cx
            .try_global::<crate::settings::Preferences>()
            .and_then(|p| p.background_text_model());
        let Some(harness) = crate::changes::pick_text_harness(self.text_provider, chosen) else {
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
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                this.generate_cancel = None;
                this.busy = None;
                match result {
                    Ok(text) => this.message.update(cx, |m, cx| m.set_text(text, cx)),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn cancel_generate(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.generate_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
            self.busy = None;
            cx.notify();
        }
    }

    fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        self.amend = !self.amend;
        if self.amend && self.message.read(cx).content.trim().is_empty() {
            let path = PathBuf::from(&self.cwd);
            let work = cx
                .background_executor()
                .spawn(async move { scm::head_message(&path) });
            cx.spawn(async move |this, cx| {
                if let Ok(head) = work.await {
                    let _ = this.update(cx, |this, cx| {
                        if this.amend && this.message.read(cx).content.trim().is_empty() {
                            this.message.update(cx, |m, cx| m.set_text(head, cx));
                        }
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        let allowed = if push {
            self.can_push(cx)
        } else {
            self.can_commit(cx)
        };
        if !allowed {
            return;
        }
        let index = self.git.read(cx).index.clone();
        let mut questions: Vec<String> = Vec::new();
        if push
            && let Some(branch) = index
                .as_ref()
                .filter(|i| i.branch.is_some() && i.branch == i.default_branch)
                .and_then(|i| i.branch.clone())
        {
            questions.push(format!("Push to default branch \"{branch}\"?"));
        }
        if self.amend && index.as_ref().is_some_and(|i| i.head_pushed) {
            questions.push(
                "Amend a commit that is already pushed? You will need a force push from the terminal."
                    .into(),
            );
        }
        let message = self.message.read(cx).content.to_string();
        let mut paths: Vec<String> = self.checked.iter().cloned().collect();
        paths.sort();
        let amend = self.amend;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            for question in questions {
                let Ok(answer) = this.update_in(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        &question,
                        None,
                        &[
                            gpui::PromptButton::ok("OK"),
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
                this.busy = Some(if push { "push" } else { "commit" });
                cx.notify();
                let cwd = PathBuf::from(&this.cwd);
                cx.background_executor().spawn(async move {
                    if let Err(error) = scm::commit_paths(&cwd, &message, &paths, amend) {
                        return Outcome::Failed(error);
                    }
                    if push {
                        return Outcome::Committed(
                            scm::push(&cwd)
                                .err()
                                .map(|e| format!("Committed, but the push failed: {e}")),
                        );
                    }
                    Outcome::Committed(None)
                })
            }) else {
                return;
            };
            let outcome = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.git.update(cx, |git, cx| git.changed(cx));
                match outcome {
                    Outcome::Committed(push_error) => {
                        cx.emit(CommitDialogEvent::Finished(push_error))
                    }
                    Outcome::Failed(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The files a row stands for: a group's, a folder's, or the file itself.
    fn members(&self, ix: usize) -> Vec<usize> {
        match self.rows.get(ix) {
            Some(Row::Group { files, .. }) | Some(Row::Tree(TreeRow::Dir { files, .. })) => {
                files.clone()
            }
            Some(Row::Tree(TreeRow::File { ix, .. })) => vec![*ix],
            None => Vec::new(),
        }
    }

    fn open_menu(
        &mut self,
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
            items.push(MenuAction::new("delete", "Delete…").danger().into());
        }
        if one_file {
            items.push(MenuItem::Separator);
            items.push(MenuAction::new("diff", "Show Diff").into());
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
                this.run_menu_action(id, open.row, window, cx);
            }
            window.focus(&this.focus, cx);
            cx.notify();
        });
        self.menu = Some(OpenMenu {
            row,
            anchor: at,
            menu,
            _events: events,
        });
        cx.notify();
    }

    fn run_menu_action(&mut self, id: &str, row: usize, _: &mut Window, cx: &mut Context<Self>) {
        let members = self.members(row);
        match id {
            "rollback" | "delete" => self.ask_rollback(&members, id == "delete", cx),
            "diff" => {
                if let Some(&ix) = members.first() {
                    self.select(ix, cx);
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

    /// Opens the confirmation for rolling back (or deleting) `members`.
    fn ask_rollback(&mut self, members: &[usize], delete: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let (tracked, untracked) = plan(&self.files, members);
        let pending = Pending {
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
    fn confirm_rollback(&mut self, cx: &mut Context<Self>) {
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

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.pending.is_some() {
            self.pending = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "z"
            && event.keystroke.modifiers.platform
            && event.keystroke.modifiers.alt
            && let Some(file) = self.selected.as_ref()
            && let Some(ix) = self.files.iter().position(|f| &f.relative == file)
        {
            self.ask_rollback(&[ix], is_untracked(&self.files[ix]), cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.generate_cancel.is_some() {
            self.cancel_generate(cx);
            cx.stop_propagation();
        } else if event.keystroke.key == "escape" && self.busy.is_none() {
            cx.emit(CommitDialogEvent::Close);
            cx.stop_propagation();
        }
    }

    fn checkbox(id: impl Into<ElementId>, state: Check) -> Stateful<gpui::Div> {
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

    fn pending_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.pending.as_ref()?;
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
                            .on_click(cx.listener(|this, _, _, cx| {
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
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pending = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            modal::danger_button(
                                "commit-rollback-confirm",
                                if deleting { "Delete" } else { "Roll back" },
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_rollback(cx))),
                        ),
                ),
        );
        Some(
            modal::overlay("commit-rollback", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.pending = None;
                        cx.notify();
                    });
                }
            })
            .into_any_element(),
        )
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
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
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    this.open_menu(ix, event.position, window, cx);
                    cx.stop_propagation();
                }),
            );
        match row {
            Row::Group {
                key,
                title,
                files,
                open,
            } => {
                let state = self.state(&files);
                let count = files.len();
                let fold_key = format!("g:{key}");
                let members = files.clone();
                base.text_color(content(0.9))
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|s| s.bg(content(0.05)))
                    .child(
                        icon(
                            if open {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            11.,
                        )
                        .text_color(content(0.45)),
                    )
                    .child(
                        Self::checkbox(("commit-check", ix), state).on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_files(&members, cx)
                            },
                        )),
                    )
                    .child(title)
                    .child(
                        div()
                            .text_color(content(0.4))
                            .child(format!("{count} file{}", if count == 1 { "" } else { "s" })),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_fold(fold_key.clone(), cx)),
                    )
                    .into_any_element()
            }
            Row::Tree(TreeRow::Dir {
                key,
                name,
                depth,
                files,
                open,
            }) => {
                let state = self.state(&files);
                base.pl(px(6. + 14. + depth as f32 * 14.))
                    .text_color(content(0.8))
                    .hover(|s| s.bg(content(0.05)))
                    .child(
                        icon(
                            if open {
                                "chevron-down"
                            } else {
                                "chevron-right"
                            },
                            11.,
                        )
                        .text_color(content(0.45)),
                    )
                    .child(
                        Self::checkbox(("commit-check", ix), state).on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_files(&files, cx)
                            },
                        )),
                    )
                    .child(file_type_icon(&name, true, open, 14.))
                    .child(div().min_w_0().truncate().child(name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(key.clone(), cx)))
                    .into_any_element()
            }
            Row::Tree(TreeRow::File { ix: file_ix, depth }) => {
                let file = &self.files[file_ix];
                let selected = self.selected.as_deref() == Some(file.relative.as_str());
                let state = self.state(&[file_ix]);
                let name = basename(&file.relative).to_owned();
                let tint = match file.status.as_str() {
                    "deleted" => Some(rgb(DIFF_DEL)),
                    "added" | "untracked" => Some(rgb(DIFF_ADD)),
                    _ => None,
                };
                base.pl(px(6. + 14. + depth as f32 * 14.))
                    .text_color(content(if selected { 1. } else { 0.8 }))
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.06)))
                    .child(div().w(px(11.)).flex_shrink_0())
                    .child(
                        Self::checkbox(("commit-check", ix), state).on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_files(&[file_ix], cx)
                            },
                        )),
                    )
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
                    .on_click(cx.listener(move |this, _, _, cx| this.select(file_ix, cx)))
                    .into_any_element()
            }
        }
    }
}

/// The diff pane's model for one file.
fn model(
    id: &SharedString,
    relative: &str,
    loaded: Option<(FileDiff, Option<diff::UnifiedFile>)>,
) -> DiffFile {
    let (binary, too_large, additions, deletions, blocks, empty_message) = match loaded {
        None => (
            false,
            false,
            0,
            0,
            Arc::new(Vec::new()),
            Some("Loading…".to_string()),
        ),
        Some((file, unified)) => {
            let (additions, deletions, blocks) = unified
                .map(|u| (u.additions, u.deletions, u.blocks))
                .unwrap_or_default();
            (
                file.binary,
                file.too_large,
                additions,
                deletions,
                Arc::new(blocks),
                None,
            )
        }
    };
    DiffFile {
        id: id.clone(),
        path: relative.to_owned(),
        label: relative.to_owned(),
        binary,
        too_large,
        empty_message,
        additions,
        deletions,
        blocks,
        can_stage: false,
        can_discard: false,
        can_stage_hunk: false,
    }
}

impl Render for CommitDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let width = (viewport.width.as_f32() - 80.).clamp(560., 1040.);
        let height = (viewport.height.as_f32() - 80.).clamp(420., 720.);
        let entity = cx.entity();
        let count = self.rows.len();
        let checked = self.checked.len();
        let total = self.files.len();
        let generating = self.generate_cancel.is_some();
        let can_generate = !self.checked.is_empty() && self.busy.is_none();
        let busy = self.busy.is_some();
        let can_commit = self.can_commit(cx);
        let can_push = self.can_push(cx);
        let amend = self.amend;
        let empty = self.files.is_empty();
        let tree = if empty {
            div()
                .flex_1()
                .p_4()
                .text_size(px(12.))
                .text_color(content(0.45))
                .child("No uncommitted changes")
                .into_any_element()
        } else {
            uniform_list("commit-rows", count, move |range, _, cx| {
                entity.update(cx, |this, cx| {
                    range.map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
                })
            })
            .py_1()
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let card = modal::card()
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .relative()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(stroke())
                    .child(modal::title("Commit"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(content(0.45))
                            .child(format!("{checked} of {total} selected")),
                    ),
            )
            .child(
                div()
                    .relative()
                    .h(px(height * 0.38))
                    .flex_shrink_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .when(busy, |d| d.opacity(0.6))
                            .child(tree),
                    )
                    .child(
                        div()
                            .w(px(340.))
                            .flex_shrink_0()
                            .border_l_1()
                            .border_color(stroke())
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .rounded_md()
                                    .bg(content(0.08))
                                    .pl_2()
                                    .pr_8()
                                    .py_1()
                                    .text_size(px(13.))
                                    .text_color(content(1.))
                                    .child(self.message.clone())
                                    .child(
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
                                                            .group_hover("generate", |s| {
                                                                s.opacity(0.)
                                                            })
                                                            .child(crate::right_panel::spinner(
                                                                "generate-spin",
                                                                "loader",
                                                                14.,
                                                            )),
                                                    )
                                                    .child(
                                                        icon("x", 14.)
                                                            .absolute()
                                                            .text_color(content(1.))
                                                            .opacity(0.)
                                                            .group_hover("generate", |s| {
                                                                s.opacity(1.)
                                                            }),
                                                    )
                                                    .into_any_element()
                                            } else {
                                                icon("wand", 12.)
                                                    .text_color(content(1.))
                                                    .into_any_element()
                                            })
                                            .when(generating || can_generate, |d| {
                                                d.on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        if generating {
                                                            this.cancel_generate(cx);
                                                        } else {
                                                            this.generate(window, cx);
                                                        }
                                                    },
                                                ))
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("commit-amend")
                                    .role(Role::CheckBox)
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(12.))
                                    .text_color(content(0.75))
                                    .cursor_pointer()
                                    .child(Self::checkbox(
                                        "commit-amend-box",
                                        if amend { Check::All } else { Check::None },
                                    ))
                                    .child("Amend last commit")
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_amend(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(stroke())
                    .child(self.diff_view.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .max_h(px(40.))
                            .overflow_hidden()
                            .text_size(px(12.))
                            .text_color(rgb(0xf87171))
                            .children(self.error.clone()),
                    )
                    .child(
                        modal::ghost_button("commit-cancel", "Cancel")
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CommitDialogEvent::Close))),
                    )
                    .child(
                        modal::disabled(
                            modal::ghost_button(
                                "commit-only",
                                if self.busy == Some("commit") {
                                    "Committing…"
                                } else {
                                    "Commit"
                                },
                            ),
                            !can_commit,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.commit(false, window, cx)),
                        ),
                    )
                    .child(
                        modal::disabled(
                            modal::primary_button(
                                "commit-push",
                                if self.busy == Some("push") {
                                    "Pushing…"
                                } else {
                                    "Commit and Push"
                                },
                            ),
                            !can_push,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))),
                    ),
            );
        div()
            .size_full()
            .track_focus(&self.focus)
            .key_context("CommitDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("commit-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if this.busy.is_none() {
                            cx.emit(CommitDialogEvent::Close);
                        }
                    });
                }
            }))
            .children(self.pending_overlay(cx))
            .children(
                self.menu
                    .as_ref()
                    .map(|open| menu_popover(&open.menu, Anchor::Point(open.anchor), cx).gap(0.)),
            )
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
                    format!("file {depth} {}", files_name(&files, *ix))
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

    fn files_name(files: &[ChangedFile], ix: usize) -> &str {
        basename(&files[ix].relative)
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

    fn repo() -> tempfile::TempDir {
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
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bravo\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "first"]);
        std::fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        std::fs::remove_file(dir.path().join("b.txt")).unwrap();
        std::fs::write(dir.path().join("loose.txt"), "x\n").unwrap();
        dir
    }

    #[gpui::test]
    fn rolling_back_asks_first_then_restores_the_file_and_the_list_follows(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(input::bind);
        let dir = repo();
        let cwd = dir.path().to_string_lossy().to_string();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let git = cx.new(|cx| ProjectGit::new(cwd.clone(), cx));
                cx.new(|cx| CommitDialog::new(git, cwd, String::new(), Kind::Codex, window, cx))
            })
            .unwrap()
        });
        for _ in 0..5 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(300));
            cx.run_until_parked();
        }
        window
            .update(cx, |dialog, window, cx| {
                let names: Vec<&str> = dialog.files.iter().map(|f| f.relative.as_str()).collect();
                assert_eq!(names, ["a.txt", "b.txt", "loose.txt"], "{names:?}");
                let a = dialog
                    .files
                    .iter()
                    .position(|f| f.relative == "a.txt")
                    .unwrap();
                // A right click offers Rollback for a tracked file, and asks first.
                let row = dialog
                    .rows
                    .iter()
                    .position(|r| matches!(r, Row::Tree(TreeRow::File { ix, .. }) if *ix == a))
                    .unwrap();
                dialog.open_menu(row, gpui::point(px(10.), px(10.)), window, cx);
                assert!(dialog.menu.is_some());
                dialog.run_menu_action("rollback", row, window, cx);
                let pending = dialog.pending.as_ref().expect("a confirmation");
                assert_eq!(
                    pending.tracked,
                    [("a.txt".to_owned(), "modified".to_owned())]
                );
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
                    "changed\n",
                    "nothing happens before the user agrees"
                );
                // Cancelling leaves everything alone.
                dialog.pending = None;
                // A folder or group scope covers its files; an unversioned file is only deleted.
                dialog.ask_rollback(&[0, 1, 2], false, cx);
                let pending = dialog.pending.as_ref().unwrap();
                assert_eq!(
                    pending.tracked.len(),
                    2,
                    "unversioned files are not rolled back"
                );
                assert!(pending.untracked.is_empty());
                dialog.ask_rollback(&[2], true, cx);
                assert_eq!(dialog.pending.as_ref().unwrap().untracked, ["loose.txt"]);
                dialog.pending = None;
                dialog.ask_rollback(&[0, 1], false, cx);
                dialog.confirm_rollback(cx);
                assert_eq!(dialog.busy, Some("rollback"));
            })
            .unwrap();
        for _ in 0..5 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(300));
            cx.run_until_parked();
        }
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
            .update(cx, |dialog, _, _| {
                assert_eq!(dialog.busy, None);
                assert!(dialog.error.is_none(), "{:?}", dialog.error);
                let names: Vec<&str> = dialog.files.iter().map(|f| f.relative.as_str()).collect();
                assert_eq!(
                    names,
                    ["loose.txt"],
                    "the list follows: only the unversioned file is left"
                );
            })
            .unwrap();
        cx.refresh().unwrap();
    }
}
