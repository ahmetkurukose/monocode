//! Code insight in the file editor: what the language servers know, shown
//! through the editor's and the shell's extension points (docs/native/code.md §4).
//! The servers themselves are `monocode_core::code::lsp`; this module keeps
//! them told about the open documents, lays their diagnostics over the text,
//! answers the editor's completion, hover, signature and navigation requests
//! from them, and adds the Usages and Outline tabs, symbol search, the
//! language servers chip and the project's "Code insight" settings.
mod chip;
mod convert;
mod outline;
#[cfg(feature = "selftest")]
pub(crate) mod probe;
mod providers;
mod settings;
mod symbol_search;
mod sync;
#[cfg(test)]
pub(crate) mod tests;
mod usages;

use gpui::{App, AppContext, Entity, Global};
use monocode_core::code::{lsp, services::services};

/// The feature's shared state: the manager every part asks, and the entity
/// that follows the documents and the servers.
struct Insight {
    manager: lsp::Manager,
    sync: Entity<sync::Sync>,
}
impl Global for Insight {}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<Insight>() {
        init_with(services().lsp().clone(), cx);
    }
}

/// `init` over a given manager; the tests pass one with scripted servers.
pub(crate) fn init_with(manager: lsp::Manager, cx: &mut App) {
    let sync = cx.new(|cx| sync::Sync::new(manager.clone(), cx));
    cx.set_global(Insight {
        manager: manager.clone(),
        sync,
    });
    providers::register(manager, cx);
    usages::install(cx);
    outline::install(cx);
    symbol_search::register(cx);
    chip::register(cx);
    settings::register(cx);
}

fn manager(cx: &App) -> Option<lsp::Manager> {
    cx.try_global::<Insight>()
        .map(|insight| insight.manager.clone())
}

fn sync(cx: &App) -> Option<Entity<sync::Sync>> {
    cx.try_global::<Insight>()
        .map(|insight| insight.sync.clone())
}
