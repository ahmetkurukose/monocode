//! Run in the interface (docs/native/code.md §3 and §6): the file header's
//! Run and Debug buttons, the gutter's run markers, runs in dock tabs kept
//! in step with the core's registry, the footer's chip for running work,
//! the project's run configurations page, the `Run:` commands of quick
//! open and the run part of the footer's Actions button. Everything is
//! attached through the shell's and the editor's extension points; `start`
//! is the one way a run begins.
mod chip;
mod commands;
pub(crate) mod footer;
mod gutter;
mod header;
mod output;
#[cfg(feature = "selftest")]
pub(crate) mod probe;
mod settings_page;
mod start;
mod targets;
#[cfg(test)]
mod tests;

use crate::code_editor;
use gpui::App;
#[cfg(test)]
use gpui::{Entity, Global};
use gutter::Marks;
use std::rc::Rc;

// What other features start runs through. The agents' tools are to call
// `start_for_agent`; nothing outside this module calls any of it yet.
#[allow(unused_imports)]
pub(crate) use start::{Outcome, debug, rerun_last, start, start_for_agent, stop};

/// What was installed on the editors, for the tests to ask directly.
#[cfg(test)]
struct Installed {
    marks: Entity<Marks>,
    header: Rc<header::RunHeader>,
}
#[cfg(test)]
impl Global for Installed {}

#[cfg(test)]
fn test_marks(cx: &App) -> Entity<Marks> {
    cx.global::<Installed>().marks.clone()
}

#[cfg(test)]
fn test_header(cx: &App) -> Rc<header::RunHeader> {
    cx.global::<Installed>().header.clone()
}

pub(crate) fn init(cx: &mut App) {
    targets::init(cx);
    start::init(cx);
    header::init(cx);
    chip::init(cx);
    commands::init(cx);
    let hub = code_editor::hub(cx);
    let marks = gutter::marks(hub, cx);
    settings_page::init(marks.clone(), cx);
    let header = Rc::new(header::RunHeader::default());
    #[cfg(test)]
    cx.set_global(Installed {
        marks: marks.clone(),
        header: header.clone(),
    });
    let markers = Rc::new(gutter::RunMarkers(marks));
    code_editor::register(cx, move |doc, providers, _| {
        providers.header.push(header.clone());
        if doc.language == Some("python") {
            providers.run_markers = Some(markers.clone());
        }
    });
}
