//! The group strips (docs/native/tab-groups.md): one tab per member,
//! switching without pane motion, reordering, moving and splitting by drag,
//! closing to the most recent tab, and the flash of a tab found open.
use super::*;
use crate::shell::tests::window;
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle, point};

/// `b` with two more sessions of its project stacked in its group, the
/// last one shown: `(window, b, c, d)`.
fn stacked(cx: &mut TestAppContext) -> (WindowHandle<Shell>, i64, i64, i64) {
    let (window, _, b) = window(cx);
    let (c, d) = window
        .update(cx, |shell, w, cx| {
            shell.workspace.preferences.close_session_tab = "keep".into();
            shell.open(b, w, cx);
            let mut ids = Vec::new();
            for _ in 0..2 {
                let summary = locked(&shell.store)
                    .unwrap()
                    .create("codex", &shell.workspace.project)
                    .unwrap();
                let id = summary.id;
                shell.add(summary, cx);
                shell.open_in_workspace(id, OpenIn::default(), w, cx);
                ids.push(id);
            }
            (ids[0], ids[1])
        })
        .unwrap();
    cx.run_until_parked();
    (window, b, c, d)
}

fn selector(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, name: String) -> Option<Bounds<Pixels>> {
    cx.debug_bounds(selector(name))
}

#[gpui::test]
fn a_group_draws_one_strip_with_a_tab_per_member(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(b), [b, c, d]);
            assert_eq!(shell.workspace.visible_of(b), Some(d));
            let tab = shell.workspace.tab_of(d).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [d]);
        })
        .unwrap();
    assert!(bounds(cx, format!("group-strip-{d}")).is_some());
    let tabs: Vec<_> = [b, c, d]
        .into_iter()
        .map(|id| bounds(cx, format!("group-tab-{id}")).expect("a tab per member"))
        .collect();
    assert!(tabs[0].right() <= tabs[1].left() && tabs[1].right() <= tabs[2].left());
    // The strip heads its pane, at the window's top as its title bar.
    let pane = bounds(cx, format!("pane-{d}")).unwrap();
    let strip = bounds(cx, format!("group-strip-{d}")).unwrap();
    assert_eq!(strip.top(), pane.top());
    assert!(bounds(cx, format!("group-add-{d}")).is_some());
    assert!(bounds(cx, format!("group-more-{d}")).is_some());
    // The focused group's shown tab carries the accent.
    assert!(bounds(cx, format!("group-tab-focus-{d}")).is_some());
    assert!(bounds(cx, format!("group-tab-focus-{c}")).is_none());
}

#[gpui::test]
fn clicking_a_hidden_tab_shows_it_in_place_without_pane_motion(cx: &mut TestAppContext) {
    let (window, b, c, d) = stacked(cx);
    cx.update(|cx| cx.set_reduce_motion(false));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let before = bounds(cx, format!("pane-{d}")).unwrap();
    let tab = bounds(cx, format!("group-tab-{c}")).unwrap();
    cx.simulate_click(tab.center(), Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.active, Some(c));
            assert_eq!(shell.workspace.visible_of(b), Some(c));
            assert_eq!(shell.workspace.group_tabs(b), [b, c, d], "nothing moved");
            assert!(shell.pane_motion.is_none(), "swapping a tab does not slide");
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(bounds(cx, format!("pane-{c}")), Some(before));
    assert!(bounds(cx, format!("group-strip-{c}")).is_some());
    assert!(bounds(cx, format!("group-tab-focus-{c}")).is_some());
}

