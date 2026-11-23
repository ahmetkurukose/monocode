//! Each tab group's strip (docs/native/tab-groups.md): one row of tabs at
//! the top of every split region. Strips along the window's top edge are
//! the title bar; the navigation and the corner's controls float over the
//! room they leave (`strip_insets`).
use super::*;
use crate::tab_motion::{Slot, TabMotion};
use crate::tab_tile::tab_tile_inset;

/// Tabs share the strip down to this, then it scrolls.
const STRIP_TAB_MIN_WIDTH: f32 = 96.;
/// The `+` and `⋯` buttons, with their gaps. The `+` rides after the last
/// tab or sits by the `⋯`; either way it takes the same room.
const STRIP_BUTTON: f32 = 24.;
const STRIP_BUTTONS: f32 = 2. * STRIP_BUTTON + 2. + 6.;
/// The empty room after the tabs never gets narrower than this.
const STRIP_SPACE_MIN: f32 = 16.;
/// How long a tab an open found already open glows.
const FLASH_DURATION: f32 = 0.6;
/// The inset layout's card floats this far below the window's top.
pub(super) const CARD_TOP_GAP: f32 = 6.;

/// What a tab shows: kept from its last frame so a closing tab can still be
/// drawn once its pane is gone.
#[derive(Clone)]
pub(super) struct TabFace {
    pub title: String,
    pub mark: PaneMark,
    pub dirty: bool,
}

/// One drawn strip. Groups have no ids, so each frame's strips are matched
/// to the last frame's by the tabs they share (`sync_strips`).
pub(super) struct StripState {
    members: Vec<i64>,
    pinned: Vec<i64>,
    visible: i64,
    motion: TabMotion<i64, TabFace>,
    scroll: ScrollHandle,
    bounds: AnchorSlot,
    /// The tab last scrolled into view.
    shown: Option<i64>,
}
impl StripState {
    fn new() -> Self {
        Self {
            members: Vec::new(),
            pinned: Vec::new(),
            visible: 0,
            motion: TabMotion::default(),
            scroll: ScrollHandle::new(),
            bounds: AnchorSlot::default(),
            shown: None,
        }
    }
}

/// A tab pressed in a strip: it reorders while the pointer stays on its
/// strip and becomes a pane drag once it leaves.
pub(super) struct StripDrag {
    pub(super) id: i64,
    strip: Bounds<Pixels>,
    from: usize,
    to: usize,
    origin: Point<Pixels>,
    offset: Pixels,
    pub(super) active: bool,
    bounds: Vec<Bounds<Pixels>>,
    pub(super) armed: bool,
}

/// Where a dragged tab, pane, session or terminal joins a group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum GroupDrop {
    /// Into the strip of the group showing `group`, before tab `index`.
    Strip { group: i64, index: usize },
    /// Onto the middle of the group's body: its strip's end.
    Center(i64),
}

#[derive(Default)]
pub(super) struct GroupStrips {
    states: Vec<StripState>,
    tab_bounds: HashMap<i64, AnchorSlot>,
    /// Tabs easing between slots while a neighbour is dragged and after a drop.
    pub(super) shifts: HashMap<i64, TabShift>,
    pub(super) drag: Option<StripDrag>,
    hover: Option<i64>,
    skip_click: bool,
    faces: HashMap<i64, TabFace>,
    /// The strips were drawn last frame, so their bounds are live.
    drawn: bool,
    /// Where an explorer file dragged over the panes would land.
    pub(super) file_drop: Option<DropPreview>,
}

/// A drag's landing place: a pane edge to split, or a group to join.
pub(super) type DropPreview = (Option<(i64, Edge)>, Option<GroupDrop>);

/// A tab as one strip draws it.
struct StripTab {
    id: i64,
    visible: i64,
    face: TabFace,
    width: f32,
    height: f32,
    pinned: bool,
    preview: bool,
    focused: bool,
    shift: f32,
    flash: Option<f32>,
}

impl GroupStrips {
    /// Session `from` was replaced by `to` in place: its tab keeps its slot,
    /// motion and hover.
    pub(super) fn replace_key(&mut self, from: i64, to: i64) {
        for state in &mut self.states {
            for m in state.members.iter_mut().chain(state.pinned.iter_mut()) {
                if *m == from {
                    *m = to;
                }
            }
            if state.visible == from {
                state.visible = to;
            }
            if state.shown == Some(from) {
                state.shown = Some(to);
            }
            state.motion.replace_key(&from, to);
        }
        if let Some(shift) = self.shifts.remove(&from) {
            self.shifts.insert(to, shift);
        }
        if let Some(face) = self.faces.remove(&from) {
            self.faces.insert(to, face);
        }
        if let Some(slot) = self.tab_bounds.remove(&from) {
            self.tab_bounds.insert(to, slot);
        }
        if self.hover == Some(from) {
            self.hover = Some(to);
        }
    }
}

impl Shell {
    /// Strip height: the inset card's strips leave its top gap and border
    /// to the title bar's height, so they end where the title bar did.
    pub(super) fn strip_height(&self) -> f32 {
        if self.workspace.boxy_layout() {
            TITLE_BAR_HEIGHT
        } else {
            TITLE_BAR_HEIGHT - CARD_TOP_GAP - 1.
        }
    }

    /// Whether the active tab's strips sit at the window's top as its title
    /// bar. Settings, sleeping or empty projects and a terminal dock beside
    /// or above the panes keep the title row. A focused Mono tab's overview
    /// controls float in the corner like the project's panel tabs, over the
    /// details panel's header.
    pub(super) fn strips_are_titlebar(&self, _: &App) -> bool {
        self.settings.is_none()
            && self.workspace.active_tab().is_some()
            && !self.project_asleep(&self.workspace.project)
            && (self.workspace.docks.get(&self.workspace.project))
                .is_none_or(|dock| !dock.open || dock.side == DockSide::Bottom)
    }

    /// The room the top-left strip leaves for the navigation and the
    /// top-right one for the corner's controls, wherever they overhang the
    /// panes rather than the sidebar or the right panel.
    pub(super) fn strip_insets(&self, window: &Window, cx: &App) -> (f32, f32) {
        // Client-decorated Linux keeps the corner left of its controls.
        let room = client_controls_width(window) - corner_controls_width(window);
        if self.workspace.boxy_layout() {
            let lead = self.navigation_width() - self.left_sidebar_width(cx);
            let trail = self.toggle_room(cx) + room - self.right_drawn;
            (lead.max(0.), trail.max(0.))
        } else {
            // The card's left edge: the sidebar, or its 6px inset, and border.
            let fraction = self.sidebar_fraction;
            let card = self.workspace.sidebar_width * fraction + 6. * (1. - fraction) + 1.;
            let lead = self.navigation_width() - card;
            let trail = self.toggle_room(cx) + room - 7. - self.right_drawn;
            (lead.max(0.), trail.max(0.))
        }
    }

