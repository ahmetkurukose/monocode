//! What the self-test (`shell/code_selftest.rs`) reads of code insight in
//! the running app; only in a build with the `selftest` feature.
use super::{outline, usages};
use gpui::App;

/// The Usages tab's symbol, count and places (`file:line: text`), when shown.
pub(crate) fn usages(cx: &App) -> Option<(String, usize, Vec<String>)> {
    Some(usages::shown(cx)?.read(cx).listed())
}

/// The Outline tab's names, each marked when it is the caret's symbol.
pub(crate) fn outline(cx: &App) -> Option<Vec<(String, bool)>> {
    Some(outline::shown(cx)?.read(cx).listed(cx))
}
