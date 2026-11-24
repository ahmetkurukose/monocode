//! App views, Search and Monos are tabs of the current scope in every
//! layout; they keep the sidebar and the conversation workspace around them.
use super::*;
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

#[derive(Clone, Copy, Debug)]
enum Surface {
    Inbox,
    Notes,
    Automations,
    Search,
    Mono,
}

impl Surface {
    fn open(self, shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
        match self {
            Self::Inbox => shell.open_inbox(window, cx),
            Self::Notes => shell.open_notes(window, cx),
            Self::Automations => {
                // An empty in-memory service keeps this navigation test
                // independent of asynchronous service initialization.
                shell.automation_state.service = Some(Arc::new(Mutex::new(
                    monocode_core::automations::Service::open(std::path::Path::new(":memory:"))
                        .unwrap(),
                )));
                shell.open_automations(window, cx);
            }
            Self::Search => shell.open_app_search(window, cx),
            Self::Mono => {
                let project = shell.workspace.project.clone();
                let session = shell.workspace.active;
                shell.mono_manager().update(cx, |monos, cx| {
                    let id = monos.roster.create(vec![project]).id.clone();
                    let mono = monos.roster.get_mut(&id).unwrap();
                    mono.name = "Parity Mono".into();
                    mono.session_id = session;
                    monos.open(&id, window, cx);
                });
            }
        }
    }

    /// Close the destination's tab, which has focus after `open`.
    fn close(self, shell: &mut Shell, _: i64, window: &mut Window, cx: &mut Context<Shell>) {
        if let Some(pane) = shell
            .workspace
            .active
            .filter(|p| shell.view_of(*p).is_some())
        {
            shell.request_close_pane(pane, window, cx);
        }
    }

    /// Whether the focused pane shows this destination.
    fn shown(self, shell: &Shell) -> bool {
        matches!(
            (self, shell.focused_view()),
            (Self::Inbox, Some(TitleTab::App(app_views::AppView::Inbox)))
                | (Self::Notes, Some(TitleTab::App(app_views::AppView::Notes)))
                | (
                    Self::Automations,
                    Some(TitleTab::App(app_views::AppView::Automations))
                )
                | (Self::Search, Some(TitleTab::Search))
                | (Self::Mono, Some(TitleTab::Mono { .. }))
        )
    }
}

fn setup(cx: &mut TestAppContext, classic: bool, boxy: bool) -> (WindowHandle<Shell>, i64) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = super::tests::window(cx);
    cx.simulate_window_resize(window.into(), gpui::size(px(1500.), px(900.)));
    window
        .update(cx, |shell, window, cx| {
            shell.workspace.classic_layout = classic;
            shell.workspace.new_layout_boxy = boxy;
            shell.workspace.sidebar_visible = true;
            shell.workspace.project_rail_open = true;
            shell.workspace.project_rail_width = 220.;
            shell.workspace.sidebar_width = 260.;
            shell.workspace.right_panel_open = false;
            shell.open(session, window, cx);
            shell.layout_changed(cx);
        })
        .unwrap();
    cx.run_until_parked();
    (window, session)
}

#[gpui::test]
fn original_project_plus_offers_local_and_remote_folders(cx: &mut TestAppContext) {
    let (window, _) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let click = |selector: &'static str, visual: &mut VisualTestContext| {
        visual.refresh().unwrap();
        let bounds = visual.debug_bounds(selector).unwrap();
        visual.simulate_click(bounds.center(), Default::default());
        visual.run_until_parked();
        visual.refresh().unwrap();
    };
    assert!(!visual.did_prompt_for_paths());
    click("rail-open-project", visual);
    assert!(
        !visual.did_prompt_for_paths(),
        "the plus opens a menu before choosing a folder source"
    );
    assert!(visual.debug_bounds("menu-action-open-project").is_some());
    assert!(
        visual
            .debug_bounds("menu-action-open-remote-project")
            .is_some()
    );
    click("menu-action-open-project", visual);
    assert!(visual.did_prompt_for_paths());
    visual.simulate_path_prompt_response(|options| {
        assert!(options.directories);
        assert!(!options.files && !options.multiple);
        None
    });
    visual.run_until_parked();
    click("rail-open-project", visual);
    click("menu-action-open-remote-project", visual);
    assert!(visual.debug_bounds("remote-project-picker").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.menu.is_none());
            assert!(shell.remote_project_picker.is_none());
            assert_eq!(shell.workspace.project, "/work/monocode");
        })
        .unwrap();
}