#[gpui::test]
fn a_tab_dragged_onto_another_strip_joins_that_group_at_the_pointer(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    window
        .update(cx, |shell, w, cx| {
            shell.split_tab_out(d, d, Edge::Right, w, cx)
        })
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(b), [b, c]);
            assert_eq!(shell.workspace.group_tabs(d), [d]);
        })
        .unwrap();
    let grab = bounds(cx, format!("group-tab-{c}")).unwrap().center();
    let space = bounds(cx, format!("group-strip-space-{d}"))
        .unwrap()
        .center();
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(space, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let drag = shell.pane_drag.as_ref().unwrap();
            assert_eq!(drag.group, Some(GroupDrop::Strip { group: d, index: 1 }));
            assert_eq!(drag.over, None);
        })
        .unwrap();
    assert!(cx.debug_bounds("group-strip-insert").is_some());
    cx.simulate_mouse_up(space, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(d), [d, c]);
            assert_eq!(shell.workspace.visible_of(d), Some(c));
            assert_eq!(shell.workspace.active, Some(c));
            assert_eq!(shell.workspace.visible_of(b), Some(b));
            assert!(shell.strips.drag.is_none() && shell.pane_drag.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn a_tab_dragged_onto_a_pane_edge_splits_out_of_its_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let pane = bounds(cx, format!("pane-{d}")).unwrap();
    let grab = bounds(cx, format!("group-tab-{c}")).unwrap().center();
    let edge = point(pane.right() - px(20.), pane.center().y);
    crate::browser::take_covers();
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let drag = shell.pane_drag.as_ref().unwrap();
            assert_eq!(drag.over, Some((d, Edge::Right)));
            assert_eq!(drag.group, None);
        })
        .unwrap();
    let hint = bounds(cx, format!("pane-edge-hint-{d}")).unwrap();
    // A browser page under the hint would be drawn above it; the hint
    // covers its pane so a page gives way to its snapshot.
    assert!(
        crate::browser::take_covers()
            .iter()
            .any(|cover| cover.intersects(&hint))
    );
    cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let tab = shell.workspace.tab_of(c).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [d, c]);
            assert_eq!(shell.workspace.group_tabs(b), [b, d]);
            assert_eq!(shell.workspace.group_tabs(c), [c]);
            assert_eq!(shell.workspace.active, Some(c));
        })
        .unwrap();
    cx.run_until_parked();
    // A group's only tab dropped on its own body goes nowhere.
    let own = bounds(cx, format!("pane-{c}")).unwrap();
    let grab = bounds(cx, format!("group-tab-{c}")).unwrap().center();
    let middle = own.center();
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(middle, MouseButton::Left, Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            let drag = shell.pane_drag.as_ref();
            assert!(drag.is_none_or(|d| d.over.is_none() && d.group.is_none()));
        })
        .unwrap();
    assert!(bounds(cx, format!("pane-edge-hint-{c}")).is_none());
    assert!(bounds(cx, format!("pane-centre-hint-{c}")).is_none());
    cx.simulate_mouse_up(middle, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let tab = shell.workspace.tab_of(c).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [d, c]);
        })
        .unwrap();
}

#[gpui::test]
fn closing_the_shown_tab_hands_over_to_the_most_recent_one(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    window
        .update(cx, |shell, w, cx| {
            // Most recent first: c, then d shown on top of it.
            shell.select_group_tab(c, w, cx);
            shell.select_group_tab(d, w, cx);
        })
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let close = bounds(cx, format!("group-tab-close-{d}")).unwrap();
    cx.simulate_click(close.center(), Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(b), [b, c]);
            assert_eq!(shell.workspace.visible_of(b), Some(c));
            assert_eq!(shell.workspace.active, Some(c));
            assert!(
                shell.session_summaries.contains_key(&d),
                "only the tab closed"
            );
        })
        .unwrap();
    // Middle-click closes too.
    let tab = bounds(cx, format!("group-tab-{b}")).unwrap().center();
    cx.simulate_mouse_down(tab, MouseButton::Middle, Modifiers::default());
    cx.simulate_mouse_up(tab, MouseButton::Middle, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(c), [c])
        })
        .unwrap();
}

#[gpui::test]
fn the_group_menus_close_others_and_split_a_tab_out(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::GroupTab(b), "pin", None, w, cx);
            assert_eq!(
                shell.workspace.groups[shell.workspace.group_of(b).unwrap()].pinned,
                [b]
            );
            // Pinned tabs survive the bulk closes.
            shell.run_menu(MenuTarget::GroupTab(d), "others", None, w, cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, w, cx| {
            assert_eq!(shell.workspace.group_tabs(d), [b, d]);
            shell.run_menu(MenuTarget::GroupTab(b), "split-right", None, w, cx);
            let tab = shell.workspace.tab_of(b).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [d, b]);
            assert!(!shell.workspace.group_tabs(d).contains(&c));
            assert_eq!(shell.workspace.active, Some(b));
        })
        .unwrap();
}

