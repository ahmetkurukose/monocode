use super::*;
use crate::review::{ReviewEvent, ReviewState, ReviewTab};
use crate::shell::tests::window;
use gpui::{TestAppContext, VisualTestContext, WindowHandle};
use monocode_core::store::Store;

fn file(name: &str) -> ReviewTab {
    ReviewTab::File {
        cwd: "/work/monocode".into(),
        path: format!("/work/monocode/{name}"),
    }
}

fn shown(shell: &Shell, pane: i64, cx: &App) -> Vec<ReviewTab> {
    shell.reviews[&pane].read(cx).state.tabs.clone()
}

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

fn modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}

#[gpui::test]
fn a_scope_keeps_one_title_tab_and_new_tabs_join_the_focused_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, other, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            let tabs = shell.workspace.tabs.clone();
            let second = add_session(shell, w, cx);
            let third = add_session(shell, w, cx);
            assert_eq!(
                shell.workspace.tabs.len(),
                tabs.len(),
                "no title tab is added"
            );
            assert_eq!(shell.workspace.group_tabs(third), [session, second, third]);
            assert_eq!(shell.workspace.active, Some(third));
            assert_eq!(
                shell.workspace.tab_of(second),
                shell.workspace.tab_of(third)
            );
            // Tab keys walk the focused group.
            shell.activate_tab(0, w, cx);
            assert_eq!(shell.workspace.active, Some(session));
            shell.activate_tab(-1, w, cx);
            assert_eq!(shell.workspace.active, Some(third));
            shell.cycle(1, w, cx);
            assert_eq!(shell.workspace.active, Some(session), "cycling wraps");
            shell.cycle(-1, w, cx);
            assert_eq!(shell.workspace.active, Some(third));
            // Another project is another scope, with its own title tab.
            assert!(shell.workspace.tab_of(other).is_some());
            assert_ne!(shell.workspace.tab_of(other), shell.workspace.tab_of(third));
            // Opening an open session shows it and flashes its tab.
            shell.open_in_workspace(second, OpenIn::default(), w, cx);
            assert_eq!(shell.workspace.active, Some(second));
            assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(second));
            assert_eq!(shell.workspace.group_tabs(second).len(), 3);
        })
        .unwrap();
}

#[gpui::test]
fn files_open_beside_a_chat_and_are_found_again_from_any_group(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            shell.open(session, w, cx);
            shell.open_review(file("a.rs"), w, cx);
            let a = shell.workspace.active.unwrap();
            assert!(a < 0, "a file is a pane of its own");
            assert_eq!(shell.workspace.review_owners[&a], session);
            assert!(!shell.workspace.reviews.contains_key(&session));
            assert_eq!(shown(shell, a, cx), [file("a.rs")]);
            // A file never covers the chat: it opens in a group to its right.
            let tab = shell.workspace.tab_of(session).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [session, a]);
            // The next file joins the editor group, even with the chat focused.
            shell.open(session, w, cx);
            shell.open_review(file("b.rs"), w, cx);
            let b = shell.workspace.active.unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [session, b]);
            assert_eq!(shell.workspace.group_tabs(b), [a, b]);
            assert_eq!(shown(shell, b, cx), [file("b.rs")]);
            // Open again from the chat: the hidden pane is shown, not copied.
            shell.open(session, w, cx);
            let panes = shell.workspace.review_owners.len();
            shell.open_review(file("a.rs"), w, cx);
            assert_eq!(shell.workspace.active, Some(a));
            assert_eq!(shell.workspace.review_owners.len(), panes);
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [session, a]);
            assert_eq!(shell.tab_flash.map(|(id, _)| id), Some(a));
            // A diff follows the same rules.
            let changes = ReviewTab::Changes {
                cwd: "/work/monocode".into(),
                path: None,
                staged: None,
            };
            shell.open_review(changes.clone(), w, cx);
            let diff = shell.workspace.active.unwrap();
            assert_eq!(shell.workspace.group_tabs(diff), [a, diff, b]);
            shell.open(session, w, cx);
            shell.open_review(changes, w, cx);
            assert_eq!(shell.workspace.active, Some(diff));
        })
        .unwrap();
}

