//! Fake features driving each of `code_shell`'s extension points through a
//! real shell: a run tab on a real process, a view in the dock, a right
//! panel tab, a footer chip, an owned process, quick open commands and a
//! prefix mode, a settings page, a rail status and the context lookups.
use super::{tests::window, *};
use crate::code_shell::{
    self,
    chips::{ChipItem, ChipState, FooterChip, Tone},
    commands::{self, Command, Item, Mode},
    context::{self, Checkout, CodeEvent},
    dock::{
        self, DockArea, DockPlacement, DockTab, DockTabHandle, RunEvent, RunExit, RunRequest,
        RunState,
    },
    panel_tabs::{self, PanelTab},
    processes::{self, ProcessKind},
    rail,
    settings::{self, PageContext, SettingsPage},
};
use crate::right_panel::{ExtraTab, RightTab};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};
use monocode_core::code::spawn::Spawn;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

const PROJECT: &str = "/work/monocode";

/// Calls a `code_shell` function the way a feature does: with the window,
/// outside any update of the shell.
fn feature<R>(
    window: WindowHandle<Shell>,
    cx: &mut VisualTestContext,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(window.into(), |_, window, cx| f(window, cx))
        .unwrap()
}

/// Lets real time pass until `done`, for what a real process does.
fn wait(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    for _ in 0..1000 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

struct FakeView(&'static str);
impl Render for FakeView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let id = self.0;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .size_full()
            .child(id)
    }
}

