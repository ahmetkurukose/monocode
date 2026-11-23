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
fn project_apps_are_offered_with_the_tree_only_and_switch_one_by_one(cx: &mut TestAppContext) {
    let mut prefs = Preferences::default();
    assert!(prefs.project_app_shown("inbox") && prefs.project_app_shown("database"));
    assert!(prefs.project_app_shown("api"));
    prefs.set_project_app_shown("notes", false);
    assert_eq!(prefs.project_apps, ["inbox", "database", "api"]);
    prefs.set_project_app_shown("notes", true);
    assert_eq!(
        prefs.project_apps,
        ["inbox", "notes", "database", "api"],
        "one order"
    );
    let window = window(Section::Appearance, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    for (classic, tree, shown) in [
        (true, true, true),
        (true, false, false),
        (false, true, false),
    ] {
        window
            .update(visual, |view, _, cx| {
                view.ctx.classic = classic;
                view.prefs.project_rail_view = if tree { "tree" } else { "list" }.into();
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert_eq!(
            visual
                .debug_bounds("settings-toggle-project-app-notes")
                .is_some(),
            shown,
            "classic {classic}, tree {tree}"
        );
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

#[gpui::test]
fn external_cdp_is_a_switch_and_its_endpoint_says_why_it_cannot_be_copied(cx: &mut TestAppContext) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let window = window(Section::General, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |view, _, cx| {
            view.ctx.browser_scope = Some(monocode_core::browser::Scope {
                session: 1,
                project: "/work/monocode".into(),
            });
            cx.notify();
        })
        .unwrap();
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("settings-toggle-browser-external-cdp")
            .is_some(),
        "the switch is on General"
    );
    let changed = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen = changed.clone();
    window
        .update(visual, |_, _, cx| {
            cx.subscribe_self(move |_, event: &SettingsEvent, _| {
                if let SettingsEvent::Changed(prefs) = event {
                    seen.borrow_mut().push(prefs.browser_external_cdp);
                }
            })
            .detach();
        })
        .unwrap();
    window
        .update(visual, |view, _, cx| {
            view.update(cx, |p| p.browser_external_cdp = false);
            view.update(cx, |p| p.browser_external_cdp = true);
            // The endpoint is process-wide and other tests' shells turn it
            // on (it is on by default): off here, the reason shows.
            monocode_core::browser::proxy::set_enabled(false);
            view.copy_cdp_endpoint(cx);
            assert!(matches!(&view.browser_endpoint, Some(Err(e)) if !e.is_empty()));
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(*changed.borrow(), [false, true]);
}

#[gpui::test]
fn integrations_hosts_the_trackers_and_inbox_keeps_its_notifications(cx: &mut TestAppContext) {
    let app = model::GROUPS[0].1;
    let at = app
        .iter()
        .position(|s| *s == Section::Integrations)
        .unwrap();
    assert_eq!(app[at - 1], Section::Connections, "right after Connections");
    assert_eq!(Section::Integrations.id(), "integrations");
    for query in ["monday", "jira", "linear", "gitlab"] {
        assert!(
            model::search(query, "")
                .iter()
                .any(|r| r.section == Section::Integrations),
            "{query}"
        );
    }
    let window = window(Section::Integrations, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    for id in [
        "settings-connect-monday",
        "settings-connect-jira",
        "settings-connect-linear",
        "settings-connect-gitlab",
        "settings-connect-azuredevops",
    ] {
        assert!(visual.debug_bounds(id).is_some(), "{id}");
    }
    let monday = visual.debug_bounds("settings-connect-monday").unwrap();
    let jira = visual.debug_bounds("settings-connect-jira").unwrap();
    assert!(monday.top() < jira.top(), "monday.com is the first tracker");
    window
        .update(visual, |view, _, cx| {
            view.select_section(Section::Inbox, cx)
        })
        .unwrap();
    visual.run_until_parked();
    assert!(visual.debug_bounds("settings-connect-monday").is_none());
    assert!(visual.debug_bounds("settings-connect-jira").is_none());
}

#[gpui::test]
fn monday_connects_through_the_inbox_and_follows_boards_once_connected(cx: &mut TestAppContext) {
    let window = window(Section::Integrations, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let events = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    window
        .update(visual, |_, _, cx| {
            cx.subscribe_self(move |_, event: &SettingsEvent, _| match event {
                SettingsEvent::ManageInboxProvider(id) => seen.borrow_mut().push(id.clone()),
                SettingsEvent::MondayBoards(boards) => seen
                    .borrow_mut()
                    .push(format!("boards:{}", boards.join(","))),
                _ => {}
            })
            .detach();
        })
        .unwrap();
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("settings-select-monday-boards")
            .is_none()
    );
    let connect = visual.debug_bounds("settings-connect-monday").unwrap();
    visual.simulate_click(connect.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(*events.borrow(), ["monday"]);
    window
        .update(visual, |view, _, cx| {
            view.integrations.monday = Some(monocode_core::inbox::Status {
                connected: true,
                email: "ayse@acme.com".into(),
                organization: "Acme".into(),
                site: "acme.monday.com".into(),
                ..Default::default()
            });
            view.integrations.boards = vec![
                monocode_core::inbox::OptionRow {
                    id: "9".into(),
                    name: "Sprint".into(),
                    key: "Product".into(),
                },
                monocode_core::inbox::OptionRow {
                    id: "5".into(),
                    name: "Bugs".into(),
                    key: String::new(),
                },
            ];
            cx.notify();
        })
        .unwrap();
    visual.run_until_parked();
    let picker = visual
        .debug_bounds("settings-select-monday-boards")
        .unwrap();
    visual.simulate_click(picker.center(), Default::default());
    visual.run_until_parked();
    let first = visual
        .debug_bounds("settings-option-monday-boards-0")
        .unwrap();
    visual.simulate_click(first.center(), Default::default());
    visual.run_until_parked();
    // The list stays open for the next board.
    let second = visual
        .debug_bounds("settings-option-monday-boards-1")
        .unwrap();
    visual.simulate_click(second.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(
        *events.borrow(),
        ["monday", "boards:9", "boards:9,5"],
        "Settings hands the shell the followed boards"
    );
    window
        .update(visual, |view, _, _| {
            assert_eq!(view.ctx.monday_boards, ["9", "5"])
        })
        .unwrap();
}

#[gpui::test]
fn a_project_board_binding_saves_to_the_shared_project_file(cx: &mut TestAppContext) {
    use monocode_core::integrations::{
        BoardSchema, Column, ProjectIntegrations, StatusKey, StatusOption,
    };
    let project = tempfile::tempdir().unwrap();
    let path = project.path().to_string_lossy().into_owned();
    let window = window(Section::Integrations, cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |view, _, cx| {
            view.selected_project = path.clone();
            view.ctx.projects = vec![(path.clone(), "acme".into())];
            view.integrations.monday = Some(monocode_core::inbox::Status {
                connected: true,
                ..Default::default()
            });
            view.integrations.boards = vec![monocode_core::inbox::OptionRow {
                id: "9".into(),
                name: "Sprint".into(),
                key: String::new(),
            }];
            cx.notify();
        })
        .unwrap();
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds("settings-select-monday-board")
            .is_some()
    );
    assert!(
        visual
            .debug_bounds("settings-select-monday-status-column")
            .is_none(),
        "no columns before a board"
    );
    let label = |label: &str, done| StatusOption {
        label: label.into(),
        color: None,
        done,
    };
    window
        .update(visual, |view, _, cx| {
            view.choose_monday_board("9".into(), cx);
            view.integrations.schema = Some(BoardSchema {
                id: "9".into(),
                name: "Sprint".into(),
                columns: vec![
                    Column {
                        id: "status".into(),
                        title: "Status".into(),
                        kind: "status".into(),
                    },
                    Column {
                        id: "person".into(),
                        title: "Owner".into(),
                        kind: "people".into(),
                    },
                ],
                statuses: vec![(
                    "status".into(),
                    vec![
                        label("Working on it", false),
                        label("Review", false),
                        label("Done", true),
                    ],
                )],
                ..Default::default()
            });
            cx.notify();
        })
        .unwrap();
    visual.run_until_parked();
    for id in [
        "settings-select-monday-status-column",
        "settings-select-monday-people-column",
        "settings-select-monday-map-todo",
        "settings-select-monday-map-in-progress",
        "settings-select-monday-map-in-review",
        "settings-select-monday-map-done",
        "settings-monday-write-back-ask",
        "settings-monday-binding-save",
    ] {
        assert!(visual.debug_bounds(id).is_some(), "{id}");
    }
    window
        .update(visual, |view, _, cx| {
            view.edit_monday_draft(cx, |draft| {
                draft.people_column = Some("person".into());
                draft
                    .status_map
                    .insert(StatusKey::InReview, "Review".into());
                draft.status_map.insert(StatusKey::Done, "Done".into());
                draft.write_back = Some(true);
            });
            view.save_monday_binding(false, cx);
            assert!(matches!(&view.integrations.notice, Some(Ok(_))));
        })
        .unwrap();
    let saved = ProjectIntegrations::load(project.path()).monday.unwrap();
    assert_eq!(
        (saved.board.as_str(), saved.board_name.as_str()),
        ("9", "Sprint")
    );
    assert_eq!(saved.people_column.as_deref(), Some("person"));
    assert_eq!(saved.label_for(StatusKey::InReview), Some("Review"));
    assert_eq!(saved.label_for(StatusKey::Todo), None);
    assert_eq!(saved.write_back, Some(true));
    window
        .update(visual, |view, _, cx| view.save_monday_binding(true, cx))
        .unwrap();
    assert_eq!(ProjectIntegrations::load(project.path()).monday, None);
}
