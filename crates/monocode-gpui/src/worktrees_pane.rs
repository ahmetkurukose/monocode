//! The right panel's Worktrees tab: every working copy of the project with where its
//! branch stands against the branch it came from (to merge, behind, uncommitted), its
//! chats, and Open / Merge / Merge & delete / Delete. The shell does the actions, the
//! same way the rail, the chat's pill and agents do.
use crate::{
    git_status::{GitChanged, ProjectGit},
    right_panel::*,
    runtime::{Database, locked},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Role,
    SharedString, Subscription, Task, Window, div, prelude::*, px, rgb,
};
use monocode_core::{
    files::is_equal_or_inside,
    worktree::{self, Ownership, merge},
};
use std::path::Path;

/// One working copy, as the tab lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeRow {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub ownership: Ownership,
    pub missing: bool,
    pub locked: bool,
    /// Against its base; `None` for the main checkout or when Git could not tell.
    pub status: Option<merge::Status>,
    /// Chats working in it.
    pub sessions: usize,
}
impl WorktreeRow {
    fn main(&self) -> bool {
        self.ownership == Ownership::Main
    }
    fn can_merge(&self) -> bool {
        !self.missing && self.status.as_ref().is_some_and(merge::Status::can_merge)
    }
    /// Commits its base does not have yet.
    fn unmerged(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.ahead)
    }
}

pub(crate) enum WorktreesPaneEvent {
    /// Show it (the shell knows the project).
    Open {
        path: String,
    },
    Merge {
        project: String,
        path: String,
        delete: bool,
    },
    Delete {
        project: String,
        path: String,
    },
    /// A new worktree for the project in view.
    Create,
}