    /// Match this frame's groups to last frame's strips: each keeps its
    /// scroll and tab motion through tab switches and moves.
    pub(super) fn sync_strips(&mut self, leaves: &[i64]) {
        let groups: Vec<(i64, Vec<i64>, Vec<i64>)> = leaves
            .iter()
            .map(|leaf| {
                let tabs = self.workspace.group_tabs(*leaf);
                let pinned = (self.workspace.group_of(*leaf))
                    .map(|ix| self.workspace.groups[ix].pinned.clone())
                    .unwrap_or_default();
                let tabs = if tabs.is_empty() { vec![*leaf] } else { tabs };
                (*leaf, tabs, pinned)
            })
            .collect();
        let old = std::mem::take(&mut self.strips.states);
        let mut pairs = Vec::new();
        for (now, (_, members, _)) in groups.iter().enumerate() {
            for (was, state) in old.iter().enumerate() {
                let shared = members.iter().filter(|m| state.members.contains(m)).count();
                if shared > 0 {
                    pairs.push((shared, members.contains(&state.visible), now, was));
                }
            }
        }
        // Most shared tabs first; a strip whose shown tab stayed wins a tie.
        pairs.sort_by_key(|a| std::cmp::Reverse((a.0, a.1)));
        let mut matched = vec![None; groups.len()];
        let mut used = vec![false; old.len()];
        for (_, _, now, was) in pairs {
            if matched[now].is_none() && !used[was] {
                matched[now] = Some(was);
                used[was] = true;
            }
        }
        let mut old: Vec<Option<StripState>> = old.into_iter().map(Some).collect();
        self.strips.states = groups
            .into_iter()
            .zip(matched)
            .map(|((visible, members, pinned), was)| {
                let mut state = was
                    .and_then(|was| old[was].take())
                    .unwrap_or_else(StripState::new);
                state.visible = visible;
                state.members = members;
                state.pinned = pinned;
                state
            })
            .collect();
        let open: HashSet<i64> = (self.strips.states.iter())
            .flat_map(|s| s.members.iter().copied())
            .collect();
        self.strips.tab_bounds.retain(|id, _| open.contains(id));
        self.strips.drawn = true;
    }

    pub(super) fn reset_strips_drawn(&mut self) {
        self.strips.drawn = false;
    }

    /// What tab `id` shows: its title, mark and unsaved state.
    pub(super) fn tab_face(&self, id: i64, cx: &App) -> Option<TabFace> {
        let face = |title: String, mark: PaneMark| TabFace {
            title,
            mark,
            dirty: false,
        };
        if let Some(title) = self.mono_pane_title(id, cx) {
            let mark = self.mono_pane_mark(id, cx).unwrap_or(PaneMark::View("bot"));
            return Some(face(title, mark));
        }
        if let Some(title) = self.terminal_pane_title(id, cx) {
            return Some(face(title, PaneMark::Terminal));
        }
        if let Some(title) = self.browser_pane_title(id, cx) {
            return Some(face(title, PaneMark::Browser));
        }
        if let Some(view) = self.workspace.view_panes.get(&id) {
            let (title, icon) = match view {
                TitleTab::App(view) | TitleTab::ProjectApp { view, .. } => {
                    (view.label().to_owned(), view.icon())
                }
                TitleTab::Search => ("Search".to_owned(), "search"),
                TitleTab::Mono { .. } | TitleTab::Session(_) => ("Mono".to_owned(), "bot"),
            };
            return Some(face(title, PaneMark::View(icon)));
        }
        if self.workspace.review_owners.contains_key(&id) {
            let (tab, dirty) = match self.reviews.get(&id) {
                Some(review) => {
                    let review = review.read(cx);
                    let state = &review.state;
                    (
                        state.tabs.get(state.active).cloned(),
                        review.unsaved_files(cx),
                    )
                }
                None => {
                    let state = self.workspace.reviews.get(&id)?;
                    (state.tabs.get(state.active).cloned(), false)
                }
            };
            let tab = tab?;
            let file = matches!(tab, crate::review::ReviewTab::File { .. });
            let title = tab.label();
            return Some(TabFace {
                mark: PaneMark::File(file.then(|| title.clone())),
                title,
                dirty,
            });
        }
        if !self.session_summaries.contains_key(&id) {
            return None;
        }
        let session = self.session_metadata(id, cx);
        let lit =
            session.responding() || session.needs_approval() || self.unseen_finished.contains(&id);
        let title = session.display_title();
        Some(face(
            if title.is_empty() {
                "New session".to_owned()
            } else {
                title
            },
            PaneMark::Session(session.summary.provider.clone(), lit.then_some(id)),
        ))
    }

    /// The pinned tab's width: its tile with the plate's inset either side.
    fn pinned_tab_width() -> f32 {
        TAB_TILE.w + 2. * tab_tile_inset()
    }

    /// How far tab `ix` of the dragged tab's strip makes room for it.
    fn strip_tab_shift(&self, id: i64, ix: usize) -> f32 {
        let Some(d) = (self.strips.drag.as_ref()).filter(|d| d.active && !d.armed) else {
            return 0.;
        };
        if id == d.id {
            return d.offset.as_f32();
        }
        if !self.workspace.group_tabs(d.id).contains(&id) || ix >= d.bounds.len() {
            return 0.;
        }
        if d.from < d.to && ix > d.from && ix <= d.to {
            return (d.bounds[ix - 1].left() - d.bounds[ix].left()).as_f32();
        }
        if d.from > d.to && ix >= d.to && ix < d.from {
            return (d.bounds[ix + 1].left() - d.bounds[ix].left()).as_f32();
        }
        0.
    }

