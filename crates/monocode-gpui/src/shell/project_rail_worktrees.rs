//! The rail tree's worktree rows (spec §7.3): under an open local Git project, its
//! managed and legacy worktrees (and external ones when the repository shows them),
//! each expandable to the sessions working there, plus creations in progress.
//!
//! The list comes from `git worktree list` joined with the stored metadata. It is
//! read in the background, only for a project whose row is drawn and open, and kept
//! per project until a create, a delete or a Settings change marks it stale.
use super::project_rail::ProjectRailRow;
use super::*;
use crate::worktree_switcher::WorktreeFocus;
use monocode_core::{
    files::{is_equal_or_inside, path_key},
    worktree::{self, Ownership},
};
use std::path::Path;

/// Sessions listed under an open worktree.
const MAX_WORKTREE_SESSIONS: usize = 8;

/// One worktree row's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RailWorktree {
    pub path: String,
    pub branch: Option<String>,
    /// The meta's display name when the user chose one, else the branch, else the
    /// folder name.
    pub name: String,
    pub ownership: Ownership,
}

/// What one load found for a project.
#[derive(Clone, Debug, Default)]
pub(super) struct Listed {
    /// The main checkout, as Git spells it.
    pub main: Option<String>,
    pub trees: Vec<RailWorktree>,
}

/// The per-project cache.
#[derive(Default)]
pub(super) struct RailWorktrees {
    lists: HashMap<String, Listed>,
    /// Projects whose list is shown but must be read again.
    stale: HashSet<String>,
    loading: HashMap<String, gpui::Task<()>>,
    /// Loads started, for tests.
    #[cfg(test)]
    pub loads: usize,
    /// Times the lists were marked stale, for tests.
    #[cfg(test)]
    pub invalidations: usize,
}

/// The worktrees the rail lists for `project`: never the main checkout or a missing
/// directory; external ones only when the repository's prefs show them.
pub(super) fn load(project: &Path, store: &Database) -> Result<Listed, String> {
    let entries = worktree::catalog::list_in(project, store)?;
    let main = entries
        .iter()
        .find(|e| e.ownership == Ownership::Main)
        .map(|e| e.path.clone());
    let show_external = match &main {
        Some(main) => {
            worktree::meta::repo_prefs(&*locked(store)?, main)
                .unwrap_or_default()
                .show_external
        }
        None => false,
    };
    let trees = entries
        .into_iter()
        .filter(|e| !e.missing)
        .filter(|e| match e.ownership {
            Ownership::Main => false,
            Ownership::Managed | Ownership::Legacy => true,
            Ownership::External => show_external,
        })
        .map(|e| {
            let path = e.path.to_string_lossy().into_owned();
            let folder = project_name(&path).to_string();
            // Only the dialog's and switcher's display names are the user's; a
            // session-made worktree's is a slug of the prompt (as is a name that
            // only repeats the folder). Its branch, renamed once the session's
            // checkout is named, reads better.
            let name = e
                .meta
                .as_ref()
                .filter(|m| {
                    matches!(
                        m.created_by,
                        worktree::CreatedBy::Dialog | worktree::CreatedBy::Switcher
                    )
                })
                .map(|m| m.display_name.trim().to_owned())
                .filter(|n| !n.is_empty() && *n != folder)
                .or_else(|| e.branch.clone())
                .unwrap_or(folder);
            RailWorktree {
                path,
                branch: e.branch,
                name,
                ownership: e.ownership,
            }
        })
        .collect();
    Ok(Listed {
        main: main.map(|m| m.to_string_lossy().into_owned()),
        trees,
    })
}

/// Marks a project's final child row, which carries the gap before the next project.
pub(super) fn mark_last(row: &mut ProjectRailRow) {
    match row {
        ProjectRailRow::Session { last, .. }
        | ProjectRailRow::Terminal { last, .. }
        | ProjectRailRow::More { last, .. }
        | ProjectRailRow::Worktree { last, .. }
        | ProjectRailRow::WorktreeSession { last, .. }
        | ProjectRailRow::PendingWorktree { last, .. }
        | ProjectRailRow::Subagent { last, .. } => *last = true,
        _ => {}
    }
}

/// A short, stable number for a worktree's element ids.
fn ordinal(path: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path_key(path).hash(&mut hasher);
    hasher.finish() as usize
}

