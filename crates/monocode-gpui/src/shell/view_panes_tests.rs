use super::*;
use crate::shell::tests::window;
use crate::workspace::AppView;
use gpui::{TestAppContext, VisualTestContext};
use monocode_core::store::Store;

/// A new session of the shell's project, opened.
fn add_session(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) -> i64 {
    let summary = locked(&shell.store)
        .unwrap()
        .create("codex", &shell.workspace.project)
        .unwrap();
    let id = summary.id;
    shell.add(summary, cx);
    shell.open(id, window, cx);
    id
}

#[gpui::test]
fn inbox_twice_focuses_the_same_tab(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            let tabs = shell.workspace.tabs.clone();
            shell.open_inbox(w, cx);
            let inbox = shell.workspace.active.unwrap();
            assert_eq!(shell.active_app_view(), Some(AppView::Inbox));
            assert_eq!(shell.workspace.group_tabs(session), [session, inbox]);
            assert_eq!(
                shell.workspace.tabs.len(),
                tabs.len(),
                "no title tab is added"
            );
            shell.open(session, w, cx);
            assert!(!shell.app_view_open());
            shell.open_inbox(w, cx);
            assert_eq!(shell.workspace.active, Some(inbox), "the same tab");
            assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(inbox));
            assert_eq!(shell.workspace.group_tabs(session), [session, inbox]);
            assert_eq!(shell.workspace.view_panes.len(), 1);
        })
        .unwrap();
    // The view fills its pane, under the group's strip.
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let inbox = window
        .update(visual, |shell, _, _| shell.workspace.active.unwrap())
        .unwrap();
    let pane = visual
        .debug_bounds(Box::leak(format!("view-pane-{inbox}").into_boxed_str()))
        .unwrap();
    let strip = visual
        .debug_bounds(Box::leak(format!("group-strip-{inbox}").into_boxed_str()))
        .unwrap();
    assert!(pane.top() >= strip.bottom());
    assert_eq!(pane.size.width, strip.size.width);
    assert!(
        visual
            .debug_bounds(Box::leak(format!("group-tab-{inbox}").into_boxed_str()))
            .is_some()
    );
}

#[gpui::test]
fn an_app_view_opens_in_the_focused_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    let db = tempfile::tempdir().unwrap();
    window
        .update(cx, |shell, w, cx| {
            shell.database_dir = db.path().to_owned();
            let right = add_session(shell, w, cx);
            shell.open(session, w, cx);
            shell.workspace.place(right, session, Edge::Right);
            shell.open(right, w, cx);
            shell.open_notes(w, cx);
            let notes = shell.workspace.active.unwrap();
            assert_eq!(shell.workspace.group_tabs(right), [right, notes]);
            assert_eq!(shell.workspace.group_tabs(session), [session]);
            // An explicit group (a strip's `+`) wins over focus.
            let database = shell.open_view_tab(
                TitleTab::App(AppView::Database),
                OpenIn {
                    group: Some(session),
                    preview: false,
                },
                w,
                cx,
            );
            assert_eq!(shell.workspace.group_tabs(session), [session, database]);
            assert_eq!(shell.active_app_view(), Some(AppView::Database));
        })
        .unwrap();
}

#[gpui::test]
fn closing_an_app_view_reveals_the_groups_last_used_tab(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            let second = add_session(shell, w, cx);
            shell.open(session, w, cx);
            shell.open_notes(w, cx);
            let notes = shell.workspace.active.unwrap();
            assert_eq!(
                shell.workspace.group_tabs(session),
                [session, notes, second]
            );
            let view = shell.notes.as_ref().unwrap().entity_id();
            shell.close_app_view(w, cx);
            assert_eq!(
                shell.workspace.active,
                Some(session),
                "the last used tab, not the neighbour"
            );
            assert_eq!(shell.workspace.group_tabs(session), [session, second]);
            assert!(shell.workspace.view_panes.is_empty());
            // Notes keeps its view, and with it what it had selected.
            shell.open_notes(w, cx);
            assert_eq!(shell.notes.as_ref().unwrap().entity_id(), view);
            let _ = notes;
            assert_eq!(shell.active_app_view(), Some(AppView::Notes));
        })
        .unwrap();
}

