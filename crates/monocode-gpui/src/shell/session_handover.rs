//! Deleting a chat keeps what it opened. Every file, terminal and browser
//! page is a pane owned by the chat that opened it (docs/native/tab-groups.md),
//! so its panes pass to another chat of the project; only when none is left
//! do they close, asking first about unsaved files and running shells.
use super::*;
use crate::file_editor::{FileEditor, FileEvent};
use monocode_core::files::path_key;
use std::{cell::Cell, rc::Rc};

impl Shell {
    /// The file, terminal and browser panes `session` owns.
    fn owned_panes(&self, session: i64) -> Vec<i64> {
        let ws = &self.workspace;
        let mut panes: Vec<i64> = (ws.review_owners.iter())
            .filter(|(_, owner)| **owner == session)
            .map(|(pane, _)| *pane)
            .chain(
                (ws.terminal_panes.iter()).filter_map(|(p, t)| (t.owner == session).then_some(*p)),
            )
            .chain(
                (ws.browser_panes.iter()).filter_map(|(p, b)| (b.owner == session).then_some(*p)),
            )
            .collect();
        panes.sort_unstable();
        panes.dedup();
        panes
    }

    /// The chat that takes over `session`'s `panes`: an open one in the
    /// same group, else in the scope's title tab, else open anywhere in the
    /// project, else any chat of the project, unarchived first.
    pub(super) fn pane_heir(&self, session: i64, panes: &[i64]) -> Option<i64> {
        let project = path_key(&self.session_summaries.get(&session)?.cwd);
        let ws = &self.workspace;
        let mine = || std::iter::once(session).chain(panes.iter().copied());
        let near: HashSet<i64> = mine().flat_map(|id| ws.group_tabs(id)).collect();
        let tabs: HashSet<i64> = mine().filter_map(|id| ws.tab_of(id)).collect();
        (self.session_summaries.iter())
            .filter(|(id, summary)| {
                **id > 0
                    && **id != session
                    && !self.deleting.contains(*id)
                    && path_key(&summary.cwd) == project
            })
            .min_by_key(|(id, summary)| {
                let tab = ws.tab_of(**id);
                let rank = if near.contains(*id) {
                    0
                } else if tab.is_some_and(|tab| tabs.contains(&tab)) {
                    1
                } else if tab.is_some() {
                    2
                } else if !summary.archived {
                    3
                } else {
                    4
                };
                let recent = self.history_recency.get(*id).copied().unwrap_or(0);
                (rank, std::cmp::Reverse(recent), std::cmp::Reverse(**id))
            })
            .map(|(id, _)| *id)
    }