#[gpui::test]
fn a_tab_found_already_open_flashes_then_fades(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, _) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-tab-flash-{c}")).is_none());
    window
        .update(cx, |shell, w, cx| {
            shell.open_in_workspace(c, OpenIn::default(), w, cx);
            assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(c));
            assert_eq!(shell.workspace.group_tabs(b).len(), 3, "not opened twice");
        })
        .unwrap();
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-tab-flash-{c}")).is_some());
    window
        .update(cx, |shell, _, cx| {
            let start = std::time::Instant::now() - std::time::Duration::from_secs(1);
            shell.tab_flash = Some((c, start));
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-tab-flash-{c}")).is_none());
    window
        .update(cx, |shell, _, _| assert!(shell.tab_flash.is_none()))
        .unwrap();
}

#[gpui::test]
fn an_explorer_file_over_a_strip_joins_it_but_a_chat_body_attaches_it(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, _, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let space = bounds(cx, format!("group-strip-space-{d}"))
        .unwrap()
        .center();
    let body = bounds(cx, format!("pane-{d}")).unwrap().center();
    window
        .update(cx, |shell, _, cx| {
            shell.drag_file_over(body, true, cx);
            assert_eq!(
                shell.strips.file_drop, None,
                "the chat takes it as an attachment"
            );
            shell.drag_file_over(space, true, cx);
            assert_eq!(
                shell.strips.file_drop,
                Some((None, Some(GroupDrop::Strip { group: d, index: 3 })))
            );
            assert_eq!(shell.drop_preview(), shell.strips.file_drop.unwrap());
            shell.drag_file_over(space, false, cx);
            assert_eq!(shell.strips.file_drop, None);
            let _ = b;
        })
        .unwrap();
}

/// Every drawn tab, `+` and `⋯` of the strips showing `leaves`.
fn strip_parts(cx: &mut VisualTestContext, members: &[i64], leaves: &[i64]) -> Vec<Bounds<Pixels>> {
    let mut parts: Vec<_> = members
        .iter()
        .filter_map(|id| bounds(cx, format!("group-tab-{id}")))
        .collect();
    for leaf in leaves {
        parts.extend(bounds(cx, format!("group-add-{leaf}")));
        parts.extend(bounds(cx, format!("group-more-{leaf}")));
    }
    parts
}

fn overlaps(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

#[gpui::test]
fn top_strips_leave_room_for_the_title_bar_controls_in_every_layout(cx: &mut TestAppContext) {
    sweep_title_bar_controls(cx, false);
}

/// A focused Mono tab keeps the strips as the title bar: its overview's
/// controls float in the corner, over the details panel, clear of the
/// top-right strip's tabs and buttons.
#[gpui::test]
fn a_focused_mono_keeps_the_strips_as_the_title_bar(cx: &mut TestAppContext) {
    sweep_title_bar_controls(cx, true);
}

fn sweep_title_bar_controls(cx: &mut TestAppContext, mono: bool) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    // A second group on the right, so the top-right strip meets the corner.
    window
        .update(cx, |shell, w, cx| {
            shell.split_tab_out(d, d, Edge::Right, w, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let mut members = vec![b, c, d];
    if mono {
        window
            .update(cx, |shell, w, cx| {
                (shell.mono_manager()).update(cx, |m, cx| m.create(Vec::new(), w, cx));
            })
            .unwrap();
        cx.run_until_parked();
        let pane = window
            .update(cx, |shell, w, cx| {
                let pane = shell.workspace.active.unwrap();
                assert!(shell.mono_active(cx));
                assert_eq!(
                    shell.workspace.group_tabs(d),
                    [d, pane],
                    "the top-right group"
                );
                if !shell.workspace.right_panel_open {
                    shell.toggle_right_panel(w, cx);
                }
                pane
            })
            .unwrap();
        members.push(pane);
    }
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let mut controls = vec![
        "toggle-sidebar",
        "visit-back",
        "visit-forward",
        "titlebar-file-search",
        "toggle-right-panel",
    ];
    if mono {
        controls.push("mono-overview-tabs");
    }
    for (classic, boxy) in [(false, false), (false, true), (true, false)] {
        for sidebar in [true, false] {
            for fullscreen in [false, true] {
                window
                    .update(cx, |shell, w, cx| {
                        shell.workspace.classic_layout = classic;
                        shell.workspace.new_layout_boxy = boxy;
                        if shell.workspace.sidebar_visible != sidebar {
                            shell.toggle_sidebar(w, cx);
                        }
                        if w.is_fullscreen() != fullscreen {
                            w.toggle_fullscreen();
                        }
                        cx.notify();
                    })
                    .unwrap();
                cx.run_until_parked();
                window.update(cx, |_, w, _| w.refresh()).unwrap();
                cx.run_until_parked();
                let case =
                    format!("classic {classic} boxy {boxy} sidebar {sidebar} full {fullscreen}");
                let (titlebar, leaves) = window
                    .update(cx, |shell, _, cx| {
                        let tab = shell.workspace.active_tab().unwrap();
                        (
                            shell.strips_are_titlebar(cx),
                            shell.workspace.layout(tab).leaf_ids(),
                        )
                    })
                    .unwrap();
                assert!(titlebar, "{case}: the strips are the title bar");
                assert_eq!(leaves.len(), 2, "{case}");
                // The strips sit where the title row was: at the window's top.
                for leaf in &leaves {
                    let strip = bounds(cx, format!("group-strip-{leaf}")).unwrap();
                    assert!(strip.top() < px(TITLE_BAR_HEIGHT), "{case}: {strip:?}");
                }
                let parts = strip_parts(cx, &members, &leaves);
                assert!(parts.len() >= 4, "{case}: tabs and buttons drawn");
                let left = bounds(cx, format!("group-strip-{}", leaves[0])).unwrap();
                let forward = cx.debug_bounds("visit-forward").unwrap();
                if !sidebar && !classic {
                    // The arrows float over the top-left strip itself (the
                    // original layout keeps them over its project rail).
                    assert!(
                        forward.right() > left.left(),
                        "{case}: {forward:?} {left:?}"
                    );
                }
                let mut found = 0;
                for &name in &controls {
                    let Some(control) = cx.debug_bounds(name) else {
                        continue;
                    };
                    found += 1;
                    for part in &parts {
                        assert!(
                            !overlaps(control, *part),
                            "{case}: {name} {control:?} overlaps a strip part {part:?}"
                        );
                    }
                }
                if mono {
                    window
                        .update(cx, |shell, _, cx| assert!(shell.mono_active(cx), "{case}"))
                        .unwrap();
                    // The overview's controls head the details panel.
                    let tabs = cx.debug_bounds("mono-overview-tabs");
                    let panel = cx.debug_bounds("right-panel-frame");
                    let (Some(tabs), Some(panel)) = (tabs, panel) else {
                        panic!("{case}: the Mono's overview controls and details are drawn");
                    };
                    assert!(tabs.top() < px(TITLE_BAR_HEIGHT), "{case}: {tabs:?}");
                    assert!(
                        tabs.left() >= panel.left() - px(1.),
                        "{case}: {tabs:?} {panel:?}"
                    );
                }
                // The original layout's corner is empty without its project
                // tree; a Mono's overview holds it.
                let expected = if classic && !mono { 3 } else { 4 };
                assert!(
                    found >= expected,
                    "{case}: the navigation and corner are drawn"
                );
            }
        }
    }
}

#[gpui::test]
fn settings_bring_the_title_row_back_over_the_strips(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, _, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-strip-{d}")).is_some());
    window
        .update(cx, |shell, w, cx| {
            assert!(shell.strips_are_titlebar(cx));
            shell.open_settings(crate::settings::Section::General, w, cx);
            assert!(!shell.strips_are_titlebar(cx));
        })
        .unwrap();
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-strip-{d}")).is_none());
}

#[gpui::test]
fn double_clicking_a_strips_empty_room_starts_a_session_in_that_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let space = bounds(cx, format!("group-strip-space-{d}"))
        .unwrap()
        .center();
    cx.simulate_event(gpui::MouseDownEvent {
        position: space,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: space,
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let tabs = shell.workspace.group_tabs(b);
            assert_eq!(tabs.len(), 4, "a new session joined the group: {tabs:?}");
            let new = *tabs.iter().find(|t| ![b, c, d].contains(t)).unwrap();
            assert_eq!(shell.workspace.active, Some(new));
            let tab = shell.workspace.tab_of(new).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [new], "no split");
        })
        .unwrap();
}

#[gpui::test]
fn a_tab_dragged_along_its_strip_reorders_and_neighbours_ease_aside(cx: &mut TestAppContext) {
    let (window, b, c, d) = stacked(cx);
    cx.update(|cx| cx.set_reduce_motion(false));
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let first = bounds(cx, format!("group-tab-{b}")).unwrap();
    let last = bounds(cx, format!("group-tab-{d}")).unwrap();
    let grab = first.center();
    let to = point(last.center().x, grab.y);
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(grab.x + px(8.), grab.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            let drag = shell.strips.drag.as_ref().unwrap();
            assert!(drag.active && !drag.armed, "still a reorder in its strip");
            assert!(shell.pane_drag.is_none());
        })
        .unwrap();
    // The neighbours make room while the dragged tab rides the pointer.
    cx.run_until_parked();
    let moved = bounds(cx, format!("group-tab-{c}")).unwrap();
    let start = window
        .update(cx, |shell, _, _| shell.strips.shifts.get(&c).map(|s| s.to))
        .unwrap();
    assert!(
        start.is_some_and(|to| to < 0.),
        "c shifts left: {start:?} {moved:?}"
    );
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(b), [c, d, b]);
            assert!(shell.strips.drag.is_none() && shell.pane_drag.is_none());
            // Each tab glides from where it was drawn into its new slot.
            assert!(!shell.strips.shifts.is_empty(), "the drop eases");
            assert!(shell.pane_motion.is_none(), "no pane slide for a reorder");
        })
        .unwrap();
}

