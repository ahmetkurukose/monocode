// The dock's tab strip (ProjectTerminalDock.tsx over SurfaceTabs.tsx) and the
// footer's terminal controls (UsageFooter.tsx's Terminal button and
// RunningTerminalChip).
use super::{DragSource, MenuTarget, PaneDrag, Shell, TabShift};
use crate::{
    popover::{Anchor, MenuAction, MenuItem, MenuStyle},
    terminal::Terminal,
    theme::*,
    workspace::{DockSide, TerminalDock, project_name},
};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, EntityId, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Point, Role, SharedString, StyleRefinement, Task, Window, div, point,
    prelude::*, px, rgb,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// useAnimatedReorder's drag of one dock tab along the strip.
pub(super) struct DockTabDrag {
    /// Where the press landed; pulling far enough off the strip turns the
    /// drag into a pane drag that can drop the terminal in a split or on
    /// the title strip.
    start: Point<Pixels>,
    from: usize,
    to: usize,
    origin: Pixels,
    offset: Pixels,
    active: bool,
    bounds: Vec<Bounds<Pixels>>,
}

/// How far off the dock's strip a tab must be pulled before it can leave
/// the dock.
const DOCK_PULL: Pixels = px(24.);

/// A terminal whose foreground process is not the shell (listRunningTerminals).
struct Running {
    ix: usize,
    process: String,
    label: String,
}

/// TerminalLiveMark: three bars that light in turn over 3.2s.
pub(super) struct LiveMark {
    epoch: Instant,
    _tick: Task<()>,
}
const LIVE_CYCLE: f32 = 3.2;
const LIVE_COLOR: u32 = 0xe39b4a;
impl LiveMark {
    fn new(cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs_f32(LIVE_CYCLE / 4.))
                    .await;
                // Each step redraws the whole window; hold the mark while
                // nobody is looking at it or motion is reduced.
                let still = cx.update(|cx| cx.reduce_motion() || !crate::pulse::app_active(cx));
                if !still && this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            epoch: Instant::now(),
            _tick: tick,
        }
    }
}
impl Render for LiveMark {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = (self.epoch.elapsed().as_secs_f32() / LIVE_CYCLE).fract();
        let reduced = cx.reduce_motion();
        div()
            .flex()
            .items_end()
            .gap(px(2.))
            .h(px(10.))
            .children((1..=3).map(|bar| {
                let opacity = if reduced {
                    0.7
                } else if phase >= bar as f32 * 0.25 {
                    0.85
                } else {
                    0.4
                };
                div()
                    .w(px(4.))
                    .h(px(8.))
                    .bg(rgb(LIVE_COLOR))
                    .opacity(opacity)
            }))
    }
}