    /// Before `session` is deleted: give its panes to an heir, or close
    /// them. True when the delete can go on now; false when it waits for
    /// an answer (it then deletes again by itself) or was cancelled.
    pub(super) fn hand_over_panes(
        &mut self,
        session: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panes = self.owned_panes(session);
        if panes.is_empty() {
            return true;
        }
        if let Some(heir) = self.pane_heir(session, &panes) {
            self.rehome_panes(&panes, heir);
            return true;
        }
        let editors: Vec<Entity<FileEditor>> = (panes.iter())
            .filter_map(|pane| self.reviews.get(pane))
            .flat_map(|review| review.read(cx).unsaved_editors(cx))
            .collect();
        if editors.is_empty() {
            self.close_panes_then_delete(session, window, cx);
            return false;
        }
        let names = (editors.iter())
            .map(|e| {
                std::path::Path::new(&e.read(cx).path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>()
            .join(", ");
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &format!("Save changes to {names}?"),
            Some(
                "Deleting the session closes the files it opened. Your unsaved edits will be \
                 lost if you discard them.",
            ),
            &["Save", "Discard", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match answer {
                Ok(0) => this.save_then_delete(session, editors, window, cx),
                Ok(1) => this.close_panes_then_delete(session, window, cx),
                _ => {}
            });
        })
        .detach();
        false
    }

    fn rehome_panes(&mut self, panes: &[i64], heir: i64) {
        let ws = &mut self.workspace;
        for pane in panes {
            if let Some(owner) = ws.review_owners.get_mut(pane) {
                *owner = heir;
            }
            if let Some(terminal) = ws.terminal_panes.get_mut(pane) {
                terminal.owner = heir;
            }
            if let Some(page) = ws.browser_panes.get_mut(pane) {
                page.owner = heir;
            }
        }
    }

    /// Save `editors`, then go on with the delete; a failed save stops it.
    fn save_then_delete(
        &mut self,
        session: i64,
        editors: Vec<Entity<FileEditor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let left = Rc::new(Cell::new(editors.len()));
        let stopped = Rc::new(Cell::new(false));
        for editor in editors {
            let (left, stopped) = (left.clone(), stopped.clone());
            cx.subscribe_in(
                &editor,
                window,
                move |this, _, event: &FileEvent, window, cx| {
                    if stopped.get() {
                        return;
                    }
                    match event {
                        FileEvent::Saved => {
                            left.set(left.get().saturating_sub(1));
                            if left.get() == 0 {
                                stopped.set(true);
                                this.close_panes_then_delete(session, window, cx);
                            }
                        }
                        FileEvent::SaveFailed => {
                            stopped.set(true);
                            this.error = Some(
                                "Could not save a file, so the session was not deleted".into(),
                            );
                            cx.notify();
                        }
                        _ => {}
                    }
                },
            )
            .detach();
            editor.update(cx, |e, cx| e.save(cx));
        }
    }

    /// Close `session`'s panes (asking about running shells), then delete
    /// it. A chat that became an heir meanwhile takes them instead.
    fn close_panes_then_delete(
        &mut self,
        session: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = self.owned_panes(session);
        if let Some(heir) = self.pane_heir(session, &panes) {
            self.rehome_panes(&panes, heir);
            self.delete_session(session, window, cx);
            return;
        }
        let asked = panes.clone();
        self.confirm_close_terminal_panes(&asked, window, cx, move |this, window, cx| {
            for pane in panes {
                if this.workspace.tab_of(pane).is_some() {
                    this.finish_close_pane(pane, window, cx);
                }
                if let Some(review) = this.reviews.remove(&pane) {
                    review.update(cx, |r, cx| r.cache_closed(cx));
                }
                let ws = &mut this.workspace;
                ws.reviews.remove(&pane);
                ws.review_owners.remove(&pane);
                ws.terminal_panes.remove(&pane);
                ws.browser_panes.remove(&pane);
                this.terminal_pane_views.remove(&pane);
                this.browser_pages.remove(&pane);
            }
            this.fit_sidebar(cx);
            this.persist(cx);
            this.delete_session(session, window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        review::ReviewTab,
        shell::tests::window,
        workspace::{BrowserPane, TerminalPane},
    };
    use gpui::{EntityInputHandler, TestAppContext, VisualTestContext};

    fn file(name: &str) -> ReviewTab {
        ReviewTab::File {
            cwd: "/work/monocode".into(),
            path: format!("/work/monocode/{name}"),
        }
    }

    /// A new chat of the shell's project, opened in its scope's group.
    fn add_session(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) -> i64 {
        let summary = locked(&shell.store)
            .unwrap()
            .create("codex", &shell.workspace.project)
            .unwrap();
        let id = summary.id;
        shell.add(summary, cx);
        shell.open_in_workspace(id, OpenIn::default(), window, cx);
        id
    }

    /// A terminal and a browser pane of `owner`, stacked out of sight in
    /// the group of `beside`.
    fn add_shell_panes(shell: &mut Shell, owner: i64, beside: i64) -> (i64, i64) {
        let terminal = shell.workspace.next_pane_id();
        shell.workspace.terminal_panes.insert(
            terminal,
            TerminalPane {
                owner,
                cwd: "/work/monocode".into(),
            },
        );
        shell.workspace.add_to_group(terminal, beside, None, false);
        let page = shell.workspace.next_pane_id();
        shell.workspace.browser_panes.insert(
            page,
            BrowserPane {
                owner,
                url: "about:blank".into(),
                ..Default::default()
            },
        );
        shell.workspace.add_to_group(page, beside, None, false);
        (terminal, page)
    }

    fn known(window: gpui::WindowHandle<Shell>, cx: &mut VisualTestContext, id: i64) -> bool {
        window
            .update(cx, |shell, _, _| shell.session_summaries.contains_key(&id))
            .unwrap()
    }

    #[gpui::test]
    fn deleting_a_chat_hands_its_panes_to_a_chat_of_its_group(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, other, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let (heir, pane, terminal, page) = window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                let heir = add_session(shell, w, cx);
                shell.open(session, w, cx);
                shell.open_review(file("a.rs"), w, cx);
                let pane = shell.workspace.active.unwrap();
                assert_eq!(shell.workspace.review_owners[&pane], session);
                let (terminal, page) = add_shell_panes(shell, session, pane);
                shell.delete_session(session, w, cx);
                assert!(shell.error.is_none(), "{:?}", shell.error);
                // Handed over at once, never to another project's chat.
                assert_eq!(shell.workspace.review_owners[&pane], heir);
                assert_ne!(heir, other);
                (heir, pane, terminal, page)
            })
            .unwrap();
        visual.run_until_parked();
        assert!(!known(window, visual, session), "the chat is deleted");
        window
            .update(visual, |shell, _, _| {
                let ws = &shell.workspace;
                assert_eq!(ws.review_owners[&pane], heir);
                assert!(ws.tab_of(pane).is_some(), "the file stays open");
                assert_eq!(ws.terminal_panes[&terminal].owner, heir);
                assert_eq!(ws.browser_panes[&page].owner, heir);
                assert!(ws.tab_of(heir).is_some());
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_heir_prefers_the_chats_group_then_its_tab_then_the_project(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open(session, w, cx);
                let closed = add_session(shell, w, cx);
                let grouped = add_session(shell, w, cx);
                // `closed` leaves every tab; `grouped` stays beside the chat.
                shell.workspace.close_pane(closed);
                shell.open(session, w, cx);
                shell.open_review(file("a.rs"), w, cx);
                let pane = shell.workspace.active.unwrap();
                assert_eq!(shell.pane_heir(session, &[pane]), Some(grouped));
                // Out of the chat's group but still in its title tab.
                let before = shell.workspace.tab_of(session);
                shell.workspace.place(grouped, pane, Edge::Bottom);
                assert_eq!(shell.workspace.tab_of(grouped), before);
                assert!(!shell.workspace.group_tabs(session).contains(&grouped));
                assert_eq!(shell.pane_heir(session, &[pane]), Some(grouped));
                // With no open chat left, any chat of the project.
                shell.workspace.close_pane(grouped);
                let heir = shell.pane_heir(session, &[pane]);
                assert!(heir == Some(closed) || heir == Some(grouped));
                shell.deleting.insert(closed);
                shell.deleting.insert(grouped);
                assert_eq!(shell.pane_heir(session, &[pane]), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_projects_last_chat_closes_its_panes_and_asks_about_unsaved_files(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let dir = std::env::temp_dir().join(format!("monocode-handover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let (pane, terminal) = window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.open_review(
                    ReviewTab::File {
                        cwd: "/work/monocode".into(),
                        path: path.to_string_lossy().into_owned(),
                    },
                    w,
                    cx,
                );
                let pane = shell.workspace.active.unwrap();
                let (terminal, _) = add_shell_panes(shell, session, pane);
                (pane, terminal)
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                shell.reviews[&pane].update(cx, |review, cx| {
                    review.navigate_file(1, 4, cx);
                    let input = review.active_editor().unwrap().read(cx).input.clone();
                    input.update(cx, |i, cx| i.replace_text_in_range(None, " edited", w, cx));
                });
            })
            .unwrap();
        visual.run_until_parked();
        let delete = |visual: &mut VisualTestContext| {
            window
                .update(visual, |shell, w, cx| shell.delete_session(session, w, cx))
                .unwrap();
            visual.run_until_parked();
        };
        delete(visual);
        assert!(visual.has_pending_prompt());
        visual.simulate_prompt_answer("Cancel");
        visual.run_until_parked();
        assert!(known(window, visual, session), "Cancel keeps the chat");
        window
            .update(visual, |shell, _, _| {
                assert_eq!(shell.workspace.review_owners[&pane], session);
                assert!(shell.workspace.tab_of(pane).is_some());
            })
            .unwrap();

        delete(visual);
        visual.simulate_prompt_answer("Save");
        visual.run_until_parked();
        assert!(std::fs::read_to_string(&path).unwrap().contains(" edited"));
        assert!(!known(window, visual, session), "the chat is deleted");
        window
            .update(visual, |shell, _, _| {
                let ws = &shell.workspace;
                assert!(!ws.review_owners.contains_key(&pane));
                assert!(ws.tab_of(pane).is_none(), "its file closed");
                assert!(!ws.terminal_panes.contains_key(&terminal));
                assert!(!shell.reviews.contains_key(&pane));
            })
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn archiving_a_chat_keeps_the_files_it_opened(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = window(cx);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        let pane = window
            .update(visual, |shell, w, cx| {
                shell.open(session, w, cx);
                shell.open_review(file("a.rs"), w, cx);
                let pane = shell.workspace.active.unwrap();
                shell.toggle_archive(session, cx);
                pane
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |shell, _, cx| {
                assert!(shell.session_metadata(session, cx).summary.archived);
                assert!(shell.workspace.tab_of(session).is_none());
                assert!(
                    shell.workspace.tab_of(pane).is_some(),
                    "the file stays open"
                );
                assert_eq!(shell.workspace.review_owners[&pane], session);
            })
            .unwrap();
    }
}
