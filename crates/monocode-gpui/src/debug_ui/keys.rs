//! The debugger's keys and its quick open commands. The step keys are
//! bound everywhere but only act while a session they apply to exists;
//! otherwise the key goes on to whatever else wants it (a terminal's
//! program, for one). F12, Shift+F12 and F1 are the editor's and stay so.
use super::{breakpoints, launch, model::Control, sessions};
use crate::{
    code_editor,
    code_shell::{
        commands::{self, Command, Scope},
        context,
    },
    editor_syntax,
};
use gpui::{App, KeyBinding, actions};
use std::{path::Path, rc::Rc};

actions!(
    debug,
    [
        Continue,
        StepOver,
        StepInto,
        StepOut,
        Stop,
        Restart,
        ToggleBreakpoint
    ]
);

pub(super) fn init(cx: &mut App) {
    let restart = if cfg!(target_os = "macos") {
        "cmd-shift-f5"
    } else {
        "ctrl-shift-f5"
    };
    cx.bind_keys([
        KeyBinding::new("f5", Continue, None),
        KeyBinding::new("f10", StepOver, None),
        KeyBinding::new("f11", StepInto, None),
        KeyBinding::new("shift-f11", StepOut, None),
        KeyBinding::new("shift-f5", Stop, None),
        KeyBinding::new(restart, Restart, None),
        KeyBinding::new("f9", ToggleBreakpoint, Some("CodeEditor")),
    ]);
    // F5 continues a paused program and pauses a running one.
    cx.on_action(|_: &Continue, cx| press(&[Control::Continue, Control::Pause], cx));
    cx.on_action(|_: &StepOver, cx| press(&[Control::StepOver], cx));
    cx.on_action(|_: &StepInto, cx| press(&[Control::StepInto], cx));
    cx.on_action(|_: &StepOut, cx| press(&[Control::StepOut], cx));
    cx.on_action(|_: &Stop, cx| press(&[Control::Stop], cx));
    cx.on_action(|_: &Restart, cx| press(&[Control::Restart], cx));
    cx.on_action(|_: &ToggleBreakpoint, cx| {
        if !toggle_at_caret(cx) {
            cx.propagate();
        }
    });
    commands::register_commands("debug", commands, cx);
}

fn press(controls: &[Control], cx: &mut App) {
    if !controls.iter().any(|control| sessions::act(*control, cx)) {
        cx.propagate();
    }
}

fn python(path: &str) -> bool {
    editor_syntax::language_id(path) == Some("python")
}

/// Toggles a breakpoint on the caret's line of the focused Python file.
pub(super) fn toggle_at_caret(cx: &mut App) -> bool {
    let Some(file) = context::focused_file(cx).filter(|file| python(&file.path)) else {
        return false;
    };
    let Some(editor) = file.editor.upgrade() else {
        return false;
    };
    let row = {
        let input = editor.read(cx).input.read(cx);
        code_editor::row_of(&input.content, input.caret())
    };
    breakpoints::toggle_row(Path::new(&file.path), row, cx);
    true
}

fn commands(_: &Scope, cx: &App) -> Vec<Command> {
    let mut list = Vec::new();
    let command = |title: String, subtitle: &str, run: commands::Action| Command {
        title: title.into(),
        subtitle: subtitle.to_owned().into(),
        run,
    };
    if let Some(file) = context::focused_file(cx).filter(|file| python(&file.path)) {
        let name = Path::new(&file.path).file_name().map_or_else(
            || file.path.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let (path, checkout) = (file.path.clone(), file.checkout.clone());
        list.push(command(
            format!("Debug: {name}"),
            &file.path,
            Rc::new(move |window, cx| {
                launch::start(
                    launch::file_spawn(&path, &checkout),
                    checkout.clone(),
                    window,
                    cx,
                )
            }),
        ));
        list.push(command(
            "Debug: Toggle Breakpoint".into(),
            "F9",
            Rc::new(|_, cx| {
                toggle_at_caret(cx);
            }),
        ));
    }
    if let Some(model) = sessions::current(cx) {
        let controls = [
            (Control::Continue, "Debug: Continue", "F5"),
            (Control::Pause, "Debug: Pause", "F5"),
            (Control::StepOver, "Debug: Step Over", "F10"),
            (Control::StepInto, "Debug: Step Into", "F11"),
            (Control::StepOut, "Debug: Step Out", "Shift+F11"),
            (
                Control::Restart,
                "Debug: Restart",
                "Restart the program under the debugger",
            ),
            (Control::Stop, "Debug: Stop", "Shift+F5"),
        ];
        for (control, title, key) in controls {
            if model.read(cx).can(control) {
                list.push(command(
                    title.into(),
                    key,
                    Rc::new(move |_, cx| {
                        sessions::act(control, cx);
                    }),
                ));
            }
        }
        if model.read(cx).driven_by().is_some() {
            let model = model.downgrade();
            list.push(command(
                "Debug: Take Over".into(),
                "Steer the session an agent is debugging",
                Rc::new(move |_, cx| {
                    let _ = model.update(cx, |model, cx| model.take_over(cx));
                }),
            ));
        }
    }
    if !breakpoints::store().files().is_empty() {
        list.push(command(
            "Debug: Remove All Breakpoints".into(),
            "In every project",
            Rc::new(|_, _| breakpoints::store().clear(None)),
        ));
    }
    list
}
