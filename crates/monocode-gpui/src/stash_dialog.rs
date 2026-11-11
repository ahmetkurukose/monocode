//! Stash: shelve the working tree's changes, and manage what is shelved: look
//! inside an entry (its files and diffs), apply or pop it, drop it, branch
//! from it. One window for both, as the IDEs' "Stash Changes" and "Unstash".
use crate::{
    change_tree::{Node, TreeRow, basename},
    dialog_resize::{self, DialogResize},
    diff_view::{DiffFile, DiffView, diff_counts},
    file_icons::file_type_icon,
    git_status::ProjectGit,
    input::{self, Input},
    modal,
    right_panel::{DIFF_ADD, DIFF_DEL, stroke},
    theme::*,
    update_dialog::{age, model},
};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    Role, SharedString, Subscription, Task, Window, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::{
    diff,
    scm::{self, ChangedFile, StashEntry},
};
use std::{collections::HashSet, path::PathBuf};

const ROW: f32 = 26.;
/// The size the user last dragged the window to; the next one opens that big.
static REMEMBERED_SIZE: dialog_resize::Remembered = dialog_resize::remembered();

pub(crate) enum StashDialogEvent {
    Close,
    /// Something changed in the repository; the text says what.
    Changed(String),
}

/// What the window puts first when it opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Intent {
    /// "Stash Changes…": the message field.
    Stash,
    /// "Unstash Changes…": the list.
    Manage,
}

/// A destructive step waiting for a second click.
#[derive(Clone, PartialEq, Eq)]
enum Armed {
    Drop(String),
    Clear,
}

