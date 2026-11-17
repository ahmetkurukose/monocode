//! A short status a feature attaches to an agent session's row in the
//! project rail, in place of the row's age: "paused at views.py:43" while
//! the agent is debugging. One status per session; the last one set wins.
use gpui::{App, Global, SharedString};
use std::collections::HashMap;

#[derive(Default)]
struct Statuses(HashMap<i64, SharedString>);
impl Global for Statuses {}

/// Shows `text` on the session's rail row until it is cleared.
pub(crate) fn set_status(session: i64, text: impl Into<SharedString>, cx: &mut App) {
    let text = text.into();
    let statuses = &mut cx.default_global::<Statuses>().0;
    if statuses.get(&session) != Some(&text) {
        statuses.insert(session, text);
        super::refresh(cx);
    }
}

pub(crate) fn clear_status(session: i64, cx: &mut App) {
    if (cx.default_global::<Statuses>().0.remove(&session)).is_some() {
        super::refresh(cx);
    }
}

pub(crate) fn status(session: i64, cx: &App) -> Option<SharedString> {
    cx.try_global::<Statuses>()?.0.get(&session).cloned()
}

/// The session's title as its rail row shows it, to name the agent that
/// drives something. Reads the shell, so not from inside a closure the shell
/// calls while it draws.
pub(crate) fn session_label(session: i64, cx: &App) -> Option<SharedString> {
    let shell = cx.try_global::<super::Host>()?.shell.upgrade()?;
    shell.read(cx).code_session_label(session, cx)
}
