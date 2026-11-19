//! The whole path with the real debugpy: a script started through the
//! launcher the run feature uses, its debuggee in a terminal tab, a stop on
//! a breakpoint, a local read from the variables rows, a step, a restart
//! and the run to the end. Runs only with `MONOCODE_TEST_PYTHON` set to an
//! interpreter that has debugpy; passes at once without it.
use super::{breakpoints, launch::file_spawn, model::Control, sessions};
use crate::{
    code_editor,
    code_shell::{context::Checkout, launch},
    shell::Shell,
};
use gpui::{TestAppContext, VisualTestContext};
use monocode_core::code::{
    dap::{Breakpoint, SessionState},
    services::services,
};
use std::{path::PathBuf, time::Duration};

const PROJECT: &str = "/work/monocode";
const SCRIPT: &str = "def work(n):\n    total = n * 2\n    print('total', total)\n    return total\n\n\nvalue = work(21)\nprint('done', value)\n";

/// A real interpreter starts slowly the first time.
fn wait(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    for _ in 0..6000 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[gpui::test]
fn a_script_is_debugged_through_the_launcher_with_the_real_debugpy(cx: &mut TestAppContext) {
    let Some(python) = std::env::var_os("MONOCODE_TEST_PYTHON").map(PathBuf::from) else {
        return;
    };
    cx.executor().allow_parking();
    cx.update(|cx| {
        crate::input::bind(cx);
        crate::file_editor::bind(cx);
        crate::popover::bind(cx);
        super::init(cx);
    });
    let (window, _, _) = Shell::test_window(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();

    // A checkout of the project whose `.venv` is the fixture's, found the
    // way any project's environment is. debugpy names files by their real
    // path, so the folder is spelled that way from the start.
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().canonicalize().unwrap();
    let venv = python.parent().and_then(|bin| bin.parent()).unwrap();
    std::os::unix::fs::symlink(venv, folder.join(".venv")).unwrap();
    let script = folder.join("script.py");
    std::fs::write(&script, SCRIPT).unwrap();
    breakpoints::store().set(&script, Breakpoint::at(3));
    let checkout = Checkout {
        project: PROJECT.into(),
        folder: folder.to_string_lossy().into_owned(),
    };
    let spawn = file_spawn(&script.to_string_lossy(), &checkout);
    assert!(visual.update(|window, cx| launch::debug(spawn, checkout, window, cx)));

    let mut found = None;
    wait(visual, "the session", |cx| {
        found = cx.update(|_, cx| {
            let models = sessions::debugger(cx)?.read(cx).models();
            (models.into_iter()).find(|model| model.read(cx).root().request().cwd == folder)
        });
        found.is_some()
    });
    let model = found.unwrap();
    let stopped_on = |line: u32, cx: &mut VisualTestContext| {
        model.read_with(cx, |model, _| {
            let state = model.root().state();
            state
                .stop()
                .is_some_and(|stop| stop.line == Some(line) && stop.path.as_ref() == Some(&script))
        })
    };
    wait(visual, "the breakpoint", |cx| stopped_on(3, cx));

    // The program runs in a terminal tab of the dock, not in the adapter.
    let run = model
        .read_with(visual, |model, _| model.terminal())
        .unwrap();
    assert!(run.read_with(visual, |run, _| run.state().running()));
    // The stop is marked in the editor and its locals are listed.
    let marked = visual.update(|_, cx| code_editor::hub(cx).read(cx).line_decorations(&script));
    assert_eq!(marked.iter().map(|line| line.row).collect::<Vec<_>>(), [2]);
    let local = |name: &str, cx: &mut VisualTestContext| {
        model.read_with(cx, |model, _| {
            (model.var_rows.iter())
                .find(|row| row.name.as_ref() == name)
                .map(|row| row.value.to_string())
        })
    };
    wait(visual, "the locals", |cx| local("total", cx).is_some());
    assert_eq!(local("total", visual).as_deref(), Some("42"));
    assert_eq!(local("n", visual).as_deref(), Some("21"));

    // A step runs the print and stops on the next line.
    assert!(visual.update(|_, cx| sessions::act(Control::StepOver, cx)));
    wait(visual, "the step", |cx| stopped_on(4, cx));
    wait(visual, "the program's output", |cx| {
        run.read_with(cx, |run, _| run.tail().contains("total 42"))
    });

    // Restart: a new session in the same tab, stopped at the breakpoint again.
    let first = model.read_with(visual, |model, _| model.root().id());
    assert!(visual.update(|_, cx| sessions::act(Control::Restart, cx)));
    wait(visual, "the restart", |cx| {
        model.read_with(cx, |model, _| model.root().id() != first) && stopped_on(3, cx)
    });

    // Continue runs it to the end, and nothing stays marked.
    assert!(visual.update(|_, cx| sessions::act(Control::Continue, cx)));
    wait(visual, "the end", |cx| {
        model.read_with(cx, |model, _| model.root().state().is_terminated())
    });
    let state = model.read_with(visual, |model, _| model.root().state());
    assert_eq!(state, SessionState::Terminated { exit_code: Some(0) });
    wait(visual, "the marks to go", |cx| {
        cx.update(|_, cx| {
            code_editor::hub(cx)
                .read(cx)
                .line_decorations(&script)
                .is_empty()
        })
    });

    let id = model.read_with(visual, |model, _| model.root().id());
    services().dap().remove(id);
    breakpoints::store().clear(Some(&script));
}