fn shell(cx: &mut TestAppContext) -> (WindowHandle<Shell>, VisualTestContext, i64) {
    let (window, _, session) = window(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    window
        .update(cx, |shell, _, _| shell.terminals_in_dock = true)
        .unwrap();
    let visual = VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    (window, visual, session)
}

fn sh(label: &str, script: &str, folder: &std::path::Path) -> Spawn {
    Spawn {
        label: label.into(),
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: folder.into(),
        env: vec![("FAKE".into(), "there".into())],
    }
}

#[gpui::test]
fn a_run_tab_runs_its_process_reports_it_and_can_be_rerun_and_stopped(cx: &mut TestAppContext) {
    // A run's process is real, and wakes the UI from its PTY thread.
    cx.executor().allow_parking();
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let dir = tempfile::tempdir().unwrap();
    let checkout = Checkout {
        project: PROJECT.into(),
        folder: dir.path().to_string_lossy().into_owned(),
    };

    let request = RunRequest {
        agent: Some("Fix the tests".into()),
        ..RunRequest::new(
            sh("exit three", "echo hi-$FAKE; exit 3", dir.path()),
            checkout.clone(),
        )
    };
    let run = feature(window, visual, |window, cx| dock::run(request, window, cx)).unwrap();
    let output = Rc::new(RefCell::new(String::new()));
    let exits = Rc::new(RefCell::new(Vec::new()));
    let _tap = visual.update(|_, cx| {
        let (output, exits) = (output.clone(), exits.clone());
        cx.subscribe(&run, move |_, event: &RunEvent, _| match event {
            RunEvent::Output(text) => output.borrow_mut().push_str(text),
            RunEvent::Exited(exit) => exits.borrow_mut().push(*exit),
            RunEvent::Started { .. } | RunEvent::Closed => {}
        })
    });
    // The process is the tab's own child, known at once.
    assert!(run.read_with(visual, |run, _| run.pid()).is_some());
    assert_eq!(
        run.read_with(visual, |run, _| run.state()),
        RunState::Running
    );

    wait(visual, "the first exit", |cx| {
        !run.read_with(cx, |run, _| run.state().running())
    });
    let three = RunExit {
        code: Some(3),
        stopped: false,
    };
    run.read_with(visual, |run, _| {
        assert_eq!(run.state(), RunState::Exited(Some(3)));
        assert_eq!(run.pid(), None);
        // The project's variables reached it, and the escape codes did not
        // reach the tap.
        assert_eq!(run.tail().trim(), "hi-there");
    });
    assert_eq!(output.borrow().trim(), "hi-there");
    assert_eq!(*exits.borrow(), [three]);
    let exited = run.update(visual, |run, _| run.exited());
    assert_eq!(exited.try_recv(), Ok(three));
    window
        .update(visual, |shell, _, cx| {
            // The tab stays with its status, titled by the label, and is not
            // something a restart would bring back.
            let tabs = &shell.terminals[PROJECT];
            assert_eq!(tabs.len(), 1);
            assert_eq!(tabs[0].read(cx).title, "exit three");
            assert!(tabs[0].read(cx).exited);
            assert!(shell.exited_terminals.is_empty());
            let dock = shell.workspace.docks[PROJECT].clone();
            assert!(dock.open && dock.is_transient(0));
            assert!(shell.dock_tab_controls(&dock, cx).is_some());
            let saved = shell.workspace.saved_json();
            assert!(!saved.contains(&checkout.folder), "{saved}");
        })
        .unwrap();
    visual.run_until_parked();
    assert!(visual.debug_bounds("run-status-dot").is_some());
    assert!(visual.debug_bounds("dock-run-agent").is_some());

    // Rerun starts it again in the same tab.
    run.update(visual, |run, cx| run.rerun(cx));
    wait(visual, "the rerun's exit", |_| exits.borrow().len() == 2);
    assert_eq!(*exits.borrow(), [three, three]);
    assert_eq!(output.borrow().matches("hi-there").count(), 2);
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.terminals[PROJECT].len(), 1)
        })
        .unwrap();

    // A long-running one is stopped: its whole group is terminated.
    let request = RunRequest::new(
        sh("server", "echo started; sleep 30", dir.path()),
        checkout.clone(),
    );
    let server = feature(window, visual, |window, cx| dock::run(request, window, cx)).unwrap();
    wait(visual, "the server to start", |cx| {
        server.read_with(cx, |run, _| run.tail().contains("started"))
    });
    let pid = server.read_with(visual, |run, _| run.pid()).unwrap();
    let waiting = server.update(visual, |run, _| run.exited());
    assert!(waiting.try_recv().is_err());
    server.update(visual, |run, cx| run.stop(cx));
    wait(visual, "the server to stop", |cx| {
        server.read_with(cx, |run, _| run.state() == RunState::Stopped)
    });
    assert!(waiting.try_recv().is_ok_and(|exit| exit.stopped));
    #[cfg(unix)]
    assert_ne!(
        unsafe { libc::kill(pid as i32, 0) },
        0,
        "the process is gone"
    );

    // Closing the tab from outside takes it out of the dock and tells the run.
    let tab = visual.update(|_, cx| DockTabHandle::of_run(&server, cx));
    assert!(tab.is_open());
    feature(window, visual, |window, cx| tab.close(window, cx));
    wait(visual, "the tab to close", |cx| {
        !server.read_with(cx, |run, _| run.is_open())
    });
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.terminals[PROJECT].len(), 1)
        })
        .unwrap();
}

#[gpui::test]
fn ordinary_terminals_are_saved_and_a_run_beside_them_is_not(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let dir = tempfile::tempdir().unwrap();
    let checkout = Checkout {
        project: PROJECT.into(),
        folder: dir.path().to_string_lossy().into_owned(),
    };
    window
        .update(visual, |shell, window, cx| shell.new_terminal(window, cx))
        .unwrap();
    let request = RunRequest {
        reveal: false,
        ..RunRequest::new(sh("quiet", "exit 0", dir.path()), checkout)
    };
    let run = feature(window, visual, |window, cx| dock::run(request, window, cx)).unwrap();
    wait(visual, "the run to end", |cx| {
        run.read_with(cx, |run, _| run.state().succeeded())
    });
    window
        .update(visual, |shell, _, cx| {
            let dock = shell.workspace.docks[PROJECT].clone();
            // Not revealed: the shell stays selected, the run sits beside it.
            assert_eq!((dock.terminals.len(), dock.active), (2, 0));
            assert!(shell.terminals[PROJECT][0].read(cx).is_shell());
            assert!(!shell.dock_tab_stays(0, cx) && shell.dock_tab_stays(1, cx));
            let saved: serde_json::Value =
                serde_json::from_str(&shell.workspace.saved_json()).unwrap();
            assert_eq!(
                saved["docks"][PROJECT]["terminals"],
                serde_json::json!([shell.workspace.docks[PROJECT].terminals[0]])
            );
        })
        .unwrap();
}

