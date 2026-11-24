//! The footer's Actions button as the run part of it works: what its menu
//! offers with and without a file, which start the project remembers as
//! its latest, that a press on it opens the menu, and what the rows do.
use super::*;
use crate::{run_ui::footer, workspace::ProjectAction};
use gpui::Modifiers;

const TOOL: [(&str, &str); 2] = [("pkg/__init__.py", ""), ("pkg/tool.py", "print('tool')\n")];

/// A menu as lines: captions in brackets, separators as dashes, a row as its
/// label, and a row with a submenu followed by what the submenu lists.
pub(super) fn shape(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Header(title) => format!("[{title}]"),
            MenuItem::Separator => "-".to_owned(),
            MenuItem::Action(action) if action.submenu.is_empty() => action.label.to_string(),
            MenuItem::Action(action) => {
                let inside: Vec<&str> = action.submenu.iter().map(|a| &*a.label).collect();
                format!("{} > {}", action.label, inside.join(", "))
            }
            _ => "?".to_owned(),
        })
        .collect()
}

impl Fixture {
    pub(super) fn menu(&mut self) -> Vec<MenuItem> {
        let project = self.project.clone();
        self.visual.update(|_, cx| footer::menu_items(&project, cx))
    }

    /// The whole menu the button opens, saved actions and all.
    pub(super) fn button_menu(&mut self) -> Vec<MenuItem> {
        (self.window)
            .update(&mut self.visual, |shell, _, cx| shell.actions_menu(cx))
            .unwrap()
    }

    /// Saves configurations and returns their ids.
    pub(super) fn configure(&mut self, configs: &[(&str, &str)]) -> Vec<String> {
        let project = self.project.clone();
        let configs: Vec<RunConfig> = configs.iter().map(|(n, c)| user(n, c)).collect();
        let ids = self.feature(move |window, cx| {
            let mut runs = settings::runs(&project, cx);
            let ids = configs.into_iter().map(|config| runs.add(config)).collect();
            settings::save_runs(&project, runs, window, cx);
            ids
        });
        self.visual.run_until_parked();
        ids
    }

    /// The project's latest start, as `run:Name`, `action:Name` or `empty`.
    pub(super) fn shown(&mut self) -> String {
        let project = self.project.clone();
        (self.window)
            .update(&mut self.visual, |shell, _, cx| {
                let actions = shell.workspace.actions.get(&project);
                let actions = actions.map_or(&[][..], Vec::as_slice);
                match shell
                    .workspace
                    .last_actions
                    .get(&project)
                    .map(String::as_str)
                {
                    Some(footer::LAST_RUN) => {
                        let runs = crate::code_shell::settings::runs(&project, cx);
                        let used = runs.last_used().map(|config| config.label());
                        used.map_or("empty".to_owned(), |label| format!("run:{label}"))
                    }
                    Some(id) => (actions.iter().find(|a| a.id == id))
                        .map_or("empty".to_owned(), |a| format!("action:{}", a.name)),
                    None => "empty".to_owned(),
                }
            })
            .unwrap()
    }

    pub(super) fn click(&mut self, selector: &'static str) {
        let bounds = (self.visual.debug_bounds(selector)).unwrap_or_else(|| panic!("{selector}"));
        self.visual
            .simulate_click(bounds.center(), Modifiers::default());
        self.visual.run_until_parked();
    }

    /// Opens the button's menu and picks a row.
    pub(super) fn pick(&mut self, row: &str) {
        self.click("footer-action-menu");
        // Selectors live as long as the test run.
        self.click(Box::leak(format!("menu-action-{row}").into_boxed_str()));
    }

    pub(super) fn started(&mut self, count: usize) {
        let root = self.root.clone();
        self.wait("the run to start", |_| fx_runs(&root).len() >= count);
    }

