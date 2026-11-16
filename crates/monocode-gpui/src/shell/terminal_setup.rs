//! A project's terminal setup: where it is kept, what a new terminal is
//! started with, and the dialog that edits it.
use super::*;
use crate::terminal_setup::{Event, Setups, TerminalSetupDialog};
use monocode_core::{
    files::path_key,
    terminal_setup::{Launch, TerminalSetup},
};

impl Shell {
    /// The setup saved for a project, however its path is written.
    pub(super) fn project_terminal_setup(&self, project: &str) -> Option<&TerminalSetup> {
        let key = path_key(project);
        (self.workspace.project_terminal_setups.iter())
            .find(|(path, _)| path_key(path) == key)
            .map(|(_, setup)| setup)
    }

    pub(super) fn set_project_terminal_setup(
        &mut self,
        project: &str,
        setup: Option<TerminalSetup>,
        cx: &mut Context<Self>,
    ) {
        let key = path_key(project);
        self.workspace
            .project_terminal_setups
            .retain(|existing, _| path_key(existing) != key);
        if let Some(setup) = setup.map(TerminalSetup::normalized)
            && !setup.is_empty()
        {
            self.workspace
                .project_terminal_setups
                .insert(project.to_owned(), setup);
        }
        self.publish_terminal_setups(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Conversations read the setups to give their agents the same
    /// environment; they have no way to the workspace.
    pub(super) fn publish_terminal_setups(&self, cx: &mut Context<Self>) {
        cx.set_global(Setups(
            (self.workspace.project_terminal_setups.iter())
                .map(|(project, setup)| (path_key(project), setup.clone()))
                .collect(),
        ));
    }

    /// What a terminal opened in `folder` of `project` is started with.
    pub(super) fn terminal_launch(&self, project: &str, folder: &str) -> Launch {
        if project.starts_with("remote://") {
            return Launch::default();
        }
        self.project_terminal_setup(project)
            .map(|setup| setup.launch(std::path::Path::new(folder), std::path::Path::new(project)))
            .unwrap_or_default()
    }

    pub(super) fn open_terminal_setup_dialog(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        self.close_preview(cx);
        let current = self.project_terminal_setup(&path).cloned();
        let view = cx
            .new(|cx| TerminalSetupDialog::new(path.clone(), self.appearance(&path), current, cx));
        let events =
            cx.subscribe_in(
                &view,
                window,
                move |this, _, event: &Event, window, cx| match event {
                    Event::Save(setup) => {
                        this.set_project_terminal_setup(&path, setup.clone(), cx);
                        this.close_project_dialog(window, cx);
                    }
                    Event::Close => this.close_project_dialog(window, cx),
                },
            );
        window.focus(&view.focus_handle(cx), cx);
        self.show_terminal_setup_dialog(view, events, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, VisualTestContext};
    use monocode_core::terminal_setup::Venv;

    #[gpui::test]
    fn a_new_terminal_starts_with_its_projects_setup_and_agents_get_the_environment(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join(".venv");
        let bin = if cfg!(windows) { "Scripts" } else { "bin" };
        std::fs::create_dir_all(venv.join(bin)).unwrap();
        std::fs::write(venv.join(bin).join("activate"), "").unwrap();
        std::fs::write(root.path().join(".env"), "TOKEN=from-file\n").unwrap();
        let project = root.path().to_string_lossy().into_owned();

        let (window, other, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                // The project in front has no setup: its terminals start bare.
                let front = shell.workspace.project.clone();
                let bare = shell.spawn_terminal(front.clone(), cx);
                assert!(bare.read(cx).pending_startup().is_empty());

                let setup = TerminalSetup {
                    environment: "MODE=dev".into(),
                    env_file: ".env".into(),
                    venv: Venv::Auto,
                    commands: "make setup".into(),
                    agents: true,
                };
                shell.set_project_terminal_setup(&project, Some(setup.clone()), cx);
                assert_eq!(shell.project_terminal_setup(&project), Some(&setup));
                // Another project's terminal, in a folder of its own: the
                // project's virtual environment and file are found.
                let folder = root.path().join("worktree");
                std::fs::create_dir_all(&folder).unwrap();
                let launch = shell.terminal_launch(&project, &folder.to_string_lossy());
                assert_eq!(
                    launch.environment,
                    vec![
                        ("TOKEN".to_owned(), "from-file".to_owned()),
                        ("MODE".to_owned(), "dev".to_owned()),
                    ]
                );
                let terminal =
                    shell.spawn_terminal_in(&project, folder.to_string_lossy().into_owned(), cx);
                let activate = venv.join(bin).join("activate");
                assert_eq!(
                    terminal.read(cx).pending_startup(),
                    vec![
                        format!("source {}", activate.to_string_lossy()),
                        "make setup".to_owned()
                    ]
                );
                // The front project is still untouched by it.
                assert!(shell.terminal_launch(&front, &front).is_empty());

                // Agents of the project get the variables and the
                // environment's programs first on their PATH.
                let agent: std::collections::HashMap<_, _> =
                    crate::terminal_setup::agent_environment(&project, &project, cx)
                        .into_iter()
                        .collect();
                assert_eq!(agent["MODE"], "dev");
                assert_eq!(agent["TOKEN"], "from-file");
                assert_eq!(agent["VIRTUAL_ENV"], venv.to_string_lossy());
                assert!(agent["PATH"].starts_with(&*venv.join(bin).to_string_lossy()));
                assert!(crate::terminal_setup::agent_environment(&front, &front, cx).is_empty());

                // Kept out of agents when the setup says so.
                let terminals_only = TerminalSetup {
                    agents: false,
                    ..setup
                };
                shell.set_project_terminal_setup(&project, Some(terminals_only), cx);
                assert!(
                    crate::terminal_setup::agent_environment(&project, &project, cx).is_empty()
                );
                assert!(!shell.terminal_launch(&project, &project).is_empty());

                // What is missing is said in the terminal, not hidden.
                std::fs::remove_file(root.path().join(".env")).unwrap();
                let terminal = shell.spawn_terminal_in(&project, project.clone(), cx);
                assert!(
                    (terminal.read(cx).text())
                        .contains("[terminal setup] Environment file not found: .env")
                );

                // Cleared: nothing is kept for the project.
                shell.set_project_terminal_setup(&project, None, cx);
                assert!(shell.workspace.project_terminal_setups.is_empty());
                assert!(shell.terminal_launch(&project, &project).is_empty());
                let _ = other;
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_dialog_opens_from_the_project_and_saves_what_is_typed(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(crate::input::bind);
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let project = window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                let project = shell.workspace.project.clone();
                shell.open_terminal_setup_dialog(project.clone(), w, cx);
                project
            })
            .unwrap();
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("project-terminal-setup-dialog")
                .is_some()
        );
        // Nothing saved yet: the variables are open for typing, no Clear.
        assert!(
            visual
                .debug_bounds("terminal-setup-variable-names")
                .is_none()
        );
        visual.simulate_input("not a variable");
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("terminal-setup-invalid-lines")
                .is_some()
        );
        let save = |visual: &mut VisualTestContext| {
            let at = visual
                .debug_bounds("project-settings-Save")
                .unwrap()
                .center();
            visual.simulate_click(at, gpui::Modifiers::none());
            visual.run_until_parked();
        };
        // A line that is not a variable keeps the dialog open, unsaved.
        save(visual);
        assert!(
            visual
                .debug_bounds("project-terminal-setup-dialog")
                .is_some()
        );
        window
            .update(visual, |shell, _, _| {
                assert!(shell.project_terminal_setup(&project).is_none());
            })
            .unwrap();
        // The keyboard is back on the lines to fix.
        window
            .update(visual, |shell, w, cx| {
                let dialog = shell.terminal_setup_dialog().unwrap();
                assert_eq!(
                    dialog.read(cx).typed_environment(w, cx),
                    ("not a variable".to_owned(), true)
                );
            })
            .unwrap();
        visual.simulate_keystrokes("cmd-a");
        visual.simulate_input("export MODE=dev");
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("terminal-setup-invalid-lines")
                .is_none()
        );
        save(visual);
        assert!(
            visual
                .debug_bounds("project-terminal-setup-dialog")
                .is_none()
        );
        window
            .update(visual, |shell, _, cx| {
                let saved = shell.project_terminal_setup(&project).unwrap();
                assert_eq!(saved.environment, "export MODE=dev");
                assert!(saved.agents);
                // And at once what the project's agents are given.
                assert_eq!(
                    crate::terminal_setup::agent_environment(&project, &project, cx),
                    vec![("MODE".to_owned(), "dev".to_owned())]
                );
            })
            .unwrap();
        window
            .update(visual, |shell, w, cx| {
                let setup = TerminalSetup {
                    environment: "MODE=dev\nTOKEN=secret".into(),
                    ..Default::default()
                };
                shell.set_project_terminal_setup(&project, Some(setup), cx);
                shell.close_project_dialog(w, cx);
                shell.open_terminal_setup_dialog(project.clone(), w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        // Opened again, the saved values are out of sight: names only.
        assert!(
            visual
                .debug_bounds("terminal-setup-variable-names")
                .is_some()
        );
        assert!(
            visual
                .debug_bounds("terminal-setup-invalid-lines")
                .is_none()
        );
    }
}
