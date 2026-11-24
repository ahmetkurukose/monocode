//! Background worktree creation (spec §7.2): the Create Worktree dialog, the rail
//! and the switcher hand a request here and close. A pending row tracks its phase;
//! success makes the worktree the project's focus and may open a "Setup" terminal;
//! failure keeps the row with its error and shows a card with Retry.
use super::*;
use crate::notifications::errors;
use crate::worktree_switcher::WorktreeFocus;
use monocode_core::{
    files::path_key,
    worktree::{self, CreateRequest, Created, Phase, SetupPlan},
};
use std::path::{Path, PathBuf};

/// Which entry point opened the Create Worktree dialog; surfaces may show only their
/// own rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorktreeOrigin {
    Rail,
    Switcher,
    Settings,
}

/// Whether to run the repository's setup script once the worktree exists, and which
/// script the user was shown (its hash). Only that script, or one already approved,
/// runs; a different script in the new worktree is not run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SetupChoice {
    pub run: bool,
    pub shown_hash: Option<String>,
}
impl SetupChoice {
    pub(crate) fn skip() -> Self {
        Self::default()
    }
    pub(crate) fn run(shown_hash: Option<String>) -> Self {
        Self {
            run: true,
            shown_hash,
        }
    }
}

/// The warning when the new worktree's setup script is not the one the dialog showed.
pub(crate) const SETUP_CHANGED: &str =
    "The setup script in the new worktree differs from the one shown; it was not run.";

