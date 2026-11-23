//! The New Pull Request dialog's views.
use super::*;
use crate::{
    change_tree::basename,
    diff_view::diff_counts,
    file_icons::file_type_icon,
    modal,
    popover::{Align, Anchor, AnchorSlot, menu_popover},
    right_panel::{DIFF_ADD, DIFF_DEL, icon_action, spinner, stroke},
};
use gpui::{AnyElement, ClickEvent, FontWeight, Hsla, Role, div, px, rgb, uniform_list};

const ROW: f32 = 26.;
const HEADER: f32 = 44.;
const BRANCH_BAR: f32 = 40.;
const FOOTER: f32 = 58.;
const RED: u32 = 0xf87171;
const AMBER: u32 = 0xfbbf24;

#[derive(Default)]
pub(crate) struct Anchors {
    base: AnchorSlot,
    head: AnchorSlot,
    commits: AnchorSlot,
    create: AnchorSlot,
    templates: AnchorSlot,
}

struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(360.))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .line_height(px(17.))
            .text_color(content(1.))
            .child(self.0.clone())
    }
}
fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone())).into()
}

/// What the push needs, as the warning beside the branches says it.
pub(crate) fn push_note(push: &PrPush) -> Option<(String, bool)> {
    match push {
        PrPush::UpToDate => None,
        PrPush::Create { remote, branch } => Some((
            format!("No remote branch for ‘{branch}’. It will be created on ‘{remote}’."),
            false,
        )),
        PrPush::Sync { remote, branch } => Some((
            format!("Local branch is ahead of {remote}/{branch}; it will be pushed."),
            false,
        )),
        PrPush::Diverged { remote, branch } => Some((
            format!("{remote}/{branch} has commits the local branch lacks. Pull or rebase first."),
            true,
        )),
    }
}

fn centered(text: impl Into<SharedString>, loading: bool) -> gpui::Div {
    div()
        .flex_1()
        .min_h_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(12.))
        .text_color(content(0.5))
        .when(loading, |d| d.child(spinner("pr-load-spin", "loader", 14.)))
        .child(text.into())
}

fn link_button(
    id: &'static str,
    label: String,
    open: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_expanded(open)
        .min_w_0()
        .h(px(26.))
        .px(px(8.))
        .rounded_md()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(12.))
        .font_family(crate::fonts::mono())
        .text_color(content(0.9))
        .bg(content(if open { 0.12 } else { 0.06 }))
        .when(enabled && !open, |d| d.hover(|s| s.bg(content(0.1))))
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.5))
        .child(div().min_w_0().truncate().child(label))
        .child(icon("chevron-down", 11.).text_color(content(0.45)))
}

fn small_button(id: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::Button)
        .aria_label(label)
        .flex_shrink_0()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .text_size(px(12.))
        .text_color(content(0.75))
        .bg(content(0.08))
        .cursor_pointer()
        .hover(|s| s.bg(content(0.14)).text_color(content(1.)))
        .child(label)
}