#[gpui::test]
fn original_collapsed_picker_selects_monos_and_projects_with_search_and_keyboard(
    cx: &mut TestAppContext,
) {
    cx.update(crate::input::bind);
    let (window, session) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let ids = window
        .update(visual, |shell, _, cx| {
            let ids = shell.mono_manager().update(cx, |manager, cx| {
                let mut ids = Vec::new();
                for (name, project) in [
                    ("Alpha assistant", "/work/monocode"),
                    ("Beta helper", "/work/another-project"),
                ] {
                    let id = manager.roster.create(vec![project.into()]).id.clone();
                    manager.roster.get_mut(&id).unwrap().name = name.into();
                    ids.push(id);
                }
                cx.notify();
                ids
            });
            shell.workspace.project_rail_open = false;
            shell.layout_changed(cx);
            ids
        })
        .unwrap();
    visual.run_until_parked();
    let open = |visual: &mut VisualTestContext| {
        visual.refresh().unwrap();
        let mascot = visual.debug_bounds("rail-current-project").unwrap();
        visual.simulate_click(mascot.center(), Default::default());
        visual.run_until_parked();
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("project-picker").is_some());
    };
    let alpha: &'static str = Box::leak(format!("project-picker-mono-{}", ids[0]).into_boxed_str());
    let beta: &'static str = Box::leak(format!("project-picker-mono-{}", ids[1]).into_boxed_str());
    open(visual);
    let alpha_row = visual.debug_bounds(alpha).unwrap();
    let beta_row = visual.debug_bounds(beta).unwrap();
    let project_row = visual.debug_bounds("project-picker-project-0").unwrap();
    assert!(alpha_row.top() < beta_row.top() && beta_row.top() < project_row.top());
    visual.simulate_input("bEtA");
    visual.run_until_parked();
    visual.refresh().unwrap();
    assert!(visual.debug_bounds(alpha).is_none());
    assert!(visual.debug_bounds(beta).is_some());
    assert!(visual.debug_bounds("project-picker-project-0").is_none());
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    window
        .read_with(visual, |shell, cx| {
            assert!(shell.menu.is_none());
            assert_eq!(shell.mono_manager().read(cx).active.as_ref(), Some(&ids[1]));
            assert_eq!(shell.workspace.project, "/work/monocode");
            assert!(shell.mono_active(cx), "the Mono is a tab of the project");
        })
        .unwrap();
    open(visual);
    visual.simulate_keystrokes("down down enter");
    visual.run_until_parked();
    window
        .read_with(visual, |shell, cx| {
            assert!(shell.menu.is_none());
            // The project comes back as it was left: its Mono tab in front,
            // beside the session in its group.
            assert_eq!(shell.workspace.project, "/work/monocode");
            let active = shell.workspace.active.unwrap();
            assert!(shell.workspace.group_tabs(active).contains(&session));
            let _ = cx;
            assert!(!shell.workspace.project_rail_open);
        })
        .unwrap();
    open(visual);
    let row = visual.debug_bounds(alpha).unwrap();
    visual.simulate_click(row.center(), Default::default());
    visual.run_until_parked();
    window
        .read_with(visual, |shell, cx| {
            assert_eq!(shell.mono_manager().read(cx).active.as_ref(), Some(&ids[0]));
        })
        .unwrap();
}

fn assert_original_surface(surface: Surface, cx: &mut TestAppContext) {
    let (window, session) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let (tabs, conversation) = window
        .read_with(visual, |shell, cx| {
            (
                shell.workspace.tabs.clone(),
                shell.sessions[&session].read(cx).input.entity_id(),
            )
        })
        .unwrap();
    window
        .update(visual, |shell, window, cx| surface.open(shell, window, cx))
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();
    assert!(
        visual.debug_bounds("project-rail-frame").is_some(),
        "{surface:?} keeps the project rail and its sessions tree"
    );
    window
        .read_with(visual, |shell, cx| {
            assert!(surface.shown(shell), "{surface:?} is the focused tab");
            let pane = shell.workspace.active.unwrap();
            assert!(
                shell.workspace.group_tabs(pane).contains(&session),
                "{surface:?} joins the session's group"
            );
            assert_eq!(
                shell.workspace.tabs.len(),
                tabs.len(),
                "no title tab is added"
            );
            assert!(!shell.classic_surface_open(cx));
            assert_eq!(
                shell.sessions[&session].read(cx).input.entity_id(),
                conversation,
                "opening a destination retains the current conversation and draft field"
            );
        })
        .unwrap();
    let strip = window
        .read_with(visual, |shell, _| shell.workspace.active.unwrap())
        .unwrap();
    assert!(
        visual
            .debug_bounds(Box::leak(format!("group-tab-{strip}").into_boxed_str()))
            .is_some(),
        "{surface:?} has a tab in its group's strip"
    );
    window
        .update(visual, |shell, window, cx| {
            surface.close(shell, session, window, cx)
        })
        .unwrap();
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert_eq!(shell.workspace.active, Some(session));
            assert!(shell.workspace.sidebar_visible);
            assert_eq!(shell.workspace.sidebar_width, 260.);
            assert_eq!(shell.workspace.tabs.len(), tabs.len());
        })
        .unwrap();
}