#[derive(Clone, Debug)]
pub(crate) struct PendingWorktree {
    pub id: u64,
    /// The project (main checkout) the worktree belongs to, as the rail spells it.
    pub project: String,
    pub display_name: String,
    pub phase: Phase,
    pub error: Option<String>,
    pub request: CreateRequest,
    pub setup: SetupChoice,
    /// Every surface lists every pending row; a creation started from the rail
    /// also opens its project, and then its new worktree, in the rail tree.
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

/// What a creation made, local or remote.
#[derive(Debug)]
pub(super) struct Made {
    pub path: PathBuf,
    pub branch: String,
    pub warnings: Vec<String>,
    /// The repository's setup script (local only).
    pub setup: Option<SetupPlan>,
    /// The setup runner, when setup was asked for.
    pub runner: Option<Result<PathBuf, String>>,
}
impl Made {
    fn local(created: Created, runner: Option<Result<PathBuf, String>>) -> Self {
        Self {
            path: created.path,
            branch: created.branch,
            warnings: created.warnings,
            setup: created.setup,
            runner,
        }
    }
}

/// Background result.
type Outcome = Result<Made, String>;

/// The old remote-capable create: `git::create_project_worktree(cwd, branch, base, existing)`.
pub(super) type RemoteCreate<'a> =
    dyn Fn(&Path, &str, Option<&str>, bool) -> Result<monocode_core::git::Worktree, String> + 'a;

/// Create the worktree `request` asks for. Local repositories go through the managed
/// core ([`worktree::create::create`], with content paths and setup). Remote (SSH)
/// projects keep the old path, `remote` (in the app,
/// [`monocode_core::git::create_project_worktree`], which routes to `git::remote`):
/// the branch is the request's branch, else its name; no meta, content paths or setup.
pub(super) fn create_routed(
    request: &CreateRequest,
    setup: &SetupChoice,
    store: &Database,
    progress: &dyn Fn(Phase),
    remote: &RemoteCreate<'_>,
) -> Outcome {
    if monocode_core::files::remote::is_remote(&request.cwd) {
        let branch = request
            .branch
            .clone()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| request.name.trim().to_owned());
        if branch.is_empty() {
            return Err("Enter a branch name for the worktree.".into());
        }
        progress(Phase::CheckingOut { percent: None });
        let tree = remote(
            &request.cwd,
            &branch,
            request.base.as_deref().filter(|b| !b.trim().is_empty()),
            request.existing_branch,
        )?;
        return Ok(Made {
            path: PathBuf::from(tree.path),
            branch: tree.branch.unwrap_or(branch),
            warnings: Vec::new(),
            setup: None,
            runner: None,
        });
    }
    let mut created = worktree::create::create(request, store, progress)?;
    // The new worktree's script may differ from the main checkout's that the dialog
    // showed: run only what the user saw, or what this repository already trusts.
    let trusted = created.setup.as_ref().is_some_and(|plan| {
        plan.approved || setup.shown_hash.as_deref() == Some(plan.hash.as_str())
    });
    if setup.run && created.setup.is_some() && !trusted {
        created.warnings.push(SETUP_CHANGED.into());
    }
    let runner = match (&created.setup, setup.run && trusted) {
        (Some(plan), true) => {
            let main = PathBuf::from(&created.meta.repo);
            if let Ok(db) = locked(store) {
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
    Ok(Made::local(created, runner))
}

/// A session whose first send makes a fresh worktree off `HEAD`, as automations and
/// the phone always did, recorded as made by `origin`. The composer's send keeps an
/// explicit base like this one (it only fills in the session's branch when none is set).
pub(super) fn fresh_worktree_from_head(
    settings: &mut monocode_core::agent::Settings,
    origin: worktree::CreatedBy,
) {
    settings.new_worktree = true;
    settings.worktree_base = Some("HEAD".into());
    settings.worktree_origin = Some(origin);
}

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
    /// pending row's id. `setup` may run the repository's setup script (and approve
    /// its hash) once the worktree exists.
    pub(crate) fn start_worktree_create(
        &mut self,
        project_main: String,
        request: CreateRequest,
        setup: SetupChoice,
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
        if origin == WorktreeOrigin::Rail && self.project_rail_tree_on() {
            // The pending row shows under its project.
            self.workspace
                .project_rail_expanded
                .insert(project_main.clone());
        }
        self.pending_worktrees.push(PendingWorktree {
            id,
            project: project_main,
            display_name,
            phase: Phase::Resolving,
            error: None,
            request,
            setup,
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

    fn worktree_pending_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_switcher_pending(cx);
        self.invalidate_chrome(cx);
        self.projects_frame.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn run_pending_worktree(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_worktrees.iter().find(|p| p.id == id) else {
            return;
        };
        let request = pending.request.clone();
        let setup = pending.setup.clone();
        let store = self.store.clone();
        let (tx, rx) = async_channel::unbounded::<Phase>();
        let work = cx.background_executor().spawn(async move {
            let progress = |phase| {
                let _ = tx.try_send(phase);
            };
            create_routed(
                &request,
                &setup,
                &store,
                &progress,
                &monocode_core::git::create_project_worktree,
            )
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
            // Dismissed while running: the focus stays, but a worktree it made still
            // shows in the rail and Settings.
            if outcome.is_ok() {
                self.worktrees_changed_elsewhere(cx);
            }
            return;
        };
        let made = match outcome {
            Ok(made) => made,
            Err(error) => {
                self.pending_worktrees[ix].error = Some(error);
                self.worktree_pending_changed(cx);
                return;
            }
        };
        let pending = self.pending_worktrees.remove(ix);
        let path = made.path.to_string_lossy().into_owned();
        let mut warnings = made.warnings.clone();
        if let Some(Err(error)) = &made.runner {
            warnings.push(format!("Could not prepare the setup script: {error}"));
        }
        self.worktree_creates.warnings = (!warnings.is_empty()).then(|| warnings.join("\n"));
        let focus = WorktreeFocus {
            path: path.clone(),
            branch: Some(made.branch.clone()),
        };
        if pending.origin == WorktreeOrigin::Rail && self.project_rail_tree_on() {
            // Created from the rail: its row opens there, on the chat it starts with.
            self.workspace
                .project_rail_expanded
                .insert(pending.project.clone());
            self.workspace
                .project_rail_worktrees_collapsed
                .remove(&path);
        }
        if path_key(&self.workspace.project) == path_key(&pending.project) {
            self.select_worktree(Some(focus), window, cx);
        } else {
            // Another project is open: the worktree is its focus when it returns.
            self.worktree_focus.insert(pending.project.clone(), focus);
            self.persist(cx);
        }
        if let (Some(Ok(runner)), Some(plan)) = (made.runner, &made.setup) {
            self.open_setup_terminal(&pending.project, &path, &runner, plan.env.clone(), cx);
        }
        self.sync_git(cx);
        self.refresh_settings_worktrees(cx);
        self.invalidate_rail_worktrees(cx);
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
            if !was_open {
                self.slide_dock(true, cx);
            }
        }
        self.persist(cx);
        cx.notify();
    }

    /// Cards for failed creations (Retry / Dismiss) and creation warnings.
    pub(super) fn worktree_notices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut cards = self.kept_branch_cards(cx);
        cards.extend(self.merged_notice(cx));
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
                    SetupChoice::skip(),
                    WorktreeOrigin::Settings,
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
                    SetupChoice::skip(),
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
                    SetupChoice::skip(),
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

    #[test]
    fn remote_requests_take_the_old_create_path_and_local_ones_the_core() {
        let calls = std::cell::RefCell::new(Vec::new());
        let remote = |cwd: &Path, branch: &str, base: Option<&str>, existing: bool| {
            calls.borrow_mut().push((
                cwd.to_string_lossy().into_owned(),
                branch.to_owned(),
                base.map(str::to_owned),
                existing,
            ));
            Ok(monocode_core::git::Worktree {
                path: "remote://host/project-worktrees/feature".into(),
                branch: Some(branch.to_owned()),
                ..Default::default()
            })
        };
        let store: Database = Arc::new(Mutex::new(
            monocode_core::store::Store::open(Path::new(":memory:")).unwrap(),
        ));
        let phases = std::cell::RefCell::new(Vec::new());
        let progress = |phase| phases.borrow_mut().push(phase);
        let request = CreateRequest {
            base: Some("origin/main".into()),
            ..CreateRequest::new(Path::new("remote://host/project"), "feature")
        };
        let made = create_routed(
            &request,
            &SetupChoice::run(None),
            &store,
            &progress,
            &remote,
        )
        .unwrap();
        assert_eq!(
            calls.borrow().as_slice(),
            [(
                "remote://host/project".to_owned(),
                "feature".to_owned(),
                Some("origin/main".to_owned()),
                false
            )]
        );
        assert_eq!(
            made.path,
            PathBuf::from("remote://host/project-worktrees/feature")
        );
        assert_eq!(made.branch, "feature");
        assert!(
            made.setup.is_none() && made.runner.is_none(),
            "no setup remotely"
        );
        assert!(!phases.borrow().is_empty());

        // An explicit branch wins over the name; an empty one is refused.
        let request = CreateRequest {
            branch: Some("feature/x".into()),
            existing_branch: true,
            ..CreateRequest::new(Path::new("remote://host/project"), "ignored")
        };
        create_routed(&request, &SetupChoice::skip(), &store, &progress, &remote).unwrap();
        assert_eq!(calls.borrow()[1].1, "feature/x");
        assert!(calls.borrow()[1].3);
        let empty = CreateRequest::new(Path::new("remote://host/project"), " ");
        assert!(create_routed(&empty, &SetupChoice::skip(), &store, &progress, &remote).is_err());

        // Local repositories never reach the old path.
        let (_temp, main, root) = repo();
        locked(&store)
            .and_then(|db| {
                worktree::meta::save_prefs(
                    &db,
                    &worktree::Prefs {
                        root: Some(root.clone()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let made = create_routed(
            &CreateRequest::new(Path::new(&main), "local-one"),
            &SetupChoice::skip(),
            &store,
            &progress,
            &remote,
        )
        .unwrap();
        assert_eq!(calls.borrow().len(), 2);
        assert!(made.path.starts_with(&root));
    }

    #[test]
    fn automations_and_the_phone_start_fresh_worktrees_from_head_as_their_own() {
        for origin in [worktree::CreatedBy::Automation, worktree::CreatedBy::Mobile] {
            let mut settings = monocode_core::agent::Settings::default();
            fresh_worktree_from_head(&mut settings, origin);
            assert!(settings.new_worktree);
            assert_eq!(settings.worktree_base.as_deref(), Some("HEAD"));
            assert_eq!(settings.worktree_origin, Some(origin));
            // Persisted settings keep the origin; older rows without it read as None.
            let saved = serde_json::to_string(&settings).unwrap();
            let read: monocode_core::agent::Settings = serde_json::from_str(&saved).unwrap();
            assert_eq!(read.worktree_origin, Some(origin));
        }
        let old: monocode_core::agent::Settings =
            serde_json::from_str(r#"{"new_worktree":true}"#).unwrap();
        assert_eq!(old.worktree_origin, None);
    }

    #[test]
    fn setup_runs_only_the_script_the_dialog_showed_or_an_approved_one() {
        let (_temp, main, root) = repo();
        let main_path = Path::new(&main);
        // main's script is what the dialog shows; the `other` branch has its own.
        std::fs::write(
            main_path.join("monocode.yaml"),
            "scripts:\n  setup: echo main\n",
        )
        .unwrap();
        git(main_path, &["add", "monocode.yaml"]);
        git(main_path, &["commit", "-q", "-m", "Setup"]);
        git(main_path, &["switch", "-q", "-c", "other"]);
        std::fs::write(
            main_path.join("monocode.yaml"),
            "scripts:\n  setup: echo other\n",
        )
        .unwrap();
        git(main_path, &["commit", "-q", "-am", "Other setup"]);
        git(main_path, &["switch", "-q", "main"]);
        let store: Database = Arc::new(Mutex::new(
            monocode_core::store::Store::open(Path::new(":memory:")).unwrap(),
        ));
        locked(&store)
            .and_then(|db| {
                worktree::meta::save_prefs(
                    &db,
                    &worktree::Prefs {
                        root: Some(root.clone()),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let approved = || {
            worktree::meta::repo_prefs(&locked(&store).unwrap(), main_path)
                .unwrap()
                .setup_approved_hash
        };
        let shown = SetupChoice::run(Some(worktree::setup::hash("echo main")));
        let remote = |_: &Path, _: &str, _: Option<&str>, _: bool| -> Result<_, String> {
            unreachable!("local")
        };

        // A different script on the base branch is not run nor approved.
        let request = CreateRequest {
            base: Some("other".into()),
            ..CreateRequest::new(main_path, "from-other")
        };
        let made = create_routed(&request, &shown, &store, &|_| {}, &remote).unwrap();
        assert_eq!(made.setup.as_ref().unwrap().script, "echo other");
        assert!(made.runner.is_none(), "the unseen script does not run");
        assert!(made.warnings.iter().any(|w| w == SETUP_CHANGED));
        assert_eq!(approved(), None);

        // The script that was shown runs and is approved.
        let request = CreateRequest {
            base: Some("main".into()),
            ..CreateRequest::new(main_path, "from-main")
        };
        let made = create_routed(&request, &shown, &store, &|_| {}, &remote).unwrap();
        assert!(matches!(made.runner, Some(Ok(_))));
        assert!(!made.warnings.iter().any(|w| w == SETUP_CHANGED));
        assert_eq!(approved(), Some(worktree::setup::hash("echo main")));

        // A script this repository already trusts runs even though it was not shown.
        {
            let db = locked(&store).unwrap();
            let mut prefs = worktree::meta::repo_prefs(&db, main_path).unwrap();
            prefs.setup_approved_hash = Some(worktree::setup::hash("echo other"));
            worktree::meta::save_repo_prefs(&db, main_path, &prefs).unwrap();
        }
        let request = CreateRequest {
            base: Some("other".into()),
            ..CreateRequest::new(main_path, "trusted-other")
        };
        let made = create_routed(&request, &shown, &store, &|_| {}, &remote).unwrap();
        assert!(matches!(made.runner, Some(Ok(_))));
        assert!(made.warnings.is_empty(), "{:?}", made.warnings);

        // Skipping never runs or warns.
        let request = CreateRequest {
            base: Some("main".into()),
            ..CreateRequest::new(main_path, "skipped")
        };
        let made = create_routed(&request, &SetupChoice::skip(), &store, &|_| {}, &remote).unwrap();
        assert!(made.runner.is_none() && made.warnings.is_empty());
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
                    SetupChoice::run(Some(worktree::setup::hash("echo ready"))),
                    WorktreeOrigin::Settings,
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