impl PrDialog {
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let slug = self.base().map(PrRepo::slug);
        div()
            .id("pr-header")
            .on_click(cx.listener(dialog_resize::header_click))
            .h(px(HEADER))
            .flex_shrink_0()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(stroke())
            .child(modal::title("New Pull Request"))
            .when_some(slug, |d, slug| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(content(0.45))
                        .child(format!("· {slug}")),
                )
            })
            .child(div().flex_1())
            .child(dialog_resize::maximize_button(
                "pr-maximize",
                self.size.maximized(),
                cx,
            ))
    }

    fn branch_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let show_owner = self.base_repo != self.head_repo;
        let open = self.popup.as_ref().map(|p| p.kind);
        let enabled = !self.busy() && self.setup.ready().is_some();
        let base = branch_label(self.base(), &self.base_branch, show_owner);
        let head = branch_label(self.head(), &self.head_branch, show_owner);
        let note = self.push.ready().and_then(push_note);
        let remote = self.head().map(|r| r.remote.clone()).unwrap_or_default();
        div()
            .h(px(BRANCH_BAR))
            .flex_shrink_0()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(stroke())
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .flex_shrink(1.)
                    .child(self.anchors.base.probe())
                    .child(
                        link_button("pr-base", base, open == Some(PopupKind::Base), enabled).when(
                            enabled,
                            |d| {
                                d.on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_popup(PopupKind::Base, window, cx)
                                }))
                            },
                        ),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(13.))
                    .text_color(content(0.45))
                    .child("←"),
            )
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .flex_shrink(1.)
                    .child(self.anchors.head.probe())
                    .child(
                        link_button("pr-head", head, open == Some(PopupKind::Head), enabled).when(
                            enabled,
                            |d| {
                                d.on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_popup(PopupKind::Head, window, cx)
                                }))
                            },
                        ),
                    ),
            )
            .when_some(note, |d, (text, blocking)| {
                d.child(
                    div()
                        .id("pr-push-note")
                        .debug_selector(|| "pr-push-note".into())
                        .role(Role::Status)
                        .aria_label(SharedString::from(text.clone()))
                        .flex_shrink_0()
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip(tip(text))
                        .child(icon("alert-circle", 14.).text_color(rgb(if blocking {
                            RED
                        } else {
                            AMBER
                        }))),
                )
            })
            .when(self.naming_remote, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .child(format!("as {remote}/")),
                )
                .child(
                    div()
                        .debug_selector(|| "pr-remote-branch".into())
                        .w(px(200.))
                        .h(px(26.))
                        .px_2()
                        .rounded_md()
                        .bg(content(0.08))
                        .flex()
                        .items_center()
                        .text_size(px(12.))
                        .child(self.remote_branch.clone()),
                )
            })
    }

    fn file_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let base = div()
            .id(("pr-row", ix))
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
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_dir(key.clone(), cx)))
                .into_any_element(),
            TreeRow::File { ix: file_ix, depth } => {
                let Some(file) = self.file_list().get(file_ix).cloned() else {
                    return base.into_any_element();
                };
                let selected = self.selected.as_deref() == Some(file.relative.as_str());
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
                    .on_click(cx.listener(move |this, _, _, cx| this.select(file_ix, cx)))
                    .into_any_element()
            }
        }
    }

    /// "Changes from N commits ▾" and the file tree under it.
    fn changes_column(&self, cx: &mut Context<Self>) -> AnyElement {
        let range = match &self.range {
            Load::Loading => return centered("Loading commits…", true).into_any_element(),
            Load::Failed(error) => {
                return div()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_size(px(12.))
                    .child(
                        div()
                            .text_color(content(0.8))
                            .child("Failed to load commits"),
                    )
                    .child(
                        div()
                            .id("pr-range-error")
                            .max_h(px(80.))
                            .overflow_y_scroll()
                            .text_color(content(0.45))
                            .child(error.clone()),
                    )
                    .child(
                        small_button("pr-range-retry", "Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .into_any_element();
            }
            Load::Ready(range) => range,
        };
        if range.commits.is_empty() {
            return centered("Pull request does not contain any commits", false).into_any_element();
        }
        let n = range.commits.len();
        let scope = match self.scope {
            Scope::All => format!("Changes from {n} commit{}", if n == 1 { "" } else { "s" }),
            Scope::Commit(ix) => range
                .commits
                .get(ix)
                .map(|c| format!("Changes from {}", c.short_sha))
                .unwrap_or_default(),
        };
        let open = self
            .popup
            .as_ref()
            .is_some_and(|p| p.kind == PopupKind::Commits);
        let entity = cx.entity();
        let count = self.rows.len();
        let list: AnyElement = match &self.files {
            Load::Loading => centered("Loading files…", true).into_any_element(),
            Load::Failed(error) => div()
                .p_4()
                .text_size(px(12.))
                .text_color(rgb(RED))
                .child(error.clone())
                .into_any_element(),
            Load::Ready(files) if files.is_empty() => div()
                .p_4()
                .text_size(px(12.))
                .text_color(content(0.45))
                .child("No file changes")
                .into_any_element(),
            Load::Ready(_) => uniform_list("pr-files", count, move |range, _, cx| {
                entity.update(cx, |this, cx| {
                    range.map(|ix| this.file_row(ix, cx)).collect::<Vec<_>>()
                })
            })
            .py_1()
            .flex_1()
            .min_h_0()
            .into_any_element(),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(32.))
                    .flex_shrink_0()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(stroke())
                    .child(
                        div()
                            .relative()
                            .min_w_0()
                            .child(self.anchors.commits.probe())
                            .child(
                                div()
                                    .id("pr-scope")
                                    .debug_selector(|| "pr-scope".into())
                                    .role(Role::Button)
                                    .aria_label(SharedString::from(scope.clone()))
                                    .aria_expanded(open)
                                    .h(px(24.))
                                    .px(px(6.))
                                    .rounded_md()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(content(0.85))
                                    .cursor_pointer()
                                    .when(open, |d| d.bg(content(0.1)))
                                    .when(!open, |d| d.hover(|s| s.bg(content(0.06))))
                                    .child(div().min_w_0().truncate().child(scope))
                                    .child(icon("chevron-down", 11.).text_color(content(0.45)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_popup(PopupKind::Commits, window, cx)
                                    })),
                            ),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn generate_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let generating = self.generating();
        let can_generate =
            !self.busy() && self.range.ready().is_some_and(|r| !r.commits.is_empty());
        div()
            .id("pr-generate")
            .debug_selector(|| "pr-generate".into())
            .role(Role::Button)
            .aria_label(if generating {
                "Cancel pull request generation"
            } else {
                "Generate title and description"
            })
            .group("pr-generate")
            .size(px(20.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(content(0.1))
            .when(!generating && !can_generate, |d| d.opacity(0.4))
            .when(generating || can_generate, |d| {
                d.cursor_pointer().hover(|s| s.bg(content(0.2)))
            })
            .tooltip(tip(if generating {
                "Stop writing"
            } else {
                "Write the title and description"
            }))
            .child(if generating {
                div()
                    .relative()
                    .size(px(14.))
                    .child(
                        div()
                            .absolute()
                            .group_hover("pr-generate", |s| s.opacity(0.))
                            .child(spinner("pr-generate-spin", "loader", 14.)),
                    )
                    .child(
                        icon("x", 14.)
                            .absolute()
                            .text_color(content(1.))
                            .opacity(0.)
                            .group_hover("pr-generate", |s| s.opacity(1.)),
                    )
                    .into_any_element()
            } else {
                icon("wand", 12.).text_color(content(1.)).into_any_element()
            })
            .when(generating || can_generate, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if generating {
                        this.cancel_generate(cx);
                    } else {
                        this.generate(cx);
                    }
                }))
            })
            .into_any_element()
    }

    fn form(&self, height: f32, cx: &mut Context<Self>) -> AnyElement {
        let templates = self.setup.ready().map_or(0, |s| s.templates.len());
        let template_name = self
            .template
            .and_then(|ix| self.setup.ready()?.templates.get(ix))
            .map(|t| t.name.clone());
        let templates_open = self
            .popup
            .as_ref()
            .is_some_and(|p| p.kind == PopupKind::Templates);
        // The description fills what the title and padding leave.
        let room = (height - 30. - 8. - 24. - 12.).max(40.);
        self.body.update(cx, |body, _| {
            body.set_min_height(room);
            body.set_max_height(room);
        });
        div()
            .flex_1()
            .min_w_0()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "pr-title".into())
                    .h(px(30.))
                    .flex_shrink_0()
                    .px_2()
                    .rounded_md()
                    .bg(content(0.08))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .child(div().flex_1().min_w_0().child(self.title.clone()))
                    .child(self.generate_button(cx)),
            )
            .child(
                div()
                    .debug_selector(|| "pr-description".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded_md()
                    .bg(content(0.08))
                    .pl_2()
                    .pr_2()
                    .py_1()
                    .text_size(px(13.))
                    .text_color(content(1.))
                    .child(self.body.clone())
                    .when(templates > 1, |d| {
                        let label = template_name.unwrap_or_else(|| "Template".into());
                        d.child(
                            div()
                                .absolute()
                                .bottom(px(6.))
                                .right(px(6.))
                                .child(self.anchors.templates.probe())
                                .child(
                                    div()
                                        .id("pr-template")
                                        .debug_selector(|| "pr-template".into())
                                        .role(Role::Button)
                                        .aria_label("Template")
                                        .aria_expanded(templates_open)
                                        .h(px(22.))
                                        .px(px(6.))
                                        .rounded(px(6.))
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .text_size(px(11.))
                                        .text_color(content(0.6))
                                        .bg(content(if templates_open { 0.14 } else { 0.08 }))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(content(0.14)).text_color(content(1.)))
                                        .child(div().max_w(px(160.)).truncate().child(label))
                                        .child(icon("chevron-down", 10.).text_color(content(0.45)))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_popup(PopupKind::Templates, window, cx)
                                        })),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The line left of the buttons: progress, failures, and why Create is off.
    fn status(&self, cx: &mut Context<Self>) -> AnyElement {
        let line = |text: String, color: Hsla| {
            div()
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let row = div()
            .id("pr-status")
            .debug_selector(|| "pr-status".into())
            .role(Role::Status)
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(12.));
        match &self.submit {
            SubmitState::Running(stage) => {
                return row
                    .text_color(content(0.6))
                    .child(spinner("pr-submit-spin", "loader", 14.))
                    .child(stage.label())
                    .into_any_element();
            }
            SubmitState::MetadataFailed { pr, error, .. } => {
                let url = pr.url.clone();
                let text = format!(
                    "Pull request #{} created, but setting metadata failed: {error}",
                    pr.number
                );
                return row
                    .tooltip(tip(text.clone()))
                    .child(line(text, rgb(AMBER).into()))
                    .child(
                        small_button("pr-metadata-retry", "Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_metadata(cx))),
                    )
                    .child(
                        small_button("pr-metadata-open", "Open")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .into_any_element();
            }
            SubmitState::Failed(error) => {
                return row
                    .tooltip(tip(error.clone()))
                    .child(line(error.clone(), rgb(RED).into()))
                    .into_any_element();
            }
            SubmitState::Idle => {}
        }
        if let Some(error) = &self.generate_error {
            return row
                .tooltip(tip(error.clone()))
                .child(line(error.clone(), rgb(RED).into()))
                .into_any_element();
        }
        if self.setup.ready().is_none() || matches!(self.range, Load::Failed(_)) {
            return row.into_any_element();
        }
        match &self.check {
            Load::Loading => row
                .text_color(content(0.55))
                .child(spinner("pr-check-spin", "loader", 14.))
                .child("Checking branches…")
                .into_any_element(),
            Load::Failed(error) => row
                .tooltip(tip(error.clone()))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(RED))
                        .child("Couldn’t check branches"),
                )
                .child(line(error.clone(), content(0.45)))
                .child(
                    icon_action("pr-check-retry", "Check again", "refresh-cw", true)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.recheck(cx))),
                )
                .into_any_element(),
            Load::Ready(PrBranchCheck::NoChanges) => row
                .child(line(
                    format!(
                        "No changes between ‘{}’ and ‘{}’",
                        self.base_branch, self.head_branch
                    ),
                    content(0.6),
                ))
                .into_any_element(),
            Load::Ready(PrBranchCheck::AlreadyExists { pr }) => {
                let existing = pr.clone();
                row.child(line(
                    format!("Pull request #{} already exists", pr.number),
                    content(0.75),
                ))
                .child(
                    small_button("pr-view-existing", "View").on_click(cx.listener(
                        move |_, _, _, cx| cx.emit(PrDialogEvent::ViewExisting(existing.clone())),
                    )),
                )
                .into_any_element()
            }
            Load::Ready(PrBranchCheck::Ok) => match self.push.ready().and_then(push_note) {
                Some((text, true)) => row.child(line(text, rgb(RED).into())).into_any_element(),
                _ => row.into_any_element(),
            },
        }
    }

    fn create_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.can_create(cx);
        let open = self
            .popup
            .as_ref()
            .is_some_and(|p| p.kind == PopupKind::CreateMenu);
        let mut ink = floating_fill();
        ink.a = 1.;
        div()
            .relative()
            .flex()
            .flex_shrink_0()
            .when(!enabled, |d| d.opacity(0.4))
            .child(self.anchors.create.probe())
            .child(
                modal::primary_button("pr-create", "Create Pull Request")
                    .debug_selector(|| "pr-create".into())
                    .rounded_r(px(0.))
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.create(false, cx)))
                    }),
            )
            .child(
                div()
                    .id("pr-create-options")
                    .debug_selector(|| "pr-create-options".into())
                    .role(Role::Button)
                    .aria_label("Create options")
                    .aria_expanded(open)
                    .h(px(32.))
                    .w(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_r(px(9.))
                    .border_l_1()
                    .border_color(ink.opacity(0.15))
                    .bg(content(if open { 0.8 } else { 0.92 }))
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(content(0.8)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_popup(PopupKind::CreateMenu, window, cx)
                            }))
                    })
                    .child(icon("chevron-down-2", 14.).text_color(ink)),
            )
            .into_any_element()
    }

    fn popup_overlay(&self, cx: &gpui::App) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let anchor = match popup.kind {
            PopupKind::Base => &self.anchors.base,
            PopupKind::Head => &self.anchors.head,
            PopupKind::Commits => &self.anchors.commits,
            PopupKind::CreateMenu => &self.anchors.create,
            PopupKind::Templates => &self.anchors.templates,
        };
        let anchor = Anchor::Slot(anchor.clone());
        let popover = match &popup.menu {
            PopupMenu::Search(menu) => menu.read(cx).popover(anchor, cx),
            PopupMenu::Plain(menu) => menu_popover(menu, anchor, cx).gap(4.).max_height(380.),
        };
        Some(
            match popup.kind {
                PopupKind::CreateMenu | PopupKind::Templates => popover.align(Align::End),
                _ => popover.align(Align::Start),
            }
            .into_any_element(),
        )
    }
}

