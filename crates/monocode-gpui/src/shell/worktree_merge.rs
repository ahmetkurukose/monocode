//! Merging a worktree's branch into the branch it came from, and "merge, then delete
//! the worktree". The chat's git pill, the Worktrees panel and agents (`worktrees.merge`)
//! all come here, so each does exactly what the others do.
use super::worktree_delete::{RemovalReport, RemoveRequest};
use super::*;
use crate::right_panel::DIFF_ADD;
use monocode_core::files::path_key;
use monocode_core::worktree::{self, RemoveOptions, merge::Merged};
use std::path::Path;

/// What a merge (and the delete after it) did.
#[derive(Clone, Debug)]
pub(crate) struct MergeReport {
    pub merged: Merged,
    pub removed: Option<RemovalReport>,
}

pub(crate) type MergeDone =
    Box<dyn FnOnce(&mut Shell, Result<MergeReport, String>, &mut Window, &mut Context<Shell>)>;

/// One merge: [`Shell::merge_worktree`].
pub(crate) struct MergeRequest {
    /// The project (any checkout of the repository).
    pub project: String,
    pub path: String,
    /// Then delete the worktree (its sessions move to the base's checkout).
    pub delete: bool,
    /// For a delete that would lose local files: go ahead (agents ask with `force`).
    pub force: bool,
    /// Told the outcome instead of the window (an agent's call).
    pub done: Option<MergeDone>,
    /// The session asking (an agent's turn in the worktree): running, but not in the way.
    pub caller: Option<i64>,
}

impl Shell {
    /// The worktree at `path` is being merged.
    pub(crate) fn worktree_merging(&self, path: &str) -> bool {
        let key = path_key(path);
        self.worktree_ui.merging.iter().any(|p| path_key(p) == key)
    }

    /// Merge the worktree's branch into its base, then (with `delete`) remove the
    /// worktree. Refuses while one of its sessions runs. Without a `done` the outcome is
    /// shown in the window; a delete that would lose local files opens the Delete dialog.
    pub(crate) fn merge_worktree(
        &mut self,
        request: MergeRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let MergeRequest {
            project,
            path,
            delete,
            force,
            done,
            caller,
        } = request;
        let refuse = if self.worktree_merging(&path) {
            Some("This worktree is already being merged.".to_owned())
        } else if self.worktree_being_removed(&path) {
            Some(worktree::remove::BEING_REMOVED.to_owned())
        } else if self
            .sessions_in_worktree(&path, cx)
            .iter()
            .filter(|id| Some(**id) != caller)
            .any(|id| self.sessions.get(id).is_some_and(|v| v.read(cx).busy))
        {
            Some("Wait for the sessions working in this worktree to finish.".to_owned())
        } else {
            None
        };
        if let Some(error) = refuse {
            self.merge_failed(error, done, window, cx);
            return;
        }
        self.worktree_ui.merging.insert(path.clone());
        self.invalidate_rail_worktrees(cx);
        cx.notify();
        let (work_path, work_project) = (path.clone(), project.clone());
        let work = cx.background_executor().spawn(async move {
            let merged = worktree::merge::merge(Path::new(&work_path))?;
            // Whether deleting now would lose files the user has not confirmed losing.
            let needs_force = delete
                && !force
                && worktree::remove::preflight(Path::new(&work_project), Path::new(&work_path))
                    .is_ok_and(|pre| pre.needs_force());
            Ok::<_, String>((merged, needs_force))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.worktree_ui.merging.remove(&path);
                // The base moved: its checkout's status, every chat's pill, the rail.
                this.worktrees_changed_elsewhere(cx);
                this.refresh_session_git_actions(cx);
                let (merged, needs_force) = match result {
                    Ok(ok) => ok,
                    Err(error) => {
                        this.merge_failed(error, done, window, cx);
                        return;
                    }
                };
                if !delete {
                    this.merge_finished(MergeReport { merged, removed: None }, done, window, cx);
                    return;
                }
                if needs_force {
                    let message = format!(
                        "Merged {} into {}. The worktree has local files that deleting it would lose.",
                        merged.branch, merged.base
                    );
                    match done {
                        Some(done) => done(
                            this,
                            Err(format!("{message} Delete it with force to discard them.")),
                            window,
                            cx,
                        ),
                        None => {
                            this.worktree_ui.merged = Some(message);
                            this.open_delete_worktree_dialog(project, path, window, cx);
                        }
                    }
                    return;
                }
                let base = merged.base.clone();
                this.remove_worktree_with(
                    RemoveRequest {
                        cwd: project,
                        path,
                        delete_sessions: false,
                        opts: RemoveOptions {
                            force,
                            ..RemoveOptions::default()
                        },
                        merged_into: Some(base),
                        done: Some(Box::new(move |this, removed, window, cx| match removed {
                            Ok(removed) => this.merge_finished(
                                MergeReport {
                                    merged,
                                    removed: Some(removed),
                                },
                                done,
                                window,
                                cx,
                            ),
                            Err(error) => this.merge_failed(
                                format!(
                                    "Merged {} into {}, but the worktree was not deleted: {error}",
                                    merged.branch, merged.base
                                ),
                                done,
                                window,
                                cx,
                            ),
                        })),
                    },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn merge_failed(
        &mut self,
        error: String,
        done: Option<MergeDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match done {
            Some(done) => done(self, Err(error), window, cx),
            None => self.error = Some(error),
        }
        self.invalidate_rail_worktrees(cx);
        cx.notify();
    }

    fn merge_finished(
        &mut self,
        report: MergeReport,
        done: Option<MergeDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.worktree_ui.merged = Some(merge_summary(&report));
        if let Some(done) = done {
            done(self, Ok(report), window, cx);
        }
        cx.notify();
    }

    /// Every open chat rereads its git pill (a merge moved a branch under some of them).
    pub(crate) fn refresh_session_git_actions(&mut self, cx: &mut Context<Self>) {
        for view in self.sessions.values() {
            view.update(cx, |view, cx| view.refresh_git_actions(cx));
        }
    }

    /// The "Merged …" toast.
    pub(super) fn merged_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.worktree_ui.merged.clone()?;
        Some(
            crate::notifications::surface::toast_frame()
                .id("worktree-merged")
                .debug_selector(|| "worktree-merged".into())
                .role(Role::Status)
                .child(
                    div()
                        .px(px(14.))
                        .py_3()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(icon("git-merge", 16.).text_color(rgb(DIFF_ADD)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .text_color(content(0.85))
                                .child(message),
                        )
                        .child(
                            icon_button("worktree-merged-dismiss", "Dismiss", "close").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.worktree_ui.merged = None;
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// "Merged 3 commits of feature into main; deleted the worktree." and the like.
pub(crate) fn merge_summary(report: &MergeReport) -> String {
    let m = &report.merged;
    let mut text = if m.commits == 0 {
        format!("{} already had everything on {}.", m.base, m.branch)
    } else {
        format!(
            "Merged {} {} of {} into {}{}.",
            m.commits,
            if m.commits == 1 { "commit" } else { "commits" },
            m.branch,
            m.base,
            if m.fast_forward {
                " (fast-forward)"
            } else {
                ""
            }
        )
    };
    if let Some(removed) = &report.removed {
        text.push_str(" Deleted the worktree");
        if removed.removed.branch_deleted {
            text.push_str(" and its branch");
        }
        if removed.moved > 0 {
            text.push_str(&format!(
                "; {} moved to {}",
                if removed.moved == 1 {
                    "its chat".to_owned()
                } else {
                    format!("its {} chats", removed.moved)
                },
                m.base
            ));
        }
        text.push('.');
    }
    text
}