impl Shell {
    /// Read `project`'s worktrees in the background unless they are known and fresh
    /// or already being read. Remote projects have no worktree rows.
    pub(super) fn ensure_rail_worktrees(&mut self, project: &str, cx: &mut Context<Self>) {
        let cache = &self.rail_worktrees;
        if monocode_core::files::remote::is_remote(Path::new(project))
            || cache.loading.contains_key(project)
            || (cache.lists.contains_key(project) && !cache.stale.contains(project))
        {
            return;
        }
        #[cfg(test)]
        {
            self.rail_worktrees.loads += 1;
        }
        let store = self.store.clone();
        let folder = project.to_owned();
        let work = cx
            .background_executor()
            .spawn(async move { load(Path::new(&folder), &store) });
        let key = project.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let listed = work.await;
            let _ = this.update(cx, |this, cx| {
                this.rail_worktrees.loading.remove(&key);
                this.rail_worktrees.stale.remove(&key);
                // A folder that is not a repository simply lists nothing.
                let listed = listed.unwrap_or_default();
                let changed = this
                    .rail_worktrees
                    .lists
                    .get(&key)
                    .is_none_or(|old| old.trees != listed.trees || old.main != listed.main);
                this.rail_worktrees.lists.insert(key, listed);
                if changed {
                    this.projects_frame.update(cx, |_, cx| cx.notify());
                }
            });
        });
        self.rail_worktrees.loading.insert(project.to_owned(), task);
    }

    /// Worktrees changed somewhere (created, deleted, reconciled, Settings): every
    /// cached list is read again the next time its project is drawn open. The old
    /// rows stay until then, so nothing flickers.
    pub(super) fn invalidate_rail_worktrees(&mut self, cx: &mut Context<Self>) {
        let cache = &mut self.rail_worktrees;
        #[cfg(test)]
        {
            cache.invalidations += 1;
        }
        // A read already running may predate the change.
        cache.loading.clear();
        cache.stale.extend(cache.lists.keys().cloned());
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    /// A worktree was made or removed outside the dialogs (an app or Mono API call):
    /// the rail, Settings and the Git state read again.
    pub(crate) fn worktrees_changed_elsewhere(&mut self, cx: &mut Context<Self>) {
        self.invalidate_rail_worktrees(cx);
        self.refresh_settings_worktrees(cx);
        self.sync_git(cx);
    }

    fn rail_worktree(&self, project: &str, path: &str) -> Option<&RailWorktree> {
        let key = path_key(path);
        self.rail_worktrees
            .lists
            .get(project)?
            .trees
            .iter()
            .find(|t| path_key(&t.path) == key)
    }

    /// Whether `path` is `project`'s main checkout, which the rail never deletes.
    fn rail_worktree_is_main(&self, project: &str, path: &str) -> bool {
        let key = path_key(path);
        key == path_key(project)
            || self
                .rail_worktrees
                .lists
                .get(project)
                .and_then(|l| l.main.as_deref())
                .is_some_and(|main| path_key(main) == key)
            || self
                .rail_worktree(project, path)
                .is_some_and(|t| t.ownership == Ownership::Main)
    }

    /// The rows for `project`'s worktrees, with the sessions of each open one, then
    /// its pending creations. Sessions working in a listed worktree leave `ids`.
    pub(super) fn project_rail_worktree_rows(
        &self,
        project: &str,
        ids: &mut Vec<i64>,
        cx: &App,
    ) -> Vec<ProjectRailRow> {
        let trees = self
            .rail_worktrees
            .lists
            .get(project)
            .map_or(&[][..], |l| l.trees.as_slice());
        let mut under: Vec<Vec<i64>> = vec![Vec::new(); trees.len()];
        if !trees.is_empty() {
            let root = path_key(project);
            ids.retain(|id| {
                let copy = self.session_working_copy(*id, cx);
                if path_key(copy) == root {
                    return true;
                }
                // A session whose worktree is gone stays with the project.
                match trees.iter().position(|t| is_equal_or_inside(copy, &t.path)) {
                    Some(ix) => {
                        under[ix].push(*id);
                        false
                    }
                    None => true,
                }
            });
        }
        let mut rows = Vec::new();
        for (tree, sessions) in trees.iter().zip(under) {
            rows.push(ProjectRailRow::Worktree {
                project: project.to_owned(),
                path: tree.path.clone(),
                sessions: sessions.len(),
                last: false,
            });
            if self
                .workspace
                .project_rail_worktrees_expanded
                .contains(&tree.path)
            {
                let hidden = sessions.len().saturating_sub(MAX_WORKTREE_SESSIONS);
                rows.extend(sessions.into_iter().take(MAX_WORKTREE_SESSIONS).map(|id| {
                    ProjectRailRow::WorktreeSession {
                        project: project.to_owned(),
                        id,
                        last: false,
                    }
                }));
                if hidden > 0 {
                    rows.push(ProjectRailRow::More {
                        project: project.to_owned(),
                        hidden,
                        worktree: Some(tree.path.clone()),
                        last: false,
                    });
                }
            }
        }
        rows.extend(self.pending_worktrees_for(project).into_iter().map(|p| {
            ProjectRailRow::PendingWorktree {
                project: project.to_owned(),
                id: p.id,
                last: false,
            }
        }));
        rows
    }

    pub(super) fn toggle_rail_worktree_expanded(&mut self, path: &str, cx: &mut Context<Self>) {
        let open = &mut self.workspace.project_rail_worktrees_expanded;
        if !open.remove(path) {
            open.insert(path.into());
        }
        self.persist(cx);
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    /// Make `path` the focus of `project`, switching to the project first.
    pub(super) fn select_rail_worktree(
        &mut self,
        project: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.project != project {
            self.select_project(project.clone(), window, cx);
        }
        if self.workspace.project != project {
            return;
        }
        let branch = self
            .rail_worktree(&project, &path)
            .and_then(|t| t.branch.clone());
        self.select_worktree(Some(WorktreeFocus { path, branch }), window, cx);
        self.invalidate_chrome(cx);
        self.projects_frame.update(cx, |_, cx| cx.notify());
    }

    /// "New session here": a new chat of `project` working in `path`.
    fn new_session_in_rail_worktree(
        &mut self,
        project: String,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_rail_worktree(project.clone(), path.clone(), window, cx);
        if self.workspace.project != project {
            return;
        }
        // Selecting may already have brought up (or be creating) a blank chat there.
        let blank_here = match self.workspace.active {
            None => true,
            Some(id) => {
                let id = self.workspace.session_for_pane(id);
                self.session_summaries.contains_key(&id)
                    && !self.session_metadata(id, cx).summary.has_messages
                    && path_key(self.session_working_copy(id, cx)) == path_key(&path)
            }
        };
        if !blank_here {
            self.new_chat_in_selected_checkout(cx);
        }
    }

    /// The worktree row's context menu.
    pub(super) fn rail_worktree_menu(
        &self,
        project: &str,
        path: &str,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let name = self
            .rail_worktree(project, path)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| project_name(path).to_string());
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("worktree-open", "Open")
                .icon("folder-tree")
                .into(),
            MenuAction::new("worktree-new-session", "New session here")
                .icon("plus")
                .into(),
            MenuItem::Separator,
            MenuAction::new("worktree-copy-path", "Copy path")
                .icon("copy")
                .into(),
            MenuAction::new("reveal", REVEAL_LABEL)
                .icon("folder-open")
                .into(),
        ];
        if !self.rail_worktree_is_main(project, path) {
            items.extend([
                MenuItem::Separator,
                MenuAction::new("worktree-delete", "Delete…")
                    .icon("trash")
                    .danger()
                    .into(),
            ]);
        }
        (
            format!("Worktree actions for {name}").into(),
            items,
            220.,
            MenuStyle::Group,
        )
    }

    pub(super) fn run_rail_worktree_menu(
        &mut self,
        project: String,
        path: String,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "worktree-open" => self.select_rail_worktree(project, path, window, cx),
            "worktree-new-session" => self.new_session_in_rail_worktree(project, path, window, cx),
            "worktree-copy-path" => cx.write_to_clipboard(ClipboardItem::new_string(path)),
            "reveal" => cx.reveal_path(Path::new(&path)),
            "worktree-delete" if !self.rail_worktree_is_main(&project, &path) => {
                self.open_delete_worktree_dialog(project, path, window, cx)
            }
            _ => {}
        }
    }

    pub(super) fn project_rail_worktree_row(
        &mut self,
        project: String,
        path: String,
        sessions: usize,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tree) = self.rail_worktree(&project, &path).cloned() else {
            return div().into_any_element();
        };
        let n = ordinal(&path);
        let expanded = self
            .workspace
            .project_rail_worktrees_expanded
            .contains(&path);
        let focused = self.workspace.project == project
            && self
                .worktree_focus
                .get(&project)
                .is_some_and(|f| path_key(&f.path) == path_key(&path));
        let branch = tree.branch.clone().filter(|b| *b != tree.name);
        let selector = format!("rail-worktree-{path}");
        let chevron_selector = format!("rail-worktree-chevron-{path}");
        let (toggle_path, click_path, menu_path) = (path.clone(), path.clone(), path.clone());
        let (click_project, menu_project) = (project.clone(), project.clone());
        let label = match &branch {
            Some(branch) => format!("Worktree, {} on {branch}", tree.name),
            None => format!("Worktree, {}", tree.name),
        };
        self.project_rail_child_frame(("rail-worktree-row", n).into(), &project, last, cx)
            .child(
                self.project_rail_child_button(&project, ("rail-worktree", n), label, focused)
                    .debug_selector(move || selector.clone())
                    .aria_expanded(expanded)
                    .gap(px(6.))
                    .child(
                        div()
                            .id(("rail-worktree-chevron", n))
                            .debug_selector(move || chevron_selector.clone())
                            .role(Role::Button)
                            .aria_label(if expanded {
                                "Collapse worktree"
                            } else {
                                "Expand worktree"
                            })
                            .aria_expanded(expanded)
                            .size(px(14.))
                            .ml(px(-4.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(|s| s.bg(content(0.1)))
                            .child(
                                icon(
                                    if expanded {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    },
                                    11.,
                                )
                                .text_color(content(0.5)),
                            )
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_rail_worktree_expanded(&toggle_path, cx);
                            })),
                    )
                    .child(
                        icon("git-branch", 13.)
                            .flex_shrink_0()
                            .text_color(content(if focused { 0.9 } else { 0.5 })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_shrink(1.)
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(tree.name.clone())),
                            )
                            .when_some(branch, |d, branch| {
                                d.child(
                                    div()
                                        .flex_shrink(1.)
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(content(0.4))
                                        .child(SharedString::from(branch)),
                                )
                            }),
                    )
                    .when(sessions > 0, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(content(0.4))
                                .child(sessions.to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let path = click_path.clone();
                        if !this
                            .workspace
                            .project_rail_worktrees_expanded
                            .contains(&path)
                        {
                            this.toggle_rail_worktree_expanded(&path, cx);
                        }
                        this.select_rail_worktree(click_project.clone(), path, window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            this.open_menu(
                                MenuTarget::RailWorktree {
                                    project: menu_project.clone(),
                                    path: menu_path.clone(),
                                },
                                Anchor::Point(e.position),
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// "N more…" under an open worktree with more sessions than the tree lists; like
    /// the project's, it opens what it stands for (here, the worktree).
    pub(super) fn project_rail_worktree_more_row(
        &mut self,
        project: String,
        path: String,
        hidden: usize,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let n = ordinal(&path);
        let selector = format!("rail-worktree-more-{path}");
        let (target_project, target_path) = (project.clone(), path);
        self.project_rail_child_frame(("rail-worktree-more", n).into(), &project, last, cx)
            .child(
                self.project_rail_child_button(
                    &project,
                    ("rail-worktree-more-button", n),
                    format!("{hidden} more sessions"),
                    false,
                )
                .debug_selector(move || selector.clone())
                .ml(px(super::project_rail_tree::NESTED_INDENT))
                .text_color(content(0.45))
                .child(format!("{hidden} more…"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_rail_worktree(
                        target_project.clone(),
                        target_path.clone(),
                        window,
                        cx,
                    )
                })),
            )
            .into_any_element()
    }

    pub(super) fn project_rail_pending_worktree_row(
        &mut self,
        project: String,
        id: u64,
        last: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(pending) = self.pending_worktrees.iter().find(|p| p.id == id) else {
            return div().into_any_element();
        };
        let failed = pending.error.is_some();
        let status = pending.status();
        let name = pending.display_name.clone();
        let small = |id: (&'static str, usize), selector: String, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || selector.clone())
                .role(Role::Button)
                .aria_label(label)
                .flex_shrink_0()
                .px(px(5.))
                .rounded_sm()
                .text_size(px(11.))
                .text_color(content(0.7))
                .cursor_pointer()
                .hover(|s| s.bg(content(0.12)).text_color(content(1.)))
                .child(label)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let n = id as usize;
        let mark = if failed {
            icon("alert-circle", 13.)
                .text_color(rgb(0xf87171))
                .into_any_element()
        } else {
            crate::right_panel::spinner(("rail-pending-spinner", n), "loader", 12.)
        };
        self.project_rail_child_frame(("rail-pending-row", n).into(), &project, last, cx)
            .child(
                self.project_rail_child_button(
                    &project,
                    ("rail-pending", n),
                    format!("Creating {name}, {status}"),
                    false,
                )
                .debug_selector(move || format!("rail-pending-worktree-{id}"))
                .cursor_default()
                .gap(px(6.))
                .child(
                    div()
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(mark),
                )
                .child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(name)),
                )
                .child(
                    div()
                        .debug_selector(move || format!("rail-pending-status-{id}"))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(if failed {
                            rgb(0xf87171).into()
                        } else {
                            content(0.45)
                        })
                        .child(SharedString::from(status)),
                )
                .when(failed, |d| {
                    d.child(
                        small(
                            ("rail-pending-retry", n),
                            format!("rail-pending-retry-{id}"),
                            "Retry",
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.retry_worktree_create(id, window, cx)
                            },
                        )),
                    )
                    .child(
                        small(
                            ("rail-pending-dismiss", n),
                            format!("rail-pending-dismiss-{id}"),
                            "Dismiss",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.dismiss_pending_worktree(id, cx)
                        })),
                    )
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::super::worktree_create::{PendingWorktree, WorktreeOrigin};
    use super::super::worktree_dialog::tests::{git, repo};
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle, size};
    use monocode_core::worktree::{CreateRequest, Phase};
    use std::path::PathBuf;

    struct Fixture {
        _temp: tempfile::TempDir,
        main: String,
        managed: String,
        legacy: String,
        external: String,
        window: WindowHandle<Shell>,
        /// In `main`, working in `managed`.
        in_worktree: i64,
        /// In `main` itself.
        direct: i64,
    }

    fn at(visual: &mut VisualTestContext, id: String) -> Option<gpui::Bounds<Pixels>> {
        visual.debug_bounds(Box::leak(id.into_boxed_str()))
    }

    /// A repository with a managed, a legacy and an external worktree, added to the
    /// test shell with the rail tree on; `main` is not expanded yet.
    fn fixture(cx: &mut TestAppContext) -> (Fixture, VisualTestContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (temp, main, root) = repo();
        let base = PathBuf::from(&main).parent().unwrap().to_path_buf();
        let (window, in_worktree, direct) = window(cx);
        let store = window
            .update(cx, |shell, _, _| shell.store.clone())
            .unwrap();
        worktree::meta::save_prefs(
            &locked(&store).unwrap(),
            &worktree::Prefs {
                root: Some(root),
                ..Default::default()
            },
        )
        .unwrap();
        let created = worktree::create::create(
            &CreateRequest::new(Path::new(&main), "managed-one"),
            &store,
            &|_| {},
        )
        .unwrap();
        let managed = created.path.to_string_lossy().into_owned();
        let legacy = base.join("project-worktrees/legacy-one");
        let external = base.join("elsewhere/ext-one");
        for (branch, path) in [("legacy-one", &legacy), ("ext-one", &external)] {
            git(
                Path::new(&main),
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    branch,
                    &path.to_string_lossy(),
                ],
            );
        }
        let visual = VisualTestContext::from_window(window.into(), cx);
        let mut fixture = Fixture {
            _temp: temp,
            main,
            managed,
            legacy: legacy.to_string_lossy().into_owned(),
            external: external.to_string_lossy().into_owned(),
            window,
            in_worktree,
            direct,
        };
        let mut visual = visual;
        visual.simulate_resize(size(px(1500.), px(900.)));
        let f = &mut fixture;
        window
            .update(&mut visual, |shell, _, cx| {
                shell.workspace.add_project(&f.main);
                for (id, copy) in [(f.in_worktree, Some(f.managed.clone())), (f.direct, None)] {
                    let summary = shell.session_summaries.get_mut(&id).unwrap();
                    summary.cwd = f.main.clone();
                    summary.has_messages = true;
                    shell.session_worktrees.insert(id, copy.clone());
                    if let Some(view) = shell.sessions.get(&id) {
                        view.update(cx, |v, _| {
                            v.summary.cwd = f.main.clone();
                            v.summary.has_messages = true;
                            v.settings.worktree_cwd = copy.clone();
                        });
                    }
                }
                shell.workspace.classic_layout = true;
                shell.workspace.project_rail_open = true;
                shell.workspace.preferences.monos = false;
                shell.workspace.preferences.project_rail_view = "tree".into();
                shell.workspace.project_rail_expanded.clear();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        (fixture, visual)
    }

    fn expand(f: &Fixture, visual: &mut VisualTestContext, worktrees: &[&str]) {
        f.window
            .update(visual, |shell, _, cx| {
                shell.workspace.project_rail_expanded.insert(f.main.clone());
                shell.workspace.project_rail_worktrees_expanded =
                    worktrees.iter().map(|p| p.to_string()).collect();
                shell.invalidate_chrome(cx);
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
    }

    fn loads(f: &Fixture, visual: &mut VisualTestContext) -> usize {
        f.window
            .update(visual, |shell, _, _| shell.rail_worktrees.loads)
            .unwrap()
    }

    #[gpui::test]
    fn an_open_project_lists_managed_and_legacy_worktrees_and_external_ones_on_request(
        cx: &mut TestAppContext,
    ) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        assert_eq!(loads(&f, visual), 0, "a closed project reads no worktrees");
        assert!(at(visual, format!("rail-worktree-{}", f.managed)).is_none());
        expand(&f, visual, &[]);
        assert_eq!(loads(&f, visual), 1);
        let project = f
            .window
            .update(visual, |s, _, _| {
                s.workspace
                    .projects
                    .iter()
                    .position(|p| *p == f.main)
                    .unwrap()
            })
            .unwrap();
        let button = at(visual, format!("rail-project-button-{project}")).unwrap();
        let managed = at(visual, format!("rail-worktree-{}", f.managed))
            .expect("a managed worktree is a row under its project");
        assert!(managed.top() >= button.bottom());
        assert!(at(visual, format!("rail-worktree-{}", f.legacy)).is_some());
        assert!(
            at(visual, format!("rail-worktree-{}", f.external)).is_none(),
            "other worktrees stay hidden"
        );
        assert!(at(visual, format!("rail-worktree-{}", f.main)).is_none());
        // Drawing again reads nothing more.
        f.window
            .update(visual, |shell, _, cx| {
                shell.projects_frame.update(cx, |_, cx| cx.notify())
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(loads(&f, visual), 1);

        // "Show other worktrees" in Settings lists them after a reload.
        f.window
            .update(visual, |shell, _, cx| {
                let db = locked(&shell.store).unwrap();
                let mut prefs = worktree::meta::repo_prefs(&db, Path::new(&f.main)).unwrap();
                prefs.show_external = true;
                worktree::meta::save_repo_prefs(&db, Path::new(&f.main), &prefs).unwrap();
                drop(db);
                shell.invalidate_rail_worktrees(cx);
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(loads(&f, visual), 2);
        assert!(at(visual, format!("rail-worktree-{}", f.external)).is_some());
        assert!(at(visual, format!("rail-worktree-{}", f.managed)).is_some());
    }

    #[gpui::test]
    fn an_open_worktree_with_many_sessions_ends_in_a_more_row(cx: &mut TestAppContext) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        // Ten more sessions in the managed worktree: eleven with the fixture's.
        f.window
            .update(visual, |shell, _, cx| {
                for _ in 0..10 {
                    let mut summary = locked(&shell.store)
                        .unwrap()
                        .create("codex", &f.main)
                        .unwrap();
                    summary.has_messages = true;
                    let id = summary.id;
                    shell.add(summary, cx);
                    shell.session_summaries.get_mut(&id).unwrap().has_messages = true;
                    shell.session_worktrees.insert(id, Some(f.managed.clone()));
                    if let Some(view) = shell.sessions.get(&id) {
                        view.update(cx, |v, _| {
                            v.summary.has_messages = true;
                            v.settings.worktree_cwd = Some(f.managed.clone());
                        });
                    }
                }
            })
            .unwrap();
        expand(&f, visual, &[f.managed.as_str()]);
        let rows = f
            .window
            .update(visual, |shell, _, _| shell.project_rail_list.rows.clone())
            .unwrap();
        assert!(rows.iter().any(|r| matches!(r,
            ProjectRailRow::Worktree { path, sessions: 11, .. } if *path == f.managed)));
        let nested = rows
            .iter()
            .filter(|r| matches!(r, ProjectRailRow::WorktreeSession { .. }))
            .count();
        assert_eq!(nested, MAX_WORKTREE_SESSIONS);
        assert!(rows.iter().any(|r| matches!(r,
            ProjectRailRow::More { hidden: 3, worktree: Some(path), .. } if *path == f.managed)));
        // The project's own count is unaffected: its one direct session, nothing hidden.
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, ProjectRailRow::More { worktree: None, .. }))
        );
        // Like the project's, the row opens what it stands for: the worktree.
        let more =
            at(visual, format!("rail-worktree-more-{}", f.managed)).expect("the more row is drawn");
        visual.simulate_click(more.center(), Default::default());
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, _| {
                assert_eq!(
                    path_key(&shell.worktree_focus[&f.main].path),
                    path_key(&f.managed)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_list_is_read_again_after_changes_made_outside_the_dialogs(cx: &mut TestAppContext) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        expand(&f, visual, &[]);
        assert_eq!(loads(&f, visual), 1);

        // A session moved to another worktree (e.g. the composer created one).
        f.window
            .update(visual, |shell, _, cx| {
                let view = shell.sessions[&f.direct].clone();
                view.update(cx, |v, cx| {
                    v.settings.worktree_cwd = Some(f.legacy.clone());
                    cx.emit(crate::conversation::SessionChanged);
                });
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(
            loads(&f, visual),
            2,
            "a session's new worktree reloads the list"
        );
        // The same worktree again changes nothing.
        f.window
            .update(visual, |shell, _, cx| {
                let view = shell.sessions[&f.direct].clone();
                view.update(cx, |_, cx| cx.emit(crate::conversation::SessionChanged));
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(loads(&f, visual), 2);

        // Coming back to the window: worktrees may have changed outside the app.
        // (Called directly: a really active test window would start watching Git.)
        f.window
            .update(visual, |shell, _, cx| {
                shell.window_activation_changed(false, cx);
                shell.window_activation_changed(true, cx);
                shell.window_active = false;
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(loads(&f, visual), 3, "activation reloads the list");

        // A creation dismissed while it ran still shows up.
        f.window
            .update(visual, |shell, w, cx| {
                let id = shell.start_worktree_create(
                    f.main.clone(),
                    CreateRequest::new(Path::new(&f.main), "dismissed-early"),
                    Default::default(),
                    WorktreeOrigin::Switcher,
                    w,
                    cx,
                );
                shell.dismiss_pending_worktree(id, cx);
            })
            .unwrap();
        visual.run_until_parked();
        let made = f
            .window
            .update(visual, |shell, _, _| {
                shell
                    .rail_worktrees
                    .lists
                    .get(&f.main)
                    .is_some_and(|l| l.trees.iter().any(|t| t.name == "dismissed-early"))
            })
            .unwrap();
        assert!(made, "the dismissed creation's worktree is listed");
        assert_eq!(loads(&f, visual), 4);
    }

    #[gpui::test]
    fn a_worktrees_sessions_are_listed_under_it_and_not_under_the_project(cx: &mut TestAppContext) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        expand(&f, visual, &[]);
        let rows = |visual: &mut VisualTestContext| {
            f.window
                .update(visual, |shell, _, _| shell.project_rail_list.rows.clone())
                .unwrap()
        };
        let listed = rows(visual);
        assert!(listed.iter().any(|r| matches!(r,
            ProjectRailRow::Worktree { path, sessions: 1, .. } if *path == f.managed)));
        assert!(
            !listed.iter().any(|r| matches!(r,
                ProjectRailRow::Session { id, .. } | ProjectRailRow::WorktreeSession { id, .. }
                    if *id == f.in_worktree)),
            "a closed worktree hides its sessions, and the project does not repeat them"
        );
        assert!(listed.iter().any(|r| matches!(r,
            ProjectRailRow::Session { id, .. } if *id == f.direct)));

        expand(&f, visual, &[f.managed.as_str()]);
        let listed = rows(visual);
        let ix = |pred: &dyn Fn(&ProjectRailRow) -> bool| listed.iter().position(pred);
        let tree =
            ix(&|r| matches!(r, ProjectRailRow::Worktree { path, .. } if *path == f.managed))
                .unwrap();
        let nested = ix(&|r| {
            matches!(r,
            ProjectRailRow::WorktreeSession { id, .. } if *id == f.in_worktree)
        })
        .expect("an open worktree lists its sessions");
        assert_eq!(nested, tree + 1);
        assert!(
            ix(&|r| matches!(r, ProjectRailRow::Session { id, .. } if *id == f.in_worktree))
                .is_none()
        );
        let row = at(visual, format!("rail-session-{}", f.in_worktree)).unwrap();
        let own = at(visual, format!("rail-session-{}", f.direct)).unwrap();
        let worktree = at(visual, format!("rail-worktree-{}", f.managed)).unwrap();
        assert!(row.top() >= worktree.bottom());
        assert!(
            row.left() > own.left(),
            "a worktree's sessions sit further in"
        );

        // Once its worktree is removed, a session is the project's again.
        f.window
            .update(visual, |shell, _, cx| {
                shell
                    .rail_worktrees
                    .lists
                    .get_mut(&f.main)
                    .unwrap()
                    .trees
                    .clear();
                shell.projects_frame.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        visual.run_until_parked();
        assert!(rows(visual).iter().any(|r| matches!(r,
            ProjectRailRow::Session { id, .. } if *id == f.in_worktree)));
    }

    #[gpui::test]
    fn clicking_a_worktree_switches_to_its_project_and_focuses_it(cx: &mut TestAppContext) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        f.window
            .update(visual, |shell, w, cx| {
                shell.select_project("/work/another-project".into(), w, cx)
            })
            .unwrap();
        expand(&f, visual, &[]);
        let row = at(visual, format!("rail-worktree-{}", f.legacy)).unwrap();
        visual.simulate_click(row.center(), Default::default());
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.project, f.main);
                let focus = &shell.worktree_focus[&f.main];
                assert_eq!(focus.path, f.legacy);
                assert_eq!(focus.branch.as_deref(), Some("legacy-one"));
                assert_eq!(shell.working_copy(), f.legacy);
            })
            .unwrap();

        // "New session here" starts a chat working in that worktree.
        let before = f
            .window
            .update(visual, |shell, _, _| shell.session_summaries.len())
            .unwrap();
        f.window
            .update(visual, |shell, w, cx| {
                shell.run_rail_worktree_menu(
                    f.main.clone(),
                    f.managed.clone(),
                    "worktree-new-session",
                    w,
                    cx,
                )
            })
            .unwrap();
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.worktree_focus[&f.main].path, f.managed);
                let id = shell.workspace.active.expect("a chat is in front");
                assert!(shell.session_summaries.len() > before);
                assert_ne!(id, f.in_worktree);
                assert_eq!(shell.session_working_copy(id, cx), f.managed);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_worktree_menu_deletes_through_the_dialog_but_never_the_main_checkout(
        cx: &mut TestAppContext,
    ) {
        super::super::worktree_dialog::tests::bind(cx);
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        expand(&f, visual, &[]);
        let row = at(visual, format!("rail-worktree-{}", f.managed)).unwrap();
        visual.simulate_mouse_down(row.center(), MouseButton::Right, Default::default());
        visual.simulate_mouse_up(row.center(), MouseButton::Right, Default::default());
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, _| {
                assert!(shell.menu_open(&MenuTarget::RailWorktree {
                    project: f.main.clone(),
                    path: f.managed.clone(),
                }))
            })
            .unwrap();
        for action in ["open", "new-session", "copy-path"] {
            assert!(at(visual, format!("menu-action-worktree-{action}")).is_some());
        }
        assert!(visual.debug_bounds("menu-action-reveal").is_some());
        let delete = visual
            .debug_bounds("menu-action-worktree-delete")
            .expect("a worktree can be deleted");
        visual.simulate_click(delete.center(), Default::default());
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, _| {
                let dialog = shell.worktree_ui.delete.as_ref().expect("the dialog opens");
                assert_eq!(dialog.path, f.managed);
            })
            .unwrap();

        // The main checkout's menu has no Delete.
        f.window
            .update(visual, |shell, w, cx| {
                shell.worktree_ui.delete = None;
                shell.open_menu(
                    MenuTarget::RailWorktree {
                        project: f.main.clone(),
                        path: f.main.clone(),
                    },
                    Anchor::Point(gpui::point(px(300.), px(60.))),
                    w,
                    cx,
                );
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("menu-action-worktree-open").is_some());
        assert!(visual.debug_bounds("menu-action-worktree-delete").is_none());
        f.window
            .update(visual, |shell, w, cx| {
                shell.run_rail_worktree_menu(
                    f.main.clone(),
                    f.main.clone(),
                    "worktree-delete",
                    w,
                    cx,
                );
                assert!(shell.worktree_ui.delete.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn pending_creations_show_their_status_and_a_failed_one_offers_retry(cx: &mut TestAppContext) {
        let (f, mut visual) = fixture(cx);
        let visual = &mut visual;
        f.window
            .update(visual, |shell, _, _| {
                for (id, phase, error) in [
                    (901, Phase::CheckingOut { percent: Some(40) }, None),
                    (902, Phase::Resolving, Some("Base not found".to_owned())),
                ] {
                    shell.pending_worktrees.push(PendingWorktree {
                        id,
                        project: f.main.clone(),
                        display_name: format!("pending-{id}"),
                        phase,
                        error,
                        request: CreateRequest::new(Path::new(&f.main), "pending"),
                        setup: Default::default(),
                        origin: WorktreeOrigin::Rail,
                    });
                }
            })
            .unwrap();
        expand(&f, visual, &[]);
        assert!(visual.debug_bounds("rail-pending-worktree-901").is_some());
        assert!(visual.debug_bounds("rail-pending-status-901").is_some());
        assert!(
            visual.debug_bounds("rail-pending-retry-901").is_none(),
            "a running creation has nothing to retry"
        );
        f.window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.pending_worktrees[0].status(), "Checking out… 40%");
            })
            .unwrap();
        assert!(visual.debug_bounds("rail-pending-status-902").is_some());
        assert!(visual.debug_bounds("rail-pending-retry-902").is_some());
        let dismiss = visual.debug_bounds("rail-pending-dismiss-902").unwrap();
        visual.simulate_click(dismiss.center(), Default::default());
        visual.run_until_parked();
        f.window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.pending_worktrees.len(), 1);
                assert_eq!(shell.pending_worktrees[0].id, 901);
            })
            .unwrap();
        assert!(visual.debug_bounds("rail-pending-worktree-902").is_none());
    }
}