impl Render for PrDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = self.size.size(window.viewport_size());
        let split_room = height - HEADER - BRANCH_BAR - FOOTER;
        let top = self.size.top_height(split_room, split_room * 0.5);
        let side = self.size.side_width(width, 280.);
        let body: AnyElement = match &self.setup {
            Load::Loading => centered("Reading the repository…", true).into_any_element(),
            Load::Failed(error) => div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_size(px(12.))
                .child(
                    div()
                        .text_color(content(0.8))
                        .child("Couldn’t prepare a pull request"),
                )
                .child(
                    div()
                        .id("pr-setup-error")
                        .max_w(px(520.))
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .text_color(content(0.45))
                        .child(error.clone()),
                )
                .child(
                    small_button("pr-setup-retry", "Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.load_setup(cx))),
                )
                .into_any_element(),
            Load::Ready(_) => {
                let no_diff = self.selected.is_none()
                    && matches!(&self.range, Load::Ready(r) if !r.commits.is_empty())
                    && matches!(&self.files, Load::Ready(f) if !f.is_empty());
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(top))
                            .flex_shrink_0()
                            .flex()
                            .child(
                                div()
                                    .w(px(side))
                                    .flex_shrink_0()
                                    .flex()
                                    .flex_col()
                                    .child(self.changes_column(cx)),
                            )
                            .child(dialog_resize::side_handle("pr-side", side, width, cx))
                            .child(
                                div()
                                    .debug_selector(|| "pr-diff".into())
                                    .flex_1()
                                    .min_w_0()
                                    .border_l_1()
                                    .border_color(stroke())
                                    .flex()
                                    .flex_col()
                                    .map(|d| {
                                        if no_diff {
                                            d.child(centered(
                                                "Select a file to see its changes",
                                                false,
                                            ))
                                        } else {
                                            d.child(self.diff_view.clone())
                                        }
                                    }),
                            ),
                    )
                    .child(dialog_resize::split_handle("pr-split", top, split_room, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .border_t_1()
                            .border_color(stroke())
                            .when(self.busy(), |d| d.opacity(0.7))
                            .child(self.form(split_room - top, cx))
                            .child(
                                div()
                                    .id("pr-metadata")
                                    .w(px(320.))
                                    .flex_shrink_0()
                                    .border_l_1()
                                    .border_color(stroke())
                                    .p_3()
                                    .overflow_y_scroll()
                                    .child(self.metadata.clone()),
                            ),
                    )
                    .into_any_element()
            }
        };
        let busy = self.busy();
        let card = modal::card()
            .debug_selector(|| "pr-card".into())
            .w(px(width))
            .h(px(height))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.header(cx))
            .child(self.branch_bar(cx))
            .child(body)
            .child(
                div()
                    .h(px(FOOTER))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(stroke())
                    .child(self.status(cx))
                    .child(
                        modal::disabled(modal::ghost_button("pr-cancel", "Cancel"), busy)
                            .when(!busy, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| this.close(cx)))
                            }),
                    )
                    .child(self.create_button(cx)),
            )
            .child(dialog_resize::corner_grip("pr-resize", (width, height), cx));
        div()
            .size_full()
            .child(dialog_resize::drag_listener(cx))
            .track_focus(&self.focus)
            .key_context("PrDialog")
            .on_key_down(cx.listener(Self::key))
            .child(modal::overlay("pr-dialog", card, {
                let weak = cx.weak_entity();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.close(cx));
                }
            }))
            .children(self.popup_overlay(cx))
    }
}