    pub(super) fn add_action(&mut self, name: &str, command: &str) {
        let project = self.project.clone();
        let action = ProjectAction {
            id: "a1".into(),
            name: name.into(),
            command: command.into(),
            icon: "rocket".into(),
        };
        (self.window)
            .update(&mut self.visual, |shell, window, cx| {
                (shell.workspace.actions.entry(project).or_default()).push(action.clone());
                shell.run_action(&action.id, window, cx);
            })
            .unwrap();
        self.visual.run_until_parked();
    }
}

#[gpui::test]
fn without_a_file_the_menu_offers_the_configurations_that_need_none(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&TOOL, cx);
    assert!(fx.menu().is_empty(), "nothing is set up yet");
    fx.configure(&[
        ("Plain", "sh -c 'echo plain'"),
        ("Named", "sh -c 'echo $FILE_NAME'"),
    ]);
    // No file is focused: the one that needs a file is not offered, there
    // is no "This file" and, without a debugger, no Debug.
    assert_eq!(shape(&fx.menu()), ["[Run configurations]", "Plain"]);
    fx.visual.update(|_, cx| {
        launch::set_debugger(Rc::new(|_, _, _, _| {}), cx);
    });
    assert_eq!(
        shape(&fx.menu()),
        ["[Run configurations]", "Plain", "-", "Debug > Plain"]
    );
}

#[gpui::test]
fn with_a_python_file_the_menu_adds_its_tasks_and_debug_lists_every_target(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&TOOL, cx);
    fx.configure(&[
        ("Plain", "sh -c 'echo plain'"),
        ("Named", "sh -c 'echo $FILE_NAME'"),
    ]);
    fx.open("pkg/tool.py");
    let both = [
        "[This file]",
        "Run tool.py",
        "Run module pkg.tool",
        "[Run configurations]",
        "Plain",
        "Named",
    ];
    assert_eq!(shape(&fx.menu()), both, "no debugger yet");
    fx.visual.update(|_, cx| {
        launch::set_debugger(Rc::new(|_, _, _, _| {}), cx);
    });
    let mut with_debug: Vec<String> = both.iter().map(|row| row.to_string()).collect();
    with_debug.push("-".into());
    with_debug.push("Debug > Run tool.py, Run module pkg.tool, Plain, Named".into());
    assert_eq!(shape(&fx.menu()), with_debug);
}

#[gpui::test]
fn the_whole_menu_keeps_the_saved_actions_between_the_run_part_and_the_settings(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&[], cx);
    // Nothing to run: the menu is what it was, plus the way to the settings.
    assert_eq!(
        shape(&fx.button_menu()),
        ["Edit run configurations\u{2026}", "Add action"]
    );
    fx.configure(&[("Plain", "sh -c 'echo plain'")]);
    fx.add_action("Deploy", "echo deployed");
    assert_eq!(
        shape(&fx.button_menu()),
        [
            "[Run configurations]",
            "Plain",
            "-",
            "Deploy",
            "-",
            "Edit run configurations\u{2026}",
            "Edit action > Deploy",
            "Add action",
        ]
    );
    assert_eq!(fx.shown(), "action:Deploy");
}

#[gpui::test]
fn picking_a_configuration_runs_it_and_the_button_then_shows_it_until_something_else_starts(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&TOOL, cx);
    let root = fx.root.clone();
    let ids = fx.configure(&[("Plain", "sh -c 'echo plain'")]);
    // Nothing has been started yet.
    assert_eq!(fx.shown(), "empty");
    assert!(fx.visual.debug_bounds("footer-action-run").is_some());

    fx.pick(&format!("go:{}", ids[0]));
    fx.started(1);
    let info = fx.runs().remove(0);
    assert_eq!(info.label, "Plain");
    assert_eq!(info.spawn.command, "sh");
    assert_eq!(info.spawn.args, ["-c", "echo plain"]);
    fx.wait_for_end(info.id);
    assert_eq!(fx.output_of(info.id).trim(), "plain");
    assert_eq!(fx.shown(), "run:Plain");

    // A saved action started afterwards is what the button shows.
    fx.add_action("Deploy", "echo deployed");
    assert_eq!(fx.shown(), "action:Deploy");

    // So is a run started anywhere else: here, the file header's button.
    fx.open("pkg/tool.py");
    let doc = fx.doc("pkg/tool.py");
    let click = HeaderClick {
        doc: Rc::new(doc.clone()),
        position: gpui::point(gpui::px(0.), gpui::px(0.)),
        offset: 0,
        row: 0,
    };
    fx.feature(|window, cx| {
        let run = super::super::test_header(cx).actions(&doc, cx).remove(0);
        (run.on_click)(&click, window, cx)
    });
    fx.started(2);
    assert_eq!(fx.shown(), "run:Plain");
    let second = fx.runs()[1].id;
    fx.wait_for_end(second);

    // A press on the main part opens the menu; Rerun last repeats it.
    fx.click("footer-action-run");
    fx.click("menu-action-rerun");
    fx.started(3);
    assert_eq!(fx.runs()[2].label, "Plain");
    assert_eq!(fx_runs(&root).len(), 3);
}