#[gpui::test]
fn a_new_tab_grows_in_and_a_closed_one_shrinks_out(cx: &mut TestAppContext) {
    let (window, b, c, d) = stacked(cx);
    cx.update(|cx| cx.set_reduce_motion(false));
    window
        .update(cx, |shell, _, _| {
            shell.workspace.preferences.tab_animations = true
        })
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let _ = b;
    window
        .update(cx, |shell, w, cx| shell.close_group_tab(c, w, cx))
        .unwrap();
    cx.run_until_parked();
    // The closed tab is still drawn, folding away beside the others.
    let ghost = bounds(cx, format!("group-tab-closing-{c}")).expect("shrinks out");
    let full = bounds(cx, format!("group-tab-{d}")).unwrap();
    assert!(ghost.size.width <= full.size.width);
    assert!(bounds(cx, format!("group-tab-{c}")).is_none());
    std::thread::sleep(std::time::Duration::from_millis(400));
    window.update(cx, |_, _, cx| cx.notify()).unwrap();
    cx.run_until_parked();
    assert!(bounds(cx, format!("group-tab-closing-{c}")).is_none());
    // A tab that opens grows in from nothing.
    let e = window
        .update(cx, |shell, w, cx| {
            let summary = locked(&shell.store)
                .unwrap()
                .create("codex", &shell.workspace.project)
                .unwrap();
            let id = summary.id;
            shell.add(summary, cx);
            shell.open_in_workspace(id, OpenIn::default(), w, cx);
            id
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let state = shell
                .strips
                .states
                .iter()
                .find(|s| s.members.contains(&e))
                .unwrap();
            assert!(state.motion.opening(&e), "the new tab's slot is growing");
        })
        .unwrap();
}

