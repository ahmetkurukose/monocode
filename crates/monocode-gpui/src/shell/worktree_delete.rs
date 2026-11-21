//! The Delete Worktree dialog (spec §7.4): a removal preflight summary, "Also delete
//! its sessions", Delete / Force delete, and a card offering to delete a branch that
//! removal kept because it was not merged.
use super::*;
use monocode_core::{
    files::is_equal_or_inside,
    worktree::{self, Ownership, Preflight, RemoveOptions, Removed},
};
use std::path::Path;

/// Sample paths of at-risk files shown in the dialog.
const AT_RISK_SAMPLE: usize = 5;

#[derive(Clone, Debug)]
pub(super) struct DeleteInfo {
    preflight: Preflight,
    ownership: Ownership,
    display_name: Option<String>,
    /// Removal deletes the branch when Git considers it merged.
    deletes_branch: bool,
}

pub(super) struct DeleteDialog {
    /// The project (any checkout of the repository) the worktree belongs to.
    project: String,
    pub(super) path: String,
    info: Option<Result<DeleteInfo, String>>,
    delete_sessions: bool,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
    _load: gpui::Task<()>,
}

/// A branch removal kept because `git branch -d` refused it.
pub(super) struct KeptBranch {
    id: u64,
    cwd: String,
    branch: String,
    busy: bool,
    error: Option<String>,
}

fn inspect(cwd: &Path, path: &Path, store: &Database) -> Result<DeleteInfo, String> {
    let preflight = worktree::remove::preflight(cwd, path)?;
    let key = worktree::meta::key(&preflight.path);
    let entry = worktree::catalog::list_in(cwd, store)?
        .into_iter()
        .find(|e| e.key == key);
    let ownership = entry.as_ref().map_or(Ownership::External, |e| e.ownership);
    let meta = entry.and_then(|e| e.meta);
    Ok(DeleteInfo {
        deletes_branch: meta.as_ref().is_some_and(|m| !m.preserve_branch_on_delete),
        display_name: meta
            .map(|m| m.display_name)
            .filter(|n| !n.trim().is_empty()),
        ownership,
        preflight,
    })
}