#[gpui::test]
fn a_preview_is_replaced_in_place_until_it_is_kept(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    let pane = window
        .update(cx, |shell, w, cx| {
            shell.open(session, w, cx);
            shell.open_review_tab(file("a.rs"), OpenIn::PREVIEW, w, cx);
            let pane = shell.workspace.active.unwrap();
            assert_eq!(shell.workspace.preview_of_group(pane), Some(pane));
            // The next preview takes the same pane.
            shell.open_review_tab(file("b.rs"), OpenIn::PREVIEW, w, cx);
            assert_eq!(shell.workspace.active, Some(pane));
            assert_eq!(shown(shell, pane, cx), [file("b.rs")]);
            assert_eq!(shell.workspace.review_owners.len(), 1);
            // Opening it for real keeps it.
            shell.open_review_tab(file("b.rs"), OpenIn::default(), w, cx);
            assert_eq!(shell.workspace.active, Some(pane));
            assert_eq!(shell.workspace.preview_of_group(pane), None);
            // So the next preview is a new pane in the same group.
            shell.open_review_tab(file("c.rs"), OpenIn::PREVIEW, w, cx);
            let next = shell.workspace.active.unwrap();
            assert_ne!(next, pane);
            assert_eq!(shell.workspace.group_tabs(next), [pane, next]);
            assert_eq!(shell.workspace.preview_of_group(next), Some(next));
            // A preview of what is open just shows it.
            shell.open_review_tab(file("b.rs"), OpenIn::PREVIEW, w, cx);
            assert_eq!(shell.workspace.active, Some(pane));
            assert_eq!(shell.workspace.preview_of_group(pane), Some(next));
            assert_eq!(shown(shell, next, cx), [file("c.rs")]);
            next
        })
        .unwrap();
    // Editing a preview keeps it too.
    window
        .update(cx, |shell, _, cx| {
            shell.reviews[&pane].update(cx, |_, cx| cx.emit(ReviewEvent::Permanent));
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.preview_of_group(pane), None);
        })
        .unwrap();
}

#[gpui::test]
fn closing_a_tab_hands_over_to_the_last_used_one_and_reopens_where_it_was(cx: &mut TestAppContext) {
    cx.update(|cx| {
        bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let (window, _, session) = window(cx);
    let (a, b, c) = window
        .update(cx, |shell, w, cx| {
            shell.open(session, w, cx);
            for name in ["a.rs", "b.rs", "c.rs"] {
                shell.open_review(file(name), w, cx);
            }
            let tabs = shell.workspace.group_tabs(shell.workspace.active.unwrap());
            let (a, b, c) = (tabs[0], tabs[1], tabs[2]);
            assert_eq!(shown(shell, c, cx), [file("c.rs")]);
            // Recency: b, then a, then c in front.
            shell.open(b, w, cx);
            shell.open(a, w, cx);
            shell.open(c, w, cx);
            (a, b, c)
        })
        .unwrap();
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    visual.simulate_keystrokes(&format!("{}-w", modifier()));
    visual.run_until_parked();
    window
        .update(visual, |shell, _, _| {
            assert_eq!(
                shell.workspace.active,
                Some(a),
                "the last used tab, not the neighbour"
            );
            assert_eq!(shell.workspace.group_tabs(a), [a, b]);
            assert!(shell.workspace.tab_of(c).is_none());
            let tab = shell.workspace.tab_of(session).unwrap();
            assert_eq!(shell.workspace.layout(tab).leaf_ids(), [session, a]);
        })
        .unwrap();
    visual.simulate_keystrokes(&format!("{}-shift-t", modifier()));
    visual.run_until_parked();
    window
        .update(visual, |shell, _, cx| {
            let reopened = shell.workspace.active.unwrap();
            assert_eq!(shown(shell, reopened, cx), [file("c.rs")]);
            assert_eq!(shell.workspace.group_tabs(a).len(), 3, "back in its group");
        })
        .unwrap();
    // A session tab closes out of its group and comes back the same way;
    // the session itself stays.
    let second = window
        .update(visual, |shell, w, cx| {
            shell.open(session, w, cx);
            add_session(shell, w, cx)
        })
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.workspace.group_tabs(second), [session, second]);
        })
        .unwrap();
    visual.simulate_keystrokes(&format!("{}-w", modifier()));
    visual.run_until_parked();
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.workspace.active, Some(session));
            assert!(shell.workspace.tab_of(second).is_none());
            assert!(shell.session_summaries.contains_key(&second));
        })
        .unwrap();
    visual.simulate_keystrokes(&format!("{}-shift-t", modifier()));
    visual.run_until_parked();
    window
        .update(visual, |shell, _, _| {
            assert_eq!(shell.workspace.active, Some(second));
            assert_eq!(shell.workspace.group_tabs(second), [session, second]);
        })
        .unwrap();
}