#[gpui::test]
fn a_feature_view_is_a_dock_tab_that_is_placed_revealed_and_closed(cx: &mut TestAppContext) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let checkout = Checkout {
        project: PROJECT.into(),
        folder: PROJECT.into(),
    };
    let placements = Rc::new(RefCell::new(Vec::new()));
    let closed = Rc::new(Cell::new(false));
    let tab = |cx: &mut App, id: &'static str| {
        let (placements, closed) = (placements.clone(), closed.clone());
        DockTab {
            id: id.into(),
            title: "Debug: app".into(),
            icon: "bug",
            view: cx.new(|_| FakeView("fake-debug-view")).into(),
            focus: None,
            controls: Some(cx.new(|_| FakeView("fake-step-buttons")).into()),
            checkout: checkout.clone(),
            on_close: Some(Box::new(move |_| closed.set(true))),
            on_placement: Some(Rc::new(move |placement, _| {
                placements.borrow_mut().push(placement)
            })),
            reveal: true,
        }
    };
    let handle = feature(window, visual, |window, cx| {
        let tab = tab(cx, "debug-1");
        dock::add_tab(tab, window, cx)
    })
    .unwrap();
    visual.run_until_parked();
    assert!(visual.debug_bounds("fake-debug-view").is_some());
    assert!(visual.debug_bounds("fake-step-buttons").is_some());
    let bottom = DockPlacement {
        area: DockArea::Bottom,
        extent: 220.,
    };
    assert_eq!(*placements.borrow(), [bottom]);
    assert_eq!(visual.update(|_, cx| handle.placement(cx)), Some(bottom));
    assert!(!bottom.narrow());
    visual.update(|_, cx| assert!(dock::find_tab(PROJECT, "debug-1", cx).is_some()));
    // The id is the tab's name in its dock.
    let again = feature(window, visual, |window, cx| {
        let tab = tab(cx, "debug-1");
        dock::add_tab(tab, window, cx)
    });
    assert!(again.is_err());

    // A terminal opened after it takes the dock; revealing brings it back.
    window
        .update(visual, |shell, window, cx| {
            shell.new_terminal(window, cx);
            assert_eq!(shell.workspace.docks[PROJECT].active, 1);
        })
        .unwrap();
    assert!(feature(window, visual, |window, cx| handle.reveal(window, cx)));
    visual.update(|_, cx| handle.set_title("Debug: paused", cx));
    window
        .update(visual, |shell, _, cx| {
            assert_eq!(shell.workspace.docks[PROJECT].active, 0);
            assert_eq!(shell.terminals[PROJECT][0].read(cx).title, "Debug: paused");
            // Moved to the side, the view is told the dock is narrow now.
            shell.set_dock_side(DockSide::Right, cx);
        })
        .unwrap();
    visual.run_until_parked();
    let side = *placements.borrow().last().unwrap();
    assert_eq!(side.area, DockArea::Right);
    assert!(side.narrow());

    assert!(!closed.get());
    feature(window, visual, |window, cx| handle.close(window, cx));
    visual.run_until_parked();
    assert!(closed.get() && !handle.is_open());
    visual.update(|_, cx| assert!(dock::find_tab(PROJECT, "debug-1", cx).is_none()));
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.terminals[PROJECT].len(), 1)
        })
        .unwrap();
}