// Tests make the sessions' detach fail after a successful removal.
// Per thread: the test dispatcher runs background work on the test's own thread, and
// other tests running in parallel must not see it.
#[cfg(test)]
thread_local! {
    static FAIL_DETACH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Move the sessions using `path` (removed) to `to`, the checkout of the branch the
/// worktree came from; without one, to the main checkout marked removed.
fn detach_sessions(
    store: &Database,
    path: &str,
    main: &Path,
    to: Option<&str>,
) -> Result<Vec<(Session, monocode_core::agent::Settings)>, String> {
    #[cfg(test)]
    if FAIL_DETACH.get() {
        return Err("database is locked".into());
    }
    locked(store)?.detach_worktree(path, &main.to_string_lossy(), to)
}

/// What a finished removal tells whoever asked for it (a merge, an agent's call).
#[derive(Clone, Debug)]
pub(crate) struct RemovalReport {
    pub removed: Removed,
    /// The checkout its sessions now use.
    pub moved_to: String,
    /// Sessions moved there (deleted ones not counted).
    pub moved: usize,
}

pub(crate) type RemovalDone =
    Box<dyn FnOnce(&mut Shell, Result<RemovalReport, String>, &mut Window, &mut Context<Shell>)>;

/// One removal: [`Shell::remove_worktree_with`].
pub(crate) struct RemoveRequest {
    /// The project (any checkout of the repository).
    pub cwd: String,
    pub path: String,
    pub delete_sessions: bool,
    pub opts: RemoveOptions,
    /// The worktree's branch was just merged into this branch: delete it even when
    /// `git branch -d` judges it unmerged against another checkout's HEAD.
    pub merged_into: Option<String>,
    /// Told the outcome; errors then go to it alone, not to the window.
    pub done: Option<RemovalDone>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Shell {
    /// Open the Delete Worktree dialog for the worktree at `path` of `project`.
    pub(crate) fn open_delete_worktree_dialog(
        &mut self,
        project: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        let load = self.inspect_for_delete_dialog(&project, &path, cx);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.worktree_ui.delete = Some(DeleteDialog {
            project,
            path,
            info: None,
            delete_sessions: false,
            busy: false,
            error: None,
            focus,
            _load: load,
        });
        cx.notify();
    }

    /// Read the worktree's preflight in the background into the open dialog for `path`.
    fn inspect_for_delete_dialog(
        &self,
        project: &str,
        path: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Task<()> {
        let store = self.store.clone();
        let (cwd, target) = (project.to_owned(), path.to_owned());
        let work = cx
            .background_executor()
            .spawn(async move { inspect(Path::new(&cwd), Path::new(&target), &store) });
        let path = path.to_owned();
        cx.spawn(async move |this, cx| {
            let info = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(dialog) = &mut this.worktree_ui.delete
                    && dialog.path == path
                {
                    dialog.info = Some(info);
                    cx.notify();
                }
            });
        })
    }

    /// A new terminal (or action) would start in a worktree being removed: say why and
    /// refuse. Returns whether it was refused.
    pub(super) fn refuse_terminal_in_removed_worktree(
        &mut self,
        cwd: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.worktree_being_removed(cwd) {
            return false;
        }
        self.error = Some(worktree::remove::BEING_REMOVED.into());
        cx.notify();
        true
    }

    /// `cwd` is (inside) a worktree this window is removing, or one being removed
    /// elsewhere in the app.
    pub(super) fn worktree_being_removed(&self, cwd: &str) -> bool {
        self.removing_worktrees
            .keys()
            .any(|path| is_equal_or_inside(cwd, path))
            || worktree::remove::is_being_removed(cwd)
    }

    fn close_delete_worktree_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktree_ui.delete.as_ref().is_some_and(|d| d.busy) {
            return;
        }
        if self.worktree_ui.delete.take().is_some() {
            self.focus_workspace(window, cx);
            cx.notify();
        }
    }

    /// Sessions whose working copy is `path` or inside it.
    pub(super) fn sessions_in_worktree(&self, path: &str, cx: &App) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .session_summaries
            .keys()
            .filter(|id| is_equal_or_inside(self.session_working_copy(**id, cx), path))
            .copied()
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Why `path` cannot be removed right now: a terminal open there or a busy session.
    fn worktree_in_use(&self, path: &str, ids: &[i64], cx: &App) -> Option<String> {
        if self
            .terminals
            .values()
            .flatten()
            .any(|t| is_equal_or_inside(&t.read(cx).cwd, path))
        {
            return Some("Close the terminals using this worktree first.".into());
        }
        if ids
            .iter()
            .any(|id| self.sessions.get(id).is_some_and(|v| v.read(cx).busy))
        {
            return Some("Stop the running sessions using this worktree first.".into());
        }
        None
    }

    /// The dialog's Delete (`force` false) and Force delete buttons.
    fn confirm_delete_worktree(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.worktree_ui.delete else {
            return;
        };
        let Some(Ok(info)) = &dialog.info else {
            return;
        };
        if dialog.busy || (info.preflight.needs_force() && !force) {
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        let (project, path, delete_sessions) = (
            dialog.project.clone(),
            dialog.path.clone(),
            dialog.delete_sessions,
        );
        let opts = RemoveOptions {
            force,
            ..RemoveOptions::default()
        };
        self.remove_worktree(project, path, delete_sessions, opts, window, cx);
        cx.notify();
    }

    /// Remove a worktree and, when asked, the sessions using it; see
    /// [`Self::remove_worktree_with`].
    pub(super) fn remove_worktree(
        &mut self,
        cwd: String,
        path: String,
        delete_sessions: bool,
        opts: RemoveOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_worktree_with(
            RemoveRequest {
                cwd,
                path,
                delete_sessions,
                opts,
                merged_into: None,
                done: None,
            },
            window,
            cx,
        );
    }

    /// Remove a worktree and, when asked, the sessions using it. Sessions kept move to
    /// the checkout of the branch the worktree came from (or, without one, to the main
    /// checkout marked `worktree_removed`). Refuses while a terminal is open there or
    /// one of its sessions is busy. Reports to [`Self::finish_worktree_removal`].
    pub(crate) fn remove_worktree_with(
        &mut self,
        request: RemoveRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let RemoveRequest {
            cwd,
            path,
            delete_sessions,
            opts,
            merged_into,
            done,
        } = request;
        let ids = self.sessions_in_worktree(&path, cx);
        if let Some(error) = self.worktree_in_use(&path, &ids, cx) {
            self.finish_worktree_removal(&path, Err(error), done, window, cx);
            return;
        }
        let (check_cwd, check_path) = (cwd.clone(), path.clone());
        let check = cx.background_executor().spawn(async move {
            let pre = worktree::remove::preflight(Path::new(&check_cwd), Path::new(&check_path))?;
            if pre.needs_force() && !opts.force {
                return Err(format!(
                    "This worktree has {} and {} that would be lost.",
                    plural(pre.dirty.len(), "uncommitted change", "uncommitted changes"),
                    plural(pre.at_risk.len(), "local file", "local files"),
                ));
            }
            Ok(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let mut done = done;
            if let Err(error) = check.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.finish_worktree_removal(&path, Err(error), done.take(), window, cx)
                });
                return;
            }
            // Recheck after the worker. Sessions are deleted only after Git removed the
            // worktree, so a failed removal never costs them; check now that they can be.
            let pending = this.update(cx, |this, cx| {
                if let Some(error) = this.worktree_in_use(&path, &ids, cx) {
                    return Err(format!("This worktree is now in use. {error}"));
                }
                if delete_sessions
                    && let Some(blocker) =
                        ids.iter().find_map(|id| this.session_delete_blocker(*id, cx))
                {
                    return Err(format!("{blocker}, or keep its sessions."));
                }
                // From here until it finishes, nothing new may start there.
                this.removing_worktrees.insert(
                    path.clone(),
                    worktree::remove::mark_removing(Path::new(&path)),
                );
                Ok((this.group_writes.take(), this.store.clone()))
            });
            let (writes, store) = match pending {
                Ok(Ok(pending)) => pending,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.finish_worktree_removal(&path, Err(error), done.take(), window, cx)
                    });
                    return;
                }
                Err(_) => return,
            };
            if let Some(writes) = writes {
                writes.await;
            }
            let (worker_cwd, worker_path) = (cwd.clone(), path.clone());
            let work = cx.background_executor().spawn(async move {
                let main = worktree::layout::main_checkout(Path::new(&worker_cwd))?;
                // Read before removal: deleting the branch also drops `branch.<b>.base`.
                let base = worktree::merge::base_of(&main, Path::new(&worker_path));
                let mut removed = worktree::remove::remove(
                    Path::new(&worker_cwd),
                    Path::new(&worker_path),
                    &opts,
                    &store,
                )?;
                if let (Some(base), Some(branch)) = (&merged_into, removed.preserved_branch.clone())
                    && worktree::merge::delete_merged_branch(&main, &branch, base)
                {
                    removed.preserved_branch = None;
                    removed.branch_deleted = true;
                }
                let to = base
                    .and_then(|base| worktree::merge::checkout_of(&main, &base))
                    .map(|p| p.to_string_lossy().into_owned());
                // The worktree is gone either way; failing to move its sessions off it
                // is reported, not a failed removal. Sessions about to be deleted are
                // moved too, so one whose deletion fails still points at a real folder.
                let detached = match detach_sessions(&store, &worker_path, &main, to.as_deref())
                {
                    Ok(detached) => detached,
                    Err(error) => {
                        removed.warnings.push(format!(
                            "The worktree was deleted, but its sessions could not be updated: {error}"
                        ));
                        Vec::new()
                    }
                };
                let moved_to = to.unwrap_or_else(|| main.to_string_lossy().into_owned());
                Ok::<_, String>((removed, detached, main, moved_to))
            });
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let removed_ok = result.is_ok();
                let result = result.map(|(removed, detached, main, moved_to)| {
                    let moved = detached
                        .iter()
                        .filter(|(s, _)| !(delete_sessions && ids.contains(&s.id)))
                        .count();
                    for (summary, settings) in detached {
                        this.session_summaries.insert(summary.id, summary.clone());
                        this.session_worktrees
                            .insert(summary.id, settings.worktree_cwd.clone());
                        if let Some(view) = this.sessions.get(&summary.id) {
                            view.update(cx, |v, cx| v.worktree_deleted(summary, settings, cx));
                        }
                    }
                    this.worktree_focus
                        .retain(|_, focus| !is_equal_or_inside(&focus.path, &path));
                    // Its layout follows its chats to the base's checkout and
                    // folds into that scope's tab (docs/native/tab-groups.md).
                    this.workspace
                        .tab_workspaces
                        .retain(|_, worktree| !is_equal_or_inside(worktree, &path));
                    this.merge_scope_tabs(cx);
                    (
                        RemovalReport {
                            removed,
                            moved_to,
                            moved,
                        },
                        main,
                    )
                });
                this.finish_worktree_removal(&path, result, done.take(), window, cx);
                // Only now that the worktree is gone; one that fails to delete stays,
                // moved to the main checkout, and reports why.
                if removed_ok && delete_sessions {
                    for id in &ids {
                        this.delete_session(*id, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn finish_worktree_removal(
        &mut self,
        path: &str,
        result: Result<(RemovalReport, std::path::PathBuf), String>,
        done: Option<RemovalDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.removing_worktrees.remove(path);
        if let Some(done) = done {
            // The asker reports errors; success changes the app as a dialog's would.
            let report = result
                .as_ref()
                .map(|(report, _)| report.clone())
                .map_err(Clone::clone);
            if let Ok((report, main)) = result {
                self.removal_succeeded(path, report.removed, main, cx);
            }
            self.sync_git(cx);
            self.sync_settings(cx);
            self.invalidate_chrome(cx);
            done(self, report, window, cx);
            cx.notify();
            return;
        }
        match result {
            Ok((report, main)) => self.removal_succeeded(path, report.removed, main, cx),
            Err(error) => match &mut self.worktree_ui.delete {
                Some(dialog) => {
                    dialog.busy = false;
                    dialog.error = Some(error);
                    // The tree may have changed since the dialog read it (e.g. new
                    // uncommitted changes): read it again, so Force shows when needed.
                    if dialog.path == path {
                        let project = dialog.project.clone();
                        let task = self.inspect_for_delete_dialog(&project, path, cx);
                        if let Some(dialog) = &mut self.worktree_ui.delete {
                            dialog._load = task;
                        }
                    }
                }
                None => self.error = Some(error),
            },
        }
        self.sync_git(cx);
        self.sync_settings(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// A removal went through: the kept-branch card, warnings, Settings and the rail.
    fn removal_succeeded(
        &mut self,
        path: &str,
        removed: Removed,
        main: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self
            .worktree_ui
            .delete
            .as_ref()
            .is_some_and(|d| d.path == path)
        {
            self.worktree_ui.delete = None;
        }
        if let Some(branch) = removed.preserved_branch {
            self.worktree_ui.next_kept += 1;
            self.worktree_ui.kept.push(KeptBranch {
                id: self.worktree_ui.next_kept,
                cwd: main.to_string_lossy().into_owned(),
                branch,
                busy: false,
                error: None,
            });
        }
        if !removed.warnings.is_empty() {
            self.error = Some(removed.warnings.join("\n"));
        }
        if let Some(view) = self.settings.clone() {
            view.update(cx, |view, cx| view.worktree_removed(path, cx));
        }
        self.invalidate_rail_worktrees(cx);
    }

    /// The kept card's "Delete branch": `git branch -D`.
    fn delete_kept_branch(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(kept) = self.worktree_ui.kept.iter_mut().find(|k| k.id == id) else {
            return;
        };
        if kept.busy {
            return;
        }
        kept.busy = true;
        kept.error = None;
        let (cwd, branch) = (kept.cwd.clone(), kept.branch.clone());
        let work = cx
            .background_executor()
            .spawn(async move { worktree::remove::force_delete_branch(Path::new(&cwd), &branch) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.worktree_ui.kept.retain(|k| k.id != id),
                    Err(error) => {
                        if let Some(kept) = this.worktree_ui.kept.iter_mut().find(|k| k.id == id) {
                            kept.busy = false;
                            kept.error = Some(error);
                        }
                    }
                }
                this.sync_git(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Kept branch <b> (not merged)" cards with Delete branch / Dismiss.
    pub(super) fn kept_branch_cards(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.worktree_ui
            .kept
            .iter()
            .map(|kept| {
                let id = kept.id;
                let detail = kept.error.clone().unwrap_or_else(|| {
                    "Deleting it also discards its commits that no other branch has.".into()
                });
                crate::notifications::surface::toast_frame()
                    .id(("worktree-kept", id as usize))
                    .debug_selector(move || format!("worktree-kept-{id}"))
                    .role(Role::Status)
                    .child(
                        div()
                            .px(px(14.))
                            .py_3()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(icon("git-branch", 16.).text_color(rgb(0xfbbf24)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(format!(
                                                "Kept branch {} (not merged)",
                                                kept.branch
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(if kept.error.is_some() {
                                                rgb(0xf87171).into()
                                            } else {
                                                content(0.7)
                                            })
                                            .child(detail),
                                    )
                                    .child(
                                        div().mt_1().flex().child(
                                            crate::modal::disabled(
                                                crate::modal::danger_ghost_button(
                                                    ("worktree-kept-delete", id as usize),
                                                    "Delete branch",
                                                ),
                                                kept.busy,
                                            )
                                            .debug_selector(move || {
                                                format!("worktree-kept-delete-{id}")
                                            })
                                            .h(px(26.))
                                            .px(px(10.))
                                            .text_size(px(12.))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.delete_kept_branch(id, cx)
                                                }),
                                            ),
                                        ),
                                    ),
                            )
                            .child(
                                icon_button("worktree-kept-dismiss", "Dismiss", "close")
                                    .debug_selector(move || format!("worktree-kept-dismiss-{id}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.worktree_ui.kept.retain(|k| k.id != id);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The Delete Worktree dialog over the window.
    pub(super) fn delete_worktree_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.worktree_ui.delete.as_ref()?;
        let focus = dialog.focus.clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let sessions = self.sessions_in_worktree(&dialog.path, cx).len();
        let busy = dialog.busy;
        let delete_sessions = dialog.delete_sessions;
        let info = dialog.info.clone();
        let error = dialog.error.clone();
        let fallback = Path::new(&dialog.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dialog.path.clone());
        let name = info
            .as_ref()
            .and_then(|i| i.as_ref().ok())
            .and_then(|i| i.display_name.clone())
            .unwrap_or(fallback);
        let line = |id: &'static str, text: String, warn: bool| {
            div()
                .debug_selector(move || id.to_owned())
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(if warn {
                    rgb(0xfbbf24).into()
                } else {
                    content(0.7)
                })
                .child(text)
        };
        let mut details = div().mt(px(12.)).flex().flex_col().gap(px(6.));
        let mut needs_force = false;
        let mut ready = false;
        match &info {
            None => {
                details = details.child(line(
                    "worktree-delete-loading",
                    "Checking the worktree…".into(),
                    false,
                ))
            }
            Some(Err(error)) => {
                details = details.child(
                    div()
                        .id("worktree-delete-error")
                        .debug_selector(|| "worktree-delete-error".into())
                        .role(Role::Alert)
                        .text_size(px(12.))
                        .text_color(rgb(0xf87171))
                        .child(error.clone()),
                )
            }
            Some(Ok(info)) => {
                ready = true;
                let pre = &info.preflight;
                needs_force = pre.needs_force();
                let owner = match info.ownership {
                    Ownership::Managed => "Managed by Monocode",
                    Ownership::Legacy => "Legacy Monocode worktree",
                    Ownership::External => "Made outside Monocode",
                    Ownership::Main => "Main checkout",
                };
                details = details.child(line(
                    "worktree-delete-branch",
                    format!("Branch {} · {owner}", pre.branch),
                    false,
                ));
                if pre.missing {
                    details = details.child(line(
                        "worktree-delete-missing",
                        "The folder is already gone; deleting only forgets it.".into(),
                        false,
                    ));
                }
                if !pre.dirty.is_empty() {
                    details = details.child(line(
                        "worktree-delete-dirty",
                        format!(
                            "{} will be discarded.",
                            plural(pre.dirty.len(), "uncommitted change", "uncommitted changes")
                        ),
                        true,
                    ));
                }
                if !pre.at_risk.is_empty() {
                    let more = pre.at_risk.len().saturating_sub(AT_RISK_SAMPLE);
                    details = details.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(line(
                                "worktree-delete-at-risk",
                                format!(
                                    "{} ignored by Git will be lost:",
                                    plural(pre.at_risk.len(), "local file", "local files")
                                ),
                                true,
                            ))
                            .child(
                                div()
                                    .pl(px(10.))
                                    .flex()
                                    .flex_col()
                                    .font_family(crate::fonts::mono())
                                    .text_size(px(11.))
                                    .line_height(px(16.))
                                    .text_color(content(0.6))
                                    .children(
                                        pre.at_risk
                                            .iter()
                                            .take(AT_RISK_SAMPLE)
                                            .map(|p| div().truncate().child(p.clone())),
                                    )
                                    .when(more > 0 || pre.at_risk_truncated, |d| {
                                        d.child(
                                            div()
                                                .debug_selector(|| {
                                                    "worktree-delete-at-risk-more".into()
                                                })
                                                .child(format!(
                                                    "+{more}{} more",
                                                    if pre.at_risk_truncated { "+" } else { "" }
                                                )),
                                        )
                                    }),
                            ),
                    );
                }
                if pre.unpushed > 0 {
                    details = details.child(line(
                        "worktree-delete-unpushed",
                        format!(
                            "{} on no other branch or remote.",
                            plural(pre.unpushed, "commit", "commits")
                        ),
                        true,
                    ));
                }
                details = details.child(line(
                    "worktree-delete-branch-policy",
                    if info.deletes_branch {
                        format!(
                            "Branch {} will be deleted if merged; kept otherwise.",
                            pre.branch
                        )
                    } else {
                        format!("Branch {} is kept.", pre.branch)
                    },
                    false,
                ));
            }
        }
        if sessions > 0 {
            details = details.child(
                div()
                    .id("worktree-delete-sessions")
                    .debug_selector(|| "worktree-delete-sessions".into())
                    .role(Role::CheckBox)
                    .aria_label(format!(
                        "Also delete its {}",
                        plural(sessions, "session", "sessions")
                    ))
                    .aria_toggled(if delete_sessions {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .mt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(content(if delete_sessions { 0.85 } else { 0.55 }))
                    .hover(|s| s.text_color(content(0.9)))
                    .child(
                        div()
                            .size(px(14.))
                            .flex_shrink_0()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(content(if delete_sessions { 0.9 } else { 0.35 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(delete_sessions, |d| {
                                d.bg(content(0.9))
                                    .child(icon("check", 10.).text_color(background_base()))
                            }),
                    )
                    .child(format!(
                        "Also delete its {}",
                        plural(sessions, "session", "sessions")
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(dialog) = &mut this.worktree_ui.delete
                            && !dialog.busy
                        {
                            dialog.delete_sessions = !dialog.delete_sessions;
                            cx.notify();
                        }
                    })),
            );
        }
        if let Some(error) = error {
            details = details.child(
                div()
                    .id("worktree-delete-failed")
                    .debug_selector(|| "worktree-delete-failed".into())
                    .role(Role::Alert)
                    .text_size(px(12.))
                    .text_color(rgb(0xf87171))
                    .child(error),
            );
        }
        let delete_label = if busy && !needs_force {
            "Deleting…"
        } else {
            "Delete"
        };
        let card = crate::modal::card().w(px(440.)).child(
            crate::modal::card_body()
                .child(crate::modal::title("Delete worktree?"))
                .child(
                    div()
                        .mt(px(12.))
                        .px(px(10.))
                        .py(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .rounded(px(9.))
                        .bg(content(0.05))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .text_size(px(13.))
                                .text_color(content(0.9))
                                .child(icon("folder-tree", 14.).text_color(content(0.6)))
                                .child(div().min_w_0().truncate().child(name)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(content(0.45))
                                .child(dialog.path.clone()),
                        ),
                )
                .child(details)
                .child(
                    crate::modal::actions()
                        .child(
                            crate::modal::disabled(
                                crate::modal::ghost_button("worktree-delete-cancel", "Cancel"),
                                busy,
                            )
                            .debug_selector(|| "worktree-delete-cancel".into())
                            .on_click(cx.listener(
                                |this, _, window, cx| this.close_delete_worktree_dialog(window, cx),
                            )),
                        )
                        .child(
                            crate::modal::disabled(
                                crate::modal::danger_button(
                                    "worktree-delete-confirm",
                                    delete_label,
                                ),
                                !ready || needs_force || busy,
                            )
                            .debug_selector(|| "worktree-delete-confirm".into())
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.confirm_delete_worktree(false, window, cx)
                                },
                            )),
                        )
                        .when(needs_force, |d| {
                            d.child(
                                crate::modal::disabled(
                                    crate::modal::danger_button(
                                        "worktree-delete-force",
                                        if busy { "Deleting…" } else { "Force delete" },
                                    ),
                                    busy,
                                )
                                .debug_selector(|| "worktree-delete-force".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.confirm_delete_worktree(true, window, cx)
                                    },
                                )),
                            )
                        }),
                ),
        );
        let entity = cx.weak_entity();
        Some(
            div()
                .id("worktree-delete-dialog")
                .debug_selector(|| "worktree-delete-dialog".into())
                .role(Role::Dialog)
                .aria_label("Delete worktree")
                .absolute()
                .inset_0()
                .track_focus(&focus)
                .key_context("DeleteWorktreeDialog")
                .on_action(cx.listener(|this, _: &DismissMenu, window, cx| {
                    cx.stop_propagation();
                    this.close_delete_worktree_dialog(window, cx);
                }))
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_delete_worktree_dialog(window, cx);
                    }
                }))
                .child(crate::modal::overlay(
                    "worktree-delete",
                    card,
                    move |window, cx| {
                        let _ = entity
                            .update(cx, |this, cx| this.close_delete_worktree_dialog(window, cx));
                    },
                ))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::worktree_dialog::tests::{bind, git, repo};
    use super::*;
    use crate::workspace::Workspace;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle};
    use monocode_core::store::{Session, Store};
    use std::path::PathBuf;

    /// A shell on `main` whose store holds the worktree prefs and `sessions` (cwd each).
    fn shell(
        cx: &mut TestAppContext,
        main: &str,
        root: &Path,
        names: &[&str],
        sessions_in: &[usize],
    ) -> (WindowHandle<Shell>, Vec<PathBuf>, Vec<Session>) {
        bind(cx);
        let store: Database = Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()));
        worktree::meta::save_prefs(
            &locked(&store).unwrap(),
            &worktree::Prefs {
                root: Some(root.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap();
        let paths: Vec<PathBuf> = names
            .iter()
            .map(|name| {
                let request = worktree::CreateRequest {
                    created_by: worktree::CreatedBy::Dialog,
                    ..worktree::CreateRequest::new(Path::new(main), name)
                };
                worktree::create::create(&request, &store, &|_| {})
                    .unwrap()
                    .path
            })
            .collect();
        let sessions: Vec<Session> = sessions_in
            .iter()
            .map(|ix| {
                locked(&store)
                    .unwrap()
                    .create("claude", &paths[*ix].to_string_lossy())
                    .unwrap()
            })
            .collect();
        let mut state = Workspace {
            new_layout_boxy: false,
            ..Default::default()
        };
        state.add_project(main);
        let saved = serde_json::to_string(&state).unwrap();
        let summaries = sessions.clone();
        let project = main.to_owned();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, summaries, project, Some(saved), cx))
            })
            .unwrap()
        });
        cx.run_until_parked();
        (window, paths, sessions)
    }

    fn open(window: WindowHandle<Shell>, cx: &mut VisualTestContext, main: &str, path: &Path) {
        window
            .update(cx, |shell, w, cx| {
                shell.open_delete_worktree_dialog(
                    main.to_owned(),
                    path.to_string_lossy().into_owned(),
                    w,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is shown"));
        cx.simulate_click(bounds.center(), Default::default());
        cx.run_until_parked();
    }

    fn info(window: WindowHandle<Shell>, cx: &mut VisualTestContext) -> DeleteInfo {
        window
            .update(cx, |shell, _, _| {
                shell
                    .worktree_ui
                    .delete
                    .as_ref()
                    .and_then(|d| d.info.clone())
                    .expect("the preflight ran")
                    .unwrap()
            })
            .unwrap()
    }

    fn open_dialog(window: WindowHandle<Shell>, cx: &mut VisualTestContext) -> bool {
        window
            .update(cx, |shell, _, _| shell.worktree_ui.delete.is_some())
            .unwrap()
    }

    fn has_branch(main: &str, branch: &str) -> bool {
        std::process::Command::new("git")
            .current_dir(main)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    #[gpui::test]
    fn a_dirty_worktree_needs_force_delete(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["dirty"], &[]);
        let wt = &paths[0];
        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        std::fs::write(wt.join("debug.log"), "x").unwrap();
        std::fs::write(wt.join(".env"), "SECRET=1").unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, wt);
        let found = info(window, visual);
        assert_eq!(found.preflight.dirty.len(), 1);
        assert_eq!(found.preflight.at_risk, [".env", "debug.log"], ".env first");
        assert_eq!(found.ownership, Ownership::Managed);
        for selector in [
            "worktree-delete-dirty",
            "worktree-delete-at-risk",
            "worktree-delete-branch-policy",
        ] {
            assert!(visual.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(visual.debug_bounds("worktree-delete-sessions").is_none());

        click(visual, "worktree-delete-confirm");
        assert!(
            open_dialog(window, visual),
            "Delete is disabled for a dirty tree"
        );
        assert!(wt.is_dir());
        click(visual, "worktree-delete-force");
        assert!(!open_dialog(window, visual));
        assert!(!wt.exists());
        assert!(
            !has_branch(&main, "dirty"),
            "a merged managed branch is deleted"
        );
    }

    #[gpui::test]
    fn an_unmerged_branch_is_kept_and_its_card_deletes_it(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["feature"], &[]);
        let wt = &paths[0];
        std::fs::write(wt.join("new.txt"), "work\n").unwrap();
        git(wt, &["add", "new.txt"]);
        git(wt, &["commit", "-q", "-m", "Work"]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, wt);
        assert_eq!(info(window, visual).preflight.unpushed, 1);
        assert!(visual.debug_bounds("worktree-delete-unpushed").is_some());
        click(visual, "worktree-delete-confirm");
        assert!(!wt.exists());
        assert!(has_branch(&main, "feature"), "an unmerged branch is kept");
        click(visual, "worktree-kept-delete-1");
        assert!(!has_branch(&main, "feature"));
        assert!(visual.debug_bounds("worktree-kept-1").is_none());
    }

    #[gpui::test]
    fn sessions_go_with_the_worktree_when_asked_and_open_terminals_block_it(
        cx: &mut TestAppContext,
    ) {
        let (_temp, main, root) = repo();
        let (window, paths, sessions) = shell(cx, &main, &root, &["busy", "used"], &[0, 1]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);

        // A terminal open in the worktree blocks its removal.
        let busy = paths[0].to_string_lossy().into_owned();
        window
            .update(visual, |shell, _, cx| {
                let launch = shell.terminal_launch(&main, &busy);
                let terminal = shell.spawn_terminal_with(busy.clone(), launch, cx);
                shell
                    .terminals
                    .entry(main.clone())
                    .or_default()
                    .push(terminal);
            })
            .unwrap();
        open(window, visual, &main, &paths[0]);
        click(visual, "worktree-delete-confirm");
        assert!(open_dialog(window, visual));
        assert!(visual.debug_bounds("worktree-delete-failed").is_some());
        assert!(paths[0].is_dir());
        click(visual, "worktree-delete-cancel");
        assert!(!open_dialog(window, visual));

        // "Also delete its sessions" deletes them with the worktree.
        open(window, visual, &main, &paths[1]);
        click(visual, "worktree-delete-sessions");
        click(visual, "worktree-delete-confirm");
        assert!(!open_dialog(window, visual));
        assert!(!paths[1].exists());
        window
            .update(visual, |shell, _, _| {
                assert!(!shell.session_summaries.contains_key(&sessions[1].id));
                assert!(shell.session_summaries.contains_key(&sessions[0].id));
                let db = locked(&shell.store).unwrap();
                assert!(
                    db.sessions()
                        .unwrap()
                        .iter()
                        .all(|s| s.id != sessions[1].id)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn kept_sessions_carry_on_in_the_checkout_of_their_base(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, paths, sessions) = shell(cx, &main, &root, &["kept", "orphan"], &[0, 1]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let settings = |window: WindowHandle<Shell>, visual: &mut VisualTestContext, id| {
            window
                .update(visual, |shell, _, _| {
                    let summary = shell.session_summaries[&id].clone();
                    let settings = locked(&shell.store)
                        .unwrap()
                        .agent_settings(id)
                        .unwrap()
                        .unwrap();
                    (summary, settings, shell.session_worktrees.get(&id).cloned())
                })
                .unwrap()
        };
        // Its base (the branch the main checkout is on) is checked out: it goes there.
        open(window, visual, &main, &paths[0]);
        assert!(visual.debug_bounds("worktree-delete-sessions").is_some());
        click(visual, "worktree-delete-confirm");
        assert!(!paths[0].exists());
        let (summary, saved, rail) = settings(window, visual, sessions[0].id);
        assert_eq!(summary.cwd, main, "the session moves to the main checkout");
        assert!(!saved.worktree_removed, "and carries on there");
        assert_eq!(saved.worktree_cwd, None);
        assert_eq!(rail, Some(None), "the rail lists it under the project");
        // A base checked out nowhere: the main checkout, marked removed (pick one).
        git(&paths[1], &["config", "branch.orphan.base", "nowhere"]);
        open(window, visual, &main, &paths[1]);
        click(visual, "worktree-delete-confirm");
        assert!(!paths[1].exists());
        let (summary, saved, _) = settings(window, visual, sessions[1].id);
        assert_eq!(summary.cwd, main);
        assert!(saved.worktree_removed);
    }

    #[gpui::test]
    fn a_tree_that_changed_since_the_dialog_opened_offers_force_after_delete_fails(
        cx: &mut TestAppContext,
    ) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["late"], &[]);
        let wt = &paths[0];
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, wt);
        assert!(!info(window, visual).preflight.needs_force());
        assert!(visual.debug_bounds("worktree-delete-force").is_none());
        // Changed after the dialog read it: Delete refuses, and the dialog reads again.
        std::fs::write(wt.join("README.md"), "changed\n").unwrap();
        click(visual, "worktree-delete-confirm");
        assert!(open_dialog(window, visual));
        assert!(wt.is_dir());
        assert!(visual.debug_bounds("worktree-delete-failed").is_some());
        assert_eq!(info(window, visual).preflight.dirty.len(), 1);
        assert!(
            visual.debug_bounds("worktree-delete-force").is_some(),
            "Force delete is offered now"
        );
        click(visual, "worktree-delete-force");
        assert!(!open_dialog(window, visual));
        assert!(!wt.exists());
    }

    #[gpui::test]
    fn a_worktree_being_removed_refuses_new_terminals_and_turns_until_it_finishes(
        cx: &mut TestAppContext,
    ) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["going"], &[]);
        let path = paths[0].to_string_lossy().into_owned();
        let inside = paths[0].join("src").to_string_lossy().into_owned();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.select_worktree(
                    Some(crate::worktree_switcher::WorktreeFocus {
                        path: path.clone(),
                        branch: None,
                    }),
                    w,
                    cx,
                );
                // What the removal's recheck does.
                shell
                    .removing_worktrees
                    .insert(path.clone(), worktree::remove::mark_removing(&paths[0]));
                assert!(shell.worktree_being_removed(&inside));
                assert!(worktree::remove::is_being_removed(&inside));
                let before = shell.terminals.get(&main).map_or(0, Vec::len);
                shell.new_terminal(w, cx);
                assert_eq!(shell.terminals.get(&main).map_or(0, Vec::len), before);
                assert_eq!(
                    shell.error.as_deref(),
                    Some(worktree::remove::BEING_REMOVED)
                );
                // A failed removal lets them start again.
                shell.finish_worktree_removal(&path, Err("refused".into()), None, w, cx);
                assert!(shell.removing_worktrees.is_empty());
                assert!(!shell.worktree_being_removed(&inside));
                assert!(!worktree::remove::is_being_removed(&inside));
            })
            .unwrap();

        // A real removal clears its mark when it succeeds.
        open(window, visual, &main, &paths[0]);
        click(visual, "worktree-delete-confirm");
        assert!(!paths[0].exists());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.removing_worktrees.is_empty());
                assert!(!worktree::remove::is_being_removed(&path));
            })
            .unwrap();
    }

    #[cfg(unix)]
    #[gpui::test]
    fn a_failed_removal_keeps_the_sessions_it_was_asked_to_delete(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, main, root) = repo();
        let (window, paths, sessions) = shell(cx, &main, &root, &["stuck"], &[0]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, &paths[0]);
        click(visual, "worktree-delete-sessions");
        // Git cannot remove the folder from a read-only parent.
        let parent = paths[0].parent().unwrap().to_path_buf();
        let mode = |m| std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(m));
        mode(0o555).unwrap();
        click(visual, "worktree-delete-confirm");
        mode(0o755).unwrap();
        assert!(paths[0].is_dir(), "the removal failed");
        assert!(open_dialog(window, visual));
        assert!(visual.debug_bounds("worktree-delete-failed").is_some());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.session_summaries.contains_key(&sessions[0].id));
                let db = locked(&shell.store).unwrap();
                assert!(
                    db.sessions()
                        .unwrap()
                        .iter()
                        .any(|s| s.id == sessions[0].id)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_worktrees_tab_shows_what_is_unmerged_and_merges_it(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["tabbed"], &[0]);
        let wt = &paths[0];
        std::fs::write(wt.join("tab.txt"), "tab\n").unwrap();
        git(wt, &["add", "tab.txt"]);
        git(wt, &["commit", "-q", "-m", "Tab work"]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, window, cx| {
                shell.workspace.right_panel_open = true;
                shell.ensure_right_panel(window, cx);
                let panel = shell.right_panel.clone().unwrap();
                panel.update(cx, |panel, cx| {
                    panel.set_shown(true, cx);
                    panel.select(crate::right_panel::RightTab::Worktrees, cx);
                });
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        let rows = |window: WindowHandle<Shell>, visual: &mut VisualTestContext| {
            window
                .update(visual, |shell, _, cx| {
                    let panel = shell.right_panel.clone().unwrap();
                    let pane = panel.read(cx).worktrees.clone();
                    pane.read(cx).rows().map(<[_]>::to_vec).unwrap_or_default()
                })
                .unwrap()
        };
        let listed = rows(window, visual);
        assert_eq!(listed.len(), 2, "the project folder and its worktree");
        let row = &listed[1];
        let status = row.status.clone().expect("its state against its base");
        assert_eq!((status.ahead, status.dirty), (1, 0));
        assert_eq!(row.sessions, 1);
        assert!(visual.debug_bounds("worktree-pane-unmerged-1").is_some());
        click(visual, "worktree-pane-merge-1");
        assert!(std::path::Path::new(&main).join("tab.txt").is_file());
        assert!(wt.exists(), "Merge alone keeps the worktree");
        let row = rows(window, visual)[1].clone();
        assert_eq!(row.status.unwrap().ahead, 0);
        assert!(visual.debug_bounds("worktree-pane-merged-1").is_some());
        assert!(
            visual.debug_bounds("worktree-merged").is_some(),
            "the merged toast"
        );
    }

    #[gpui::test]
    fn sessions_that_cannot_be_detached_after_removal_are_a_warning(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, paths, _) = shell(cx, &main, &root, &["gone"], &[0]);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, &paths[0]);
        FAIL_DETACH.set(true);
        click(visual, "worktree-delete-confirm");
        FAIL_DETACH.set(false);
        assert!(!paths[0].exists());
        assert!(!open_dialog(window, visual), "the removal still finishes");
        window
            .update(visual, |shell, _, _| {
                let error = shell.error.clone().unwrap_or_default();
                assert!(
                    error.contains("its sessions could not be updated: database is locked"),
                    "{error}"
                );
                assert!(shell.removing_worktrees.is_empty());
            })
            .unwrap();
    }
}
