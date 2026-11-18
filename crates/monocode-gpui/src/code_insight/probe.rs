//! What the self-test (`shell/code_selftest.rs`) reads of code insight in
//! the running app; only in a build with the `selftest` feature.
use super::settings;
use super::{outline, usages};
use crate::code_shell::settings::PageHost;
use gpui::{App, Window};

/// The Usages tab's symbol, count and places (`file:line: text`), when shown.
pub(crate) fn usages(cx: &App) -> Option<(String, usize, Vec<String>)> {
    Some(usages::shown(cx)?.read(cx).listed())
}

/// The Outline tab's names, each marked when it is the caret's symbol.
pub(crate) fn outline(cx: &App) -> Option<Vec<(String, bool)>> {
    Some(outline::shown(cx)?.read(cx).listed())
}

/// Sets the idle time of unused servers, as saving the page's form does.
pub(crate) fn set_idle_minutes(
    project: &str,
    minutes: u64,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let mut chosen = settings::load(project, cx);
    chosen.idle_minutes = Some(minutes);
    let Ok(value) = serde_json::to_value(&chosen) else {
        return false;
    };
    PageHost {
        project: project.to_owned(),
        page: settings::PAGE,
    }
    .save(Some(value), window, cx);
    let Some(sync) = super::sync(cx) else {
        return false;
    };
    sync.update(cx, |sync, cx| sync.settings_changed(project, cx));
    true
}