/// runningTerminalChipLabel: `vite`, or `vite · jest`, or `vite ×2`.
fn chip_label<'a>(processes: impl IntoIterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for process in processes {
        match counts.iter_mut().find(|(name, _)| *name == process) {
            Some((_, n)) => *n += 1,
            None => counts.push((process, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(name, n)| {
            if n > 1 {
                format!("{name} ×{n}")
            } else {
                name.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Where a tab is drawn now, easing toward `target` (the title strip's
/// `eased_shift`).
fn eased_shift(
    shifts: &mut HashMap<EntityId, TabShift>,
    id: EntityId,
    target: f32,
    follow: bool,
    reduced: bool,
) -> (f32, bool) {
    let now = Instant::now();
    if follow || reduced {
        shifts.remove(&id);
        return (target, false);
    }
    let current = shifts.get(&id).map(|s| s.at(now));
    if current.is_none_or(|_| shifts[&id].to != target) {
        let from = current.map_or(0., |(value, _)| value);
        if from == target {
            shifts.remove(&id);
            return (target, false);
        }
        shifts.insert(
            id,
            TabShift {
                from,
                to: target,
                start: now,
            },
        );
    }
    let (value, done) = shifts[&id].at(now);
    if done && target == 0. {
        shifts.remove(&id);
    }
    (value, !done)
}

impl Shell {
    fn live_terminals(&self) -> Vec<Entity<Terminal>> {
        self.terminals
            .get(&self.workspace.project)
            .cloned()
            .unwrap_or_default()
    }
    fn running_terminals(&self, cx: &App) -> Vec<Running> {
        self.live_terminals()
            .iter()
            .enumerate()
            .filter_map(|(ix, terminal)| {
                let t = terminal.read(cx);
                let process = t.foreground.as_deref()?.trim();
                (!process.is_empty()).then(|| Running {
                    ix,
                    process: process.to_owned(),
                    label: project_name(&t.cwd),
                })
            })
            .collect()
    }
    fn dock_open(&self) -> bool {
        self.workspace
            .docks
            .get(&self.workspace.project)
            .is_some_and(|d| d.open && !d.terminals.is_empty())
    }
    /// Terminals open as their own title bar tabs; the dock stays for
    /// actions, the footer button and remote projects.
    pub(super) fn terminal_in_title(&self) -> bool {
        #[cfg(test)]
        if self.terminals_in_dock {
            return false;
        }
        !self.workspace.project.starts_with("remote://")
    }
    /// A shell that ended (`exit`, `logout`, a crash) takes its tab with it, in
    /// the dock or on the title strip, instead of leaving a dead prompt.
    pub(super) fn close_exited_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for id in std::mem::take(&mut self.exited_terminals) {
            if let Some(pane) = self
                .terminal_pane_views
                .iter()
                .find(|(_, t)| t.entity_id() == id)
                .map(|(pane, _)| *pane)
            {
                self.close_pane(pane, window, cx);
                continue;
            }
            let Some((project, ix)) = self.terminals.iter().find_map(|(project, shells)| {
                let ix = shells.iter().position(|t| t.entity_id() == id)?;
                Some((project.clone(), ix))
            }) else {
                continue;
            };
            if project == self.workspace.project {
                self.close_terminal(ix, window, cx);
            } else {
                // Another project's dock: only its lists change; it is not on screen.
                if let Some(live) = self.terminals.get_mut(&project) {
                    live.remove(ix);
                }
                if let Some(dock) = self.workspace.docks.get_mut(&project)
                    && !dock.close(ix)
                {
                    dock.open = false;
                }
                self.persist(cx);
                cx.notify();
            }
        }
    }
    /// onToggleRunningTerminal: hide the open dock, else show it on `ix`.
    fn toggle_running_terminal(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dock_open()
            && let Some(dock) = self.workspace.docks.get_mut(&self.workspace.project)
        {
            dock.active = ix.min(dock.terminals.len().saturating_sub(1));
            self.dock_tab_scroll.scroll_to_item(dock.active);
        }
        self.toggle_terminal(window, cx);
    }
    pub(super) fn running_terminals_menu(&self, cx: &App) -> Vec<MenuItem> {
        self.running_terminals(cx)
            .into_iter()
            .map(|r| {
                MenuAction::new(SharedString::from(r.ix.to_string()), r.process)
                    .value(r.label)
                    .into()
            })
            .collect()
    }
    pub(super) fn run_running_terminals_menu(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Ok(ix) = action.parse() {
            self.toggle_running_terminal(ix, window, cx);
        }
    }
    pub(super) fn terminal_tab_menu(&self, ix: usize) -> Vec<MenuItem> {
        let count = self
            .workspace
            .docks
            .get(&self.workspace.project)
            .map_or(0, |d| d.terminals.len());
        vec![
            MenuAction::new("close", "Close").into(),
            MenuAction::new("close-others", "Close Others")
                .disabled(count < 2 || ix >= count)
                .into(),
        ]
    }
    pub(super) fn run_terminal_tab_menu(
        &mut self,
        ix: usize,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "close" => self.close_terminal(ix, window, cx),
            "close-others" => self.close_other_terminals(ix, window, cx),
            _ => {}
        }
    }
    /// confirmCloseTerminals: the terminals still running something ask first.
    fn close_other_terminals(&mut self, keep: usize, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.workspace.project.clone();
        let live = self.live_terminals();
        let Some(kept) = live.get(keep).cloned() else {
            return;
        };
        let running: Vec<(String, String)> = live
            .iter()
            .enumerate()
            .filter(|(ix, _)| *ix != keep)
            .filter_map(|(_, terminal)| {
                let t = terminal.read(cx);
                let process = t.foreground.as_deref()?.trim().to_owned();
                (!process.is_empty()).then(|| (t.title.clone(), process))
            })
            .collect();
        let message = match running.as_slice() {
            [] => None,
            [(label, process)] => Some(format!(
                "\"{process}\" is still running in {label}. Close this terminal anyway?"
            )),
            many => Some(format!(
                "These terminals are still running:\n{}\n\nClose them anyway?",
                many.iter()
                    .map(|(label, process)| format!("• {label} ({process})"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
        };
        let Some(message) = message else {
            self.keep_terminal(&project, &kept, window, cx);
            return;
        };
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &message,
            None,
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if matches!(answer.await, Ok(0)) {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.keep_terminal(&project, &kept, window, cx)
                });
            }
        })
        .detach();
    }
    fn keep_terminal(
        &mut self,
        project: &str,
        kept: &Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .terminals
            .get(project)
            .and_then(|t| t.iter().position(|e| e == kept))
        else {
            return;
        };
        let had_focus = self.dock_focused(window, cx).is_some();
        if let Some(live) = self.terminals.get_mut(project) {
            live.retain(|e| e == kept);
        }
        if let Some(dock) = self.workspace.docks.get_mut(project) {
            dock.keep_only(ix);
        }
        self.dock_shifts.clear();
        if had_focus {
            self.focus_terminal(window, cx);
        }
        self.persist(cx);
        cx.notify();
    }
    /// SurfaceTabs selects a tab as it is pressed, then lets it be dragged.
    fn start_dock_drag(
        &mut self,
        ix: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_skip_click = false;
        self.select_terminal(ix, window, cx);
        let count = self.live_terminals().len();
        if count == 0 {
            return;
        }
        let bounds: Vec<_> = (0..count)
            .filter_map(|ix| self.dock_tab_scroll.bounds_for_item(ix))
            .collect();
        if bounds.len() != count {
            return;
        }
        self.dock_drag = Some(DockTabDrag {
            start: event.position,
            from: ix,
            to: ix,
            origin: event.position.x,
            offset: px(0.),
            active: false,
            bounds,
        });
    }
    pub(super) fn move_dock_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // A run or a feature's view reorders but never leaves the dock.
        let stays = (self.dock_drag.as_ref()).is_some_and(|d| self.dock_tab_stays(d.from, cx));
        let Some(drag) = self.dock_drag.as_mut() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.dock_drag = None;
            cx.notify();
            return;
        }
        if (event.position.y - drag.start.y).abs() > DOCK_PULL && !stays {
            let (source, start) = (DragSource::DockTerminal(drag.from), drag.start);
            self.dock_drag = None;
            self.dock_skip_click = true;
            self.strips.drag = None;
            self.pane_drag = Some(PaneDrag {
                source,
                start,
                position: event.position,
                active: false,
                over: None,
                group: None,
            });
            cx.notify();
            return;
        }
        let delta = event.position.x - drag.origin;
        if delta.abs() < px(4.) && !drag.active {
            return;
        }
        drag.active = true;
        self.dock_skip_click = true;
        let start = drag.bounds[drag.from].left();
        drag.offset = delta.clamp(
            drag.bounds[0].left() - start,
            drag.bounds.last().unwrap().left() - start,
        );
        let at = start + drag.offset;
        drag.to = drag
            .bounds
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (a.left() - at)
                    .abs()
                    .partial_cmp(&(b.left() - at).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map_or(drag.from, |(ix, _)| ix);
        cx.notify();
    }
    pub(super) fn finish_dock_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.dock_drag.take() else {
            return;
        };
        if !drag.active || drag.from == drag.to {
            cx.notify();
            return;
        }
        let project = self.workspace.project.clone();
        let now = Instant::now();
        let live = self.live_terminals();
        // Each tab glides from where it is drawn into its new slot.
        let drawn: Vec<(EntityId, f32)> = live
            .iter()
            .enumerate()
            .map(|(ix, t)| {
                let id = t.entity_id();
                let shift = self.dock_shifts.get(&id).map_or(0., |s| s.at(now).0);
                let shift = if ix == drag.from {
                    drag.offset.as_f32()
                } else {
                    shift
                };
                (
                    id,
                    drag.bounds.get(ix).map_or(0., |b| b.left().as_f32()) + shift,
                )
            })
            .collect();
        if let Some(live) = self.terminals.get_mut(&project) {
            let moved = live.remove(drag.from);
            live.insert(drag.to, moved);
        }
        if let Some(dock) = self.workspace.docks.get_mut(&project) {
            dock.reorder(drag.from, drag.to);
        }
        self.dock_shifts.clear();
        if !cx.reduce_motion() {
            for (ix, terminal) in self.live_terminals().iter().enumerate() {
                let id = terminal.entity_id();
                let (Some((_, left)), Some(slot)) =
                    (drawn.iter().find(|(t, _)| *t == id), drag.bounds.get(ix))
                else {
                    continue;
                };
                let from = left - slot.left().as_f32();
                if from.abs() > 0.5 {
                    self.dock_shifts.insert(
                        id,
                        TabShift {
                            from,
                            to: 0.,
                            start: now,
                        },
                    );
                }
            }
        }
        self.persist(cx);
        cx.notify();
    }
    fn dock_tab_shift(&self, ix: usize) -> Pixels {
        let Some(d) = self.dock_drag.as_ref().filter(|d| d.active) else {
            return px(0.);
        };
        if ix == d.from {
            return d.offset;
        }
        if d.from < d.to && ix > d.from && ix <= d.to {
            return d.bounds[ix - 1].left() - d.bounds[ix].left();
        }
        if d.from > d.to && ix >= d.to && ix < d.from {
            return d.bounds[ix + 1].left() - d.bounds[ix].left();
        }
        px(0.)
    }
    /// SurfaceTabs for the dock's terminals, then New, Move and Hide.
    /// `in_panel` drops the controls that only make sense for the dock.
    pub(super) fn dock_header(
        &mut self,
        dock: &TerminalDock,
        in_panel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let live = self.live_terminals();
        let hide = match dock.side {
            DockSide::Bottom => "chevron-down",
            DockSide::Top => "chevron-up",
            DockSide::Left => "chevron-left",
            DockSide::Right => "chevron-right",
        };
        let side = match dock.side {
            DockSide::Bottom => "panel-bottom",
            DockSide::Top => "panel-top",
            DockSide::Left => "panel-left",
            DockSide::Right => "panel-right",
        };
        let reduced = cx.reduce_motion();
        let dragged = self.dock_drag.as_ref().filter(|d| d.active).map(|d| d.from);
        let mut moving = false;
        let shifts: Vec<f32> = live
            .iter()
            .enumerate()
            .map(|(ix, terminal)| {
                let target = self.dock_tab_shift(ix).as_f32();
                let (value, active) = eased_shift(
                    &mut self.dock_shifts,
                    terminal.entity_id(),
                    target,
                    dragged == Some(ix),
                    reduced,
                );
                moving |= active;
                value
            })
            .collect();
        let animate = self.workspace.preferences.tab_animations && !reduced && dragged.is_none();
        let keyed = live
            .iter()
            .map(|t| (t.entity_id(), SharedString::from(t.read(cx).title.clone())))
            .collect();
        let (slots, opening) = self
            .dock_tab_motion
            .sync(keyed, animate, std::time::Instant::now());
        if moving || opening {
            window.request_animation_frame();
        }
        let move_anchor = self.move_terminal_anchor.clone();
        let controls = self.dock_tab_controls(dock, cx);
        div()
            .h(px(36.))
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(crate::right_panel::stroke())
            .child(
                div()
                    .id("terminal-tabs")
                    .role(Role::TabList)
                    .aria_label("Terminals")
                    .track_scroll(&self.dock_tab_scroll)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(6.))
                    .pr(px(10.))
                    // Every tab fits, sharing the strip however many there are.
                    .overflow_hidden()
                    .children({
                        let mut tabs: Vec<Option<AnyElement>> = live
                            .iter()
                            .enumerate()
                            .map(|(ix, terminal)| {
                                let t = terminal.read(cx);
                                let label: SharedString = t.title.clone().into();
                                // A shell's tile, a run's status dot or a view's icon.
                                let tile = t.tile(cx);
                                let description =
                                    SharedString::from(format!("{} — {}", t.title, t.cwd));
                                let active = ix == dock.active;
                                let held = dragged == Some(ix);
                                let hovered = self.dock_hover == Some(ix) && dragged.is_none();
                                div()
                                    .id(("terminal-tab", ix))
                                    .debug_selector(move || format!("terminal-tab-{ix}"))
                                    .relative()
                                    .left(px(shifts[ix]))
                                    .when(held, |d| d.cursor_grabbing())
                                    .h_full()
                                    .w(px(224.))
                                    .min_w_0()
                                    .flex_shrink(1.)
                                    .flex()
                                    .items_center()
                                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                                        let next = if *hovering {
                                            Some(ix)
                                        } else if this.dock_hover == Some(ix) {
                                            None
                                        } else {
                                            this.dock_hover
                                        };
                                        if next != this.dock_hover {
                                            this.dock_hover = next;
                                            cx.notify();
                                        }
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                this.start_dock_drag(ix, event, window, cx)
                                            },
                                        ),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Middle,
                                        cx.listener(move |this, _, window, cx| {
                                            this.close_terminal(ix, window, cx)
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                cx.stop_propagation();
                                                this.select_terminal(ix, window, cx);
                                                this.open_menu(
                                                    MenuTarget::TerminalTab(ix),
                                                    Anchor::Point(event.position),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        ),
                                    )
                                    .child(
                                        div()
                                            .id(("terminal-tab-button", ix))
                                            .role(Role::Tab)
                                            .aria_label(label.clone())
                                            .aria_description(description)
                                            .aria_selected(active)
                                            .h(px(TAB_PLATE_HEIGHT))
                                            .min_w_0()
                                            .flex_1()
                                            .flex()
                                            .items_center()
                                            .gap(px(crate::tab_tile::TAB_TILE_GAP))
                                            .pl(px(crate::tab_tile::tab_tile_inset()))
                                            .pr(px(28.))
                                            .rounded_md()
                                            .text_size(px(13.))
                                            .map(|d| {
                                                if active {
                                                    d.bg(crate::right_panel::selection())
                                                        .text_color(content(1.))
                                                } else if hovered {
                                                    d.bg(content(0.05)).text_color(content(1.))
                                                } else {
                                                    d.text_color(content(0.5))
                                                }
                                            })
                                            .child(tile)
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(label.clone()),
                                            )
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                if std::mem::take(&mut this.dock_skip_click) {
                                                    return;
                                                }
                                                this.focus_terminal(window, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(("terminal-tab-close", ix))
                                            .role(Role::Button)
                                            .aria_label(SharedString::from(format!(
                                                "Close {label}"
                                            )))
                                            .group("terminal-tab-close")
                                            .absolute()
                                            .right_1()
                                            .top(px(8.))
                                            .size(px(20.))
                                            .rounded(px(4.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .opacity(if (active || hovered) && !held {
                                                1.
                                            } else {
                                                0.
                                            })
                                            .hover(|s| s.bg(content(0.1)))
                                            .child(
                                                icon("x", 12.)
                                                    .text_color(content(0.5))
                                                    .group_hover("terminal-tab-close", |s| {
                                                        s.text_color(content(1.))
                                                    }),
                                            )
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.close_terminal(ix, window, cx);
                                            })),
                                    )
                                    .into_any_element()
                            })
                            .map(Some)
                            .collect();
                        let ids: Vec<_> = live.iter().map(|t| t.entity_id()).collect();
                        // TabWidthMotion: `w-56` eases in from nothing, or out to it.
                        let slot = |open: f32| {
                            div()
                                .h_full()
                                .w(px(224. * open))
                                .min_w_0()
                                .flex_shrink(1.)
                                .mr(px(-crate::tab_motion::GAP * (1. - open)))
                                .overflow_hidden()
                                .flex()
                        };
                        slots
                            .into_iter()
                            .filter_map(move |s| match s {
                                crate::tab_motion::Slot::Live { key, open } => {
                                    let ix = ids.iter().position(|id| *id == key)?;
                                    let tab = tabs.get_mut(ix)?.take()?;
                                    Some(if open >= 1. {
                                        tab
                                    } else {
                                        slot(open).child(tab).into_any_element()
                                    })
                                }
                                crate::tab_motion::Slot::Closing { ghost, open, .. } => Some(
                                    slot(open)
                                        .child(
                                            div()
                                                .w(px(224.))
                                                .h_full()
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .h(px(TAB_PLATE_HEIGHT))
                                                        .w_full()
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(crate::tab_tile::TAB_TILE_GAP))
                                                        .pl(px(crate::tab_tile::tab_tile_inset()))
                                                        .pr_2()
                                                        .rounded_md()
                                                        .text_size(px(13.))
                                                        .text_color(content(0.5))
                                                        .child(crate::tab_tile::terminal_tile(
                                                            crate::terminal::agent_cli(&ghost),
                                                        ))
                                                        .child(
                                                            div().min_w_0().truncate().child(ghost),
                                                        ),
                                                ),
                                        )
                                        .into_any_element(),
                                ),
                            })
                            .collect::<Vec<_>>()
                    })
                    // In the panel, New follows the tabs as the title bar's does.
                    .when(in_panel, |d| {
                        d.child(
                            div()
                                .id("new-terminal")
                                .debug_selector(|| "new-terminal".into())
                                .group("new-terminal")
                                .role(Role::Button)
                                .aria_label(NEW_TERMINAL_LABEL)
                                .cursor_pointer()
                                .size(px(TAB_PLATE_HEIGHT))
                                .flex_shrink_0()
                                .ml(px(2.))
                                .rounded_md()
                                .bg(content(0.04))
                                .opacity(0.65)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|s| s.opacity(1.).bg(content(0.07)))
                                .child(
                                    icon("plus", 14.)
                                        .group_hover("new-terminal", |s| s.text_color(content(1.))),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.new_terminal(window, cx)
                                    }),
                                ),
                        )
                    }),
            )
            // The selected run's or view's own controls.
            .children(controls)
            .when(!in_panel, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(2.))
                        .pr(px(6.))
                        .child(
                            icon_button("new-terminal", NEW_TERMINAL_LABEL, "plus").on_click(
                                cx.listener(|this, _, window, cx| this.new_terminal(window, cx)),
                            ),
                        )
                        .child(
                            icon_button("move-terminal", "Move Terminal", side)
                                .child(move_anchor.probe())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // ExplorerMenu hangs from the button's bottom-left corner.
                                    let at = this
                                        .move_terminal_anchor
                                        .get()
                                        .map(|b| point(b.left(), b.bottom() + px(4.)))
                                        .unwrap_or_default();
                                    this.open_menu(
                                        MenuTarget::TerminalDock,
                                        Anchor::Point(at),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            icon_button("hide-terminal", HIDE_TERMINAL_LABEL, hide).on_click(
                                cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx)),
                            ),
                        ),
                )
            })
            .into_any_element()
    }
    /// UsageFooter's right side: the running-terminal chip, else the
    /// Terminal button.
    pub(super) fn terminal_footer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.workspace.project.is_empty() {
            self.live_mark = None;
            return None;
        }
        let running = self.running_terminals(cx);
        let open = self.dock_open();
        let classic = self.workspace.boxy_layout();
        let actions = self.actions_button(cx);
        let has_actions = actions.is_some();
        // The classic footer's own `px-3` and border hold these in place.
        let group = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(FOOTER_GAP))
            .when(!classic, |d| d.mr(px(6.)).pb(px(2.)))
            .text_size(px(11.))
            .children(actions);
        // Title bar terminal tabs take over the footer's terminal controls.
        if self.terminal_in_title() {
            self.live_mark = None;
            return Some(group.into_any_element());
        }
        if running.is_empty() {
            self.live_mark = None;
            let has_terminals = self
                .workspace
                .docks
                .get(&self.workspace.project)
                .is_some_and(|d| !d.terminals.is_empty());
            let label: SharedString = if has_terminals {
                "Terminal".into()
            } else {
                NEW_TERMINAL_LABEL.into()
            };
            return Some(
                group
                    .when(has_actions, |d| d.child(footer_separator()))
                    .child(
                        div()
                            .id("footer-terminal")
                            .debug_selector(|| "footer-terminal".into())
                            .role(Role::Button)
                            .aria_label(label)
                            .aria_toggled(if open {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .h(px(20.))
                            .map(|d| {
                                if classic {
                                    d.px(px(6.)).rounded_md()
                                } else {
                                    d.px_2().rounded_full()
                                }
                            })
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(6.))
                            .whitespace_nowrap()
                            .map(|d| {
                                if open {
                                    d.text_color(accent_color()).hover(|s| s.bg(content(0.1)))
                                } else {
                                    d.text_color(content(0.4))
                                        .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                                }
                            })
                            .child(icon("terminal", 14.))
                            .child("Terminal")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx)),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let mark = self
            .live_mark
            .get_or_insert_with(|| cx.new(LiveMark::new))
            .clone();
        let many = running.len() > 1;
        let aria = match (running.as_slice(), open) {
            ([one], true) => format!("Hide {}", one.process),
            ([one], false) => format!("Show {}", one.process),
            (_, true) => "Hide running terminals".into(),
            (all, false) => format!("{} terminals are running processes", all.len()),
        };
        let description = running
            .iter()
            .map(|r| format!("\"{}\" in {}", r.process, r.label))
            .collect::<Vec<_>>()
            .join("\n");
        let first = running[0].ix;
        let label = chip_label(running.iter().map(|r| r.process.as_str()));
        let anchor = self.terminal_chip_anchor.clone();
        Some(
            group
                .when(has_actions, |d| d.child(footer_separator()))
                .child(
                    div()
                        .id("footer-running-terminals")
                        .role(Role::Button)
                        .aria_label(aria)
                        .aria_description(description)
                        .aria_toggled(if open { gpui::Toggled::True } else { gpui::Toggled::False })
                        .when(many && !open, |d| {
                            d.aria_expanded(
                                matches!(&self.menu, Some(m) if m.target == MenuTarget::RunningTerminals),
                            )
                        })
                        .relative()
                        .map(|d| {
                            // `-mx-1` pulls the hover wash to the footer's edge;
                            // the left pull would close the gap after Actions.
                            if classic {
                                d.mr(px(-4.)).px(px(4.)).rounded_md()
                            } else {
                                d.h(px(20.)).px_2().rounded_full()
                            }
                        })
                        .max_w(px(256.))
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .whitespace_nowrap()
                        .text_color(content(0.55))
                        .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                        .child(anchor.probe())
                        .child(
                            mark.cached(StyleRefinement::default().w(px(16.)).h(px(10.)).flex_shrink_0()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::fonts::mono())
                                .text_size(px(10.))
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.dock_open() || !many {
                                this.toggle_running_terminal(first, window, cx);
                            } else if matches!(&this.menu, Some(m) if m.target == MenuTarget::RunningTerminals) {
                                this.close_menu(cx);
                            } else {
                                let anchor = Anchor::Slot(this.terminal_chip_anchor.clone());
                                this.open_menu(MenuTarget::RunningTerminals, anchor, window, cx);
                            }
                        })),
                )
                .into_any_element(),
        )
    }
    pub(super) fn terminal_menu_items(
        &self,
        target: &MenuTarget,
        cx: &App,
    ) -> Option<(SharedString, Vec<MenuItem>, f32, MenuStyle)> {
        match target {
            MenuTarget::TerminalTab(ix) => Some((
                "File tab actions".into(),
                self.terminal_tab_menu(*ix),
                160.,
                MenuStyle::Explorer,
            )),
            MenuTarget::RunningTerminals => Some((
                "Running terminals".into(),
                self.running_terminals_menu(cx),
                192.,
                MenuStyle::Explorer,
            )),
            MenuTarget::FooterChip(id) => Some(self.code_chip_menu(id, cx)),
            _ => None,
        }
    }
}