#[gpui::test]
fn a_feature_tab_joins_the_right_panel_where_it_was_left_and_goes_when_removed(
    cx: &mut TestAppContext,
) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    window
        .update(visual, |shell, _, cx| {
            shell.workspace.classic_layout = false;
            shell.workspace.new_layout_boxy = true;
            shell.layout_changed(cx);
            // Where the user dragged it to in an earlier session.
            let strip = vec!["fake-outline".to_owned(), "files".into(), "graph".into()];
            shell.workspace.right_panel_strip = strip.clone();
            if let Some(panel) = &shell.right_panel {
                panel.update(cx, |panel, cx| panel.set_strip(strip, cx));
            }
        })
        .unwrap();
    let closed = Rc::new(Cell::new(false));
    let outline = RightTab::Extra(ExtraTab("fake-outline"));
    feature(window, visual, |window, cx| {
        let closed = closed.clone();
        panel_tabs::show(
            PanelTab {
                id: "fake-outline",
                title: "Outline",
                icon: "clock",
                view: cx.new(|_| FakeView("fake-outline-view")).into(),
                focus: None,
                closable: true,
                on_close: Some(Rc::new(move |_| closed.set(true))),
            },
            cx,
        );
        assert!(panel_tabs::reveal("fake-outline", window, cx));
    });
    visual.run_until_parked();
    window
        .update(visual, |shell, _, cx| {
            assert!(shell.workspace.right_panel_open);
            let panel = shell.right_panel.clone().unwrap();
            assert_eq!(panel.read(cx).tab(), outline);
            assert_eq!(
                panel.read(cx).tabs(),
                [
                    outline,
                    RightTab::Files,
                    RightTab::Graph,
                    RightTab::Worktrees
                ]
            );
            assert_eq!(shell.right_tab_count(cx), 4);
            // A restart finds no such tab, so Explorer is what is saved.
            let saved: serde_json::Value =
                serde_json::from_str(&shell.workspace.saved_json()).unwrap();
            assert_eq!(saved["right_panel_tab"], "files");
            assert_eq!(
                saved["right_panel_tabs"],
                serde_json::json!(["files", "graph", "worktrees"])
            );
        })
        .unwrap();
    assert!(visual.debug_bounds("fake-outline-view").is_some());
    assert!(visual.debug_bounds("fake-outline").is_some(), "its tab");

    visual.update(|_, cx| panel_tabs::remove("fake-outline", cx));
    visual.run_until_parked();
    assert!(closed.get());
    window
        .update(visual, |shell, _, cx| {
            let panel = shell.right_panel.clone().unwrap();
            assert_eq!(panel.read(cx).tab(), RightTab::Files);
            assert_eq!(
                panel.read(cx).tabs(),
                [RightTab::Files, RightTab::Graph, RightTab::Worktrees]
            );
        })
        .unwrap();
    assert!(visual.debug_bounds("fake-outline-view").is_none());
}

