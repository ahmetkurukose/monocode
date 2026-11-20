//! The Commit window: the commit form (the changed files as a tree with a
//! checkbox each, the message, amend) with the checked file's diff below.
//! The Changes panel shows the same form; see [`crate::commit_form`].
use crate::{
    commit_form::{CommitForm, CommitFormEvent, Surface},
    dialog_resize::{self, DialogResize},
    diff_view::{DiffFile, DiffView},
    modal,
    right_panel::stroke,
    theme::*,
};
use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};
use monocode_core::{
    diff,
    scm::{self, ChangedFile, FileDiff},
};
use std::{path::PathBuf, sync::Arc};

pub(crate) enum CommitDialogEvent {
    Close,
}

pub(crate) struct CommitDialog {
    form: Entity<CommitForm>,
    diff_view: Entity<DiffView>,
    /// The file and the list the diff was loaded for.
    loaded: Option<(String, Vec<ChangedFile>)>,
    diff_request: u64,
    _diff_task: Option<Task<()>>,
    focus: FocusHandle,
    size: DialogResize,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<CommitDialogEvent> for CommitDialog {}
impl dialog_resize::Resizable for CommitDialog {
    fn dialog_resize(&mut self) -> &mut DialogResize {
        &mut self.size
    }
}

/// The size the user last dragged the dialog to; the next one opens that big.
static REMEMBERED_SIZE: dialog_resize::Remembered = dialog_resize::remembered();
impl Focusable for CommitDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CommitDialog {
    pub(crate) fn new(
        form: Entity<CommitForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let diff_view = cx.new(DiffView::new);
        let cwd = form.read(cx).cwd().to_owned();
        diff_view.update(cx, |view, cx| {
            view.allow_split = true;
            view.source_root = Some(cwd);
            view.set_split(true, cx);
        });
        // "Jump to Source" opens the file behind the dialog: step aside.
        let subscriptions = vec![
            cx.subscribe(
                &diff_view,
                |this, _, event: &crate::diff_view::DiffEvent, cx| {
                    if matches!(event, crate::diff_view::DiffEvent::Jumped)
                        && this.form.read(cx).busy.is_none()
                    {
                        cx.emit(CommitDialogEvent::Close);
                    }
                },
            ),
            cx.observe(&form, |this, _, cx| this.form_changed(cx)),
            cx.subscribe(&form, |this, _, event: &CommitFormEvent, cx| match event {
                CommitFormEvent::Jumped(Surface::Dialog) if this.form.read(cx).busy.is_none() => {
                    cx.emit(CommitDialogEvent::Close)
                }
                CommitFormEvent::Committed { .. } => cx.emit(CommitDialogEvent::Close),
                _ => {}
            }),
        ];
        window.focus(&form.read(cx).message.focus_handle(cx), cx);
        let mut this = Self {
            form,
            diff_view,
            loaded: None,
            diff_request: 0,
            _diff_task: None,
            focus: cx.focus_handle(),
            size: DialogResize::new(&REMEMBERED_SIZE),
            _subscriptions: subscriptions,
        };
        this.form_changed(cx);
        this
    }

    /// Follows the form: the picked file's diff, reloaded when the list moves.
    fn form_changed(&mut self, cx: &mut Context<Self>) {
        self.form
            .update(cx, |form, cx| form.select_first_if_none(cx));
        let form = self.form.read(cx);
        let now = form.selected.clone().map(|path| (path, form.files.clone()));
        if now != self.loaded {
            self.loaded = now;
            self.load_diff(cx);
        }
        cx.notify();
    }