#[gpui::test]
fn original_app_surfaces_inbox_is_a_tab_and_closes_back_to_the_session(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Inbox, cx);
}

#[gpui::test]
fn original_app_surfaces_notes_is_a_tab_and_closes_back_to_the_session(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Notes, cx);
}

#[gpui::test]
fn original_app_surfaces_automations_is_a_tab_and_closes_back_to_the_session(
    cx: &mut TestAppContext,
) {
    assert_original_surface(Surface::Automations, cx);
}

#[gpui::test]
fn original_app_surfaces_search_is_a_tab_and_closes_back_to_the_session(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Search, cx);
}

#[gpui::test]
fn original_app_surfaces_mono_is_a_tab_and_closes_back_to_the_session(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Mono, cx);
}

#[gpui::test]
fn new_layout_app_tabs_survive_frame_changes_and_close_like_session_tabs(cx: &mut TestAppContext) {
    cx.update(super::bind_keys);
    for boxy in [false, true] {
        let (window, session) = setup(cx, false, boxy);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let mut panes = Vec::new();
        for surface in [Surface::Inbox, Surface::Notes, Surface::Automations] {
            window
                .update(visual, |shell, window, cx| surface.open(shell, window, cx))
                .unwrap();
            visual.run_until_parked();
            panes.push(
                window
                    .read_with(visual, |shell, _| shell.workspace.active.unwrap())
                    .unwrap(),
            );
        }
        let group = window
            .update(visual, |shell, _, cx| {
                let group = shell.workspace.group_tabs(session);
                assert_eq!(group.len(), 4, "one tab of each, beside the session");
                shell.workspace.new_layout_boxy = !boxy;
                shell.layout_changed(cx);
                group
            })
            .unwrap();
        visual.run_until_parked();
        visual.refresh().unwrap();
        for (pane, app) in panes.iter().zip([
            app_views::AppView::Inbox,
            app_views::AppView::Notes,
            app_views::AppView::Automations,
        ]) {
            let selector = Box::leak(format!("group-tab-{pane}").into_boxed_str());
            let tab = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(tab.center(), Default::default());
            visual.run_until_parked();
            window
                .read_with(visual, |shell, _| {
                    assert_eq!(shell.active_app_view(), Some(app));
                    assert_eq!(shell.workspace.group_tabs(session), group);
                })
                .unwrap();
        }
        // ⌘W closes each in turn; the conversation stays.
        for _ in 0..3 {
            visual.simulate_keystrokes("cmd-w");
            visual.run_until_parked();
        }
        window
            .read_with(visual, |shell, _| {
                assert!(!shell.app_view_open());
                assert_eq!(shell.workspace.active, Some(session));
                assert_eq!(shell.workspace.group_tabs(session), [session]);
                assert!(shell.workspace.view_panes.is_empty());
            })
            .unwrap();
    }
}

fn assert_settings_navigation(classic: bool, cx: &mut TestAppContext) {
    let (window, session) = setup(cx, classic, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |shell, window, cx| {
            shell.open_settings(crate::settings::Section::General, window, cx)
        })
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();

    // The toggle steps aside but keeps its slot, so the title bar holds
    // still, and the shortcut can't hide what Settings put in the sidebar's
    // place.
    let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
    assert_eq!(toggle.size.width, px(24.));
    visual.dispatch_action(ToggleSidebar);
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.settings.is_some());
            assert!(shell.workspace.sidebar_visible);
            assert!(shell.workspace.project_rail_open);
        })
        .unwrap();

    visual.refresh().unwrap();
    let back = visual.debug_bounds("visit-back").unwrap();
    visual.simulate_click(back.center(), Default::default());
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.settings.is_none(), "Back leaves Settings");
            assert_eq!(shell.workspace.active, Some(session));
        })
        .unwrap();
}

#[gpui::test]
fn settings_back_and_sidebar_toggle_in_new_layout(cx: &mut TestAppContext) {
    assert_settings_navigation(false, cx);
}

#[gpui::test]
fn settings_back_and_sidebar_toggle_in_original_layout(cx: &mut TestAppContext) {
    assert_settings_navigation(true, cx);
}