pub(crate) struct WorktreesPane {
    git: Entity<ProjectGit>,
    store: Option<Database>,
    project: String,
    enabled: bool,
    rows: Option<Result<Vec<WorktreeRow>, String>>,
    /// Paths with an action under way, until the next read.
    busy: Vec<String>,
    loading: Option<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<WorktreesPaneEvent> for WorktreesPane {}
impl Focusable for WorktreesPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The project's working copies with their state; Git work, for a background thread.
pub(crate) fn read_rows(project: &str, store: &Database) -> Result<Vec<WorktreeRow>, String> {
    let entries = worktree::catalog::list_in(Path::new(project), store)?;
    // Where each chat runs: its worktree, else its own folder.
    let working: Vec<String> = {
        let db = locked(store)?;
        let worktrees = db.session_worktrees()?;
        db.sessions()?
            .into_iter()
            .filter(|s| !s.archived)
            .map(|s| worktrees.get(&s.id).cloned().flatten().unwrap_or(s.cwd))
            .collect()
    };
    Ok(entries
        .into_iter()
        .map(|entry| {
            let path = entry.path.to_string_lossy().into_owned();
            let is_main = entry.ownership == Ownership::Main;
            let key = monocode_core::files::path_key(&path);
            let chats = working
                .iter()
                .filter(|cwd| {
                    if is_main {
                        monocode_core::files::path_key(cwd) == key
                    } else {
                        is_equal_or_inside(cwd, &path)
                    }
                })
                .count();
            let status = (!is_main && !entry.missing)
                .then(|| merge::status(&entry.path).ok())
                .flatten();
            let name = entry
                .meta
                .as_ref()
                .map(|m| m.display_name.trim().to_owned())
                .filter(|n| !n.is_empty())
                .or_else(|| entry.branch.clone())
                .unwrap_or_else(|| {
                    entry
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            WorktreeRow {
                path,
                name,
                branch: entry.branch,
                ownership: entry.ownership,
                missing: entry.missing,
                locked: entry.locked,
                status,
                sessions: chats,
            }
        })
        .collect())
}

impl WorktreesPane {
    pub(crate) fn new(git: Entity<ProjectGit>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&git, |this, _, _: &GitChanged, cx| {
            if this.enabled {
                this.reload(cx);
            }
        })];
        Self {
            git,
            store: None,
            project: String::new(),
            enabled: false,
            rows: None,
            busy: Vec::new(),
            loading: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }
    pub(crate) fn set_store(&mut self, store: Database) {
        self.store = Some(store);
    }
    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let was = self.enabled;
        self.enabled = enabled;
        if enabled && !was {
            self.reload(cx);
        }
    }
    /// Read the list again (a worktree came, went, merged, or the project changed).
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if !self.enabled {
            // Read when shown next.
            self.rows = None;
            return;
        }
        let project = self.git.read(cx).project().to_owned();
        if project != self.project {
            self.rows = None;
            self.project = project.clone();
        }
        if project.is_empty() || monocode_core::files::remote::is_remote(Path::new(&project)) {
            self.rows = Some(Err(worktree::REMOTE_UNAVAILABLE.into()));
            cx.notify();
            return;
        }
        let work = cx
            .background_executor()
            .spawn(async move { read_rows(&project, &store) });
        self.loading = Some(cx.spawn(async move |this, cx| {
            let rows = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy.clear();
                this.rows = Some(rows);
                this.loading = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
    pub(crate) fn rows(&self) -> Option<&[WorktreeRow]> {
        self.rows.as_ref()?.as_ref().ok().map(Vec::as_slice)
    }

    fn act(&mut self, path: &str, event: WorktreesPaneEvent, cx: &mut Context<Self>) {
        if matches!(event, WorktreesPaneEvent::Merge { .. }) {
            self.busy.push(path.to_owned());
        }
        cx.emit(event);
        cx.notify();
    }

    fn row(&self, ix: usize, row: &WorktreeRow, cx: &mut Context<Self>) -> AnyElement {
        let project = self.project.clone();
        let busy = self.busy.contains(&row.path);
        let chip = |id: &'static str, text: String, color: u32| {
            div()
                .id(SharedString::from(format!("{id}-{ix}")))
                .debug_selector(move || format!("{id}-{ix}"))
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(gpui::Hsla::from(rgb(color)).opacity(0.12))
                .text_color(rgb(color))
                .text_size(px(10.5))
                .font_weight(FontWeight::MEDIUM)
                .child(text)
        };
        let mut chips = div().flex().flex_wrap().items_center().gap(px(4.));
        if row.missing {
            chips = chips.child(chip(
                "worktree-pane-missing",
                "Folder missing".into(),
                RED_400,
            ));
        }
        if row.locked {
            chips = chips.child(chip("worktree-pane-locked", "Locked".into(), AMBER_400));
        }
        if let Some(status) = &row.status {
            if status.ahead > 0 {
                chips = chips.child(chip(
                    "worktree-pane-unmerged",
                    format!("{} to merge", status.ahead),
                    DIFF_ADD,
                ));
            } else if status.base.is_some() {
                chips = chips.child(chip("worktree-pane-merged", "Merged".into(), 0x9ca3af));
            }
            if status.behind > 0 {
                chips = chips.child(chip(
                    "worktree-pane-behind",
                    format!("{} behind", status.behind),
                    AMBER_400,
                ));
            }
            if status.dirty > 0 {
                chips = chips.child(chip(
                    "worktree-pane-dirty",
                    format!("{} uncommitted", status.dirty),
                    AMBER_400,
                ));
            }
        }
        if matches!(row.ownership, Ownership::Legacy | Ownership::External) {
            let label = if row.ownership == Ownership::Legacy {
                "Legacy"
            } else {
                "Not made here"
            };
            chips = chips.child(chip("worktree-pane-owner", label.into(), 0x9ca3af));
        }
        let subtitle = match (
            &row.branch,
            row.status.as_ref().and_then(|s| s.base.clone()),
        ) {
            _ if row.main() => format!(
                "{} · project folder",
                row.branch.clone().unwrap_or_else(|| "detached".into())
            ),
            (Some(branch), Some(base)) => format!("{branch} → {base}"),
            (Some(branch), None) => branch.clone(),
            (None, _) => "detached".into(),
        };
        let action = |id: &'static str, label: &'static str, enabled: bool| {
            div()
                .id(SharedString::from(format!("{id}-{ix}")))
                .debug_selector(move || format!("{id}-{ix}"))
                .role(Role::Button)
                .aria_label(label)
                .px(px(6.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .text_size(px(11.))
                .text_color(content(if enabled { 0.75 } else { 0.3 }))
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                })
                .child(label)
        };
        let (open_path, merge_path, merge_delete_path, delete_path) = (
            row.path.clone(),
            row.path.clone(),
            row.path.clone(),
            row.path.clone(),
        );
        let (p2, p3, p4) = (project.clone(), project.clone(), project.clone());
        let can_merge = row.can_merge() && !busy;
        let actions = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .when(!row.missing, |d| {
                d.child(
                    action("worktree-pane-open", "Open", true).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.act(
                                &open_path,
                                WorktreesPaneEvent::Open {
                                    path: open_path.clone(),
                                },
                                cx,
                            )
                        },
                    )),
                )
            })
            .when(!row.main(), |d| {
                d.when(row.status.as_ref().is_some_and(|s| s.base.is_some()), |d| {
                    d.child(action("worktree-pane-merge", "Merge", can_merge).when(
                        can_merge,
                        |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.act(
                                    &merge_path,
                                    WorktreesPaneEvent::Merge {
                                        project: p2.clone(),
                                        path: merge_path.clone(),
                                        delete: false,
                                    },
                                    cx,
                                )
                            }))
                        },
                    ))
                    .child(
                        action("worktree-pane-merge-delete", "Merge & delete", can_merge).when(
                            can_merge,
                            |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| {
                                    this.act(
                                        &merge_delete_path,
                                        WorktreesPaneEvent::Merge {
                                            project: p3.clone(),
                                            path: merge_delete_path.clone(),
                                            delete: true,
                                        },
                                        cx,
                                    )
                                }))
                            },
                        ),
                    )
                })
                .child(
                    action("worktree-pane-delete", "Delete…", !busy).when(!busy, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.act(
                                &delete_path,
                                WorktreesPaneEvent::Delete {
                                    project: p4.clone(),
                                    path: delete_path.clone(),
                                },
                                cx,
                            )
                        }))
                    }),
                )
            });
        div()
            .id(SharedString::from(format!("worktree-pane-row-{ix}")))
            .debug_selector(move || format!("worktree-pane-row-{ix}"))
            .role(Role::ListItem)
            .aria_label(SharedString::from(row.name.clone()))
            .px_3()
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .border_b_1()
            .border_color(stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        icon(
                            if row.main() {
                                "folder-open"
                            } else {
                                "git-branch"
                            },
                            14.,
                        )
                        .text_color(content(0.55)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.9))
                            .child(row.name.clone()),
                    )
                    .when(busy, |d| {
                        d.child(spinner(("worktree-pane-busy", ix), "loader", 12.))
                    })
                    .when(row.sessions > 0, |d| {
                        d.child(div().text_size(px(11.)).text_color(content(0.45)).child(
                            if row.sessions == 1 {
                                "1 chat".to_owned()
                            } else {
                                format!("{} chats", row.sessions)
                            },
                        ))
                    }),
            )
            .child(
                div()
                    .pl(px(22.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .child(subtitle),
            )
            .child(
                div()
                    .pl(px(22.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(chips)
                    .child(actions),
            )
            .into_any_element()
    }
}

impl Render for WorktreesPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.project.clone();
        let rows: Vec<WorktreeRow> = self.rows().map(<[_]>::to_vec).unwrap_or_default();
        let unmerged = rows.iter().filter(|r| r.unmerged() > 0).count();
        let worktrees = rows.iter().filter(|r| !r.main()).count();
        let summary = match worktrees {
            0 => "No worktrees".to_owned(),
            n => {
                let mut text = format!("{n} {}", if n == 1 { "worktree" } else { "worktrees" });
                if unmerged > 0 {
                    text.push_str(&format!(" · {unmerged} not merged"));
                }
                text
            }
        };
        let header = pane_header("worktrees-pane-header", "Worktrees")
            .child(div().flex_1().min_w_0().truncate().child(summary))
            .child(
                icon_action("worktrees-pane-refresh", "Refresh", "refresh-cw", true)
                    .debug_selector(|| "worktrees-pane-refresh".into())
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
            .child(
                icon_action(
                    "worktrees-pane-new",
                    "New worktree",
                    "plus",
                    !project.is_empty(),
                )
                .debug_selector(|| "worktrees-pane-new".into())
                .on_click(cx.listener(|_, _, _, cx| cx.emit(WorktreesPaneEvent::Create))),
            );
        let body: AnyElement = match &self.rows {
            None => div()
                .p_3()
                .text_size(px(12.))
                .text_color(content(0.5))
                .child("Reading worktrees…")
                .into_any_element(),
            Some(Err(error)) => div()
                .id("worktrees-pane-error")
                .debug_selector(|| "worktrees-pane-error".into())
                .p_3()
                .text_size(px(12.))
                .text_color(content(0.6))
                .child(error.clone())
                .into_any_element(),
            Some(Ok(_)) => div()
                .id("worktrees-pane-list")
                .debug_selector(|| "worktrees-pane-list".into())
                .role(Role::List)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(ix, row)| self.row(ix, row, cx)),
                )
                .into_any_element(),
        };
        div()
            .id("worktrees-pane")
            .debug_selector(|| "worktrees-pane".into())
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(body)
    }
}