#[gpui::test]
fn a_footer_chip_shows_while_it_has_something_to_report_and_lists_its_rows(
    cx: &mut TestAppContext,
) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let servers = Rc::new(Cell::new(0usize));
    let picked = Rc::new(RefCell::new(Vec::new()));
    visual.update(|_, cx| {
        let (count, rows, picked) = (servers.clone(), servers.clone(), picked.clone());
        code_shell::chips::register(
            FooterChip {
                id: "fake-servers",
                label: "Language servers",
                state: Rc::new(move |_| {
                    (count.get() > 0).then(|| ChipState {
                        icon: "bug",
                        text: format!("{} servers", count.get()).into(),
                        tone: Tone::Good,
                        tooltip: "Language servers".into(),
                    })
                }),
                items: Rc::new(move |_| {
                    (0..rows.get())
                        .map(|ix| ChipItem {
                            id: format!("server-{ix}").into(),
                            title: "basedpyright".into(),
                            detail: Some("120 MB".into()),
                            actions: vec![(format!("stop-{ix}").into(), "Stop".into())],
                            ..Default::default()
                        })
                        .collect()
                }),
                pick: Rc::new(move |id, _, _| picked.borrow_mut().push(id.to_owned())),
            },
            cx,
        );
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("fake-servers").is_none(), "nothing yet");

    servers.set(2);
    visual.update(|_, cx| code_shell::refresh(cx));
    visual.run_until_parked();
    let chip = visual.debug_bounds("fake-servers").expect("the chip");
    visual.simulate_click(chip.center(), Default::default());
    visual.run_until_parked();
    let target = MenuTarget::FooterChip("fake-servers");
    window
        .update(visual, |shell, window, cx| {
            assert!(shell.menu_open(&target));
            let (label, items, _, _) = shell.code_chip_menu("fake-servers", cx);
            assert_eq!(label, "Language servers");
            assert_eq!(items.len(), 2);
            shell.close_menu(cx);
            shell.run_menu(target.clone(), "stop-1", None, window, cx);
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(*picked.borrow(), ["stop-1"]);

    servers.set(0);
    visual.update(|_, cx| code_shell::refresh(cx));
    visual.run_until_parked();
    assert!(visual.debug_bounds("fake-servers").is_none());
}

#[test]
fn a_registered_process_is_listed_with_its_memory_and_can_be_stopped() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let registration = processes::register(pid, "basedpyright", ProcessKind::LanguageServer);
    assert!(processes::owns(pid));
    let rows = processes::rows();
    let row = rows.iter().find(|row| row.pid == pid).expect("listed");
    assert_eq!(row.command, "basedpyright");
    assert!(row.memory_bytes > 0 && !row.agent);
    assert_eq!(
        processes::group_title(row.session),
        Some("Language servers")
    );
    assert!(processes::stop(pid, "not its birth time").is_err());
    processes::stop(pid, &row.identity).unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success(), "it was terminated: {status:?}");
    // Gone, it is no longer listed; and a dropped registration forgets it.
    assert!(processes::rows().iter().all(|row| row.pid != pid));
    drop(registration);
    assert!(!processes::owns(pid));
}