#[gpui::test]
fn a_sidebar_session_dragged_onto_a_strip_or_a_pane_edge_lands_there(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    // Two sessions of the project that are not open anywhere.
    let (e, f) = window
        .update(cx, |shell, _, cx| {
            let mut ids = Vec::new();
            for _ in 0..2 {
                let summary = locked(&shell.store)
                    .unwrap()
                    .create("codex", &shell.workspace.project)
                    .unwrap();
                ids.push(summary.id);
                shell.add(summary, cx);
            }
            (ids[0], ids[1])
        })
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let press = |shell: &mut Shell, id: i64, at: Point<Pixels>| {
        shell.pane_drag = Some(PaneDrag {
            source: DragSource::Session(id),
            start: at,
            position: at,
            active: false,
            over: None,
            group: None,
        });
    };
    let start = point(px(40.), px(300.));
    let tab = bounds(cx, format!("group-tab-{c}")).unwrap();
    let grab = point(tab.left() + px(4.), tab.center().y);
    window
        .update(cx, |shell, _, _| press(shell, e, start))
        .unwrap();
    cx.simulate_mouse_move(grab, MouseButton::Left, Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            let drag = shell.pane_drag.as_ref().unwrap();
            assert_eq!(drag.group, Some(GroupDrop::Strip { group: d, index: 1 }));
        })
        .unwrap();
    cx.simulate_mouse_up(grab, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(b), [b, e, c, d]);
            assert_eq!(shell.workspace.active, Some(e));
        })
        .unwrap();
    cx.run_until_parked();
    let pane = bounds(cx, format!("pane-{e}")).unwrap();
    let edge = point(pane.left() + px(20.), pane.center().y);
    window
        .update(cx, |shell, _, _| press(shell, f, start))
        .unwrap();
    cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::default());
    window
        .update(cx, |shell, _, _| {
            let drag = shell.pane_drag.as_ref().unwrap();
            assert_eq!(drag.over, Some((e, Edge::Left)));
        })
        .unwrap();
    cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let tab = shell.workspace.tab_of(f).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [f, e]);
            assert_eq!(shell.workspace.group_tabs(f), [f]);
            assert_eq!(shell.workspace.group_tabs(e), [b, e, c, d]);
        })
        .unwrap();
}