#[gpui::test]
fn a_built_in_task_picked_from_this_file_runs_and_is_debugged_from_the_submenu(
    cx: &mut TestAppContext,
) {
    let mut fx = Fixture::new(&TOOL, cx);
    let debugged: Rc<RefCell<Vec<Spawn>>> = Rc::default();
    fx.visual.update(|_, cx| {
        let sink = debugged.clone();
        launch::set_debugger(
            Rc::new(move |spawn, _, _, _| sink.borrow_mut().push(spawn)),
            cx,
        );
    });
    fx.open("pkg/tool.py");
    fx.pick("go:builtin:python:module:pkg.tool");
    fx.started(1);
    let info = fx.runs().remove(0);
    assert_eq!(info.label, "Run module pkg.tool");
    assert_eq!(info.spawn.args, ["-m", "pkg.tool"]);
    assert_eq!(fx.shown(), "run:Run module pkg.tool");

    // Debug has a submenu: opening the row lists the targets, a click on
    // one hands that target to the debugger.
    fx.click("footer-action-menu");
    fx.click("menu-action-debug");
    fx.click("menu-action-debug:builtin:python:file:pkg/tool.py");
    fx.wait("the debugger to be handed the file", |_| {
        !debugged.borrow().is_empty()
    });
    assert_eq!(debugged.borrow()[0].args, ["pkg/tool.py"]);
}

#[gpui::test]
fn a_run_in_progress_has_a_stop_and_the_last_run_can_be_rerun(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    let root = fx.root.clone();
    assert!(!shape(&fx.menu()).contains(&"Rerun last".to_owned()));
    let ids = fx.configure(&[("Sleeper", "sleep 30")]);
    fx.pick(&format!("go:{}", ids[0]));
    fx.started(1);
    let first = fx.runs()[0].id;
    fx.wait("the run to be running", |_| {
        (services().runs().get(first)).is_some_and(|info| info.state.is_running())
    });
    let menu = shape(&fx.menu());
    assert_eq!(
        menu,
        [
            "[Running]",
            "Stop Sleeper",
            "[Run configurations]",
            "Sleeper",
            "-",
            "Rerun last"
        ]
    );

    // Rerun last starts the same process again, in its tab.
    fx.pick("rerun");
    fx.wait("the rerun", |_| fx_runs(&root).len() == 2);

    // Stop ends what runs.
    let running = fx.runs().last().map(|info| info.id).unwrap();
    fx.pick(&format!("stop:{}", running.0));
    fx.wait("the stop", |_| {
        (services().runs().get(running)).is_some_and(|info| !info.state.is_running())
    });
    assert!(!shape(&fx.menu()).iter().any(|row| row.starts_with("Stop")));
}

#[gpui::test]
fn an_empty_project_opens_the_add_action_dialog(cx: &mut TestAppContext) {
    let mut fx = Fixture::new(&[], cx);
    assert_eq!(fx.shown(), "empty");
    fx.click("footer-action-run");
    assert!(
        (fx.window)
            .update(&mut fx.visual, |shell, _, _| shell.action_dialog_open())
            .unwrap(),
        "the dialog opens"
    );
}
