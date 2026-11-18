//! What the self-test (`shell/code_selftest.rs`) reads of run in the
//! running app, and the one thing it does through the page's model; only
//! in a build with the `selftest` feature.
use super::{
    chip, header,
    settings_page::{Field, RunsPage},
    start,
};
use crate::{code_editor::DocumentInfo, popover::MenuItem};
use gpui::{App, AppContext, Window};
use monocode_core::code::run::{RunId, RunInfo};
use std::path::Path;

/// Whether a run has an open dock tab.
pub(crate) fn has_tab(id: RunId, cx: &App) -> bool {
    start::tab_of(id, cx).is_some()
}

/// A run's row in the footer chip's list as `(title, detail)`.
pub(crate) fn chip_row(info: &RunInfo, cx: &App) -> (String, Option<String>) {
    let item = chip::item(info, cx);
    (
        item.title.to_string(),
        item.detail.map(|detail| detail.to_string()),
    )
}

/// The last sentence a start showed the user.
pub(crate) fn last_notice(cx: &App) -> Option<String> {
    start::last_notice(cx)
}

/// The header's run menu for a file as `(id, label, ticked)`.
pub(crate) fn header_menu(path: &Path, root: &Path, cx: &App) -> Vec<(String, String, bool)> {
    let doc = DocumentInfo::new(path, root);
    (header::menu(&doc, None, cx).iter())
        .filter_map(|item| match item {
            MenuItem::Action(action) => Some((
                action.id.to_string(),
                action.label.to_string(),
                action.checked,
            )),
            _ => None,
        })
        .collect()
}

/// Adds a configuration through the settings page's own model, as filling
/// in its form and pressing Save does, and returns its id.
pub(crate) fn add_configuration(
    project: &str,
    name: &str,
    command: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<String> {
    let page = cx.new(|cx| RunsPage::new(project.to_owned(), None, cx));
    page.update(cx, |page, cx| {
        page.add(window, cx);
        page.set_field(Field::Name, name, cx);
        page.set_field(Field::Command, command, cx);
        page.save(window, cx);
        page.selected().map(|config| config.id.clone())
    })
}