#[gpui::test]
fn an_overflowing_strip_scrolls_to_keep_the_shown_tab_in_view(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, _, _) = stacked(cx);
    let last = window
        .update(cx, |shell, w, cx| {
            let mut id = 0;
            for _ in 0..16 {
                let summary = locked(&shell.store)
                    .unwrap()
                    .create("codex", &shell.workspace.project)
                    .unwrap();
                id = summary.id;
                shell.add(summary, cx);
                shell.open_in_workspace(id, OpenIn::default(), w, cx);
            }
            id
        })
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.run_until_parked();
    let inside = |cx: &mut VisualTestContext, id: i64, strip: i64| {
        let tab = bounds(cx, format!("group-tab-{id}")).unwrap();
        let strip = bounds(cx, format!("group-strip-{strip}")).unwrap();
        tab.left() >= strip.left() && tab.right() <= strip.right()
    };
    assert_eq!(
        window
            .update(cx, |s, _, _| s.workspace.group_tabs(b).len())
            .unwrap(),
        19
    );
    assert!(
        inside(cx, last, last),
        "the newest tab is scrolled into view"
    );
    assert!(!inside(cx, b, last), "the strip overflows");
    window
        .update(cx, |shell, w, cx| shell.select_group_tab(b, w, cx))
        .unwrap();
    cx.run_until_parked();
    cx.run_until_parked();
    assert!(inside(cx, b, b), "the first tab scrolled back into view");
}

#[gpui::test]
fn the_plus_after_the_last_tab_starts_a_session_in_that_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, b, c, d) = stacked(cx);
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let last = bounds(cx, format!("group-tab-{d}")).unwrap();
    let plus = bounds(cx, format!("group-new-tab-{d}")).unwrap();
    // It rides right after the last tab, not at the strip's far end.
    assert!(plus.left() >= last.right() && plus.left() - last.right() < px(8.));
    cx.simulate_click(plus.center(), Modifiers::default());
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            let tabs = shell.workspace.group_tabs(b);
            assert_eq!(tabs.len(), 4, "a new session joined the group: {tabs:?}");
            let new = *tabs.iter().find(|t| ![b, c, d].contains(t)).unwrap();
            assert_eq!(shell.workspace.active, Some(new));
            let tab = shell.workspace.tab_of(new).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [new], "no split");
        })
        .unwrap();
}
