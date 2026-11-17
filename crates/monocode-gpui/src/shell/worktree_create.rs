//! Background worktree creation (spec §7.2): the Create Worktree dialog, the rail
//! and the switcher hand a request here and close. A pending row tracks its phase;
//! success makes the worktree the project's focus and may open a "Setup" terminal;
//! failure keeps the row with its error and shows a card with Retry.
// The dialog, rail rows and switcher rows that call this arrive in the next phase.
#![allow(dead_code)]
use super::*;
use crate::notifications::errors;
use crate::worktree_switcher::WorktreeFocus;
use monocode_core::{
    files::path_key,
    worktree::{self, CreateRequest, Created, Phase},
};
use std::path::PathBuf;

/// Where a creation was started; surfaces may show only their own rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorktreeOrigin {
    Dialog,
    Rail,
    Switcher,
    Settings,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingWorktree {
    pub id: u64,
    /// The project (main checkout) the worktree belongs to, as the rail spells it.
    pub project: String,
    pub display_name: String,
    pub phase: Phase,
    pub error: Option<String>,
    pub request: CreateRequest,
    pub run_setup: bool,
    pub origin: WorktreeOrigin,
}
impl PendingWorktree {
    /// The row's status text: the phase, or the error.
    pub(crate) fn status(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        match self.phase {
            Phase::Resolving => "Preparing…".into(),
            Phase::Fetching => "Fetching…".into(),
            Phase::CheckingOut { percent: Some(p) } => format!("Checking out… {p}%"),
            Phase::CheckingOut { percent: None } => "Checking out…".into(),
            Phase::CopyingFiles => "Copying files…".into(),
            Phase::Done => "Finishing…".into(),
        }
    }
}

/// What the Shell keeps besides the pending rows.
#[derive(Default)]
pub(super) struct Creates {
    next: u64,
    tasks: HashMap<u64, gpui::Task<()>>,
    /// Warnings from the last successful creation, shown until dismissed.
    pub warnings: Option<String>,
}

