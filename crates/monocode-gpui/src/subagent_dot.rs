//! A subagent's state glyph, the same on the rail, the switcher and the
//! status bar: the working spinner, a check, a cross, the amber attention
//! mark, or a dashed ring when nothing has been heard for long.
use crate::session_mark::{self, SessionMark};
use crate::theme::*;
use gpui::{AnyElement, App, AppContext, Entity, Role, div, prelude::*, px, rgb};
use monocode_core::subagents::State;

pub(crate) const SIDE: f32 = 12.;

/// A spinner in its own small view, so its clock repaints only the mark.
pub(crate) fn working_mark(cx: &mut App) -> Entity<SessionMark> {
    let mark = cx.new(|_| SessionMark::tab(String::new()));
    mark.update(cx, |mark, cx| {
        mark.set_size(SIDE, cx);
        mark.set("", session_mark::State::Working, cx);
    });
    mark
}

/// `mark` draws the working state; without one a working dot is static.
pub(crate) fn dot(state: State, mark: Option<&Entity<SessionMark>>) -> AnyElement {
    let frame = div()
        .id(state.label())
        .size(px(SIDE))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .role(Role::Image)
        .aria_label(state.label());
    match state {
        State::Working => match mark {
            Some(mark) => frame.child(mark.clone()),
            None => frame.child(session_mark::spinner_at(0.3)),
        },
        State::Waiting => frame.child(icon("alert-circle", SIDE).text_color(rgb(0xfbbf24))),
        State::Failed => frame.child(icon("circle-x", SIDE).text_color(rgb(0xf87171))),
        State::Interrupted => frame.child(icon("circle-dot", SIDE).text_color(content(0.45))),
        State::Done => frame.child(icon("check-circle-2", SIDE).text_color(rgb(0x34d399))),
        State::Unverifiable => frame.child(icon("circle-dashed", SIDE).text_color(content(0.4))),
    }
    .into_any_element()
}
