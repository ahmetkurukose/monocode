//! A dock tab that shows a view a feature supplies instead of a terminal
//! grid: a debug session's stack, variables and console. It sits in the
//! dock's tab list as a terminal does, so it is selected, reordered, closed
//! and moved with the dock exactly like one, and has no process behind it.
// Reached through `code_shell::dock`, whose callers are the feature modules.
#![allow(dead_code)]
use super::{GridSize, Terminal, detached};
use gpui::{AnyElement, AnyView, App, Context, FocusHandle, Role, SharedString, div, prelude::*};
use std::rc::Rc;

/// Where the dock holding a tab is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DockArea {
    Bottom,
    Top,
    Left,
    Right,
    /// The right panel's Terminal tab.
    Panel,
}

/// The room a hosted view has: which edge its dock is on, and the dock's
/// size across that edge (its height at the top or bottom, else its width).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DockPlacement {
    pub area: DockArea,
    pub extent: f32,
}
impl DockPlacement {
    /// A tall, narrow dock: columns should stack.
    pub(crate) fn narrow(&self) -> bool {
        !matches!(self.area, DockArea::Bottom | DockArea::Top) && self.extent < 560.
    }
}

/// Told where the dock is, at first and when it changes.
pub(crate) type PlacementChanged = Rc<dyn Fn(DockPlacement, &mut App)>;
/// Told once the tab is gone.
pub(crate) type TabClosed = Box<dyn FnOnce(&mut App)>;

pub(crate) struct Hosted {
    /// The feature's name for the tab, unique in its project's dock.
    pub(crate) id: SharedString,
    pub(crate) view: AnyView,
    /// What takes the keyboard when the tab is focused; the tab's own
    /// surface when the view names nothing.
    pub(crate) focus: Option<FocusHandle>,
    pub(crate) icon: &'static str,
    /// Drawn in the dock's header while the tab is selected.
    pub(crate) controls: Option<AnyView>,
    pub(crate) placement: Option<DockPlacement>,
    pub(crate) on_placement: Option<PlacementChanged>,
    pub(crate) on_close: Option<TabClosed>,
}

impl Hosted {
    pub(super) fn surface(&self, focus: &FocusHandle) -> AnyElement {
        div()
            .id("dock-view")
            .debug_selector(|| "dock-view".into())
            .role(Role::Region)
            .track_focus(focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(self.view.clone())
            .into_any_element()
    }
}

impl Terminal {
    /// A dock tab titled `title` that shows `hosted.view`, for the checkout
    /// in `cwd`.
    pub(crate) fn hosted(
        cwd: String,
        title: String,
        hosted: Hosted,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize {
            columns: 80,
            lines: 24,
        };
        let mut this = Self::assemble(cwd, size, detached(size), Vec::new(), cx);
        this.title = title;
        this.hosted = Some(hosted);
        // Every way a tab closes drops its terminal; the feature hears of
        // it here, whichever way it was.
        this._release = Some(cx.on_release(|this, cx| {
            if let Some(close) = this.hosted.as_mut().and_then(|h| h.on_close.take()) {
                close(cx);
            }
        }));
        this
    }

    pub(crate) fn hosted_view(&self) -> Option<&Hosted> {
        self.hosted.as_ref()
    }

    /// Renames a run's or a hosted view's tab.
    pub(crate) fn set_title(&mut self, title: String, cx: &mut Context<Self>) {
        if !self.is_shell() && self.title != title {
            self.title = title;
            cx.emit(super::TerminalEvent::Changed);
            cx.notify();
        }
    }

    /// What closing the tab would interrupt, for the dock's close prompt;
    /// `None` closes it without asking.
    pub(crate) fn set_busy(&mut self, busy: Option<String>, cx: &mut Context<Self>) {
        if self.hosted.is_some() {
            self.set_foreground(busy, cx);
        }
    }

    /// Tells a hosted view where its dock is, when that has changed.
    pub(crate) fn place(&mut self, placement: DockPlacement, cx: &mut Context<Self>) {
        let Some(hosted) = &mut self.hosted else {
            return;
        };
        if hosted.placement == Some(placement) {
            return;
        }
        hosted.placement = Some(placement);
        if let Some(changed) = hosted.on_placement.clone() {
            // The view may be drawing; it hears once this frame is done.
            cx.defer(move |cx| changed(placement, cx));
        }
        cx.notify();
    }
}
