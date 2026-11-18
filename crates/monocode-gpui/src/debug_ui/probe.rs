//! What the self-test (`shell/code_selftest.rs`) reads of the debugger in
//! the running app: each session as its tab shows it; only in a build with
//! the `selftest` feature.
use super::{
    breakpoints,
    model::{Model, StackRow, VarKind},
    sessions,
};
use gpui::App;
use monocode_core::code::dap::{Actor, SessionState};
use serde_json::{Value, json};
use std::path::Path;

/// The lines with a breakpoint in a file, from 1.
pub(crate) fn breakpoint_lines(path: &Path) -> Vec<u32> {
    let mut lines: Vec<u32> = (breakpoints::store().in_file(path).iter())
        .map(|breakpoint| breakpoint.line)
        .collect();
    lines.sort();
    lines
}

/// Every session that has a tab, oldest first.
pub(crate) fn sessions(cx: &App) -> Vec<Value> {
    let Some(debugger) = sessions::debugger(cx) else {
        return Vec::new();
    };
    (debugger.read(cx).models().iter())
        .map(|model| session(model.read(cx), cx))
        .collect()
}

fn session(model: &Model, cx: &App) -> Value {
    let root = model.root();
    let state = root.state();
    let stack: Vec<Value> = (model.stack_rows.iter())
        .filter_map(|row| match row {
            StackRow::Frame {
                frame,
                selected,
                dim,
                ..
            } => Some(json!({
                "name": frame.name, "line": frame.line, "selected": selected, "dim": dim,
                "file": frame.path.as_ref().and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned()),
            })),
            _ => None,
        })
        .collect();
    let variables: Vec<Value> = (model.var_rows.iter())
        .filter(|row| row.kind == VarKind::Variable)
        .map(|row| json!([row.name, row.value, row.depth]))
        .collect();
    let scopes: Vec<String> = (model.var_rows.iter())
        .filter(|row| row.kind == VarKind::Scope)
        .map(|row| row.name.to_string())
        .collect();
    let console: Vec<Value> = (model.console.iter())
        .map(|line| json!([format!("{:?}", line.tone), line.text]))
        .collect();
    let terminal = model.terminal().map(|run| {
        let run = run.read(cx);
        json!({ "open": run.is_open(), "running": run.state().running(), "tail": run.tail() })
    });
    json!({
        "id": root.id().to_string(),
        "title": model.title(),
        "state": Model::state_word(&state),
        "stop": state.stop().map(|stop| json!({
            "reason": stop.reason, "line": stop.line, "exception": stop.exception,
            "path": stop.path.as_ref().map(|path| path.to_string_lossy().into_owned()),
        })),
        "exit_code": match state {
            SessionState::Terminated { exit_code } => json!(exit_code),
            _ => Value::Null,
        },
        "failure": root.failure(),
        "cwd": root.request().cwd,
        "started_by_agent": matches!(root.origin(), Actor::Agent(_)),
        "user_drives": model.user_drives(),
        "driven_by": model.driven_by().map(|title| title.to_string()),
        "tab_open": model.tab.as_ref().is_some_and(|tab| tab.is_open()),
        "notice": model.notice.as_ref().map(|notice| notice.to_string()),
        "stack": stack,
        "scopes": scopes,
        "variables": variables,
        "console": console,
        "terminal": terminal,
    })
}