#[gpui::test]
fn search_lists_a_features_commands_and_the_rows_of_its_prefix_mode(cx: &mut TestAppContext) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("views.py");
    std::fs::write(&file, "import os\n\n\ndef index():\n    pass\n").unwrap();
    let path = file.to_string_lossy().into_owned();
    let ran = Rc::new(RefCell::new(Vec::new()));
    visual.update(|_, cx| {
        let ran = ran.clone();
        commands::register_commands(
            "fake-run",
            move |scope, _| {
                let (ran, project) = (ran.clone(), scope.project.clone());
                vec![Command {
                    title: "Run: pytest".into(),
                    subtitle: scope.project.clone().into(),
                    run: Rc::new(move |_, _| ran.borrow_mut().push(project.clone())),
                }]
            },
            cx,
        );
        let path = path.clone();
        commands::register_mode(
            Mode {
                id: "fake-symbols",
                prefix: '@',
                hint: "Type to search symbols",
                applies: Rc::new(|_| true),
                search: Rc::new(move |query, _, cx| {
                    let path = path.clone();
                    // The feature's own work, off the UI thread.
                    cx.background_executor().spawn(async move {
                        vec![Item {
                            title: format!("{query}()").into(),
                            detail: "views.py:4".into(),
                            path: Some(path),
                            line: 4,
                            column: 5,
                            ..Default::default()
                        }]
                    })
                }),
            },
            cx,
        );
    });
    window
        .update(visual, |shell, window, cx| {
            shell.open_palette(window, cx);
            shell.set_palette_query("run: pytest".into(), cx);
        })
        .unwrap();
    visual.run_until_parked();
    let palette_rows = window
        .update(visual, |shell, _, cx| {
            let open = shell.palette.as_ref().unwrap();
            open.picker().read(cx).row_labels()
        })
        .unwrap();
    assert_eq!(
        palette_rows.first(),
        Some(&("Run: pytest".to_owned(), PROJECT.to_owned())),
        "{palette_rows:?}"
    );
    window
        .update(visual, |shell, _, cx| {
            let picker = shell.palette.as_ref().unwrap().picker().clone();
            picker.update(cx, |picker, cx| picker.pick_row(0, cx));
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(*ran.borrow(), [PROJECT]);
    let rows = |window: WindowHandle<Shell>, cx: &mut VisualTestContext| {
        window
            .update(cx, |shell, _, cx| {
                let open = shell.everywhere.as_ref().unwrap();
                open.view.read(cx).row_labels()
            })
            .unwrap()
    };
    window
        .update(visual, |shell, window, cx| {
            assert!(shell.palette.is_none());
            shell.open_everywhere(
                Some(crate::everywhere::Tab::All),
                Some("@index".into()),
                window,
                cx,
            );
        })
        .unwrap();
    visual.run_until_parked();
    // The mode asks its feature a moment after typing stops.
    visual.executor().advance_clock(Duration::from_millis(200));
    visual.run_until_parked();
    assert_eq!(
        rows(window, visual),
        [("index()".to_owned(), "views.py:4".to_owned())]
    );
    window
        .update(visual, |shell, _, cx| {
            let view = shell.everywhere.as_ref().unwrap().view.clone();
            view.update(cx, |view, cx| view.pick_row(0, cx));
        })
        .unwrap();
    visual.run_until_parked();
    // The row opened its file, which is now the focused one.
    visual.update(|_, cx| {
        let focused = context::focused_file(cx).expect("a focused file");
        assert_eq!(focused.path, file.to_string_lossy());
    });
}

#[gpui::test]
fn a_feature_knows_its_projects_checkouts_and_focused_file_and_hears_them_change(
    cx: &mut TestAppContext,
) {
    let heard = Rc::new(RefCell::new(Vec::new()));
    // A feature subscribes when it is installed, before the window is up.
    let _events = cx.update(|cx| {
        code_shell::register(cx);
        let heard = heard.clone();
        let events = context::events(cx);
        cx.subscribe(&events, move |_, event: &CodeEvent, _| {
            heard.borrow_mut().push(match event {
                CodeEvent::ProjectOpened(project) => format!("opened {project}"),
                CodeEvent::ProjectClosed(project) => format!("closed {project}"),
                CodeEvent::ProjectSlept(project) => format!("slept {project}"),
                CodeEvent::ProjectWoke(project) => format!("woke {project}"),
                CodeEvent::CheckoutAdded(checkout) => format!("checkout {}", checkout.folder),
                CodeEvent::CheckoutRemoved(checkout) => format!("gone {}", checkout.folder),
                CodeEvent::FocusedFileChanged(file) => {
                    format!("file {:?}", file.as_ref().map(|file| file.path.clone()))
                }
            })
        })
    });
    let (window, mut visual, session) = shell(cx);
    let visual = &mut visual;
    {
        let heard = heard.borrow();
        assert!(
            heard.contains(&"opened /work/monocode".to_owned()),
            "{heard:?}"
        );
        assert!(heard.contains(&"opened /work/another-project".to_owned()));
        assert!(heard.contains(&"checkout /work/monocode".to_owned()));
    }
    let monocode = Checkout {
        project: PROJECT.into(),
        folder: PROJECT.into(),
    };
    visual.update(|_, cx| {
        let snapshot = context::snapshot(cx);
        assert_eq!(snapshot.projects.len(), 2);
        assert_eq!(context::focused_checkout(cx), Some(monocode.clone()));
        assert_eq!(
            context::checkout_for_path("/work/monocode/app/views.py", cx),
            Some(monocode.clone())
        );
        assert_eq!(context::checkout_for_path("/elsewhere/a.py", cx), None);
        assert_eq!(
            context::checkout_for_pane(session, cx),
            Some(monocode.clone())
        );
        assert!(context::focused_file(cx).is_none());
        assert!(context::terminal_setup(PROJECT, cx).is_empty());
    });
    // The project's terminal setup is what a toolchain is built from.
    let setup = monocode_core::terminal_setup::TerminalSetup {
        environment: "MODE=dev".into(),
        ..Default::default()
    };
    window
        .update(visual, |shell, _, cx| {
            shell.set_project_terminal_setup(PROJECT, Some(setup.clone()), cx)
        })
        .unwrap();
    visual.update(|_, cx| {
        assert_eq!(context::terminal_setup(PROJECT, cx), setup);
        let toolchain = context::toolchain(&monocode, cx);
        assert!((toolchain.environment.iter()).any(|(key, value)| key == "MODE" && value == "dev"));
    });

    // Opening a file at a line makes it the focused file, and says so.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.py");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let path = file.to_string_lossy().into_owned();
    assert!(feature(window, visual, |window, cx| {
        context::open_file(&path, Some((2, 1)), window, cx)
    }));
    visual.run_until_parked();
    visual.update(|_, cx| {
        let focused = context::focused_file(cx).expect("a focused file");
        assert_eq!(focused.path, path);
        assert!(focused.editor.upgrade().is_some());
    });
    assert_eq!(
        heard.borrow().last(),
        Some(&format!("file {:?}", Some(path.clone())))
    );

    // A project that closes is heard of, with its checkout.
    window
        .update(visual, |shell, _, cx| {
            shell
                .workspace
                .projects
                .retain(|project| project != "/work/another-project");
            cx.notify();
        })
        .unwrap();
    visual.run_until_parked();
    let heard = heard.borrow();
    assert!(
        heard.contains(&"closed /work/another-project".to_owned()),
        "{heard:?}"
    );
    assert!(heard.contains(&"gone /work/another-project".to_owned()));
}

#[gpui::test]
fn a_settings_page_is_built_for_its_project_and_its_value_is_kept(cx: &mut TestAppContext) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    let built: Rc<RefCell<Option<PageContext>>> = Rc::default();
    visual.update(|_, cx| {
        let built = built.clone();
        settings::register(
            SettingsPage {
                id: "fake-run",
                title: "Run configurations",
                icon: "play",
                build: Rc::new(move |context, _, cx| {
                    *built.borrow_mut() = Some(context);
                    cx.new(|_| FakeView("fake-run-form")).into()
                }),
            },
            cx,
        );
    });
    window
        .update(visual, |shell, _, cx| {
            let rows = shell.code_settings_menu_items(cx);
            assert!(matches!(
                rows.as_slice(),
                [MenuItem::Action(row)] if row.id == "code-settings:fake-run"
            ));
        })
        .unwrap();
    assert!(feature(window, visual, |window, cx| {
        settings::open(PROJECT, "fake-run", window, cx)
    }));
    visual.run_until_parked();
    assert!(visual.debug_bounds("fake-run-form").is_some());
    let context = built.borrow().clone().expect("the page was built");
    assert_eq!((context.project.as_str(), &context.value), (PROJECT, &None));

    let value = serde_json::json!({"configurations": [{"name": "server"}]});
    feature(window, visual, |window, cx| {
        context.host.save(Some(value.clone()), window, cx)
    });
    visual.update(|_, cx| {
        assert_eq!(
            settings::value(PROJECT, "fake-run", cx),
            Some(value.clone())
        );
        assert_eq!(
            settings::value("/work/another-project", "fake-run", cx),
            None
        );
    });
    window
        .update(visual, |shell, _, _| {
            assert!(shell.code_page_dialog().is_some());
            assert!(shell.workspace.saved_json().contains("fake-run"));
        })
        .unwrap();
    feature(window, visual, |window, cx| context.host.close(window, cx));
    window
        .update(visual, |shell, _, _| {
            assert!(shell.code_page_dialog().is_none())
        })
        .unwrap();
    // Opened again, the page is handed what it saved.
    feature(window, visual, |window, cx| {
        settings::open(PROJECT, "fake-run", window, cx)
    });
    visual.run_until_parked();
    assert_eq!(built.borrow().as_ref().unwrap().value, Some(value));
}

