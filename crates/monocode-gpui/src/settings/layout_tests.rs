use super::*;
use gpui::{
    Bounds, MouseButton, TestAppContext, VisualTestContext, WindowBounds, WindowHandle,
    WindowOptions, point, size,
};

fn window(section: Section, cx: &mut TestAppContext) -> WindowHandle<SettingsView> {
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1200.), px(800.)),
                ))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let mut view = SettingsView::new(
                        section,
                        Preferences::default(),
                        SettingsContext::default(),
                        cx,
                    );
                    view.discovery_task = None;
                    view.discovery_loading = false;
                    view
                })
            },
        )
        .unwrap()
    });
    cx.refresh().unwrap();
    cx.run_until_parked();
    window
}

#[gpui::test]
fn workspace_style_is_only_offered_under_the_new_layout(cx: &mut TestAppContext) {
    let window = window(Section::Appearance, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    for classic in [false, true, false] {
        window
            .update(visual, |view, _, cx| {
                view.ctx.classic = classic;
                view.ctx.new_layout_boxy = true;
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(
            visual
                .debug_bounds("settings-new-layout-style-boxy")
                .is_some(),
            !classic
        );
        assert_eq!(
            visual
                .debug_bounds("settings-new-layout-style-rounded")
                .is_some(),
            !classic
        );
        assert!(
            visual
                .debug_bounds("settings-collapsed-project-rail-hidden")
                .is_none()
        );
    }
}

#[gpui::test]
fn the_project_list_choice_is_only_offered_in_classic_and_defaults_to_the_list(
    cx: &mut TestAppContext,
) {
    assert!(!Preferences::default().project_rail_tree());
    assert!(
        !serde_json::from_str::<Preferences>("{}")
            .unwrap()
            .project_rail_tree()
    );
    let window = window(Section::Appearance, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    for classic in [true, false, true] {
        window
            .update(visual, |view, _, cx| {
                view.ctx.classic = classic;
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        for option in ["list", "tree"] {
            let id = match option {
                "list" => "settings-project-rail-view-list",
                _ => "settings-project-rail-view-tree",
            };
            assert_eq!(visual.debug_bounds(id).is_some(), classic, "{id}");
        }
    }
}

#[gpui::test]
fn skills_document_has_its_own_full_height_pane_and_compact_actions(cx: &mut TestAppContext) {
    let window = window(Section::Skills, cx);
    window
        .update(cx, |view, _, cx| {
            let path = std::path::PathBuf::from("/project/.agents/skills/review/SKILL.md");
            view.discovery.skills = vec![data::Skill {
                name: "review".into(),
                description: "Review changes".into(),
                path: path.clone(),
                scope: "project".into(),
                provider: "agents".into(),
            }];
            view.skill_preview = Some(path);
            view.skill_preview_text = Some(Ok(skill_document::DocumentData::new(format!(
                "---\nname: review\n---\n# Review\n\n{}",
                "Instructions.\n\n".repeat(150)
            ))));
            cx.notify();
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let left = visual.debug_bounds("skills-list-scroll").unwrap();
    let right = visual.debug_bounds("skill-preview-pane").unwrap();
    let body = visual.debug_bounds("skill-document").unwrap();
    assert_eq!(left.size.height, px(800.));
    assert_eq!(right.size.height, px(800.));
    assert!(right.left() >= left.right());
    assert!(body.bottom() <= right.bottom());
    assert!(body.size.height > px(600.));
    assert!(visual.debug_bounds("transcript").is_none());
    for id in [
        "skill-preview-action",
        "skill-copy-action",
        "skill-reveal-action",
    ] {
        assert_eq!(
            visual.debug_bounds(id).unwrap().size,
            size(px(20.), px(20.))
        );
    }
    let source = visual
        .debug_bounds("settings-skill-document-source")
        .unwrap();
    assert!(source.size.width < px(100.));
    visual.simulate_click(source.center(), Default::default());
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert_eq!(view.prefs.skill_source_paths.len(), 1)
        })
        .unwrap();
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    // Deliver the frame that applies the measured Skills container breakpoint.
    visual.update(|window, cx| {
        window.simulate_next_frame(cx);
    });
    visual.run_until_parked();
    let left = visual.debug_bounds("skills-list-scroll").unwrap();
    let right = visual.debug_bounds("skill-preview-pane").unwrap();
    assert!(
        right.top() >= left.bottom(),
        "list={left:?}, preview={right:?}"
    );
    assert_eq!(left.size.width, px(700.));
    assert_eq!(right.size.width, px(700.));
    assert!(visual.debug_bounds("skill-document").unwrap().size.height > px(200.));
}

#[gpui::test]
fn mono_mascot_is_beside_name_and_sidebar_switch_stays_visible(cx: &mut TestAppContext) {
    let window = window(Section::Monos, cx);
    window
        .update(cx, |view, _, cx| {
            view.ctx.monos = vec![MonoSetting {
                id: "layout-mono".into(),
                name: "Big Boss".into(),
                mascot: "ghost".into(),
                color: 0xee99bb,
                projects: vec!["monocode".into(), "mono-web".into()],
                show: false,
                folders: false,
            }];
            cx.notify();
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let mascot = visual.debug_bounds("settings-mono-mascot").unwrap();
    let switch = visual
        .debug_bounds("settings-toggle-mono-sidebar-show")
        .unwrap();
    let reset = visual.debug_bounds("settings-mono-reset").unwrap();
    assert_eq!(mascot.size, size(px(16.), px(16.)));
    assert_eq!(switch.size, size(px(36.), px(20.)));
    let name = visual.debug_bounds("settings-mono-name").unwrap();
    assert!(mascot.right() < name.left());
    assert!(mascot.left() < px(200.));
    assert!(switch.left() > px(800.));
    assert!(switch.right() <= reset.left());
    assert!(reset.right() <= px(1200.));
    assert!((mascot.top() - switch.top()).abs() < px(4.));
    assert!(
        visual
            .debug_bounds("settings-toggle-mono-sidebar-folders")
            .is_none()
    );
    visual.simulate_click(reset.center(), Default::default());
    visual.run_until_parked();
    assert!(visual.debug_bounds("settings-management-dialog").is_some());
    window
        .update(visual, |view, window, cx| {
            view.close_management(window, cx);
            cx.notify();
        })
        .unwrap();
    visual.simulate_resize(size(px(700.), px(800.)));
    visual.run_until_parked();
    let switch = visual
        .debug_bounds("settings-toggle-mono-sidebar-show")
        .unwrap();
    assert_eq!(switch.size, size(px(36.), px(20.)));
    assert!(switch.right() <= px(700.));
}

#[gpui::test]
fn the_terminal_page_sets_the_font_size_and_copy_on_select(cx: &mut TestAppContext) {
    let window = window(Section::Terminal, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    // The slider's track runs from the smallest size to the largest.
    let track = visual
        .debug_bounds("settings-slider-terminal-font-size")
        .unwrap();
    visual.simulate_mouse_down(
        point(track.left() + px(2.), track.center().y),
        MouseButton::Left,
        Default::default(),
    );
    visual.simulate_mouse_up(
        point(track.left() + px(2.), track.center().y),
        MouseButton::Left,
        Default::default(),
    );
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert_eq!(view.prefs.terminal_font_size, 9, "the smallest size");
            assert!(!view.prefs.terminal_copy_on_select, "off until chosen");
        })
        .unwrap();
    let toggle = visual
        .debug_bounds("settings-row-terminal-copy-on-select")
        .unwrap();
    visual.simulate_click(
        point(toggle.right() - px(30.), toggle.top() + px(24.)),
        Default::default(),
    );
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.prefs.terminal_copy_on_select);
        })
        .unwrap();
}

#[gpui::test]
fn terminal_settings_are_in_the_rail_and_found_by_search(cx: &mut TestAppContext) {
    let _ = cx;
    assert!(Section::ALL.contains(&Section::Terminal));
    assert!(
        model::GROUPS
            .iter()
            .any(|(_, sections)| sections.contains(&Section::Terminal))
    );
    let found = |query: &str| {
        model::search(query, "")
            .iter()
            .any(|r| r.section == Section::Terminal)
    };
    assert!(found("font size"));
    assert!(found("copy on select"));
    assert!(found("clipboard"));
}