    /// The selected file against HEAD, in the diff below.
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some((relative, _)) = self.loaded.clone() else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        self.diff_request += 1;
        let request = self.diff_request;
        let cwd = PathBuf::from(self.form.read(cx).cwd());
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

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self
            .form
            .update(cx, |form, cx| form.key(Surface::Dialog, event, cx))
        {
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key != "escape" {
            return;
        }
        let (generating, busy) = {
            let form = self.form.read(cx);
            (form.generating(), form.busy.is_some())
        };
        if generating {
            self.form.update(cx, |form, cx| form.cancel_generate(cx));
            cx.stop_propagation();
        } else if !busy {
            cx.emit(CommitDialogEvent::Close);
            cx.stop_propagation();
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
        let (width, height) = self.size.size(window.viewport_size());
        // The list and message above, the diff below: the header and the
        // buttons take the rest.
        let split_room = height - 110.;
        let top = self.size.top_height(split_room, height * 0.38);
        let form_entity = self.form.clone();
        let form = self.form.read(cx);
        let checked = form.checked.len();
        let total = form.files.len();
        let busy = form.busy.is_some();
        let can_commit = form.can_commit(cx);
        let (can_push, can_pr) = form.push_options(cx);
        let (commit_label, push_label, pr_label) =
            (form.commit_label(), form.push_label(), form.pr_label());
        let message = form.message.clone();
        let error = form.error_line();
        let menu = form.menu_overlay(Surface::Dialog, cx);
        let tree = if form.files.is_empty() {
            div()
                .flex_1()
                .p_4()
                .text_size(px(12.))
                .text_color(content(0.45))
                .child("No uncommitted changes")
                .into_any_element()
        } else {
            CommitForm::file_list(&form_entity, Surface::Dialog, cx)
                .py_1()
                .flex_1()
                .min_h_0()
                .into_any_element()
        };
        let generate = CommitForm::generate_button(&form_entity, window, cx);
        let amend = CommitForm::amend_toggle(&form_entity, window, cx);
        let pending = CommitForm::pending_overlay(&form_entity, Surface::Dialog, window, cx);
        let on_commit = window.listener_for(&form_entity, |form, _: &ClickEvent, window, cx| {
            form.commit(false, false, window, cx)
        });
        let on_push = window.listener_for(&form_entity, |form, _: &ClickEvent, window, cx| {
            form.commit(true, false, window, cx)
        });
        let on_pr = window.listener_for(&form_entity, |form, _: &ClickEvent, window, cx| {
            form.commit(true, true, window, cx)
        });
        let card = modal::card()
            .debug_selector(|| "commit-card".into())
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("commit-header")
                    .on_click(cx.listener(dialog_resize::header_click))
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
                    )
                    .child(div().flex_1())
                    .child(dialog_resize::maximize_button(
                        "commit-maximize",
                        self.size.maximized(),
                        cx,
                    )),
            )
            .child(
                div()
                    .relative()
                    .h(px(top))
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
                                    .child(message)
                                    .child(generate),
                            )
                            .child(amend),
                    ),
            )
            .child(dialog_resize::split_handle(
                "commit-split",
                top,
                split_room,
                cx,
            ))
            .child(
                div()
                    .debug_selector(|| "commit-diff".into())
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
                    .child(div().flex_1().min_w_0().children(error))
                    .child(
                        modal::ghost_button("commit-cancel", "Cancel")
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CommitDialogEvent::Close))),
                    )
                    .child(
                        modal::disabled(modal::ghost_button("commit-pr", pr_label), !can_pr)
                            .when(can_pr, |d| d.on_click(on_pr)),
                    )
                    .child(
                        modal::disabled(
                            modal::ghost_button("commit-only", commit_label),
                            !can_commit,
                        )
                        .when(can_commit, |d| d.on_click(on_commit)),
                    )
                    .child(
                        modal::disabled(
                            modal::primary_button("commit-push", push_label),
                            !can_push,
                        )
                        .when(can_push, |d| d.on_click(on_push)),
                    ),
            )
            .child(dialog_resize::corner_grip(
                "commit-resize",
                (width, height),
                cx,
            ));
        div()
            .size_full()
            .child(dialog_resize::drag_listener(cx))
            .track_focus(&self.focus)
            .key_context("CommitDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("commit-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if this.form.read(cx).busy.is_none() {
                            cx.emit(CommitDialogEvent::Close);
                        }
                    });
                }
            }))
            .children(pending)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{git_status::ProjectGit, input};

    fn open_dialog(cwd: String, window: &mut Window, cx: &mut gpui::App) -> Entity<CommitDialog> {
        let git = cx.new(|cx| ProjectGit::new(cwd, cx));
        let form = cx.new(|cx| CommitForm::new(git, window, cx));
        cx.new(|cx| CommitDialog::new(form, window, cx))
    }

    /// The remembered size is one per process: tests that set it take turns.
    static SIZE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[gpui::test]
    fn the_dialog_fills_the_window_on_request_and_grows_by_its_corner(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{MouseButton, VisualTestContext, point};
        cx.update(input::bind);
        let _serial = SIZE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let open = |cx: &mut gpui::TestAppContext| {
            let cwd = cwd.clone();
            cx.update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    open_dialog(cwd.clone(), window, cx)
                })
                .unwrap()
            })
        };
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let card = visual.debug_bounds("commit-card").expect("the dialog");
        let viewport = visual.update(|window, _| window.viewport_size());
        let full = viewport - gpui::size(px(24.), px(24.));
        let toggle = visual.debug_bounds("commit-maximize").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("commit-card").unwrap().size, full);
        let toggle = visual.debug_bounds("commit-maximize").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("commit-card").unwrap().size, card.size);
        // A double click on the header fills the window too.
        let header = visual.debug_bounds("commit-card").unwrap().origin + point(px(200.), px(20.));
        visual.simulate_click(header, Default::default());
        visual.simulate_event(gpui::MouseDownEvent {
            position: header,
            button: MouseButton::Left,
            modifiers: Default::default(),
            click_count: 2,
            first_mouse: false,
        });
        visual.simulate_event(gpui::MouseUpEvent {
            position: header,
            button: MouseButton::Left,
            modifiers: Default::default(),
            click_count: 2,
        });
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("commit-card").unwrap().size, full);
        let toggle = visual.debug_bounds("commit-maximize").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        // Dragging the corner resizes about the centre, and is remembered.
        let grip = visual.debug_bounds("commit-resize").unwrap().center();
        visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
        let to = grip - point(px(40.), px(30.));
        visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
        visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
        visual.run_until_parked();
        let smaller = visual.debug_bounds("commit-card").unwrap().size;
        assert_eq!(smaller.width, card.size.width - px(80.));
        assert_eq!(smaller.height, card.size.height - px(60.));
        let again = open(cx);
        let visual = &mut VisualTestContext::from_window(again.into(), cx);
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("commit-card").unwrap().size, smaller);
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
    }

    #[gpui::test]
    fn the_line_above_the_diff_drags_to_give_the_diff_more_or_less_room(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{MouseButton, VisualTestContext, point};
        cx.update(input::bind);
        let _serial = SIZE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let open = |cx: &mut gpui::TestAppContext| {
            let cwd = cwd.clone();
            cx.update(|cx| {
                cx.open_window(Default::default(), |window, cx| {
                    open_dialog(cwd.clone(), window, cx)
                })
                .unwrap()
            })
        };
        let window = open(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let diff = visual.debug_bounds("commit-diff").expect("the diff");
        let drag = |visual: &mut VisualTestContext, by: f32| {
            let at = visual.debug_bounds("commit-split").unwrap().center();
            visual.simulate_mouse_down(at, MouseButton::Left, Default::default());
            let to = at + point(px(0.), px(by));
            visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
            visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
            visual.run_until_parked();
            visual.debug_bounds("commit-diff").unwrap()
        };
        // Up: the diff grows by as much, and its bottom stays put.
        let taller = drag(visual, -60.);
        // Within a pixel: the grip sits on a half pixel at some window sizes.
        let near = |a: gpui::Pixels, b: gpui::Pixels| (a - b).abs() <= px(1.);
        assert!(near(taller.size.height, diff.size.height + px(60.)));
        assert!(near(taller.bottom(), diff.bottom()));
        // Far down: the diff keeps some room.
        let least = drag(visual, 2000.);
        assert!(least.size.height > px(20.) && least.size.height < diff.size.height);
        // Far up: so does the list.
        let most = drag(visual, -2000.);
        assert!(most.top() - visual.debug_bounds("commit-card").unwrap().top() > px(100.));
        // The next dialog opens with the same split.
        let again = open(cx);
        let visual = &mut VisualTestContext::from_window(again.into(), cx);
        visual.run_until_parked();
        assert_eq!(visual.debug_bounds("commit-diff").unwrap().size, most.size);
        *REMEMBERED_SIZE.lock().unwrap() = Default::default();
    }
}