#[gpui::test]
fn a_mono_is_a_tab_found_again_rather_than_opened_twice(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        super::super::bind_keys(cx);
    });
    let (window, _, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            shell
                .mono_manager()
                .update(cx, |m, cx| m.create(Vec::new(), w, cx));
        })
        .unwrap();
    cx.run_until_parked();
    let (id, pane) = window
        .update(cx, |shell, w, cx| {
            let id = shell.mono_manager().read(cx).active.clone().unwrap();
            let pane = shell.workspace.active.unwrap();
            assert_eq!(shell.mono_pane_id(pane), Some(id.as_str()));
            assert!(shell.mono_active(cx));
            assert_eq!(shell.workspace.group_tabs(session), [session, pane]);
            shell.open(session, w, cx);
            assert!(!shell.mono_active(cx), "its page goes with the focus");
            assert!(shell.mono_manager().read(cx).active.is_none());
            (id, pane)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, w, cx| {
            shell.mono_manager().update(cx, |m, cx| m.open(&id, w, cx));
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.active, Some(pane), "the same tab");
            assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(pane));
            assert_eq!(shell.workspace.group_tabs(session), [session, pane]);
            assert_eq!(shell.workspace.mono_panes.len(), 1);
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    assert!(
        visual
            .debug_bounds(Box::leak(format!("mono-pane-{pane}").into_boxed_str()))
            .is_some()
    );
    // ⌘W closes it like any tab.
    visual.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-w"
    } else {
        "ctrl-w"
    });
    visual.run_until_parked();
    window
        .update(visual, |shell, _, cx| {
            assert_eq!(shell.workspace.active, Some(session));
            assert!(shell.workspace.mono_panes.is_empty());
            assert!(!shell.mono_active(cx));
        })
        .unwrap();
}

#[gpui::test]
fn saved_app_search_and_mono_title_tabs_become_view_panes(cx: &mut TestAppContext) {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let a = store.create("codex", "/work/another-project").unwrap();
    let b = store.create("claude", "/work/monocode").unwrap();
    let mut state = Workspace::default();
    state.add_project("/work/another-project");
    state.add_project("/work/monocode");
    state.project = "/work/monocode".into();
    state.open(a.id);
    state.open(b.id);
    let scope = |path: &str| {
        let key = monocode_core::files::path_key(path);
        format!("{key}\0{key}")
    };
    let mono = TitleTab::Mono {
        id: "m".into(),
        project: "/work/another-project".into(),
        worktree: "/work/another-project".into(),
    };
    state.title_order.extend([
        TitleTab::App(AppView::Inbox),
        TitleTab::Search,
        mono.clone(),
    ]);
    state
        .scope_focus
        .insert(scope("/work/monocode"), TitleTab::App(AppView::Inbox));
    state.pinned_tabs.push(TitleTab::Search);
    let saved = serde_json::to_string(&state).unwrap();
    let store = Arc::new(Mutex::new(store));
    let summaries = vec![b.clone(), a.clone()];
    let (a, b) = (a.id, b.id);
    cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                let shell = Shell::new(store, summaries, "/work/monocode".into(), Some(saved), cx);
                let w = &shell.workspace;
                assert!(
                    w.title_order
                        .iter()
                        .all(|tab| matches!(tab, TitleTab::Session(_))),
                    "no app, Search or Mono title tab is left"
                );
                let pane = |view: &TitleTab| {
                    (w.view_panes.iter().chain(w.mono_panes.iter()))
                        .find(|(_, v)| *v == view)
                        .map(|(id, _)| *id)
                        .unwrap()
                };
                let (inbox, search) = (
                    pane(&TitleTab::App(AppView::Inbox)),
                    pane(&TitleTab::Search),
                );
                // Both go to the current scope, the Inbox in front as it was.
                assert_eq!(w.group_tabs(b), [search, b, inbox]);
                assert_eq!(w.active, Some(inbox));
                assert!(w.groups[w.group_of(b).unwrap()].pinned.contains(&search));
                assert_eq!(
                    w.scope_focus[&scope("/work/monocode")],
                    TitleTab::Session(inbox)
                );
                // The Mono goes to the scope it opened in.
                let mono = pane(&mono);
                assert_eq!(w.group_tabs(a), [a, mono]);
                assert_eq!(w.tab_of(a), w.tab_of(mono));
                shell
            })
        })
        .unwrap()
    });
}
