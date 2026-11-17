//! Debug in the interface: breakpoints in the gutter and the debug dock tab
//! over the sessions agents and the user share (docs/native/code.md §5).
//! The debugger itself is `monocode_core::code::dap`; everything here reads
//! its shared instances (`services().dap()`, `services().breakpoints()`)
//! and draws through the shell's and the editor's extension points.
use gpui::{App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

mod breakpoints;
mod columns;
mod hover;
mod keys;
mod launch;
mod model;
mod sessions;
mod tab;
mod values;

#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod tests;

pub(crate) fn init(cx: &mut App) {
    breakpoints::init(cx);
    sessions::init(cx);
    launch::init(cx);
    // After code insight's registration, whose hover this one wraps.
    hover::init(cx);
    keys::init(cx);
}

/// A button's tooltip, as the rest of the chrome draws them.
struct Tip(SharedString);
impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(crate::theme::pane_fill())
            .border_1()
            .border_color(crate::theme::content(0.15))
            .text_size(px(12.))
            .text_color(crate::theme::content(1.))
            .child(self.0.clone())
    }
}