#[gpui::test]
fn close_others_and_close_all_act_on_the_focused_group_and_keep_its_pins(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    let (second, third) = window
        .update(cx, |shell, w, cx| {
            let second = add_session(shell, w, cx);
            let third = add_session(shell, w, cx);
            shell.workspace.set_pinned(session, true);
            shell.close_other_tabs(w, cx);
            (second, third)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, w, cx| {
            assert_eq!(shell.workspace.group_tabs(third), [session, third]);
            assert!(shell.workspace.tab_of(second).is_none());
            w.dispatch_action(Box::new(CloseAllTabs), cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, _| {
            assert_eq!(shell.workspace.active, Some(session), "the pin stays");
            assert!(shell.workspace.tab_of(third).is_none());
        })
        .unwrap();
}

fn migrated(cx: &mut TestAppContext) -> (WindowHandle<Shell>, i64, i64) {
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let first = store.create("codex", "/work/monocode").unwrap();
    let second = store.create("claude", "/work/monocode").unwrap();
    let ids = (first.id, second.id);
    let mut state = Workspace::default();
    state.add_project("/work/monocode");
    state.open(ids.0);
    state.open(ids.1);
    // Files attached beside the first chat, as older builds kept them.
    state.reviews.insert(
        ids.0,
        ReviewState {
            tabs: vec![file("a.rs"), file("b.rs")],
            active: 1,
            ratio: 0.4,
            chat_hidden: false,
        },
    );
    let saved = serde_json::to_string(&state).unwrap();
    let store = Arc::new(Mutex::new(store));
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                Shell::new(
                    store,
                    vec![first, second],
                    "/work/monocode".into(),
                    Some(saved),
                    cx,
                )
            })
        })
        .unwrap()
    });
    cx.run_until_parked();
    (window, ids.0, ids.1)
}

#[gpui::test]
fn older_workspaces_fold_into_one_tab_with_attached_files_as_panes(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, first, second) = migrated(cx);
    window
        .update(cx, |shell, _, cx| {
            assert_eq!(shell.workspace.tabs.len(), 1, "one title tab for the scope");
            assert!(!shell.workspace.reviews.contains_key(&first));
            let files: Vec<i64> = (shell.workspace.group_tabs(second).into_iter())
                .filter(|id| *id < 0)
                .collect();
            assert_eq!(files.len(), 2);
            for (pane, name) in files.iter().zip(["a.rs", "b.rs"]) {
                assert_eq!(shell.workspace.review_owners[pane], first);
                let state = &shell.workspace.reviews[pane];
                assert_eq!(state.tabs, [file(name)]);
                assert!(state.chat_hidden);
            }
            // The tab in front takes the other's panes, files right after
            // their chat.
            assert_eq!(
                shell.workspace.group_tabs(second),
                [second, first, files[0], files[1]]
            );
            assert_eq!(shell.workspace.active, Some(second));
            // Again changes nothing.
            let groups = shell.workspace.groups.clone();
            shell.migrate_tab_groups(cx);
            assert_eq!(shell.workspace.groups, groups);
            assert_eq!(shell.workspace.tabs.len(), 1);
        })
        .unwrap();
    // The files open again from their panes, not as copies.
    window
        .update(cx, |shell, w, cx| {
            let panes = shell.workspace.review_owners.len();
            shell.open(first, w, cx);
            shell.open_review(file("b.rs"), w, cx);
            assert_eq!(shell.workspace.review_owners.len(), panes);
            assert_eq!(
                shown(shell, shell.workspace.active.unwrap(), cx),
                [file("b.rs")]
            );
        })
        .unwrap();
}

