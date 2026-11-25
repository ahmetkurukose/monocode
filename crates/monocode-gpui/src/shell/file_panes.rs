//! File panes are tabs of the session pane tree's groups. Moving a pane
//! transfers its entity, preserving dirty documents, selection and scroll.
use super::*;

impl Shell {
    pub(super) fn open_file_pane(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let owner = self.workspace.session_for_pane(id);
        if self.deleting.contains(&owner) {
            return;
        }
        let Some(session) = self.ensure_session(owner, cx) else {
            return;
        };
        self.workspace.project = session.read(cx).summary.cwd.clone();
        if let Some(monos) = &self.monos {
            monos.update(cx, |m, cx| m.close(cx));
        }
        self.navigation_revision += 1;
        self.leave_app_views(cx);
        self.close_preview(cx);
        if self.workspace.tab_of(id).is_none() {
            self.place_new_tab(id, OpenIn::default(), cx);
        }
        self.workspace.open(id);
        self.workspace
            .project_focus
            .insert(self.workspace.project.clone(), id);
        self.record_worktree_session(id, cx);
        self.merge_scope_tabs(cx);
        self.history.record(id);
        self.review_focused = true;
        if let Some(review) = self.review_for(id, cx) {
            review.update(cx, |r, cx| r.focus_active(window, cx));
        }
        self.remember_title_tab(TitleTab::Session(self.workspace.tab_of(id).unwrap()));
        self.reveal_active_tab(cx);
        self.persist(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{review::ReviewTab, shell::tests::window};
    use gpui::{EntityInputHandler, TestAppContext, VisualTestContext, point};

    #[gpui::test]
    fn the_explorer_is_told_the_file_in_front_and_shown_on_request(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let front = |shell: &Shell, cx: &App| {
            let panel = shell.right_panel.as_ref()?;
            let explorer = panel.read(cx).explorer.clone();
            explorer.read(cx).front().map(str::to_owned)
        };
        let file = |name: &str| ReviewTab::File {
            cwd: "/work/monocode".into(),
            path: format!("/work/monocode/{name}"),
        };
        window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.open_review(file("a.rs"), w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        // The panel made after the file was opened starts on that file.
        window
            .update(visual, |shell, w, cx| shell.ensure_right_panel(w, cx))
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert_eq!(front(shell, cx).as_deref(), Some("/work/monocode/a.rs"));
                shell.open_review(file("b.rs"), w, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(front(shell, cx).as_deref(), Some("/work/monocode/b.rs"));
                // The tab's menu asks for the row: the panel opens on Explorer.
                shell.workspace.right_panel_open = false;
                shell.pending_explorer_reveal = Some("/work/monocode/a.rs".into());
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert!(shell.pending_explorer_reveal.is_none());
                assert!(shell.workspace.right_panel_open || shell.workspace.classic_layout);
                let panel = shell.right_panel.as_ref().unwrap();
                assert_eq!(panel.read(cx).tab(), crate::right_panel::RightTab::Files);
                assert_eq!(front(shell, cx).as_deref(), Some("/work/monocode/a.rs"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_file_tab_opens_its_files_history_and_escape_closes_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                // What a file tab's "Git History" leaves for the next frame.
                shell.pending_file_history =
                    Some(("/work/monocode".into(), "/work/monocode/a.rs".into(), None));
                cx.notify();
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("file-history-card").is_some());
        window
            .update(visual, |shell, _, _| {
                assert!(shell.pending_file_history.is_none());
                assert!(shell.file_history.is_some());
            })
            .unwrap();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(visual.debug_bounds("file-history-card").is_none());
        window
            .update(visual, |shell, _, _| assert!(shell.file_history.is_none()))
            .unwrap();
    }

    #[gpui::test]
    fn mono_file_link_opens_visible_editor_and_preserves_its_chat(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let folder =
            std::env::temp_dir().join(format!("monocode-mono-file-link-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("README.md").to_string_lossy().into_owned();
        std::fs::write(&path, "# Heading\nsecond line\nthird line\n").unwrap();
        window
            .update(cx, |shell, w, cx| {
                shell
                    .mono_manager()
                    .update(cx, |m, cx| m.create(Vec::new(), w, cx));
            })
            .unwrap();
        cx.run_until_parked();
        let (mono, chat) = window
            .update(cx, |shell, _, cx| {
                assert!(shell.mono_active(cx));
                let mono = shell.workspace.active.unwrap();
                let chat = shell.mono_manager().read(cx).chat().unwrap();
                chat.read(cx).input.clone().update(cx, |input, cx| {
                    input.set_text("Keep my Mono draft".into(), cx)
                });
                (mono, chat)
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("mono-composer").is_some(),
            "the action starts in the visible Mono"
        );
        window
            .update(visual, |_, w, cx| {
                w.dispatch_action(
                    Box::new(crate::file_editor::OpenPath(format!("{path}:2:3"))),
                    cx,
                );
            })
            .unwrap();
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("review-frame").is_some(),
            "the file pane is rendered"
        );
        // The file opens beside the Mono, which stays on screen.
        assert!(visual.debug_bounds("mono-composer").is_some());
        let (pane, editor) = window
            .update(visual, |shell, w, cx| {
                assert!(!shell.mono_active(cx));
                let pane = shell.workspace.active.unwrap();
                assert_ne!(pane, session, "the file has its own title tab");
                assert_eq!(shell.workspace.session_for_pane(pane), session);
                let tab = shell.workspace.tab_of(pane);
                assert_eq!(shell.workspace.tab_of(mono), tab, "beside the Mono");
                assert_eq!(shell.workspace.tab_of(session), tab);
                let review = shell.active_review(cx).unwrap();
                assert!(review.read(cx).state.chat_hidden);
                let editor = review
                    .read(cx)
                    .active_editor()
                    .expect("the file editor is mounted");
                assert_eq!(editor.read(cx).path, path);
                assert!(
                    editor.read(cx).focus_handle(cx).contains_focused(w, cx),
                    "focus is inside the visible editor"
                );
                assert_eq!(
                    editor.read(cx).input.read(cx).selection_range(),
                    12..12,
                    "line and column survive navigation"
                );
                editor.read(cx).input.clone().update(cx, |input, cx| {
                    input.replace_text_in_range(None, "unsaved", w, cx)
                });
                shell.open(mono, w, cx);
                (pane, editor)
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("mono-composer").is_some());
        window
            .update(visual, |shell, w, cx| {
                assert_eq!(shell.workspace.active, Some(mono));
                let restored = shell.mono_manager().read(cx).chat().unwrap();
                assert_eq!(
                    restored, chat,
                    "the original live Mono conversation is retained"
                );
                assert_eq!(
                    restored.read(cx).input.read(cx).content.as_ref(),
                    "Keep my Mono draft"
                );
                assert!(restored.read(cx).input.focus_handle(cx).is_focused(w));
                w.dispatch_action(Box::new(crate::file_editor::OpenPath(path.clone())), cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert_eq!(shell.workspace.active, Some(pane));
                let reopened = shell
                    .active_review(cx)
                    .unwrap()
                    .read(cx)
                    .active_editor()
                    .unwrap();
                assert_eq!(
                    reopened, editor,
                    "reopening keeps the existing dirty editor"
                );
                assert!(reopened.read(cx).dirty);
                assert!(reopened.read(cx).focus_handle(cx).contains_focused(w, cx));
                assert!(shell.workspace.tab_of(mono).is_some());
            })
            .unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[gpui::test]
    fn file_word_wrap_is_shared_and_persisted_across_restart(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        let folder =
            std::env::temp_dir().join(format!("monocode-wrap-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for file in ["one.txt", "two.txt"] {
            std::fs::write(folder.join(file), "a long line ".repeat(50)).unwrap();
        }
        window
            .update(cx, |shell, w, cx| {
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: folder.join("one.txt").to_string_lossy().into_owned(),
                    },
                    w,
                    cx,
                );
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        window
            .update(visual, |_, w, cx| {
                w.dispatch_action(Box::new(crate::file_editor::ToggleWordWrap), cx)
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert!(!shell.workspace.preferences.file_word_wrap);
                assert!(
                    !shell.reviews[&shell.workspace.active.unwrap()]
                        .read(cx)
                        .active_editor()
                        .unwrap()
                        .read(cx)
                        .input
                        .read(cx)
                        .word_wrap()
                );
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: folder.join("two.txt").to_string_lossy().into_owned(),
                    },
                    w,
                    cx,
                );
            })
            .unwrap();
        visual.run_until_parked();
        let (store, summaries, saved, project) = window
            .update(visual, |shell, _, cx| {
                assert!(
                    !shell.reviews[&shell.workspace.active.unwrap()]
                        .read(cx)
                        .active_editor()
                        .unwrap()
                        .read(cx)
                        .input
                        .read(cx)
                        .word_wrap(),
                    "new files inherit the setting"
                );
                let store = locked(&shell.store).unwrap();
                let saved = store.ui_state().unwrap();
                let persisted: Workspace = serde_json::from_str(saved.as_ref().unwrap()).unwrap();
                assert!(!persisted.preferences.file_word_wrap);
                (
                    shell.store.clone(),
                    store.sessions().unwrap(),
                    saved,
                    shell.workspace.project.clone(),
                )
            })
            .unwrap();
        let restored = visual.update(|_, cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, summaries, project, saved, cx))
            })
            .unwrap()
        });
        visual.run_until_parked();
        visual.refresh().unwrap();
        restored
            .update(visual, |shell, _, cx| {
                assert!(!shell.workspace.preferences.file_word_wrap);
                let editor = shell.reviews[&shell.workspace.active.unwrap()]
                    .read(cx)
                    .active_editor()
                    .unwrap();
                assert!(
                    !editor.read(cx).input.read(cx).word_wrap(),
                    "restart restores the setting"
                );
            })
            .unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[gpui::test]
    fn numbered_shortcuts_pick_the_tabs_of_the_focused_group(cx: &mut TestAppContext) {
        cx.update(|cx| {
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        let (window, _, session) = window(cx);
        let file = window
            .update(cx, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: "/missing/shortcut-test.txt".into(),
                    },
                    w,
                    cx,
                );
                let file = shell.workspace.active.unwrap();
                // The file stacked behind the chat, in its group.
                shell.workspace.add_to_group(file, session, None, false);
                shell.open(session, w, cx);
                assert_eq!(shell.workspace.group_tabs(session), [session, file]);
                file
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        // A restored file pane has saved state before its review is made.
        for restored in [false, true] {
            if restored {
                window
                    .update(visual, |shell, _, _| {
                        shell.reviews.remove(&file);
                    })
                    .unwrap();
            }
            visual.simulate_keystrokes(&format!("{modifier}-2"));
            visual.run_until_parked();
            window
                .update(visual, |shell, _, _| {
                    assert_eq!(shell.workspace.active, Some(file));
                    assert_eq!(shell.workspace.group_tabs(file), [session, file]);
                })
                .unwrap();
            visual.simulate_keystrokes(&format!("{modifier}-1"));
            visual.run_until_parked();
            window
                .update(visual, |shell, w, cx| {
                    assert_eq!(
                        shell.workspace.active,
                        Some(session),
                        "Cmd+1 returns to the chat (restored={restored})"
                    );
                    assert!(
                        shell.sessions[&session]
                            .read(cx)
                            .input
                            .focus_handle(cx)
                            .is_focused(w)
                    );
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn a_chat_tab_dragged_beside_its_file_keeps_draft_and_dirty_edits(cx: &mut TestAppContext) {
        cx.update(|cx| {
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, session) = window(cx);
        let path = std::env::temp_dir().join(format!(
            "monocode-chat-pane-drag-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "original\n").unwrap();
        let (chat, file, review) = window
            .update(cx, |shell, w, cx| {
                let chat = shell.sessions[&session].clone();
                chat.read(cx).input.clone().update(cx, |input, cx| {
                    input.set_text("Keep this draft".into(), cx);
                });
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: path.to_string_lossy().into_owned(),
                    },
                    w,
                    cx,
                );
                let file = shell.workspace.active.unwrap();
                (chat, file, shell.reviews[&file].clone())
            })
            .unwrap();
        cx.run_until_parked();
        let editor = window
            .update(cx, |_, w, cx| {
                let editor = review.read(cx).active_editor().unwrap();
                editor.read(cx).input.clone().update(cx, |input, cx| {
                    input.replace_text_in_range(None, "unsaved ", w, cx);
                });
                editor
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let leaves = |visual: &mut VisualTestContext| {
            window
                .update(visual, |shell, _, _| {
                    let tab = shell.workspace.tab_of(session).unwrap();
                    shell.workspace.layout(tab).leaf_ids()
                })
                .unwrap()
        };
        assert_eq!(
            leaves(visual),
            [session, file],
            "the file opened beside the chat"
        );
        let grip_selector = Box::leak(format!("group-tab-{session}").into_boxed_str());
        let grip = visual.debug_bounds(grip_selector).unwrap().center();
        // Clicks, tiny moves and outside drops move nothing.
        for end in [grip, grip + point(px(2.), px(0.)), point(px(6.), px(100.))] {
            visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
            visual.simulate_mouse_move(end, MouseButton::Left, Default::default());
            visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
        }
        visual.run_until_parked();
        assert_eq!(leaves(visual), [session, file]);
        let bounds = window
            .update(visual, |shell, _, _| shell.pane_bounds.get().unwrap())
            .unwrap();
        let right = point(bounds.right() - px(12.), bounds.center().y);
        visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
        visual.simulate_mouse_move(right, MouseButton::Left, Default::default());
        visual.simulate_mouse_up(right, MouseButton::Left, Default::default());
        visual.run_until_parked();
        assert_eq!(leaves(visual), [file, session]);
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.active, Some(session));
                assert_eq!(shell.sessions[&session], chat);
                assert_eq!(
                    chat.read(cx).input.read(cx).content.as_ref(),
                    "Keep this draft"
                );
                assert_eq!(shell.reviews[&file], review);
                assert_eq!(review.read(cx).active_editor().unwrap(), editor);
                assert!(editor.read(cx).dirty);
                let saved = serde_json::to_string(&shell.workspace).unwrap();
                let sessions: Vec<_> = shell
                    .sessions
                    .iter()
                    .map(|(id, s)| (*id, s.read(cx).summary.cwd.clone()))
                    .collect();
                let restored =
                    Workspace::restore(Some(&saved), &sessions, &shell.workspace.project);
                let tab = shell.workspace.tab_of(session).unwrap();
                assert_eq!(restored.layout(tab), shell.workspace.layout(tab));
                assert_eq!(restored.active, Some(session));
                assert_eq!(restored.review_owners, shell.workspace.review_owners);
            })
            .unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[gpui::test]
    fn a_chat_tab_dropped_on_each_edge_of_its_file_splits_there(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let (window, _, session) = window(cx);
            let file = window
                .update(cx, |shell, w, cx| {
                    shell.open_review(
                        ReviewTab::File {
                            cwd: shell.workspace.project.clone(),
                            path: "/tmp/chat-pane-edge.rs".into(),
                        },
                        w,
                        cx,
                    );
                    shell.workspace.active.unwrap()
                })
                .unwrap();
            let visual = &mut VisualTestContext::from_window(window.into(), cx);
            visual.run_until_parked();
            let grip = visual
                .debug_bounds(Box::leak(format!("group-tab-{session}").into_boxed_str()))
                .unwrap()
                .center();
            let file_bounds = visual.debug_bounds("review-frame").unwrap();
            let end = match edge {
                Edge::Left => point(file_bounds.left() + px(12.), file_bounds.center().y),
                Edge::Right => point(file_bounds.right() - px(12.), file_bounds.center().y),
                Edge::Top => point(file_bounds.center().x, file_bounds.top() + px(12.)),
                Edge::Bottom => point(file_bounds.center().x, file_bounds.bottom() - px(12.)),
            };
            visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
            visual.simulate_mouse_move(end, MouseButton::Left, Default::default());
            window
                .update(visual, |shell, _, _| {
                    assert_eq!(shell.pane_drag.as_ref().unwrap().over, Some((file, edge)));
                })
                .unwrap();
            visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
            visual.run_until_parked();
            window
                .update(visual, |shell, _, _| {
                    let tab = shell.workspace.tab_of(session).unwrap();
                    let leaves = shell.workspace.layout(tab).leaves();
                    let chat_rect = leaves.iter().find(|l| l.id == session).unwrap().rect;
                    let file_rect = leaves.iter().find(|l| l.id == file).unwrap().rect;
                    assert_eq!(shell.workspace.active, Some(session));
                    assert!(match edge {
                        Edge::Left => chat_rect.x < file_rect.x,
                        Edge::Right => chat_rect.x > file_rect.x,
                        Edge::Top => chat_rect.y < file_rect.y,
                        Edge::Bottom => chat_rect.y > file_rect.y,
                    });
                })
                .unwrap();
        }
    }

    /// The explorer's eye flips Settings > Appearance > Show excluded files.
    #[gpui::test]
    fn the_explorer_eye_toggles_ignored_files(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                assert!(shell.workspace.preferences.show_excluded_files);
                shell.ensure_right_panel(w, cx);
                let explorer = shell
                    .right_panel
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .explorer
                    .clone();
                explorer.update(cx, |_, cx| cx.emit(crate::explorer::ToggleExcluded));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(!shell.workspace.preferences.show_excluded_files);
                assert!(!crate::settings::show_excluded_files(cx), "search follows");
                let explorer = shell
                    .right_panel
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .explorer
                    .clone();
                explorer.update(cx, |_, cx| cx.emit(crate::explorer::ToggleExcluded));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.workspace.preferences.show_excluded_files);
                assert!(crate::settings::show_excluded_files(cx));
            })
            .unwrap();
    }

    /// The old Settings > General > File tabs choice is read but no longer
    /// decides anything: every file is a pane of its own beside the chat.
    #[gpui::test]
    fn every_file_is_its_own_pane_whatever_the_old_file_tabs_setting(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        for (setting, name) in [("pane", "beside.txt"), ("workspace", "top.txt")] {
            window
                .update(cx, |shell, w, cx| {
                    shell.workspace.preferences.file_tabs = setting.into();
                    shell.open(session, w, cx);
                    shell.open_review(
                        ReviewTab::File {
                            cwd: shell.workspace.project.clone(),
                            path: format!("/tmp/{name}"),
                        },
                        w,
                        cx,
                    );
                    let pane = shell.workspace.active.unwrap();
                    assert_ne!(pane, session, "the file has its own pane");
                    assert_eq!(shell.workspace.review_owners.get(&pane), Some(&session));
                    assert!(!shell.workspace.reviews.contains_key(&session));
                    assert!(shell.reviews[&pane].read(cx).state.chat_hidden);
                    assert_eq!(
                        shell.workspace.tab_of(pane),
                        shell.workspace.tab_of(session),
                        "in the chat's title tab, not one of its own"
                    );
                })
                .unwrap();
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn a_file_tab_dragged_out_and_closed_keeps_dirty_editors_for_reopening(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let path =
            std::env::temp_dir().join(format!("monocode-pane-drag-{}.txt", std::process::id()));
        std::fs::write(&path, "original\n").unwrap();
        let file = path.to_string_lossy().into_owned();
        let (pane, review) = window
            .update(cx, |shell, w, cx| {
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: file.clone(),
                    },
                    w,
                    cx,
                );
                let pane = shell.workspace.active.unwrap();
                (pane, shell.reviews[&pane].clone())
            })
            .unwrap();
        cx.run_until_parked();
        let editor = window
            .update(cx, |_, w, cx| {
                let editor = review.read(cx).active_editor().unwrap();
                let input = editor.read(cx).input.clone();
                input.update(cx, |i, cx| i.replace_text_in_range(None, "unsaved ", w, cx));
                editor
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let tab = window
            .update(visual, |shell, _, _| {
                shell.workspace.tab_of(session).unwrap()
            })
            .unwrap();
        // Its grip reorders the panes of the split.
        let grip = visual
            .debug_bounds(Box::leak(format!("group-tab-{pane}").into_boxed_str()))
            .unwrap()
            .center();
        let bounds = window
            .update(visual, |shell, _, _| shell.pane_bounds.get().unwrap())
            .unwrap();
        let left = point(bounds.left() + px(12.), bounds.center().y);
        visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
        visual.simulate_mouse_move(left, MouseButton::Left, Default::default());
        visual.simulate_mouse_up(left, MouseButton::Left, Default::default());
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.layout(tab).leaf_ids(), [pane, session]);
                assert_eq!(shell.reviews[&pane], review);
                assert_eq!(review.read(cx).active_editor().unwrap(), editor);
                assert!(editor.read(cx).dirty);
                let saved = serde_json::to_string(&shell.workspace).unwrap();
                let sessions: Vec<_> = shell
                    .sessions
                    .iter()
                    .map(|(id, s)| (*id, s.read(cx).summary.cwd.clone()))
                    .collect();
                let restored =
                    Workspace::restore(Some(&saved), &sessions, &shell.workspace.project);
                assert_eq!(restored.layout(tab).leaf_ids(), [pane, session]);
                assert_eq!(restored.review_owners, shell.workspace.review_owners);
                let remaining: Vec<_> = sessions
                    .into_iter()
                    .filter(|(id, _)| *id != session)
                    .collect();
                let pruned = Workspace::restore(Some(&saved), &remaining, &shell.workspace.project);
                assert!(!pruned.review_owners.contains_key(&pane));
                assert!(pruned.tab_of(pane).is_none());
            })
            .unwrap();
        // A closed title tab retains its dirty documents for reopening.
        window
            .update(visual, |shell, w, cx| shell.close(session, w, cx))
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert!(shell.workspace.tab_of(pane).is_none());
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: file.clone(),
                    },
                    w,
                    cx,
                );
                assert_eq!(shell.workspace.active, Some(pane));
                assert_eq!(shell.reviews[&pane], review);
                assert_eq!(review.read(cx).active_editor().unwrap(), editor);
                assert!(editor.read(cx).dirty);
            })
            .unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[gpui::test]
    fn files_stack_in_one_group_and_reorder_keeping_their_editors(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                for name in ["a.rs", "b.rs", "c.rs"] {
                    shell.open_review(
                        ReviewTab::File {
                            cwd: shell.workspace.project.clone(),
                            path: format!("/tmp/{name}"),
                        },
                        w,
                        cx,
                    );
                }
                let tabs = shell.workspace.group_tabs(shell.workspace.active.unwrap());
                let label = |shell: &Shell, pane: i64, cx: &App| {
                    let state = &shell.reviews[&pane].read(cx).state;
                    assert_eq!(state.tabs.len(), 1, "one file per pane");
                    state.tabs[0].label()
                };
                let names = |shell: &Shell, cx: &App| {
                    (shell.workspace.group_tabs(tabs[0]).iter())
                        .map(|pane| label(shell, *pane, cx))
                        .collect::<Vec<_>>()
                };
                assert_eq!(names(shell, cx), ["a.rs", "b.rs", "c.rs"]);
                let editor = shell.reviews[&tabs[0]].read(cx).active_editor();
                assert!(shell.workspace.move_in_group(tabs[0], 2));
                assert_eq!(names(shell, cx), ["b.rs", "c.rs", "a.rs"]);
                assert_eq!(shell.reviews[&tabs[0]].read(cx).active_editor(), editor);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_file_tab_dropped_on_each_edge_of_its_chat_splits_there(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let (pane, review) = window
            .update(cx, |shell, w, cx| {
                shell.open_review(
                    ReviewTab::File {
                        cwd: shell.workspace.project.clone(),
                        path: "/tmp/pane-edge.rs".into(),
                    },
                    w,
                    cx,
                );
                let pane = shell.workspace.active.unwrap();
                (pane, shell.reviews[&pane].clone())
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        for edge in [Edge::Left, Edge::Top, Edge::Bottom, Edge::Right] {
            let grip = visual
                .debug_bounds(Box::leak(format!("group-tab-{pane}").into_boxed_str()))
                .unwrap()
                .center();
            let target = visual
                .debug_bounds(Box::leak(format!("pane-{session}").into_boxed_str()))
                .unwrap();
            let end = match edge {
                Edge::Left => point(target.left() + px(12.), target.center().y),
                Edge::Right => point(target.right() - px(12.), target.center().y),
                // Below the chat's strip, which would take the file as a tab.
                Edge::Top => point(target.center().x, target.top() + px(TITLE_BAR_HEIGHT + 12.)),
                Edge::Bottom => point(target.center().x, target.bottom() - px(12.)),
            };
            visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
            visual.simulate_mouse_move(end, MouseButton::Left, Default::default());
            visual.simulate_mouse_up(end, MouseButton::Left, Default::default());
            visual.run_until_parked();
            window
                .update(visual, |shell, _, _| {
                    assert_eq!(shell.workspace.active, Some(pane));
                    assert_eq!(shell.workspace.review_owners.len(), 1);
                    assert_eq!(shell.reviews[&pane], review);
                    let tab = shell.workspace.tab_of(session).unwrap();
                    let leaves = shell.workspace.layout(tab).leaves();
                    let chat = leaves.iter().find(|l| l.id == session).unwrap().rect;
                    let file = leaves.iter().find(|l| l.id == pane).unwrap().rect;
                    assert!(match edge {
                        Edge::Left => file.x < chat.x,
                        Edge::Right => file.x > chat.x,
                        Edge::Top => file.y < chat.y,
                        Edge::Bottom => file.y > chat.y,
                    });
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn add_to_chat_from_a_file_in_its_own_tab_brings_the_chat_forward(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("main.rs").to_string_lossy().into_owned();
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let cwd = folder.path().to_string_lossy().into_owned();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let pane = window
            .update(visual, |shell, w, cx| {
                shell.open_file_at(cwd, path, None, w, cx);
                let pane = shell.workspace.active.unwrap();
                // Stacked over the chat in its group: the chat is hidden.
                shell.workspace.add_to_group(pane, session, None, true);
                shell.open_file_pane(pane, w, cx);
                pane
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert_eq!(shell.workspace.active, Some(pane));
                assert_ne!(shell.workspace.visible_of(session), Some(session));
                shell.reviews[&pane].update(cx, |_, cx| {
                    cx.emit(crate::review::ReviewEvent::Comment(
                        "@main.rs (lines 1-1)".into(),
                    ))
                });
            })
            .unwrap();
        visual.run_until_parked();
        visual.refresh().unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                let chat = shell.sessions[&session].read(cx);
                assert_eq!(chat.input.read(cx).content, "@main.rs (lines 1-1)\n\n");
                assert_eq!(
                    shell.workspace.active,
                    Some(session),
                    "the chat that got the text is shown"
                );
                assert!(chat.input.focus_handle(cx).is_focused(w));
            })
            .unwrap();
    }
}