/// The space between the footer's right-hand buttons: the resource chip,
/// Actions, and Terminal or the running-terminals chip.
pub(super) const FOOTER_GAP: f32 = 4.;

/// A short hairline between footer controls: High usage, actions, Terminal.
pub(super) fn footer_separator() -> gpui::Div {
    div()
        .debug_selector(|| "footer-separator".into())
        .flex_shrink_0()
        .w(px(1.))
        .h(px(12.))
        .mx(px(3.))
        .rounded_full()
        .bg(content(0.12))
}
const NEW_TERMINAL_LABEL: &str = if cfg!(target_os = "macos") {
    "New Terminal (⌘`)"
} else {
    "New Terminal (Ctrl+`)"
};
const HIDE_TERMINAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Hide Terminal (⌘J)"
} else {
    "Hide Terminal (Ctrl+J)"
};

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn a_shell_that_exits_leaves_the_dock(cx: &mut gpui::TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        cx.update(|cx| {
            crate::terminal::bind(cx);
            cx.set_reduce_motion(true);
        });
        let (project, other_shell) = window
            .update(cx, |shell, window, cx| {
                shell.terminals_in_dock = true;
                shell.new_terminal(window, cx);
                shell.new_terminal(window, cx);
                let project = shell.workspace.project.clone();
                assert_eq!(shell.terminals[&project].len(), 2);
                let keep = shell.terminals[&project][0].clone();
                let gone = shell.terminals[&project][1].clone();
                gone.update(cx, |t, cx| t.finish(true, cx));
                (project, keep)
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert_eq!(
                    shell.terminals[&project],
                    std::slice::from_ref(&other_shell)
                );
                assert_eq!(shell.workspace.docks[&project].terminals.len(), 1);
                assert!(shell.workspace.docks[&project].open, "one shell is left");
            })
            .unwrap();
        window
            .update(visual, |shell, _, cx| {
                other_shell.update(cx, |t, cx| t.finish(true, cx));
                let _ = shell;
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert!(shell.terminals[&project].is_empty());
                assert!(!shell.workspace.docks[&project].open);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_shell_that_failed_stays_open_with_its_output(cx: &mut gpui::TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        cx.update(|cx| {
            crate::terminal::bind(cx);
            cx.set_reduce_motion(true);
        });
        let project = window
            .update(cx, |shell, window, cx| {
                shell.terminals_in_dock = true;
                shell.new_terminal(window, cx);
                let project = shell.workspace.project.clone();
                let shell_view = shell.terminals[&project][0].clone();
                shell_view.update(cx, |t, cx| t.finish(false, cx));
                project
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| {
                assert_eq!(
                    shell.terminals[&project].len(),
                    1,
                    "an error leaves the tab"
                );
                assert!(shell.workspace.docks[&project].open);
                assert!(shell.exited_terminals.is_empty());
            })
            .unwrap();
    }

    #[test]
    fn chip_labels_count_repeated_processes_in_order() {
        assert_eq!(chip_label(["vite"]), "vite");
        assert_eq!(chip_label(["vite", "jest"]), "vite · jest");
        assert_eq!(chip_label(["vite", "jest", "vite"]), "vite ×2 · jest");
    }
}