#[gpui::test]
fn a_second_title_tab_in_a_scope_folds_back_into_the_first(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = window(cx);
    window
        .update(cx, |shell, w, cx| {
            let second = add_session(shell, w, cx);
            // Something that still makes a title tab of its own.
            shell.workspace.detach(second, shell.workspace.tabs.len());
            let tabs = shell.workspace.tabs.len();
            assert!(shell.merge_scope_tabs(cx));
            assert_eq!(shell.workspace.tabs.len(), tabs - 1);
            assert_eq!(
                shell.workspace.tab_of(second),
                shell.workspace.tab_of(session)
            );
            assert!(!shell.merge_scope_tabs(cx), "nothing left to fold");
        })
        .unwrap();
}

#[gpui::test]
fn each_worktree_keeps_its_own_layout_and_new_chats_stay_in_it(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, main) = window(cx);
    let feature = crate::worktree_switcher::WorktreeFocus {
        path: "/work/monocode-worktrees/feature".into(),
        branch: Some("feature".into()),
    };
    window
        .update(cx, |shell, w, cx| {
            shell.select_worktree(Some(feature.clone()), w, cx);
            assert!(shell.empty_workspace_showing(cx));
            shell.new_chat(cx);
        })
        .unwrap();
    cx.run_until_parked();
    let chat = window
        .update(cx, |shell, _, cx| {
            let chat = shell.workspace.active.expect("the copy's first chat");
            assert_eq!(shell.working_copy(), feature.path, "the view stays");
            assert_eq!(shell.session_working_copy(chat, cx), feature.path);
            // The copy's chat starts the copy's own layout; main's is untouched.
            assert_ne!(shell.workspace.tab_of(chat), shell.workspace.tab_of(main));
            assert_eq!(shell.workspace.group_tabs(main), [main]);
            assert_eq!(shell.workspace.group_tabs(chat), [chat]);
            shell.split_session(SplitDir::Right, cx);
            chat
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, w, cx| {
            let split = shell.workspace.active.unwrap();
            assert_ne!(split, chat);
            assert_eq!(shell.session_working_copy(split, cx), feature.path);
            assert_eq!(shell.workspace.tab_of(split), shell.workspace.tab_of(chat));
            let feature_tab = shell.workspace.tab_of(chat).unwrap();
            assert_eq!(shell.workspace.layout(feature_tab).leaf_ids().len(), 2);
            // Tabs of different worktrees are different scopes: never folded.
            assert!(!shell.merge_scope_tabs(cx));
            assert_ne!(shell.workspace.tab_of(main), Some(feature_tab));
            // Switching worktrees swaps the layouts.
            shell.select_worktree(None, w, cx);
            assert_eq!(shell.workspace.active, Some(main));
            let main_tab = shell.workspace.tab_of(main).unwrap();
            assert_eq!(shell.visible_tabs(cx), [TitleTab::Session(main_tab)]);
            shell.select_worktree(Some(feature.clone()), w, cx);
            assert_eq!(shell.workspace.active_tab(), Some(feature_tab));
            assert_eq!(shell.visible_tabs(cx), [TitleTab::Session(feature_tab)]);
            assert!(!shell.merge_scope_tabs(cx));
            // A chat of the main checkout's layout opened here (a sidebar
            // click) moves into this layout alone; main keeps the rest.
            shell.select_worktree(None, w, cx);
            let second = add_session(shell, w, cx);
            assert_eq!(shell.workspace.group_tabs(main), [main, second]);
            shell.select_worktree(Some(feature.clone()), w, cx);
            shell.open(main, w, cx);
            assert_eq!(shell.working_copy(), feature.path);
            assert_eq!(shell.workspace.tab_of(main), shell.workspace.tab_of(chat));
            assert_eq!(shell.workspace.group_tabs(second), [second]);
            assert_ne!(shell.workspace.tab_of(second), shell.workspace.tab_of(chat));
            assert!(!shell.merge_scope_tabs(cx));
            shell.select_worktree(None, w, cx);
            assert_eq!(shell.workspace.active, Some(second));
        })
        .unwrap();
}
