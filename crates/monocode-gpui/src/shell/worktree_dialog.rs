//! Opening the Create Worktree dialog from the switcher, the rail and Settings, and
//! keeping open switchers' pending rows current.
use super::worktree_create::WorktreeOrigin;
use super::*;
use crate::worktree_dialog::{CreateWorktreeDialog, DialogEvent};
use crate::worktree_switcher::{Event as SwitcherEvent, PendingRow, WorktreeSwitcher};
use monocode_core::worktree;

/// The open Create Worktree dialog.
pub(super) struct OpenCreate {
    pub view: Entity<CreateWorktreeDialog>,
    project: String,
    origin: WorktreeOrigin,
    _events: Subscription,
}

/// The worktree dialogs, cards and live switchers the Shell keeps.
#[derive(Default)]
pub(super) struct WorktreeUi {
    pub create: Option<OpenCreate>,
    pub delete: Option<super::worktree_delete::DeleteDialog>,
    pub kept: Vec<super::worktree_delete::KeptBranch>,
    pub next_kept: u64,
    /// Open switchers and the project each lists.
    /// (The menu builds its flyout from `&Shell`.)
    switchers: std::cell::RefCell<Vec<(String, gpui::WeakEntity<WorktreeSwitcher>)>>,
}

impl Shell {
    /// Open the Create Worktree dialog for `project` with `prefill` as its name.
    pub(crate) fn open_create_worktree_dialog(
        &mut self,
        project: String,
        prefill: String,
        origin: WorktreeOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        let store = self.store.clone();
        let view = cx.new(|cx| CreateWorktreeDialog::new(project.clone(), prefill, store, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event: &DialogEvent, window, cx| {
            this.answer_create_worktree_dialog(event, window, cx)
        });
        window.focus(&view.read(cx).initial_focus(cx), cx);
        self.worktree_ui.create = Some(OpenCreate {
            view,
            project,
            origin,
            _events: events,
        });
        cx.notify();
    }

    fn answer_create_worktree_dialog(
        &mut self,
        event: &DialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.worktree_ui.create.take() else {
            return;
        };
        if let DialogEvent::Submit {
            request,
            run_setup,
            default_base,
        } = event
        {
            if let Some((main, base)) = default_base
                && let Ok(db) = locked(&self.store)
            {
                let mut prefs = worktree::meta::repo_prefs(&db, main).unwrap_or_default();
                prefs.default_base = Some(base.clone());
                if let Err(error) = worktree::meta::save_repo_prefs(&db, main, &prefs) {
                    self.error = Some(format!("Could not save the default base: {error}"));
                }
            }
            self.start_worktree_create(
                open.project,
                (**request).clone(),
                *run_setup,
                open.origin,
                window,
                cx,
            );
            self.refresh_settings_worktrees(cx);
        }
        self.focus_workspace(window, cx);
        cx.notify();
    }

    /// The Settings › Worktrees page reloads, when it is open.
    pub(super) fn refresh_settings_worktrees(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.clone() {
            view.update(cx, |view, cx| view.refresh_worktrees(cx));
        }
    }

    /// The dialog overlays, above everything but popovers opened inside them.
    pub(super) fn worktree_modals(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut modals = Vec::new();
        if let Some(open) = &self.worktree_ui.create {
            if let Some(focus) = open.view.read(cx).lost_focus(window, cx) {
                window.focus(&focus, cx);
            }
            modals.push(
                div()
                    .absolute()
                    .inset_0()
                    .child(open.view.clone())
                    .into_any_element(),
            );
        }
        modals.extend(self.delete_worktree_modal(window, cx));
        modals
    }

    /// The switcher rows for `project`'s pending creations.
    fn pending_rows_for(&self, project: &str) -> Vec<PendingRow> {
        self.pending_worktrees_for(project)
            .into_iter()
            .map(|p| PendingRow {
                id: p.id,
                label: p.display_name.clone(),
                status: p.status(),
                failed: p.error.is_some(),
            })
            .collect()
    }

    /// Show `project`'s pending creations in `view`, now and as they change.
    pub(super) fn track_worktree_switcher(
        &self,
        project: &str,
        view: &Entity<WorktreeSwitcher>,
        cx: &mut Context<Self>,
    ) {
        let rows = self.pending_rows_for(project);
        view.update(cx, |view, cx| view.set_pending(rows, cx));
        let mut switchers = self.worktree_ui.switchers.borrow_mut();
        switchers.retain(|(_, weak)| weak.upgrade().is_some());
        switchers.push((project.to_owned(), view.downgrade()));
    }

    /// Push the pending rows to every open switcher.
    pub(super) fn sync_switcher_pending(&mut self, cx: &mut Context<Self>) {
        let updates: Vec<_> = self
            .worktree_ui
            .switchers
            .borrow()
            .iter()
            .filter_map(|(project, weak)| Some((weak.upgrade()?, self.pending_rows_for(project))))
            .collect();
        for (view, rows) in updates {
            view.update(cx, |view, cx| view.set_pending(rows, cx));
        }
    }

    /// The switcher events about creating worktrees. Returns whether `event` was one.
    pub(super) fn worktree_switcher_event(
        &mut self,
        project: &str,
        event: &SwitcherEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event {
            SwitcherEvent::NewWorktree(prefill) => {
                if self.menu.is_some() {
                    self.close_menu(cx);
                }
                if self.classic_worktree_picker.is_some() {
                    self.classic_worktree_picker = None;
                    self.notify_sidebar(cx);
                }
                self.open_create_worktree_dialog(
                    project.to_owned(),
                    prefill.clone(),
                    WorktreeOrigin::Switcher,
                    window,
                    cx,
                );
                true
            }
            SwitcherEvent::RetryPending(id) => {
                self.retry_worktree_create(*id, window, cx);
                true
            }
            SwitcherEvent::DismissPending(id) => {
                self.dismiss_pending_worktree(*id, cx);
                true
            }
            _ => false,
        }
    }

    /// The rail project menu offers "New worktree…" for local Git projects.
    pub(super) fn offers_new_worktree(&self, project: &str) -> bool {
        !monocode_core::files::remote::is_remote(std::path::Path::new(project))
            && (crate::git_cache::get(project).is_some_and(|s| !s.worktrees.is_empty())
                || std::path::Path::new(project).join(".git").exists())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle};
    use std::path::{Path, PathBuf};

    pub(in crate::shell) fn git(cwd: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository (`main`, one commit) and a worktree root, both in a temp dir.
    pub(in crate::shell) fn repo() -> (tempfile::TempDir, String, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let main = base.join("project");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", ".git/no-hooks"),
        ] {
            git(&main, &["config", k, v]);
        }
        std::fs::write(main.join("README.md"), "hello\n").unwrap();
        std::fs::write(main.join(".gitignore"), ".env\n*.log\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "Base"]);
        (
            temp,
            main.to_string_lossy().into_owned(),
            base.join("workspaces"),
        )
    }

    pub(in crate::shell) fn bind(cx: &mut TestAppContext) {
        cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
            crate::shell::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
    }

    /// The test shell with `main` as its selected project and `root` as the worktree root.
    fn shell(cx: &mut TestAppContext, main: &str, root: &Path) -> WindowHandle<Shell> {
        bind(cx);
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                worktree::meta::save_prefs(
                    &locked(&shell.store).unwrap(),
                    &worktree::Prefs {
                        root: Some(root.to_path_buf()),
                        ..Default::default()
                    },
                )
                .unwrap();
                shell.workspace.add_project(main);
                shell.select_project(main.to_owned(), w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
    }

    fn dialog(
        window: WindowHandle<Shell>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<CreateWorktreeDialog>> {
        window
            .update(cx, |shell, _, _| {
                shell.worktree_ui.create.as_ref().map(|o| o.view.clone())
            })
            .unwrap()
    }

    fn text(
        view: &Entity<CreateWorktreeDialog>,
        field: &str,
        cx: &mut VisualTestContext,
    ) -> String {
        view.update(cx, |view, cx| {
            let input = match field {
                "name" => &view.name,
                "base" => &view.base,
                _ => &view.branch,
            };
            input.read(cx).content.to_string()
        })
    }

    fn open(window: WindowHandle<Shell>, cx: &mut VisualTestContext, main: &str, prefill: &str) {
        window
            .update(cx, |shell, w, cx| {
                shell.open_create_worktree_dialog(
                    main.to_owned(),
                    prefill.to_owned(),
                    WorktreeOrigin::Rail,
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

    #[gpui::test]
    fn the_switcher_and_the_rail_open_the_dialog_and_submitting_creates_in_the_background(
        cx: &mut TestAppContext,
    ) {
        let (_temp, main, root) = repo();
        let window = shell(cx, &main, &root);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);

        // The switcher hands its typed text to the dialog instead of creating inline.
        window
            .update(visual, |shell, w, cx| {
                shell.open_menu(
                    MenuTarget::Header,
                    Anchor::Slot(shell.project_anchor.clone()),
                    w,
                    cx,
                )
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "menu-action-switch-worktree");
        visual.simulate_input("Login fix");
        visual.run_until_parked();
        click(visual, "create-sidebar-worktree");
        let view = dialog(window, visual).expect("New worktree… opens the dialog");
        assert_eq!(text(&view, "name", visual), "Login fix");
        window
            .update(visual, |shell, _, _| assert!(shell.menu.is_none()))
            .unwrap();
        assert!(visual.debug_bounds("worktree-dialog").is_some());

        // Submitting closes the dialog at once and leaves a pending row.
        window
            .update(visual, |_, _, cx| {
                view.update(cx, |view, cx| view.submit(cx))
            })
            .unwrap();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.worktree_ui.create.is_none(),
                    "Submit closes the dialog"
                );
                let pending = shell.pending_worktrees_for(&main);
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].display_name, "Login fix");
                assert_eq!(pending[0].request.created_by, worktree::CreatedBy::Dialog);
            })
            .unwrap();
        visual.run_until_parked();
        let created = root.join("project/Login-fix");
        assert!(created.is_dir(), "the worktree is under the test root");
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty());
                assert_eq!(
                    Path::new(shell.working_copy()),
                    created.canonicalize().unwrap(),
                    "the new worktree becomes the project's focus"
                );
                let meta = worktree::meta::load(&locked(&shell.store).unwrap(), &created)
                    .unwrap()
                    .unwrap();
                assert_eq!(meta.created_by, worktree::CreatedBy::Dialog);
            })
            .unwrap();

        // The rail project menu offers it too; Escape cancels.
        window
            .update(visual, |shell, w, cx| {
                shell.open_menu(
                    MenuTarget::Project {
                        path: main.clone(),
                        rail: true,
                    },
                    Anchor::Point(gpui::point(px(300.), px(60.))),
                    w,
                    cx,
                )
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "menu-action-new-worktree");
        let view = dialog(window, visual).expect("the rail menu opens the dialog");
        assert_eq!(text(&view, "name", visual), "");
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(dialog(window, visual).is_none(), "Escape cancels");
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty())
            })
            .unwrap();
    }

    #[gpui::test]
    fn invalid_or_taken_branches_and_an_empty_name_show_inline_errors(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        git(Path::new(&main), &["branch", "taken"]);
        let window = shell(cx, &main, &root);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);

        open(window, visual, &main, "");
        assert!(visual.debug_bounds("worktree-dialog-error").is_none());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(
            dialog(window, visual).is_some(),
            "an empty name does not submit"
        );
        assert!(visual.debug_bounds("worktree-dialog-error").is_some());

        visual.simulate_input("review");
        click(visual, "worktree-dialog-advanced");
        visual.simulate_input("bad..branch");
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("worktree-dialog-error").is_some(),
            "the branch is checked as it is typed"
        );
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(dialog(window, visual).is_some());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty())
            })
            .unwrap();

        // An existing branch asks to be used explicitly.
        visual.simulate_keystrokes("cmd-a");
        visual.simulate_input("taken");
        visual.run_until_parked();
        assert!(visual.debug_bounds("worktree-dialog-error").is_some());
        click(visual, "worktree-dialog-use-existing");
        assert!(visual.debug_bounds("worktree-dialog-error").is_none());
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(dialog(window, visual).is_none());
        let created = root.join("project/review");
        assert!(created.is_dir());
        let head = std::process::Command::new("git")
            .current_dir(&created)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "taken");
    }

    #[gpui::test]
    fn set_as_default_saves_the_projects_default_base(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        git(Path::new(&main), &["branch", "develop"]);
        let window = shell(cx, &main, &root);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, "from develop");
        assert!(
            visual.debug_bounds("worktree-dialog-set-default").is_none(),
            "the default base needs no saving"
        );
        click(visual, "worktree-dialog-base");
        assert!(
            visual
                .debug_bounds("worktree-dialog-base-options")
                .is_some(),
            "branches are offered while the field has focus"
        );
        visual.simulate_input("develop");
        visual.run_until_parked();
        click(visual, "worktree-dialog-set-default");
        let view = dialog(window, visual).unwrap();
        assert_eq!(text(&view, "base", visual), "develop");
        window
            .update(visual, |_, _, cx| {
                view.update(cx, |view, cx| view.submit(cx))
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                let prefs =
                    worktree::meta::repo_prefs(&locked(&shell.store).unwrap(), Path::new(&main))
                        .unwrap();
                assert_eq!(prefs.default_base.as_deref(), Some("develop"));
                let created = root.join("project/from-develop");
                let meta = worktree::meta::load(&locked(&shell.store).unwrap(), &created)
                    .unwrap()
                    .unwrap();
                assert_eq!(meta.base_ref, "develop");
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_setup_row_needs_a_script_and_shows_one_not_yet_approved(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let window = shell(cx, &main, &root);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, &main, "plain");
        assert!(visual.debug_bounds("worktree-dialog-name").is_some());
        assert!(visual.debug_bounds("worktree-dialog-setup").is_none());
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();

        std::fs::write(
            Path::new(&main).join("monocode.yaml"),
            "scripts:\n  setup: echo ready\n",
        )
        .unwrap();
        open(window, visual, &main, "with setup");
        assert!(visual.debug_bounds("worktree-dialog-setup").is_some());
        assert!(
            visual.debug_bounds("worktree-dialog-script").is_some(),
            "a new script is shown before it runs"
        );
        let view = dialog(window, visual).unwrap();
        assert!(
            !view.read_with(visual, |v, _| v.run_setup),
            "a new script waits for a yes"
        );
        click(visual, "worktree-dialog-setup-run");
        assert!(view.read_with(visual, |v, _| v.run_setup));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();

        window
            .update(visual, |shell, _, _| {
                let db = locked(&shell.store).unwrap();
                let mut prefs = worktree::meta::repo_prefs(&db, Path::new(&main)).unwrap();
                prefs.setup_approved_hash = Some(worktree::setup::hash("echo ready"));
                worktree::meta::save_repo_prefs(&db, Path::new(&main), &prefs).unwrap();
            })
            .unwrap();
        open(window, visual, &main, "approved");
        assert!(visual.debug_bounds("worktree-dialog-setup").is_some());
        assert!(visual.debug_bounds("worktree-dialog-script").is_none());
        let view = dialog(window, visual).unwrap();
        assert!(
            view.read_with(visual, |v, _| v.run_setup),
            "an approved script runs"
        );
    }

    #[gpui::test]
    fn the_switcher_lists_pending_creations_with_retry_and_dismiss(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let window = shell(cx, &main, &root);
        let id = window
            .update(cx, |shell, w, cx| {
                let request = worktree::CreateRequest {
                    base: Some("missing".into()),
                    ..worktree::CreateRequest::new(Path::new(&main), "later")
                };
                let id = shell.start_worktree_create(
                    main.clone(),
                    request,
                    false,
                    WorktreeOrigin::Switcher,
                    w,
                    cx,
                );
                let rows = shell.pending_rows_for(&main);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].label, "later");
                assert_eq!(rows[0].status, "Preparing…");
                assert!(!rows[0].failed);
                id
            })
            .unwrap();
        cx.run_until_parked();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.open_menu(
                    MenuTarget::Header,
                    Anchor::Slot(shell.project_anchor.clone()),
                    w,
                    cx,
                )
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "menu-action-switch-worktree");
        let row: &'static str = Box::leak(format!("pending-worktree-{id}").into_boxed_str());
        assert!(
            visual.debug_bounds(row).is_some(),
            "the failed creation is listed"
        );
        let retry: &'static str =
            Box::leak(format!("pending-worktree-retry-{id}").into_boxed_str());
        assert!(visual.debug_bounds(retry).is_some());

        // Retry runs it again: with the base there now, it succeeds and the row goes.
        git(Path::new(&main), &["branch", "missing"]);
        click(visual, retry);
        assert!(root.join("project/later").is_dir());
        assert!(visual.debug_bounds(row).is_none());

        // A failed row can be dismissed from the switcher.
        let id = window
            .update(visual, |shell, w, cx| {
                let request = worktree::CreateRequest {
                    base: Some("nowhere".into()),
                    ..worktree::CreateRequest::new(Path::new(&main), "never")
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
        let dismiss: &'static str =
            Box::leak(format!("pending-worktree-dismiss-{id}").into_boxed_str());
        click(visual, dismiss);
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_worktrees.is_empty())
            })
            .unwrap();
    }

    #[gpui::test]
    fn settings_create_and_delete_open_the_dialogs(cx: &mut TestAppContext) {
        let (_temp, main, root) = repo();
        let window = shell(cx, &main, &root);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.open_settings(crate::settings::Section::Worktrees, w, cx)
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "create-settings-worktree");
        let view = dialog(window, visual).expect("Create worktree opens the dialog");
        visual.simulate_input("from settings");
        visual.run_until_parked();
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(dialog(window, visual).is_none());
        drop(view);
        let created = root.join("project/from-settings");
        assert!(created.is_dir());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.settings.is_some(), "Settings stays open")
            })
            .unwrap();
        visual.run_until_parked();
        click(visual, "delete-tree-0");
        window
            .update(visual, |shell, _, _| {
                let open = shell
                    .worktree_ui
                    .delete
                    .as_ref()
                    .expect("Delete opens the dialog");
                assert_eq!(
                    Path::new(&open.path).canonicalize().unwrap(),
                    created.canonicalize().unwrap()
                );
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(
                    shell.worktree_ui.delete.is_none(),
                    "Escape closes the dialog"
                );
                assert!(shell.settings.is_some(), "and not Settings behind it");
            })
            .unwrap();
        assert!(created.is_dir());
    }

    #[gpui::test]
    fn remote_projects_show_the_unavailable_text(cx: &mut TestAppContext) {
        bind(cx);
        let (window, _, _) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        open(window, visual, "remote://host/project", "x");
        let view = dialog(window, visual).unwrap();
        let problem = view.read_with(visual, |v, cx| v.problem(cx));
        assert_eq!(problem.as_deref(), Some(worktree::REMOTE_UNAVAILABLE));
        window
            .update(visual, |shell, _, cx| {
                view.update(cx, |v, cx| v.submit(cx));
                let _ = shell;
            })
            .unwrap();
        visual.run_until_parked();
        assert!(dialog(window, visual).is_some(), "nothing to submit");
    }
}