    /// Where tab `id` is drawn now, easing toward `target`; the dragged tab
    /// rides the pointer.
    fn strip_eased_shift(
        &mut self,
        id: i64,
        target: f32,
        follow: bool,
        reduced: bool,
    ) -> (f32, bool) {
        let now = std::time::Instant::now();
        let shifts = &mut self.strips.shifts;
        if follow || reduced {
            shifts.remove(&id);
            if follow {
                shifts.insert(
                    id,
                    TabShift {
                        from: target,
                        to: target,
                        start: now - std::time::Duration::from_secs(1),
                    },
                );
            }
            return (target, false);
        }
        let current = shifts.get(&id).map(|s| (s.at(now), s.to));
        if current.is_none_or(|(_, to)| to != target) {
            let from = current.map_or(0., |((value, _), _)| value);
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

    /// The flashed tab's glow, 1 when it starts and 0 once it has faded.
    fn tab_flash_glow(&mut self, id: i64, window: &mut Window) -> Option<f32> {
        let (flashed, start) = self.tab_flash?;
        if flashed != id {
            return None;
        }
        let t = start.elapsed().as_secs_f32() / FLASH_DURATION;
        if t >= 1. {
            self.tab_flash = None;
            return None;
        }
        window.request_animation_frame();
        // Up quickly, then a long ease back down.
        let glow = if t < 0.15 {
            t / 0.15
        } else {
            let fall = (t - 0.15) / 0.85;
            1. - crate::reveal::bezier(fall, 0.22, 1., 0.36, 1.)
        };
        Some(glow.clamp(0., 1.))
    }

    /// One group's strip: pinned tabs, the tabs sharing the rest (scrolling
    /// once they reach their narrowest), then `+` and `⋯`. `top` strips are
    /// the window's title bar and leave `insets` for what floats over them.
    pub(super) fn group_strip(
        &mut self,
        visible: i64,
        width: f32,
        top: bool,
        (lead, trail): (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(six) = self.strips.states.iter().position(|s| s.visible == visible) else {
            return self.static_strip(visible, cx);
        };
        let height = self.strip_height();
        let members = self.strips.states[six].members.clone();
        let pinned = self.strips.states[six].pinned.clone();
        let preview = self.workspace.preview_of_group(visible);
        let focused = (self.workspace.active)
            .is_some_and(|active| self.workspace.visible_of(active) == Some(visible));
        let mut faces = Vec::with_capacity(members.len());
        for id in &members {
            let face = self
                .tab_face(*id, cx)
                .or_else(|| self.strips.faces.get(id).cloned());
            let face = face.unwrap_or(TabFace {
                title: String::new(),
                mark: PaneMark::View("file"),
                dirty: false,
            });
            self.strips.faces.insert(*id, face.clone());
            faces.push(face);
        }
        let reduced = cx.reduce_motion();
        let dragging = self.strips.drag.as_ref().is_some_and(|d| d.active);
        let animate = self.workspace.preferences.tab_animations && !reduced && !dragging;
        let now = std::time::Instant::now();
        let state = &mut self.strips.states[six];
        let unpinned: Vec<(i64, TabFace)> = (members.iter().copied())
            .zip(faces.iter().cloned())
            .filter(|(id, _)| !pinned.contains(id))
            .collect();
        let (slots, opening) = state.motion.sync(unpinned, animate, now);
        // Tabs share the strip like the title strip's did, down to a floor.
        let share: f32 = (slots.iter())
            .map(|s| match s {
                Slot::Live { open, .. } | Slot::Closing { open, .. } => *open,
            })
            .sum();
        let pin = Self::pinned_tab_width();
        let avail = (width
            - lead
            - trail
            - STRIP_PAD
            - STRIP_BUTTONS
            - STRIP_SPACE_MIN
            - pinned.len() as f32 * (pin + 2.)
            - share * 2.)
            .max(0.);
        let mut tab_w = if share <= 0. {
            TITLE_TAB_MAX_WIDTH
        } else {
            (avail / share).clamp(STRIP_TAB_MIN_WIDTH, TITLE_TAB_MAX_WIDTH)
        };
        // The shown tab keeps a wider floor; the others give way first.
        let visible_unpinned = !pinned.contains(&visible);
        let selected_w = tab_w.max(SELECTED_TAB_MIN_WIDTH.min(TITLE_TAB_MAX_WIDTH));
        if visible_unpinned && selected_w > tab_w && share > 1. {
            tab_w = ((avail - selected_w) / (share - 1.))
                .clamp(STRIP_TAB_MIN_WIDTH, TITLE_TAB_MAX_WIDTH);
        }
        let scale = window.scale_factor();
        let snap = |x: f32| (x * scale).floor() / scale;
        let width_of = |id: i64| {
            if pinned.contains(&id) {
                pin
            } else if id == visible {
                snap(selected_w)
            } else {
                snap(tab_w)
            }
        };
        // The `+` follows the last tab while the tabs fit; once they hit their
        // floor and scroll, it stays put by the `⋯`. The room it takes is
        // the same in both places, so the choice cannot flip back and forth.
        let laid: f32 = (slots.iter())
            .map(|s| match s {
                Slot::Live { key, open } => width_of(*key) * open,
                Slot::Closing { open, .. } => snap(tab_w) * open,
            })
            .sum();
        let add_inline = laid <= avail + 0.5;
        // The shown tab (or the flashed one) scrolls into view.
        let flashed = self
            .tab_flash
            .map(|(id, _)| id)
            .filter(|id| members.contains(id));
        let want = flashed.unwrap_or(visible);
        let state = &mut self.strips.states[six];
        if state.shown != Some(want) && !pinned.contains(&want) {
            let ix = slots.iter().position(|s| match s {
                Slot::Live { key, .. } | Slot::Closing { key, .. } => *key == want,
            });
            if let Some(ix) = ix {
                state.scroll.scroll_to_item(ix);
            }
            state.shown = Some(want);
        }
        let scroll = state.scroll.clone();
        let bounds = state.bounds.clone();
        let insert = match self.drop_preview().1 {
            Some(GroupDrop::Strip { group, index }) if group == visible => Some(index),
            _ => None,
        };
        let follow = self
            .strips
            .drag
            .as_ref()
            .filter(|d| d.active && !d.armed)
            .map(|d| d.id);
        let mut moving = opening;
        let mut tabs: HashMap<i64, AnyElement> = HashMap::new();
        for (ix, (id, face)) in members.iter().copied().zip(faces).enumerate() {
            let target = self.strip_tab_shift(id, ix);
            let (shift, eases) = self.strip_eased_shift(id, target, follow == Some(id), reduced);
            moving |= eases;
            let flash = self.tab_flash_glow(id, window);
            let element = self.strip_tab(
                StripTab {
                    id,
                    visible,
                    face,
                    width: width_of(id),
                    height,
                    pinned: pinned.contains(&id),
                    preview: preview == Some(id),
                    focused,
                    shift,
                    flash,
                },
                cx,
            );
            let element = if follow == Some(id) {
                gpui::deferred(element).with_priority(1).into_any_element()
            } else {
                element.into_any_element()
            };
            tabs.insert(id, element);
        }
        if moving {
            window.request_animation_frame();
        }
        let bar = |left: bool| {
            div()
                .debug_selector(|| "group-strip-insert".into())
                .absolute()
                .top(px((height - 22.) / 2.))
                .h(px(22.))
                .w(px(2.))
                .rounded_full()
                .bg(accent_color())
                .map(|d| {
                    if left {
                        d.left(px(-2.))
                    } else {
                        d.right(px(-2.))
                    }
                })
        };
        let marked = |element: AnyElement, ix: usize| {
            let last = ix + 1 == members.len();
            if insert == Some(ix) || (last && insert == Some(members.len())) {
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(element)
                    .when(insert == Some(ix), |d| d.child(bar(true)))
                    .when(last && insert == Some(members.len()), |d| {
                        d.child(bar(false))
                    })
                    .into_any_element()
            } else {
                element
            }
        };
        let mut pinned_row = Vec::new();
        for id in &pinned {
            if let Some(element) = tabs.remove(id) {
                let ix = members.iter().position(|m| m == id).unwrap_or(0);
                pinned_row.push(marked(element, ix));
            }
        }
        let gap = crate::tab_motion::GAP;
        let mut row = Vec::new();
        for slot in slots {
            match slot {
                Slot::Live { key, open } => {
                    let Some(element) = tabs.remove(&key) else {
                        continue;
                    };
                    let ix = members.iter().position(|m| *m == key).unwrap_or(0);
                    let element = marked(element, ix);
                    row.push(if open >= 1. {
                        element
                    } else {
                        // TabWidthMotion: the slot grows while the tab keeps its width.
                        div()
                            .flex_shrink_0()
                            .h(px(height))
                            .w(px(snap(width_of(key) * open)))
                            .mr(px(-snap(gap * (1. - open))))
                            .overflow_hidden()
                            .child(element)
                            .into_any_element()
                    });
                }
                Slot::Closing { key, ghost, open } => {
                    let w = snap(tab_w);
                    let (face, mark) = self.pane_tile(&ghost.mark, false);
                    row.push(
                        div()
                            .debug_selector(move || format!("group-tab-closing-{key}"))
                            .flex_shrink_0()
                            .h(px(height))
                            .w(px(snap(w * open)))
                            .mr(px(-snap(gap * (1. - open))))
                            .overflow_hidden()
                            .child(
                                div().w(px(w)).h_full().flex().items_center().child(
                                    title_tab_plate(false, false)
                                        .w_full()
                                        .min_w_0()
                                        .gap(px(TAB_TILE_GAP))
                                        .child(crate::tab_tile::mark_tile(&TAB_TILE, face, mark))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .h(px(20.))
                                                .text_size(px(TITLE_TAB_TEXT_SIZE))
                                                .text_color(content(0.5))
                                                .child(crate::tab_label::tab_label(
                                                    ghost.title.clone(),
                                                )),
                                        ),
                                ),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        div()
            .id(("group-strip", visible as usize))
            .debug_selector(move || format!("group-strip-{visible}"))
            .role(Role::TabList)
            .aria_label("Tabs")
            .h(px(height))
            .w_full()
            .flex_shrink_0()
            .relative()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(crate::right_panel::stroke())
            .child(bounds.probe())
            // The room the navigation floats over still drags the window.
            .when(lead > 0., |d| {
                d.child(titlebar_drag_area().w(px(lead)).flex_shrink_0())
            })
            .when(!pinned_row.is_empty(), |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .pl(px(STRIP_LEAD))
                        .children(pinned_row),
                )
            })
            .child(
                div()
                    .id(("group-strip-tabs", visible as usize))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(STRIP_LEAD))
                    .children(row)
                    .when(add_inline, |d| {
                        d.child(self.strip_button(visible, false, cx))
                    })
                    .child(self.strip_drag_area(visible, top, cx)),
            )
            .when(!add_inline, |d| {
                d.child(self.strip_button(visible, false, cx))
            })
            .child(self.strip_button(visible, true, cx).mr(px(STRIP_TRAIL)))
            .when(trail > 0., |d| {
                d.child(titlebar_drag_area().w(px(trail)).flex_shrink_0())
            })
            .into_any_element()
    }

    /// A strip drawn from its tabs' last faces, for a pane sliding out.
    fn static_strip(&self, id: i64, cx: &mut Context<Self>) -> AnyElement {
        let height = self.strip_height();
        let face = (self.tab_face(id, cx)).or_else(|| self.strips.faces.get(&id).cloned());
        div()
            .debug_selector(move || format!("group-strip-leaving-{id}"))
            .h(px(height))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .pl(px(STRIP_LEAD))
            .border_b_1()
            .border_color(crate::right_panel::stroke())
            .children(face.map(|face| {
                let (tile, mark) = self.pane_tile(&face.mark, false);
                title_tab_plate(true, false)
                    .w(px(TITLE_TAB_MAX_WIDTH.min(SELECTED_TAB_MIN_WIDTH * 1.5)))
                    .gap(px(TAB_TILE_GAP))
                    .child(crate::tab_tile::mark_tile(&TAB_TILE, tile, mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(20.))
                            .text_size(px(TITLE_TAB_TEXT_SIZE))
                            .child(crate::tab_label::tab_label(face.title)),
                    )
            }))
            .into_any_element()
    }

    fn strip_tab(&mut self, t: StripTab, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let StripTab {
            id,
            visible,
            height,
            width,
            pinned,
            preview,
            focused,
            ..
        } = t;
        let active = id == visible;
        let hovered = self.strips.hover == Some(id)
            || (self.strips.drag.as_ref()).is_some_and(|d| d.id == id && !d.active);
        let (tile, mark) = self.pane_tile(&t.face.mark, true);
        let close = !pinned && (active || width >= TAB_CLOSE_MIN_WIDTH);
        let dirty = t.face.dirty;
        let title = t.face.title.clone();
        let label = (id > 0)
            .then(|| {
                self.rename_field(id, session_groups::RenameHost::Tab, TITLE_TAB_TEXT_SIZE, cx)
            })
            .flatten()
            .unwrap_or_else(|| crate::tab_label::tab_label(title.clone()).into_any_element());
        let plate_top = ((height - TAB_PLATE_HEIGHT) / 2.).max(0.);
        let probe = self.strips.tab_bounds.entry(id).or_default().clone();
        div()
            .id(("group-tab", id as usize))
            .debug_selector(move || format!("group-tab-{id}"))
            .group("group-tab")
            .role(Role::Tab)
            .aria_label(title.clone())
            .aria_selected(active)
            .cursor_pointer()
            .w(px(width))
            .flex_shrink_0()
            .h(px(height))
            .relative()
            .left(px(t.shift))
            .flex()
            .items_center()
            .child(probe.probe())
            .child(
                title_tab_plate(active, hovered)
                    .w_full()
                    .min_w_0()
                    .gap(px(TAB_TILE_GAP))
                    .group_hover("group-tab", |s| s.bg(title_tab_fill(active, true)))
                    .when(pinned, |d| d.px(px(((width - TAB_TILE.w) / 2.).max(0.))))
                    .when(close, |d| d.pr_7())
                    .child(
                        crate::tab_tile::mark_tile(&TAB_TILE, tile, mark)
                            .relative()
                            // Discarded by Memory Saver (K64): it loads again
                            // when shown.
                            .when(self.browser_discarded(id, cx), |d| d.opacity(0.45))
                            .when(self.browser_agent_active(id, cx), |d| {
                                d.child(super::browser_presence::agent_badge(id))
                            }),
                    )
                    .when(!pinned, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(20.))
                                .text_size(px(TITLE_TAB_TEXT_SIZE))
                                .text_color(content(if active { 1. } else { 0.55 }))
                                .group_hover("group-tab", |s| s.text_color(content(1.)))
                                .when(preview, |d| d.italic())
                                .child(label),
                        )
                    })
                    .when_some(self.browser_tab_sound(id, cx), |d, sound| {
                        d.child(self.tab_sound_button(id, sound, cx))
                    }),
            )
            .when_some(t.flash, |d, glow| {
                d.child(
                    div()
                        .debug_selector(move || format!("group-tab-flash-{id}"))
                        .absolute()
                        .top(px(plate_top))
                        .left_0()
                        .w_full()
                        .h(px(TAB_PLATE_HEIGHT))
                        .rounded_md()
                        .border_1()
                        .border_color(accent_color().opacity(0.6 * glow))
                        .bg(accent_color().opacity(0.22 * glow)),
                )
            })
            .when(active && focused, |d| {
                d.child(
                    // The focused group's tab wears its accent along the
                    // plate's whole top edge, bending with its corners, so
                    // the mark reads as part of the tab rather than a bar
                    // stopping short beneath it.
                    div()
                        .debug_selector(move || format!("group-tab-focus-{id}"))
                        .absolute()
                        .top(px(plate_top))
                        .left_0()
                        .w_full()
                        .h(px(TAB_PLATE_HEIGHT))
                        .rounded_md()
                        .border_t_2()
                        .border_color(accent_color()),
                )
            })
            .when(close, |d| {
                d.child(
                    div()
                        .id(("group-tab-close", id as usize))
                        .debug_selector(move || format!("group-tab-close-{id}"))
                        .group("group-tab-close")
                        .role(Role::Button)
                        .aria_label(format!("Close {title}"))
                        .absolute()
                        .right(px(4.))
                        .top(px((height - 20.) / 2.))
                        .size(px(20.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(content(0.1)))
                        .when(dirty, |d| {
                            d.child(
                                div()
                                    .debug_selector(move || format!("group-tab-dirty-{id}"))
                                    .absolute()
                                    .top(px(7.))
                                    .left(px(7.))
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(content(0.7))
                                    .group_hover("group-tab", |s| s.opacity(0.)),
                            )
                        })
                        .child(
                            div()
                                .absolute()
                                .top_1()
                                .left_1()
                                .size(px(12.))
                                .opacity(if active && !dirty { 1. } else { 0. })
                                .group_hover("group-tab", |s| s.opacity(1.))
                                .child(
                                    icon("close", 12.).group_hover("group-tab-close", |s| {
                                        s.text_color(content(1.))
                                    }),
                                ),
                        )
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_group_tab(id, window, cx);
                        })),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.start_strip_drag(id, event, cx);
                }),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                if *hovered {
                    this.strips.hover = Some(id);
                } else if this.strips.hover == Some(id) {
                    this.strips.hover = None;
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let anchor = Anchor::Point(event.position);
                    this.open_menu(MenuTarget::GroupTab(id), anchor, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_group_tab(id, window, cx);
                }),
            )
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if std::mem::take(&mut this.strips.skip_click) {
                        return;
                    }
                    if event.click_count() == 2 {
                        if this.workspace.preview_of_group(id) == Some(id) {
                            this.keep_preview(id, cx);
                            this.persist(cx);
                            cx.notify();
                        } else if this.session_summaries.contains_key(&id) {
                            this.start_rename_in(
                                session_groups::RenameTarget::Session(id),
                                session_groups::RenameHost::Tab,
                                window,
                                cx,
                            );
                        }
                        return;
                    }
                    this.select_group_tab(id, window, cx);
                }),
            )
    }

    /// The strip's empty room: the window's drag area at the top edge, and a
    /// double click there starts a session in this group.
    fn strip_drag_area(&self, visible: i64, top: bool, cx: &mut Context<Self>) -> AnyElement {
        let bounds = AnchorSlot::default();
        div()
            .id(("group-strip-space", visible as usize))
            .debug_selector(move || format!("group-strip-space-{visible}"))
            .relative()
            .flex_1()
            .min_w(px(STRIP_SPACE_MIN))
            .h_full()
            .child(bounds.probe())
            .when(top, |d| d.window_control_area(WindowControlArea::Drag))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if event.click_count == 2 {
                        cx.stop_propagation();
                        this.new_session_in_group(visible, window, cx);
                        return;
                    }
                    let native = cfg!(target_os = "macos")
                        || (cfg!(target_os = "linux")
                            && matches!(
                                window.window_decorations(),
                                gpui::Decorations::Client { .. }
                            ));
                    if top
                        && native
                        && event.click_count == 1
                        && bounds.get().is_some_and(|b| b.contains(&event.position))
                    {
                        cx.stop_propagation();
                        window.start_window_move();
                    }
                }),
            )
            .into_any_element()
    }

    /// `+` (`more` false) or `⋯`. Both open their menu; the `+` picks what
    /// the new tab opens.
    fn strip_button(
        &self,
        visible: i64,
        more: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let target = if more {
            MenuTarget::GroupMore(visible)
        } else {
            MenuTarget::GroupAdd(visible)
        };
        let open = self.menu_open(&target);
        let anchor = AnchorSlot::default();
        let (name, label, glyph) = if more {
            ("group-more", "Group actions", "more-horizontal")
        } else {
            ("group-add", "New tab", "plus")
        };
        div()
            .id((name, visible as usize))
            .debug_selector(move || format!("{name}-{visible}"))
            .group(name)
            .role(Role::Button)
            .aria_label(label)
            .cursor_pointer()
            .size(px(STRIP_BUTTON))
            .flex_shrink_0()
            .ml(px(2.))
            .rounded_md()
            .flex()
            .items_center()
            .justify_center()
            .relative()
            .opacity(if open { 1. } else { 0.6 })
            .when(open, |d| d.bg(content(0.07)))
            .hover(|s| s.opacity(1.).bg(content(0.07)))
            .child(anchor.probe())
            .child(icon(glyph, 14.).group_hover(name, |s| s.text_color(content(1.))))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.menu_open(&target) {
                    this.close_menu(cx);
                } else {
                    this.open_menu(target.clone(), Anchor::Slot(anchor.clone()), window, cx);
                }
            }))
    }

    /// A tab clicked in its strip: shown in its group's place and focused.
    /// The group's pane stays where it is.
    pub(super) fn select_group_tab(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        self.open(id, window, cx);
    }

    /// A strip's close button: a file asks about its unsaved edits, the rest
    /// close as a tab of their group.
    pub(super) fn close_group_tab(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(cx);
        if self.workspace.review_owners.contains_key(&id)
            && let Some(review) = self.review_for(id, cx)
        {
            review.update(cx, |r, cx| r.close_active(cx));
            return;
        }
        self.request_close_pane(id, window, cx);
    }

    /// Focus the group showing `visible`, so what opens next lands there.
    fn focus_group(&mut self, visible: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.active != Some(visible) && self.workspace.tab_of(visible).is_some() {
            self.focus_pane(visible, window, cx);
        }
    }

    pub(super) fn new_session_in_group(
        &mut self,
        visible: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_group(visible, window, cx);
        // The group belongs to the checkout on screen; a chat started from
        // its strip stays there instead of falling back to the main one.
        self.new_chat_in_selected_checkout(cx);
    }

    /// Move `id` out of its group into a new one at `edge` of `target`'s
    /// group; the group it left keeps its place.
    pub(super) fn split_tab_out(
        &mut self,
        id: i64,
        target: i64,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut target) = self.workspace.visible_of(target) else {
            return;
        };
        if target == id {
            // Its own group: the group's next tab takes its place first.
            let Some(next) = self.workspace.group_mru_next(id) else {
                return;
            };
            self.workspace.show(next);
            target = next;
        }
        let before = self.active_leaves();
        self.workspace.place(id, target, edge);
        self.begin_pane_motion(before, cx);
        self.fit_sidebar(cx);
        self.open(id, window, cx);
    }

    /// A drop into a group's strip or body centre.
    pub(super) fn drop_into_group(
        &mut self,
        source: DragSource,
        drop: GroupDrop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (group, index) = match drop {
            GroupDrop::Strip { group, index } => (group, Some(index)),
            GroupDrop::Center(group) => (group, None),
        };
        if self.workspace.visible_of(group).is_none() {
            return;
        }
        let before = self.active_leaves();
        let id = match source {
            DragSource::DockTerminal(ix) => match self.take_dock_terminal(ix, cx) {
                Some(pane) => pane,
                None => return,
            },
            DragSource::Pane(id) | DragSource::Session(id) => id,
        };
        if source == DragSource::Session(id) && self.ensure_session(id, cx).is_none() {
            return;
        }
        if !self.workspace.add_to_group(id, group, index, true) {
            return;
        }
        self.begin_pane_motion(before, cx);
        self.history
            .prune(&self.workspace.open_sessions(), self.workspace.active);
        self.fit_sidebar(cx);
        self.settle_mono_panes(window, cx);
        self.open(id, window, cx);
        self.persist(cx);
    }

    /// The strip under `position` and the tab slot nearest it.
    fn strip_insert_at(&self, position: Point<Pixels>) -> Option<(i64, usize)> {
        if !self.strips.drawn {
            return None;
        }
        let state = (self.strips.states.iter())
            .find(|s| s.bounds.get().is_some_and(|b| b.contains(&position)))?;
        let index = (state.members.iter())
            .position(|m| {
                (self.strips.tab_bounds.get(m))
                    .and_then(AnchorSlot::get)
                    .is_some_and(|b| position.x < b.center().x)
            })
            .unwrap_or(state.members.len());
        Some((state.visible, index))
    }

    /// Whether `position` is in the middle of `leaf`'s body, where a drop
    /// joins its group rather than splitting beside it.
    fn pane_centre(&self, leaf: i64, position: Point<Pixels>) -> bool {
        let (Some(bounds), Some(tab)) = (self.pane_bounds.get(), self.workspace.active_tab())
        else {
            return false;
        };
        let Some(leaf) = (self.workspace.layout(tab).leaves().into_iter()).find(|l| l.id == leaf)
        else {
            return false;
        };
        let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
        let x = (position.x - bounds.left()).as_f32() / w.max(1.);
        let y = (position.y - bounds.top()).as_f32() / h.max(1.);
        let r = leaf.rect;
        let nx = (x - r.x) / r.w.max(0.0001) - 0.5;
        let ny = (y - r.y) / r.h.max(0.0001) - 0.5;
        nx.abs() < 0.25 && ny.abs() < 0.25
    }

    /// Where a drag of `source` at `position` would land: a pane edge to
    /// split, or a group to join. A group's only tab dropped on its own
    /// group goes nowhere.
    pub(super) fn pane_drop_target(
        &self,
        position: Point<Pixels>,
        source: DragSource,
        cx: &App,
    ) -> (Option<(i64, Edge)>, Option<GroupDrop>) {
        let id = source.session();
        if let Some((group, index)) = self.strip_insert_at(position) {
            let own = self.workspace.group_tabs(group).contains(&id);
            return (None, (!own).then_some(GroupDrop::Strip { group, index }));
        }
        let Some((leaf, edge)) = self.pane_hit(position, cx) else {
            return (None, None);
        };
        let members = self.workspace.group_tabs(leaf);
        let own = members.contains(&id);
        if own && members.len() <= 1 {
            return (None, None);
        }
        if self.pane_centre(leaf, position) {
            return (None, (!own).then_some(GroupDrop::Center(leaf)));
        }
        (Some((leaf, edge)), None)
    }

    /// What the drag in flight would do on release, for the hints.
    pub(super) fn drop_preview(&self) -> DropPreview {
        match self.pane_drag.as_ref().filter(|d| d.active) {
            Some(drag) => (drag.over, drag.group),
            None => self.strips.file_drop.unwrap_or_default(),
        }
    }

    /// An explorer file dragged over the panes: a strip takes it as a tab,
    /// and so do the edges and middle of panes that are not chats. A chat
    /// keeps taking dropped files as attachments.
    pub(super) fn drag_file_over(
        &mut self,
        position: Point<Pixels>,
        inside: bool,
        cx: &mut Context<Self>,
    ) {
        let target = inside
            .then(|| self.pane_drop_target(position, DragSource::Pane(i64::MIN), cx))
            .filter(|(over, group)| {
                let leaf = match (over, group) {
                    (_, Some(GroupDrop::Strip { .. })) => return true,
                    (Some((leaf, _)), _) | (_, Some(GroupDrop::Center(leaf))) => *leaf,
                    (None, None) => return false,
                };
                !self.session_summaries.contains_key(&leaf)
                    && !self.workspace.mono_panes.contains_key(&leaf)
            });
        if self.strips.file_drop != target {
            self.strips.file_drop = target;
            cx.notify();
        }
    }

    /// An explorer file released over the panes: open it for good where the
    /// hint showed.
    pub(super) fn drop_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((over, group)) = self.strips.file_drop.take() else {
            return;
        };
        let target = match (over, group) {
            (_, Some(GroupDrop::Strip { group, .. } | GroupDrop::Center(group))) => group,
            (Some((leaf, _)), None) => leaf,
            (None, None) => return,
        };
        let tab = crate::review::ReviewTab::File {
            cwd: self.git.read(cx).project().into(),
            path,
        };
        let how = OpenIn {
            group: Some(target),
            preview: false,
        };
        self.open_review_tab(tab, how, window, cx);
        let Some(pane) = self
            .workspace
            .active
            .filter(|p| self.workspace.review_owners.contains_key(p))
        else {
            return;
        };
        match (over, group) {
            (_, Some(GroupDrop::Strip { group, index })) => {
                // Already open in another group: it moves to the pointer.
                let before = self.active_leaves();
                self.workspace.add_to_group(pane, group, Some(index), true);
                self.begin_pane_motion(before, cx);
            }
            (Some((leaf, edge)), None) => self.split_tab_out(pane, leaf, edge, window, cx),
            _ => {}
        }
        self.persist(cx);
        cx.notify();
    }

    fn start_strip_drag(&mut self, id: i64, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.strips.skip_click = false;
        self.close_menu(cx);
        let Some(state) = self.strips.states.iter().find(|s| s.members.contains(&id)) else {
            return;
        };
        let Some(strip) = state.bounds.get() else {
            return;
        };
        let bounds: Vec<_> = (state.members.iter())
            .filter_map(|m| self.strips.tab_bounds.get(m).and_then(AnchorSlot::get))
            .collect();
        if bounds.len() != state.members.len() {
            return;
        }
        let from = state.members.iter().position(|m| *m == id).unwrap_or(0);
        self.strips.drag = Some(StripDrag {
            id,
            strip,
            from,
            to: from,
            origin: event.position,
            offset: px(0.),
            active: false,
            bounds,
            armed: false,
        });
        cx.notify();
    }

    pub(super) fn move_strip_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.strips.drag.as_ref() else {
            return;
        };
        // A press whose release never reached the shell must not wake up on
        // the next drag elsewhere.
        if event.pressed_button != Some(MouseButton::Left) {
            self.strips.drag = None;
            self.cancel_pane_drag(cx);
            cx.notify();
            return;
        }
        let position = event.position;
        let delta = position - drag.origin;
        if !drag.active && delta.x.abs() < px(4.) && delta.y.abs() < px(4.) {
            return;
        }
        let (id, strip, was_armed) = (drag.id, drag.strip, drag.armed);
        let elsewhere = self
            .strip_insert_at(position)
            .is_some_and(|(group, _)| !self.workspace.group_tabs(group).contains(&id));
        let pulled = (position.y - strip.bottom()).as_f32() >= PANE_DROP_PULL
            || (strip.top() - position.y).as_f32() >= PANE_DROP_PULL
            || position.x < strip.left()
            || position.x > strip.right();
        let armed = if strip.contains(&position) {
            false
        } else {
            elsewhere || pulled || was_armed
        };
        let target = armed.then(|| self.pane_drop_target(position, DragSource::Pane(id), cx));
        self.strips.skip_click = true;
        let drag = self.strips.drag.as_mut().unwrap();
        drag.active = true;
        drag.armed = armed;
        if armed {
            // Out of its strip: the tab settles back while the drag targets
            // a pane or another strip.
            drag.offset = px(0.);
            drag.to = drag.from;
        } else {
            let start = drag.bounds[drag.from].left();
            drag.offset = (position.x - drag.origin.x).clamp(
                drag.bounds[0].left() - start,
                drag.bounds.last().unwrap().left() - start,
            );
            let at = start + drag.offset;
            drag.to = (drag.bounds.iter().enumerate())
                .min_by(|(_, a), (_, b)| {
                    (a.left() - at)
                        .abs()
                        .partial_cmp(&(b.left() - at).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map_or(drag.from, |(ix, _)| ix);
        }
        let origin = drag.origin;
        self.pane_drag = target.map(|(over, group)| PaneDrag {
            source: DragSource::Pane(id),
            start: origin,
            position,
            active: true,
            over,
            group,
        });
        cx.notify();
    }

    pub(super) fn finish_strip_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.strips.drag.take() else {
            return;
        };
        cx.notify();
        if !drag.active {
            return;
        }
        self.strips.skip_click = true;
        if drag.armed {
            if self.pane_drag.is_some() {
                self.finish_pane_drag(window, cx);
            }
            return;
        }
        self.cancel_pane_drag(cx);
        let members = self.workspace.group_tabs(drag.id);
        let now = std::time::Instant::now();
        // Each tab glides from where it is drawn into its new slot.
        let drawn: Vec<(i64, f32)> = (members.iter().enumerate())
            .map(|(ix, id)| {
                let shift = self.strips.shifts.get(id).map_or(0., |s| s.at(now).0);
                let left = drag.bounds.get(ix).map_or(0., |b| b.left().as_f32());
                (*id, left + shift)
            })
            .collect();
        let moved = self.workspace.move_in_group(drag.id, drag.to);
        self.strips.shifts.clear();
        if !moved || cx.reduce_motion() {
            return;
        }
        for (ix, id) in self.workspace.group_tabs(drag.id).into_iter().enumerate() {
            let (Some((_, left)), Some(slot)) =
                (drawn.iter().find(|(t, _)| *t == id), drag.bounds.get(ix))
            else {
                continue;
            };
            let from = left - slot.left().as_f32();
            if from.abs() > 0.5 {
                self.strips.shifts.insert(
                    id,
                    TabShift {
                        from,
                        to: 0.,
                        start: now,
                    },
                );
            }
        }
        self.persist(cx);
    }

    /// The group tab menu: closes scoped to the group (pins stay), pinning,
    /// keeping a preview, splitting it out, and its chat's own items.
    pub(super) fn group_tab_menu(
        &self,
        id: i64,
        cx: &App,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let members = self.workspace.group_tabs(id);
        let pinned = (self.workspace.group_of(id))
            .map(|ix| self.workspace.groups[ix].pinned.clone())
            .unwrap_or_default();
        let is_pinned = pinned.contains(&id);
        let ix = members.iter().position(|m| *m == id).unwrap_or(0);
        let others = (members.iter()).any(|m| *m != id && !pinned.contains(m));
        let right = members[ix + 1..].iter().any(|m| !pinned.contains(m));
        let lone = members.len() <= 1;
        let mut items: Vec<MenuItem> = vec![
            MenuAction::new("close", "Close")
                .shortcut(format!("{MOD}W"))
                .into(),
            MenuAction::new("others", "Close Others")
                .disabled(!others)
                .into(),
            MenuAction::new("right", "Close to the Right")
                .disabled(!right)
                .into(),
            MenuAction::new("all", "Close All").into(),
            MenuItem::Separator,
            MenuAction::new(
                if is_pinned { "unpin" } else { "pin" },
                if is_pinned { "Unpin Tab" } else { "Pin Tab" },
            )
            .into(),
        ];
        if self.workspace.preview_of_group(id) == Some(id) {
            items.push(MenuAction::new("keep", "Keep Open").into());
        }
        items.extend(self.browser_mute_menu(id, cx));
        items.extend([
            MenuItem::Separator,
            MenuAction::new("split-right", "Split Right")
                .disabled(lone)
                .into(),
            MenuAction::new("split-down", "Split Down")
                .disabled(lone)
                .into(),
        ]);
        // A file tab can send the file to a data source's console.
        let sources = self
            .tab_file(id)
            .map(|_| crate::database::send_targets(&self.database_dir, &self.workspace.project))
            .unwrap_or_default();
        if !sources.is_empty() {
            items.extend([
                MenuItem::Separator,
                MenuAction::new("send-database", "Send to Database")
                    .icon("database")
                    .submenu(
                        (sources.iter().enumerate())
                            .map(|(ix, source)| {
                                MenuAction::new(
                                    format!("send-db:{}", source.id),
                                    source.name.clone(),
                                )
                                .checked(ix == 0)
                            })
                            .collect(),
                    )
                    .into(),
            ]);
        }
        if self.session_summaries.contains_key(&id) {
            let summary = &self.session_metadata(id, cx).summary;
            items.extend([
                MenuItem::Separator,
                MenuAction::new("rename", "Rename").shortcut("F2").into(),
                MenuAction::new("copy-id", "Copy Session ID").into(),
            ]);
            // Closing a tab keeps its session; archive hides it from the
            // active history, delete removes it.
            if summary.has_messages {
                items.extend([
                    MenuAction::new(
                        "archive",
                        if summary.archived {
                            "Unarchive"
                        } else {
                            "Archive"
                        },
                    )
                    .into(),
                    MenuAction::new("delete", "Delete").danger().into(),
                ]);
            }
        }
        let title = self.tab_face(id, cx).map(|f| f.title).unwrap_or_default();
        (
            format!("Tab actions for {title}").into(),
            items,
            220.,
            MenuStyle::Explorer,
        )
    }

    pub(super) fn run_group_tab_menu(
        &mut self,
        id: i64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let members = self.workspace.group_tabs(id);
        let pinned = (self.workspace.group_of(id))
            .map(|ix| self.workspace.groups[ix].pinned.clone())
            .unwrap_or_default();
        let ix = members.iter().position(|m| *m == id).unwrap_or(0);
        let unpinned = |ids: Vec<i64>| -> Vec<i64> {
            ids.into_iter().filter(|m| !pinned.contains(m)).collect()
        };
        match action {
            "close" => self.close_group_tab(id, window, cx),
            "others" => {
                let ids = unpinned(members.iter().copied().filter(|m| *m != id).collect());
                self.close_group_tabs(ids, window, cx);
            }
            "right" => {
                let ids = unpinned(members[ix + 1..].to_vec());
                self.close_group_tabs(ids, window, cx);
            }
            "all" => self.close_group_tabs(unpinned(members.clone()), window, cx),
            "pin" | "unpin" => {
                self.workspace.set_pinned(id, action == "pin");
                self.persist(cx);
            }
            "mute" | "unmute" => self.set_browser_muted(id, action == "mute", cx),
            "keep" => {
                self.keep_preview(id, cx);
                self.persist(cx);
            }
            "split-right" => self.split_tab_out(id, id, Edge::Right, window, cx),
            "split-down" => self.split_tab_out(id, id, Edge::Bottom, window, cx),
            "rename" => self.start_rename_in(
                session_groups::RenameTarget::Session(id),
                session_groups::RenameHost::Tab,
                window,
                cx,
            ),
            "copy-id" => cx.write_to_clipboard(ClipboardItem::new_string(id.to_string())),
            "archive" => self.toggle_archive(id, cx),
            "delete" => self.confirm_delete_session(id, window, cx),
            _ => {
                if let Some(source) = action.strip_prefix("send-db:")
                    && let Some(path) = self.tab_file(id)
                {
                    self.send_file_to_database(path, source.to_owned(), cx);
                }
            }
        }
        cx.notify();
    }

    /// The strip's `+`: what opens lands in this group.
    pub(super) fn group_add_menu(&self) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        (
            "New tab".into(),
            vec![
                MenuAction::new("session", "New Session")
                    .icon("plus")
                    .shortcut(format!("{MOD}T"))
                    .into(),
                MenuAction::new("terminal", "New Terminal")
                    .icon("terminal")
                    .into(),
                MenuAction::new("browser", "New Browser")
                    .icon("globe")
                    .into(),
                MenuItem::Separator,
                MenuAction::new("file", "Open File…").icon("search").into(),
            ],
            200.,
            MenuStyle::Explorer,
        )
    }

    pub(super) fn run_group_add_menu(
        &mut self,
        visible: i64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_group(visible, window, cx);
        match action {
            "session" => self.new_chat(cx),
            "terminal" => self.new_title_terminal(window, cx),
            "browser" => self.new_browser_pane(None, window, cx),
            "file" => self.open_everywhere(Some(crate::everywhere::Tab::Files), None, window, cx),
            _ => {}
        }
    }

    /// The strip's `⋯`: the group's own layout.
    pub(super) fn group_more_menu(
        &self,
        visible: i64,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let split = (self.workspace.tab_of(visible))
            .is_some_and(|tab| self.workspace.layout(tab).is_split());
        (
            "Group actions".into(),
            vec![
                MenuAction::new("split-right", "Split Right")
                    .shortcut(format!("{MOD}D"))
                    .into(),
                MenuAction::new("split-down", "Split Down")
                    .shortcut(if cfg!(target_os = "macos") {
                        "⌘⇧D"
                    } else {
                        "Ctrl+Shift+D"
                    })
                    .into(),
                MenuAction::new("equalize", "Equalize Sizes")
                    .disabled(!split)
                    .into(),
                MenuItem::Separator,
                MenuAction::new("close-group", "Close Group").into(),
            ],
            200.,
            MenuStyle::Explorer,
        )
    }

    pub(super) fn run_group_more_menu(
        &mut self,
        visible: i64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "split-right" | "split-down" => {
                self.focus_group(visible, window, cx);
                let dir = if action == "split-right" {
                    SplitDir::Right
                } else {
                    SplitDir::Down
                };
                self.split_session(dir, cx);
            }
            "equalize" => {
                if let Some(tab) = self.workspace.tab_of(visible) {
                    let before = self.active_leaves();
                    self.workspace.equalize(tab);
                    self.begin_pane_motion(before, cx);
                    self.persist(cx);
                }
            }
            "close-group" => {
                let ids = self.workspace.group_tabs(visible);
                self.close_group_tabs(ids, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }
}

#[cfg(test)]
#[path = "group_strip_tests.rs"]
mod tests;