pub(crate) struct StashDialog {
    git: Entity<ProjectGit>,
    cwd: String,
    intent: Intent,
    entries: Vec<StashEntry>,
    loaded: bool,
    selected: Option<String>,
    files: Vec<ChangedFile>,
    rows: Vec<TreeRow>,
    collapsed: HashSet<String>,
    file: Option<String>,
    diff_view: Entity<DiffView>,
    diff_request: u64,
    files_request: u64,
    message: Entity<Input>,
    branch: Entity<Input>,
    branching: bool,
    include_untracked: bool,
    keep_index: bool,
    reinstate_index: bool,
    armed: Option<Armed>,
    busy: bool,
    error: Option<String>,
    size: DialogResize,
    tasks: Vec<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<StashDialogEvent> for StashDialog {}
impl dialog_resize::Resizable for StashDialog {
    fn dialog_resize(&mut self) -> &mut DialogResize {
        &mut self.size
    }
}
impl Focusable for StashDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl StashDialog {
    pub(crate) fn new(
        git: Entity<ProjectGit>,
        cwd: String,
        intent: Intent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let diff_view = cx.new(DiffView::new);
        diff_view.update(cx, |view, cx| {
            view.allow_split = true;
            view.set_split(true, cx);
        });
        let message = cx.new(|cx| {
            let mut input = Input::field("Stash message", "Message (optional)", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        let branch = cx.new(|cx| {
            let mut input = Input::field("Branch name", "New branch name", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        let subscriptions = vec![
            cx.subscribe(&message, |this, _, _: &input::Submitted, cx| {
                this.stash_now(cx)
            }),
            cx.subscribe(&branch, |this, _, _: &input::Submitted, cx| {
                this.create_branch(cx)
            }),
            cx.subscribe(&message, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe(&branch, |_, _, _: &input::Changed, cx| cx.notify()),
        ];
        let focus = cx.focus_handle();
        if intent == Intent::Stash {
            window.focus(&message.focus_handle(cx), cx);
        } else {
            window.focus(&focus, cx);
        }
        let mut this = Self {
            git,
            cwd,
            intent,
            entries: vec![],
            loaded: false,
            selected: None,
            files: vec![],
            rows: vec![],
            collapsed: HashSet::new(),
            file: None,
            diff_view,
            diff_request: 0,
            files_request: 0,
            message,
            branch,
            branching: false,
            include_untracked: false,
            keep_index: false,
            reinstate_index: false,
            armed: None,
            busy: false,
            error: None,
            size: DialogResize::new(&REMEMBERED_SIZE).with_usual_min_height(440.),
            tasks: vec![],
            focus,
            _subscriptions: subscriptions,
        };
        this.git.update(cx, |git, cx| git.changed(cx));
        this.refresh(cx);
        this
    }

    fn has_changes(&self, cx: &gpui::App) -> bool {
        self.git
            .read(cx)
            .index
            .as_ref()
            .is_some_and(|index| !index.files.is_empty())
    }

    fn entry(&self) -> Option<&StashEntry> {
        let sha = self.selected.as_deref()?;
        self.entries.iter().find(|entry| entry.sha == sha)
    }

    /// Reads the list again, keeping the selection while its entry exists.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let cwd = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::stash_list(&cwd) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = true;
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        let keep = this
                            .selected
                            .as_ref()
                            .is_some_and(|sha| this.entries.iter().any(|e| &e.sha == sha));
                        if !keep {
                            this.selected = this.entries.first().map(|e| e.sha.clone());
                            this.load_files(cx);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn select(&mut self, sha: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&sha) {
            return;
        }
        self.selected = Some(sha);
        self.armed = None;
        self.error = None;
        self.load_files(cx);
        cx.notify();
    }

    fn load_files(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        self.rows.clear();
        self.file = None;
        let Some(sha) = self.selected.clone() else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        self.files_request += 1;
        let request = self.files_request;
        let cwd = PathBuf::from(&self.cwd);
        let work = cx
            .background_executor()
            .spawn(async move { scm::stash_files(&cwd, &sha) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.files_request != request {
                    return;
                }
                match result {
                    Ok(files) => {
                        this.files = files;
                        this.rebuild_rows();
                        this.file = this.files.first().map(|f| f.relative.clone());
                        this.load_diff(cx);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn rebuild_rows(&mut self) {
        let mut root = Node::default();
        for (ix, file) in self.files.iter().enumerate() {
            root.insert(&file.relative.split('/').collect::<Vec<_>>(), ix);
        }
        let mut rows = Vec::new();
        root.flatten(&self.files, "stash", "", 0, &self.collapsed, &mut rows);
        self.rows = rows;
    }

    fn select_file(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.files.get(ix).map(|f| f.relative.clone()) else {
            return;
        };
        if self.file.as_ref() == Some(&path) {
            return;
        }
        self.file = Some(path);
        self.load_diff(cx);
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(relative), Some(sha)) = (self.file.clone(), self.selected.clone()) else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        self.diff_request += 1;
        let request = self.diff_request;
        let cwd = PathBuf::from(&self.cwd);
        let id: SharedString = format!("stash-{relative}").into();
        self.diff_view.update(cx, |view, cx| {
            view.set_files(vec![model(&id, &relative, None)], cx)
        });
        let path = relative.clone();
        let work = cx.background_executor().spawn(async move {
            scm::stash_file_diff(&cwd, &sha, &path).map(|file| {
                let unified = (!file.binary && !file.too_large).then(|| {
                    diff::build_unified_file(&file.original, &file.current, diff::CONTEXT_DEFAULT)
                });
                (file, unified)
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_request != request {
                    return;
                }
                let file = match result {
                    Ok(loaded) => model(&id, &relative, Some(loaded)),
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

    /// Runs `work` off the thread; `done` sees its result back on the dialog.
    fn run<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(PathBuf) -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.armed = None;
        let cwd = PathBuf::from(&self.cwd);
        let work = cx.background_executor().spawn(async move { work(cwd) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.git.update(cx, |git, cx| git.changed(cx));
                done(this, result, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn stash_now(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.has_changes(cx) {
            return;
        }
        let message = self.message.read(cx).content.trim().to_owned();
        let (untracked, keep) = (self.include_untracked, self.keep_index);
        let label = if message.is_empty() {
            "Stashed changes".to_owned()
        } else {
            format!("Stashed “{message}”")
        };
        let from_form = self.intent == Intent::Stash;
        self.run(
            move |cwd| scm::stash_push(&cwd, &message, untracked, keep),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.message
                        .update(cx, |input, cx| input.set_text(String::new(), cx));
                    cx.emit(StashDialogEvent::Changed(label));
                    if from_form {
                        cx.emit(StashDialogEvent::Close);
                    } else {
                        this.refresh(cx);
                    }
                }
                Err(error) => this.error = Some(error),
            },
            cx,
        );
    }

    /// Brings the selected entry back: `pop` also drops it.
    fn apply(&mut self, pop: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.entry().cloned() else {
            return;
        };
        let reinstate = self.reinstate_index;
        let sha = entry.sha.clone();
        let label = if pop { "Popped" } else { "Applied" };
        let name = entry.message.clone();
        self.run(
            move |cwd| scm::stash_apply(&cwd, &sha, pop, reinstate),
            move |this, result, cx| match result {
                Ok(outcome) if outcome.conflicts.is_empty() => {
                    cx.emit(StashDialogEvent::Changed(format!("{label} “{name}”")));
                    cx.emit(StashDialogEvent::Close);
                }
                Ok(outcome) => {
                    cx.emit(StashDialogEvent::Changed(format!(
                        "{} while applying “{name}”",
                        plural(outcome.conflicts.len(), "conflict", "conflicts")
                    )));
                    this.error = Some(format!(
                        "These files conflict, and the stash was kept: {}. Resolve them in your editor.",
                        outcome.conflicts.join(", ")
                    ));
                    this.refresh(cx);
                }
                Err(error) => this.error = Some(error),
            },
            cx,
        );
    }

    fn drop_selected(&mut self, cx: &mut Context<Self>) {
        let Some(sha) = self.selected.clone() else {
            return;
        };
        if self.armed != Some(Armed::Drop(sha.clone())) {
            self.armed = Some(Armed::Drop(sha));
            cx.notify();
            return;
        }
        self.selected = None;
        self.run(
            move |cwd| scm::stash_drop(&cwd, &sha),
            |this, result, cx| {
                match result {
                    Ok(()) => cx.emit(StashDialogEvent::Changed("Dropped a stash".into())),
                    Err(error) => this.error = Some(error),
                }
                this.refresh(cx);
            },
            cx,
        );
    }

    fn clear_all(&mut self, cx: &mut Context<Self>) {
        if self.armed != Some(Armed::Clear) {
            self.armed = Some(Armed::Clear);
            cx.notify();
            return;
        }
        self.selected = None;
        self.run(
            |cwd| scm::stash_clear(&cwd),
            |this, result, cx| {
                match result {
                    Ok(()) => cx.emit(StashDialogEvent::Changed("Cleared all stashes".into())),
                    Err(error) => this.error = Some(error),
                }
                this.refresh(cx);
            },
            cx,
        );
    }

    fn toggle_branching(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.branching = !self.branching;
        if self.branching {
            window.focus(&self.branch.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn create_branch(&mut self, cx: &mut Context<Self>) {
        let Some(sha) = self.selected.clone() else {
            return;
        };
        let name = self.branch.read(cx).content.trim().to_owned();
        if name.is_empty() {
            self.error = Some("Enter a branch name".into());
            cx.notify();
            return;
        }
        let label = format!("Created branch {name} from a stash");
        self.run(
            move |cwd| scm::stash_branch(&cwd, &sha, &name),
            move |this, result, cx| match result {
                Ok(()) => {
                    cx.emit(StashDialogEvent::Changed(label));
                    cx.emit(StashDialogEvent::Close);
                }
                Err(error) => this.error = Some(error),
            },
            cx,
        );
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" || self.busy {
            return;
        }
        if self.armed.take().is_some() {
            cx.notify();
        } else if self.branching {
            self.toggle_branching(window, cx);
        } else {
            cx.emit(StashDialogEvent::Close);
        }
        cx.stop_propagation();
    }

    fn toggle(
        id: &'static str,
        label: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        flip: impl Fn(&mut Self) + 'static,
    ) -> AnyElement {
        let ink = background_base();
        div()
            .id(id)
            .role(Role::CheckBox)
            .aria_label(label)
            .aria_selected(on)
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .text_color(content(0.75))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(content(if on { 0.9 } else { 0.35 }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(on, |d| {
                        d.bg(content(0.9)).child(icon("check", 10.).text_color(ink))
                    }),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                flip(this);
                cx.notify();
            }))
            .into_any_element()
    }

    fn entry_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.entries[ix];
        let sha = entry.sha.clone();
        let selected = self.selected.as_deref() == Some(entry.sha.as_str());
        div()
            .id(("stash-entry", ix))
            .debug_selector(move || format!("stash-entry-{ix}"))
            .role(Role::Button)
            .aria_label(entry.message.clone())
            .aria_selected(selected)
            .mx(px(6.))
            .px(px(8.))
            .py(px(5.))
            .rounded(px(5.))
            .cursor_pointer()
            .when(selected, |d| d.bg(content(0.1)))
            .hover(|s| s.bg(content(0.06)))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.9))
                    .truncate()
                    .child(entry.message.clone()),
            )
            .child(
                div()
                    .mt(px(2.))
                    .flex()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .text_color(content(0.4))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(if entry.branch.is_empty() {
                                "—".to_owned()
                            } else {
                                entry.branch.clone()
                            }),
                    )
                    .child(div().child(format!("· {}", age(entry.timestamp)))),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(sha.clone(), cx)))
            .into_any_element()
    }

    fn file_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let base = div()
            .id(("stash-row", ix))
            .h(px(ROW))
            .mx(px(6.))
            .px(px(6.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .cursor_pointer();
        match row {
            TreeRow::Dir {
                key,
                name,
                depth,
                open,
                ..
            } => base
                .pl(px(6. + depth as f32 * 14.))
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
                .child(file_type_icon(&name, true, open, 14.))
                .child(div().min_w_0().truncate().child(name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.remove(&key) {
                        this.collapsed.insert(key.clone());
                    }
                    this.rebuild_rows();
                    cx.notify();
                }))
                .into_any_element(),
            TreeRow::File { ix: file_ix, depth } => {
                let Some(file) = self.files.get(file_ix).cloned() else {
                    return base.into_any_element();
                };
                let selected = self.file.as_deref() == Some(file.relative.as_str());
                let name = basename(&file.relative).to_owned();
                let tint = match file.status.as_str() {
                    "deleted" => Some(rgb(DIFF_DEL)),
                    "added" => Some(rgb(DIFF_ADD)),
                    _ => None,
                };
                base.pl(px(6. + 14. + depth as f32 * 14.))
                    .text_color(content(if selected { 1. } else { 0.8 }))
                    .when(selected, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.06)))
                    .child(file_type_icon(&name, false, false, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when_some(tint, |d, tint| d.text_color(tint))
                            .child(name),
                    )
                    .child(diff_counts(
                        file.additions.max(0) as u32,
                        file.deletions.max(0) as u32,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_file(file_ix, cx)))
                    .into_any_element()
            }
        }
    }
}

impl gpui::Render for StashDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = self.size.size(window.viewport_size());
        // The form, header and buttons take about this much of the height.
        let split_room = height - 200.;
        let top = self.size.top_height(split_room, height * 0.26);
        let entity = cx.entity();
        let count = self.rows.len();
        let has_changes = self.has_changes(cx);
        let has_entry = self.entry().is_some();
        let busy = self.busy;
        let subtitle = if !self.loaded {
            "Reading stashes…".to_owned()
        } else if self.entries.is_empty() {
            "No stashes".to_owned()
        } else {
            plural(self.entries.len(), "stash", "stashes")
        };

        let form = div()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(stroke())
            .child(if has_changes {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(content(0.12))
                            .bg(content(0.04))
                            .px_2()
                            .py(px(5.))
                            .child(self.message.clone()),
                    )
                    .child(Self::toggle(
                        "stash-untracked",
                        "Include untracked",
                        self.include_untracked,
                        cx,
                        |this| this.include_untracked = !this.include_untracked,
                    ))
                    .child(Self::toggle(
                        "stash-keep-index",
                        "Keep staged",
                        self.keep_index,
                        cx,
                        |this| this.keep_index = !this.keep_index,
                    ))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(content(0.45))
                    .child("No local changes to stash")
                    .into_any_element()
            })
            .child(
                modal::disabled(
                    modal::primary_button("stash-now", "Stash Changes"),
                    busy || !has_changes,
                )
                .debug_selector(|| "stash-now".into())
                .on_click(cx.listener(|this, _, _, cx| this.stash_now(cx))),
            );

        let body: AnyElement = if !self.loaded {
            div().flex_1().into_any_element()
        } else if self.entries.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(content(0.5))
                .child("Nothing is stashed. Stash Changes puts the working tree's edits aside.")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .id("stash-entries")
                        .w(px(width * 0.34))
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .py_1()
                        .border_r_1()
                        .border_color(stroke())
                        .children((0..self.entries.len()).map(|ix| self.entry_row(ix, cx))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(top))
                                .flex_shrink_0()
                                .border_b_1()
                                .border_color(stroke())
                                .flex()
                                .flex_col()
                                .child(if count == 0 {
                                    div()
                                        .p_4()
                                        .text_size(px(12.))
                                        .text_color(content(0.45))
                                        .child("No file changes")
                                        .into_any_element()
                                } else {
                                    uniform_list("stash-files", count, move |range, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            range
                                                .map(|ix| this.file_row(ix, cx))
                                                .collect::<Vec<_>>()
                                        })
                                    })
                                    .py_1()
                                    .flex_1()
                                    .min_h_0()
                                    .into_any_element()
                                }),
                        )
                        .child(dialog_resize::split_handle(
                            "stash-split",
                            top,
                            split_room,
                            cx,
                        ))
                        .child(div().flex_1().min_h_0().child(self.diff_view.clone())),
                )
                .into_any_element()
        };

        let armed_drop =
            matches!(&self.armed, Some(Armed::Drop(sha)) if self.selected.as_ref() == Some(sha));
        let armed_clear = self.armed == Some(Armed::Clear);
        let footer = div()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(stroke())
            .when(self.branching, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(content(0.12))
                                .bg(content(0.04))
                                .px_2()
                                .py(px(5.))
                                .child(self.branch.clone()),
                        )
                        .child(
                            modal::disabled(
                                modal::primary_button("stash-branch-create", "Create Branch"),
                                busy,
                            )
                            .debug_selector(|| "stash-branch-create".into())
                            .on_click(cx.listener(|this, _, _, cx| this.create_branch(cx))),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        modal::disabled(
                            modal::primary_button("stash-apply", "Apply"),
                            busy || !has_entry,
                        )
                        .debug_selector(|| "stash-apply".into())
                        .on_click(cx.listener(|this, _, _, cx| this.apply(false, cx))),
                    )
                    .child(
                        modal::disabled(
                            modal::ghost_button("stash-pop", "Pop"),
                            busy || !has_entry,
                        )
                        .debug_selector(|| "stash-pop".into())
                        .on_click(cx.listener(|this, _, _, cx| this.apply(true, cx))),
                    )
                    .child(Self::toggle(
                        "stash-reinstate",
                        "Restore staged state",
                        self.reinstate_index,
                        cx,
                        |this| this.reinstate_index = !this.reinstate_index,
                    ))
                    .child(div().flex_1())
                    .child(
                        modal::disabled(
                            modal::ghost_button("stash-branch", "Branch…"),
                            busy || !has_entry,
                        )
                        .debug_selector(|| "stash-branch".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_branching(window, cx)),
                        ),
                    )
                    .child(
                        modal::disabled(
                            modal::danger_button(
                                "stash-drop",
                                if armed_drop {
                                    "Click again to drop"
                                } else {
                                    "Drop"
                                },
                            ),
                            busy || !has_entry,
                        )
                        .debug_selector(|| "stash-drop".into())
                        .on_click(cx.listener(|this, _, _, cx| this.drop_selected(cx))),
                    )
                    .child(
                        modal::disabled(
                            modal::danger_button(
                                "stash-clear",
                                if armed_clear {
                                    "Click again to clear"
                                } else {
                                    "Clear all"
                                },
                            ),
                            busy || self.entries.is_empty(),
                        )
                        .debug_selector(|| "stash-clear".into())
                        .on_click(cx.listener(|this, _, _, cx| this.clear_all(cx))),
                    )
                    .child(
                        modal::disabled(modal::ghost_button("stash-close", "Close"), busy)
                            .debug_selector(|| "stash-close".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(StashDialogEvent::Close))),
                    ),
            )
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(0xf87171))
                        .child(error),
                )
            });

        let card = modal::card()
            .debug_selector(|| "stash-card".into())
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("stash-header")
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(stroke())
                    .on_click(cx.listener(dialog_resize::header_click))
                    .child(modal::title("Stash"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(content(0.5))
                            .child(subtitle),
                    )
                    .child(dialog_resize::maximize_button(
                        "stash-maximize",
                        self.size.maximized(),
                        cx,
                    )),
            )
            .child(form)
            .child(body)
            .child(footer)
            .child(dialog_resize::corner_grip(
                "stash-resize",
                (width, height),
                cx,
            ));
        div()
            .size_full()
            .child(dialog_resize::drag_listener(cx))
            .track_focus(&self.focus)
            .key_context("StashDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("stash-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if !this.busy {
                            cx.emit(StashDialogEvent::Close);
                        }
                    });
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle};
    use std::{cell::RefCell, rc::Rc};

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap()
                .success()
        );
    }

    /// A repository with a commit and an uncommitted edit.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "--quiet", "-m", "first"]);
        std::fs::write(dir.path().join("a.txt"), "one\nchanged\n").unwrap();
        dir
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..6 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(300));
            cx.run_until_parked();
        }
    }

    fn open(
        cx: &mut TestAppContext,
        dir: &tempfile::TempDir,
        intent: Intent,
    ) -> (WindowHandle<StashDialog>, Rc<RefCell<Vec<String>>>) {
        cx.update(input::bind);
        let cwd = dir.path().to_string_lossy().to_string();
        let events = Rc::new(RefCell::new(Vec::new()));
        let log = events.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let git = cx.new(|cx| ProjectGit::new(cwd.clone(), cx));
                let dialog = cx.new(|cx| StashDialog::new(git, cwd, intent, window, cx));
                let subscription = cx.subscribe(&dialog, move |_, event: &StashDialogEvent, _| {
                    log.borrow_mut().push(match event {
                        StashDialogEvent::Close => "close".to_owned(),
                        StashDialogEvent::Changed(what) => what.clone(),
                    });
                });
                std::mem::forget(subscription);
                dialog
            })
            .unwrap()
        });
        settle(cx);
        (window, events)
    }

    fn click(visual: &mut VisualTestContext, id: &'static str) {
        let bounds = visual
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} is on screen"));
        visual.simulate_click(bounds.center(), Default::default());
    }

    #[gpui::test]
    fn stashing_from_the_form_shelves_the_changes_and_closes(cx: &mut TestAppContext) {
        let dir = repo();
        let (window, events) = open(cx, &dir, Intent::Stash);
        window
            .update(cx, |dialog, _, cx| {
                assert!(dialog.has_changes(cx), "the edit is seen");
                dialog
                    .message
                    .update(cx, |input, cx| input.set_text("half done".into(), cx));
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        click(visual, "stash-now");
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );
        let list = scm::stash_list(dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].message, "half done");
        assert_eq!(*events.borrow(), ["Stashed “half done”", "close"]);
    }

    #[gpui::test]
    fn the_list_shows_entries_and_apply_brings_one_back_and_keeps_it(cx: &mut TestAppContext) {
        let dir = repo();
        scm::stash_push(dir.path(), "wip", false, false).unwrap();
        let (window, events) = open(cx, &dir, Intent::Manage);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        assert!(visual.debug_bounds("stash-entry-0").is_some());
        window
            .update(visual, |dialog, _, _| {
                assert_eq!(dialog.files.len(), 1, "its files are listed");
                assert_eq!(dialog.file.as_deref(), Some("a.txt"));
            })
            .unwrap();
        click(visual, "stash-apply");
        settle(cx);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\nchanged\n"
        );
        assert_eq!(
            scm::stash_list(dir.path()).unwrap().len(),
            1,
            "apply keeps it"
        );
        assert_eq!(*events.borrow(), ["Applied “wip”", "close"]);
    }

    #[gpui::test]
    fn pop_drops_the_entry_it_applied(cx: &mut TestAppContext) {
        let dir = repo();
        scm::stash_push(dir.path(), "wip", false, false).unwrap();
        let (window, _) = open(cx, &dir, Intent::Manage);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        click(visual, "stash-pop");
        settle(cx);
        assert!(scm::stash_list(dir.path()).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\nchanged\n"
        );
    }

    #[gpui::test]
    fn dropping_and_clearing_need_a_second_click(cx: &mut TestAppContext) {
        let dir = repo();
        scm::stash_push(dir.path(), "one", false, false).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        scm::stash_push(dir.path(), "two", false, false).unwrap();
        let (window, _) = open(cx, &dir, Intent::Manage);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        click(visual, "stash-drop");
        settle(cx);
        assert_eq!(
            scm::stash_list(dir.path()).unwrap().len(),
            2,
            "armed, not dropped"
        );
        click(visual, "stash-drop");
        settle(cx);
        let left = scm::stash_list(dir.path()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, "one", "the selected newest one went");
        click(visual, "stash-clear");
        settle(cx);
        assert_eq!(scm::stash_list(dir.path()).unwrap().len(), 1, "armed only");
        click(visual, "stash-clear");
        settle(cx);
        assert!(scm::stash_list(dir.path()).unwrap().is_empty());
        window
            .update(visual, |dialog, _, _| assert!(dialog.entries.is_empty()))
            .unwrap();
    }

    #[gpui::test]
    fn branching_from_an_entry_creates_the_branch_and_closes(cx: &mut TestAppContext) {
        let dir = repo();
        scm::stash_push(dir.path(), "wip", false, false).unwrap();
        let (window, events) = open(cx, &dir, Intent::Manage);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        click(visual, "stash-branch");
        visual.run_until_parked();
        window
            .update(visual, |dialog, _, cx| {
                assert!(dialog.branching);
                dialog
                    .branch
                    .update(cx, |input, cx| input.set_text("from-stash".into(), cx));
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "stash-branch-create");
        settle(cx);
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "from-stash");
        assert_eq!(
            *events.borrow(),
            ["Created branch from-stash from a stash", "close"]
        );
    }

    #[gpui::test]
    fn a_clean_tree_cannot_be_stashed_and_an_empty_list_says_so(cx: &mut TestAppContext) {
        let dir = repo();
        git(dir.path(), &["checkout", "--", "a.txt"]);
        let (window, _) = open(cx, &dir, Intent::Manage);
        window
            .update(cx, |dialog, _, cx| {
                assert!(!dialog.has_changes(cx));
                assert!(dialog.entries.is_empty() && dialog.loaded);
                dialog.stash_now(cx);
                assert!(!dialog.busy, "nothing to stash, nothing started");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_window_fills_the_app_on_request_and_grows_by_its_corner(cx: &mut TestAppContext) {
        use gpui::{MouseButton, point};
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
        let dir = repo();
        let (window, _) = open(cx, &dir, Intent::Stash);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let card = visual.debug_bounds("stash-card").expect("the window");
        let viewport = visual.update(|window, _| window.viewport_size());
        click(visual, "stash-maximize");
        visual.run_until_parked();
        let full = viewport - gpui::size(px(24.), px(24.));
        assert_eq!(visual.debug_bounds("stash-card").unwrap().size, full);
        click(visual, "stash-maximize");
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("stash-card").unwrap().size, card.size);
        let grip = visual.debug_bounds("stash-resize").unwrap().center();
        visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
        let to = grip - point(px(40.), px(30.));
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        let smaller = visual.debug_bounds("stash-card").unwrap().size;
        assert_eq!(smaller.width, card.size.width - px(80.));
        assert_eq!(smaller.height, card.size.height - px(60.));
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
    }
}