#[gpui::test]
fn escape_closes_a_settings_page_with_a_field_focused(cx: &mut TestAppContext) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    visual.update(|_, cx| {
        settings::register(
            SettingsPage {
                id: "fake-run",
                title: "Run configurations",
                icon: "play",
                build: Rc::new(|_, window, cx| {
                    let field = cx.new(crate::input::Input::new);
                    window.focus(&field.focus_handle(cx), cx);
                    field.into()
                }),
            },
            cx,
        );
    });
    assert!(feature(window, visual, |window, cx| {
        settings::open(PROJECT, "fake-run", window, cx)
    }));
    visual.run_until_parked();
    window
        .update(visual, |shell, window, cx| {
            let field = shell.code_page_dialog().is_some();
            assert!(field);
            let _ = (window, cx);
        })
        .unwrap();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    window
        .update(visual, |shell, _, _| {
            assert!(shell.code_page_dialog().is_none())
        })
        .unwrap();
}

#[gpui::test]
fn a_feature_puts_a_status_on_a_sessions_rail_row_and_clears_it(cx: &mut TestAppContext) {
    let (_, mut visual, session) = shell(cx);
    let visual = &mut visual;
    visual.update(|_, cx| {
        assert_eq!(rail::status(session, cx), None);
        rail::set_status(session, "paused at views.py:43", cx);
        assert_eq!(
            rail::status(session, cx).as_deref(),
            Some("paused at views.py:43")
        );
    });
    visual.run_until_parked();
    visual.update(|_, cx| {
        rail::clear_status(session, cx);
        assert_eq!(rail::status(session, cx), None);
    });
}