/// Background result: the worktree and, when setup was asked for, its runner.
type Outcome = Result<(Created, Option<Result<PathBuf, String>>), String>;

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl Shell {
    /// Pending creations of `project`, oldest first.
    pub(crate) fn pending_worktrees_for(&self, project: &str) -> Vec<&PendingWorktree> {
        let key = path_key(project);
        self.pending_worktrees
            .iter()
            .filter(|p| path_key(&p.project) == key)
            .collect()
    }

    /// Start creating a worktree of `project_main` in the background. Returns the
    /// pending row's id. `run_setup` runs the repository's setup script (and
    /// approves its hash) once the worktree exists.
    pub(crate) fn start_worktree_create(
        &mut self,
        project_main: String,
        request: CreateRequest,
        run_setup: bool,
        origin: WorktreeOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.worktree_creates.next += 1;
        let id = self.worktree_creates.next;
        let display_name = request
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| Some(request.name.trim().to_owned()).filter(|n| !n.is_empty()))
            .or_else(|| request.branch.clone())
            .unwrap_or_else(|| "New worktree".into());
        self.pending_worktrees.push(PendingWorktree {
            id,
            project: project_main,
            display_name,
            phase: Phase::Resolving,
            error: None,
            request,
            run_setup,
            origin,
        });
        self.run_pending_worktree(id, window, cx);
        id
    }

    /// Run a failed creation again with the same request.
    pub(crate) fn retry_worktree_create(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_worktrees.iter_mut().find(|p| p.id == id) else {
            return;
        };
        if pending.error.is_none() {
            return;
        }
        pending.error = None;
        pending.phase = Phase::Resolving;
        self.run_pending_worktree(id, window, cx);
    }

    /// Forget a pending row. A creation still running finishes in the background
    /// (Git work is not interrupted) but no longer changes the focus.
    pub(crate) fn dismiss_pending_worktree(&mut self, id: u64, cx: &mut Context<Self>) {
        self.pending_worktrees.retain(|p| p.id != id);
        self.worktree_pending_changed(cx);
    }

    fn worktree_pending_changed(&self, cx: &mut Context<Self>) {
        self.invalidate_chrome(cx);
        self.projects_frame.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn run_pending_worktree(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_worktrees.iter().find(|p| p.id == id) else {
            return;
        };
        let request = pending.request.clone();
        let run_setup = pending.run_setup;
        let store = self.store.clone();
        let (tx, rx) = async_channel::unbounded::<Phase>();
        let work = cx.background_executor().spawn(async move {
            let created = worktree::create::create(&request, &store, &|phase| {
                let _ = tx.try_send(phase);
            })?;
            let runner = match (&created.setup, run_setup) {
                (Some(plan), true) => {
                    let main = PathBuf::from(&created.meta.repo);
                    if let Ok(db) = locked(&store) {
                        let mut prefs = worktree::meta::repo_prefs(&db, &main).unwrap_or_default();
                        if prefs.setup_approved_hash.as_deref() != Some(plan.hash.as_str()) {
                            prefs.setup_approved_hash = Some(plan.hash.clone());
                            let _ = worktree::meta::save_repo_prefs(&db, &main, &prefs);
                        }
                    }
                    Some(worktree::setup::write_runner(&created.path, &plan.script))
                }
                _ => None,
            };
            Ok::<_, String>((created, runner))
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(phase) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(p) = this.pending_worktrees.iter_mut().find(|p| p.id == id)
                        && p.phase != phase
                    {
                        p.phase = phase;
                        this.worktree_pending_changed(cx);
                    }
                });
            }
            let outcome = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_worktree_create(id, outcome, window, cx)
            });
        });
        self.worktree_creates.tasks.insert(id, task);
        self.worktree_pending_changed(cx);
    }

    fn finish_worktree_create(
        &mut self,
        id: u64,
        outcome: Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.worktree_creates.tasks.remove(&id);
        let Some(ix) = self.pending_worktrees.iter().position(|p| p.id == id) else {
            // Dismissed while running.
            return;
        };
        let (created, runner) = match outcome {
            Ok(done) => done,
            Err(error) => {
                self.pending_worktrees[ix].error = Some(error);
                self.worktree_pending_changed(cx);
                return;
            }
        };
        let pending = self.pending_worktrees.remove(ix);
        let path = created.path.to_string_lossy().into_owned();
        let mut warnings = created.warnings.clone();
        if let Some(Err(error)) = &runner {
            warnings.push(format!("Could not prepare the setup script: {error}"));
        }
        self.worktree_creates.warnings = (!warnings.is_empty()).then(|| warnings.join("\n"));
        let focus = WorktreeFocus {
            path: path.clone(),
            branch: Some(created.branch.clone()),
        };
        if path_key(&self.workspace.project) == path_key(&pending.project) {
            self.select_worktree(Some(focus), window, cx);
        } else {
            // Another project is open: the worktree is its focus when it returns.
            self.worktree_focus.insert(pending.project.clone(), focus);
            self.persist(cx);
        }
        if let (Some(Ok(runner)), Some(plan)) = (runner, &created.setup) {
            self.open_setup_terminal(&pending.project, &path, &runner, plan.env.clone(), cx);
        }
        self.sync_git(cx);
        self.worktree_pending_changed(cx);
    }

    /// A dock terminal titled "Setup" in the new worktree that runs `runner`.
    fn open_setup_terminal(
        &mut self,
        project: &str,
        cwd: &str,
        runner: &std::path::Path,
        env: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let mut launch = self.terminal_launch(project, cwd);
        launch.environment.extend(env);
        launch.commands.push(quote(&runner.to_string_lossy()));
        self.ensure_terminals(project, cx);
        let terminal = self.spawn_terminal_with(cwd.to_owned(), launch, cx);
        terminal.update(cx, |t, cx| t.pin_title("Setup".into(), cx));
        let dock = self.workspace.docks.entry(project.to_owned()).or_default();
        let was_open = dock.open && !dock.terminals.is_empty();
        let added = dock.add(cwd.to_owned());
        self.terminals
            .entry(project.to_owned())
            .or_default()
            .push(terminal);
        if project == self.workspace.project {
            self.dock_tab_scroll.scroll_to_item(added);
            if !self.terminal_in_panel(cx) && !was_open {
                self.slide_dock(true, cx);
            }
        }
        self.persist(cx);
        cx.notify();
    }

    /// Cards for failed creations (Retry / Dismiss) and creation warnings.
    pub(super) fn worktree_notices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut cards = Vec::new();
        for pending in self.pending_worktrees.iter().filter(|p| p.error.is_some()) {
            let id = pending.id;
            let message = format!(
                "Could not create {}: {}",
                pending.display_name,
                pending.error.clone().unwrap_or_default()
            );
            let actions =
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id(("worktree-retry", id as usize))
                            .debug_selector(move || format!("worktree-retry-{id}"))
                            .role(Role::Button)
                            .aria_label("Retry")
                            .px_2()
                            .rounded_md()
                            .text_size(px(12.))
                            .cursor_pointer()
                            .hover(|s| s.bg(content(0.1)))
                            .child("Retry")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.retry_worktree_create(id, window, cx)
                            })),
                    )
                    .child(
                        icon_button("worktree-error-dismiss", "Dismiss", "close")
                            .debug_selector(move || format!("worktree-error-dismiss-{id}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dismiss_pending_worktree(id, cx)
                            })),
                    );
            // Each card gets its own id scope; the card's ids repeat.
            cards.push(
                div()
                    .id(("worktree-error-card", id as usize))
                    .child(errors::card("worktree-error", message, actions))
                    .into_any_element(),
            );
        }
        if let Some(warnings) = self.worktree_creates.warnings.clone() {
            cards.push(
                crate::notifications::surface::toast_frame()
                    .id("worktree-warnings")
                    .debug_selector(|| "worktree-warnings".into())
                    .role(Role::Status)
                    .child(
                        div()
                            .px(px(14.))
                            .py_3()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(icon("triangle-alert", 16.).text_color(rgb(0xfbbf24)))
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
                                            .child("Worktree created with warnings"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(content(0.7))
                                            .child(warnings),
                                    ),
                            )
                            .child(
                                icon_button("worktree-warnings-dismiss", "Dismiss", "close")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.worktree_creates.warnings = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        cards
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::TestAppContext;

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository and a worktree root, both in a temp dir.
    fn repo() -> (tempfile::TempDir, String, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let main = base.join("project");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        git(&main, &["config", "commit.gpgsign", "false"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "Base"]);
        (
            temp,
            main.to_string_lossy().into_owned(),
            base.join("workspaces"),
        )
    }

    #[gpui::test]
    fn pending_creation_selects_the_worktree_or_keeps_its_error_for_retry(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                worktree::meta::save_prefs(
                    &locked(&shell.store).unwrap(),
                    &worktree::Prefs {
                        root: Some(root.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();
                shell.workspace.add_project(&main);
                shell.select_project(main.clone(), w, cx);
                let request = CreateRequest {
                    created_by: worktree::CreatedBy::Dialog,
                    ..CreateRequest::new(std::path::Path::new(&main), "Review Fixes")
                };
                shell.start_worktree_create(
                    main.clone(),
                    request,
                    false,
                    WorktreeOrigin::Dialog,
                    w,
                    cx,
                );
                let pending = shell.pending_worktrees_for(&main);
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].display_name, "Review Fixes");
                assert!(pending[0].error.is_none());
            })
            .unwrap();
        cx.run_until_parked();
        let created = root.join("project/Review-Fixes");
        window
            .update(cx, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty(), "success clears the row");
                let focus = &shell.worktree_focus[&main];
                assert_eq!(
                    std::path::Path::new(&focus.path),
                    created.canonicalize().unwrap()
                );
                assert_eq!(focus.branch.as_deref(), Some("Review-Fixes"));
                assert_eq!(shell.working_copy(), focus.path);
                let meta = worktree::meta::load(&locked(&shell.store).unwrap(), &created)
                    .unwrap()
                    .unwrap();
                assert_eq!(meta.created_by, worktree::CreatedBy::Dialog);
            })
            .unwrap();

        // A base that does not exist yet fails; the row keeps its error.
        let id = window
            .update(cx, |shell, w, cx| {
                let request = CreateRequest {
                    base: Some("later".into()),
                    ..CreateRequest::new(std::path::Path::new(&main), "second")
                };
                shell.start_worktree_create(
                    main.clone(),
                    request,
                    false,
                    WorktreeOrigin::Rail,
                    w,
                    cx,
                )
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let pending = shell.pending_worktrees_for(&main);
                assert_eq!(pending.len(), 1);
                assert!(pending[0].error.is_some());
                assert_eq!(pending[0].status(), pending[0].error.clone().unwrap());
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let retry: &'static str = Box::leak(format!("worktree-retry-{id}").into_boxed_str());
        assert!(
            visual.debug_bounds(retry).is_some(),
            "the failure card offers Retry"
        );
        git(std::path::Path::new(&main), &["branch", "later"]);
        window
            .update(visual, |shell, w, cx| {
                shell.retry_worktree_create(id, w, cx)
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty());
                assert!(root.join("project/second").is_dir());
                assert!(shell.worktree_focus[&main].path.ends_with("second"));
            })
            .unwrap();

        // Dismissing a failed row forgets it.
        let id = window
            .update(visual, |shell, w, cx| {
                let request = CreateRequest {
                    base: Some("missing".into()),
                    ..CreateRequest::new(std::path::Path::new(&main), "third")
                };
                shell.start_worktree_create(
                    main.clone(),
                    request,
                    false,
                    WorktreeOrigin::Switcher,
                    w,
                    cx,
                )
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.pending_worktrees.len(), 1);
                shell.dismiss_pending_worktree(id, cx);
                assert!(shell.pending_worktrees.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn setup_runs_in_a_setup_terminal_and_approves_the_script(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        std::fs::write(
            std::path::Path::new(&main).join("monocode.yaml"),
            "scripts:\n  setup: echo ready\n",
        )
        .unwrap();
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                worktree::meta::save_prefs(
                    &locked(&shell.store).unwrap(),
                    &worktree::Prefs {
                        root: Some(root.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();
                shell.workspace.add_project(&main);
                shell.select_project(main.clone(), w, cx);
                let request = CreateRequest::new(std::path::Path::new(&main), "with-setup");
                shell.start_worktree_create(
                    main.clone(),
                    request,
                    true,
                    WorktreeOrigin::Dialog,
                    w,
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.pending_worktrees.is_empty());
                let terminals = &shell.terminals[&main];
                let setup = terminals.last().unwrap().read(cx);
                assert_eq!(setup.title, "Setup");
                assert!(setup.cwd.ends_with("with-setup"));
                let dock = &shell.workspace.docks[&main];
                assert_eq!(dock.terminals.len(), terminals.len());
                let prefs = worktree::meta::repo_prefs(
                    &locked(&shell.store).unwrap(),
                    std::path::Path::new(&main),
                )
                .unwrap();
                assert_eq!(
                    prefs.setup_approved_hash.as_deref(),
                    Some(worktree::setup::hash("echo ready").as_str())
                );
            })
            .unwrap();
    }
}