/// A form far taller than any window, in the frame the project pages use.
struct TallPage;
impl Render for TallPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let body = div()
            .debug_selector(|| "tall-page-form".into())
            .h(px(3000.))
            .child("a very tall form");
        let footer = div().h(px(30.)).child("Save");
        settings::page_frame("tall-page-scroll", body, Some(footer))
    }
}

#[gpui::test]
fn a_tall_settings_page_scrolls_inside_the_window_and_keeps_its_footer(cx: &mut TestAppContext) {
    let (window, mut visual, _) = shell(cx);
    let visual = &mut visual;
    visual.update(|_, cx| {
        settings::register(
            SettingsPage {
                id: "tall-page",
                title: "Tall page",
                icon: "play",
                build: Rc::new(|_, _, cx| cx.new(|_| TallPage).into()),
            },
            cx,
        );
    });
    assert!(feature(window, visual, |window, cx| {
        settings::open(PROJECT, "tall-page", window, cx)
    }));
    // The dialog is placed by the height it measured in the frame before.
    for _ in 0..3 {
        visual.run_until_parked();
        visual.update(|window, _| window.refresh());
    }
    visual.run_until_parked();
    let viewport = visual.update(|window, _| window.viewport_size());
    let dialog = visual.debug_bounds("project-code-settings-dialog").unwrap();
    let body = visual.debug_bounds("tall-page-scroll").unwrap();
    let footer = visual.debug_bounds("tall-page-scroll-footer").unwrap();
    assert!(
        dialog.bottom() <= viewport.height,
        "{dialog:?} in {viewport:?}"
    );
    assert!(footer.bottom() <= dialog.bottom() && footer.origin.y >= body.bottom() - px(1.));
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: body.origin + gpui::point(px(6.), px(6.)),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-300.))),
        modifiers: Default::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    visual.run_until_parked();
    // The form moved under the footer, which stayed where it was.
    assert_eq!(
        visual.debug_bounds("tall-page-scroll-footer").unwrap(),
        footer
    );
    let form = visual.debug_bounds("tall-page-form").unwrap();
    assert!(
        form.origin.y < body.origin.y,
        "the form did not scroll: {form:?}"
    );
}
