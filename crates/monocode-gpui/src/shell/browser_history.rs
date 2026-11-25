//! The shell's side of the browser's history and new tab page
//! (docs/native/browser/context.md K55, K66): where the history is kept,
//! the history window (⌘Y), and what a page learns of the app: the tabs
//! its address bar can switch to and the processes of its project, whose
//! listening ports are the new tab page's dev servers.
use super::*;
use crate::browser::{
    history_view::{self, HistoryView},
    new_tab::{OpenTab, ShellLinks},
};
use monocode_core::browser::dev_servers::Root;
use monocode_core::files::path_key;
use std::rc::Rc;

pub(super) struct HistoryOpen {
    view: Entity<HistoryView>,
    restore: Option<FocusHandle>,
    _events: Subscription,
}

impl Shell {
    /// `<data>/Browser History`, beside Chromium's `Browser` folder (which
    /// Chromium owns); none (memory) without a data folder.
    pub(super) fn browser_history_dir(&self) -> Option<std::path::PathBuf> {
        locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(|dir| dir.join("Browser History")))
    }

    /// `<data>/Browser Passwords`, beside Chromium's `Browser` folder: the
    /// password vaults (K56); none (memory) without a data folder.
    /// The data mode's folder, for the browser's extensions (K69).
    pub(super) fn browser_data_dir(&self) -> Option<std::path::PathBuf> {
        locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(std::path::Path::to_path_buf))
    }

    pub(super) fn browser_passwords_dir(&self) -> Option<std::path::PathBuf> {
        locked(&self.store)
            .ok()
            .and_then(|store| store.data_dir().map(|dir| dir.join("Browser Passwords")))
    }

    /// What the page of `pane` learns from this shell.
    pub(super) fn browser_page_links(&self, pane: i64, cx: &mut Context<Self>) -> ShellLinks {
        let tabs = cx.weak_entity();
        let roots = cx.weak_entity();
        ShellLinks {
            open_tabs: Rc::new(move |cx| {
                (tabs.upgrade())
                    .map_or_else(Vec::new, |shell| shell.read(cx).open_browser_tabs(pane, cx))
            }),
            dev_roots: Rc::new(move |cx| {
                (roots.upgrade())
                    .map_or_else(Vec::new, |shell| shell.read(cx).dev_server_roots(pane, cx))
            }),
        }
    }

    /// The browser tabs of this window other than `pane`'s.
    fn open_browser_tabs(&self, pane: i64, cx: &App) -> Vec<OpenTab> {
        let mut tabs: Vec<OpenTab> = (self.workspace.browser_panes.iter())
            .filter(|(id, _)| {
                **id != pane
                    && (self.workspace.tab_of(**id).is_some() || self.workspace.browse.owns(**id))
            })
            .map(|(id, state)| {
                let (url, title) = match self.browser_pages.get(id) {
                    Some((page, _)) => {
                        let page = page.read(cx);
                        (page.url.clone(), page.title.clone())
                    }
                    None => (state.url.clone(), state.title.clone()),
                };
                OpenTab {
                    pane: *id,
                    url,
                    title,
                }
            })
            .collect();
        tabs.sort_by_key(|tab| tab.pane.unsigned_abs());
        tabs
    }

    /// The project of `pane`'s page: its owner session's folder, as a key.
    fn browser_pane_project(&self, pane: i64) -> Option<String> {
        let owner = self.workspace.browser_panes.get(&pane)?.owner;
        Some(path_key(&self.session_summaries.get(&owner)?.cwd))
    }

    /// The processes of `pane`'s project: its running runs and the shells
    /// of its terminals (the dock's and the pane tree's).
    fn dev_server_roots(&self, pane: i64, cx: &App) -> Vec<Root> {
        let Some(project) = self.browser_pane_project(pane) else {
            return Vec::new();
        };
        let mut roots = Vec::new();
        for info in monocode_core::code::services::services().runs().list() {
            let Some(pid) = info.pid.filter(|_| info.state.is_running()) else {
                continue;
            };
            let folder = info.checkout.to_string_lossy().into_owned();
            let owner = crate::code_shell::context::checkout_for_path(&folder, cx)
                .map_or(folder, |checkout| checkout.project);
            if path_key(&owner) == project {
                roots.push(Root {
                    pid,
                    name: info.label.clone(),
                    url: info.url.clone(),
                });
            }
        }
        let shell_root = |terminal: &Entity<crate::terminal::Terminal>| {
            terminal.read(cx).shell_pid().map(|pid| Root {
                pid,
                name: "Terminal".into(),
                url: None,
            })
        };
        for (folder, terminals) in &self.terminals {
            if path_key(folder) == project {
                roots.extend(terminals.iter().filter_map(shell_root));
            }
        }
        for (id, terminal) in &self.terminal_pane_views {
            let here = (self.workspace.terminal_panes.get(id))
                .and_then(|state| self.session_summaries.get(&state.owner))
                .is_some_and(|owner| path_key(&owner.cwd) == project);
            if here {
                roots.extend(shell_root(terminal));
            }
        }
        roots
    }

    /// ⌘Y: the history window, this project's visits first.
    pub(super) fn open_browser_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.browser_history {
            window.focus(&open.view.focus_handle(cx), cx);
            return;
        }
        crate::browser::history::init(self.browser_history_dir(), cx);
        let restore = window.focused(cx);
        self.close_menu(cx);
        // Browser mode's own pages first in Browser mode, else the project's.
        let own = (self.workspace.browse.active)
            .is_some_and(|pane| self.browser_mode() && self.workspace.browse.owns(pane));
        let project = if own {
            Some(super::modes::BROWSE_HISTORY_PROJECT.to_string())
        } else {
            (!self.workspace.project.is_empty()).then(|| path_key(&self.workspace.project))
        };
        let view = cx.new(|cx| HistoryView::new(project, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &history_view::Event, window, cx| match event {
                history_view::Event::Open(url) => {
                    let url = url.clone();
                    this.close_browser_history(window, cx);
                    this.open_history_page(url, window, cx);
                }
                history_view::Event::Dismiss => {
                    this.close_browser_history(window, cx);
                }
            },
        );
        window.focus(&view.focus_handle(cx), cx);
        self.browser_history = Some(HistoryOpen {
            view,
            restore,
            _events: events,
        });
        cx.notify();
    }

    pub(super) fn close_browser_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(open) = self.browser_history.take() else {
            return false;
        };
        match open.restore {
            Some(restore) => window.focus(&restore, cx),
            None => self.focus_active(window, cx),
        }
        cx.notify();
        true
    }

    /// A page picked in the history: in the focused page, or a new one.
    fn open_history_page(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_browser_pane()
            && let Some(page) = self.browser_page(pane, cx)
        {
            page.update(cx, |page, cx| page.open_address(url, cx));
            self.focus_browser_pane(window, cx);
            return;
        }
        if self.browser_mode() {
            self.browse_new_tab(Some(url), None, true, window, cx);
            return;
        }
        self.new_browser_pane(Some(url), window, cx);
    }

    pub(super) fn browser_history_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.browser_history.as_ref()?;
        let viewport = window.viewport_size();
        let width = 760f32.min(viewport.width.as_f32() - 32.);
        let anchor = Anchor::Point(gpui::point(
            px((viewport.width.as_f32() - width) / 2.),
            px((viewport.height.as_f32() * 0.12).max(32.)),
        ));
        let weak = cx.weak_entity();
        Some(
            div()
                .absolute()
                .size_full()
                .child(crate::modal::scrim("browser-history-scrim"))
                .child(
                    popover("browser-history-modal", anchor)
                        .modal_surface(self.workspace.boxy_layout())
                        .width(width)
                        .gap(0.)
                        .priority(80)
                        .on_dismiss(move |window, cx| {
                            let _ =
                                weak.update(cx, |this, cx| this.close_browser_history(window, cx));
                        })
                        .child(open.view.clone()),
                )
                .into_any_element(),
        )
    }

    /// The history window's view, while open (tests).
    #[cfg(test)]
    pub(super) fn browser_history_view(&self) -> Option<Entity<HistoryView>> {
        self.browser_history.as_ref().map(|open| open.view.clone())
    }
}

#[cfg(test)]
mod tests {
    use monocode_core::files::path_key;

    #[gpui::test]
    fn cmd_t_on_a_page_opens_a_new_tab_page_of_its_project(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, session) = super::super::tests::window(cx);
        let first = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx);
                shell.workspace.active.unwrap()
            })
            .unwrap();
        cx.run_until_parked();
        let m = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(window.into(), &format!("{m}-t"));
        cx.run_until_parked();
        let (pane, page) = window
            .update(cx, |shell, _, cx| {
                let pane = shell.workspace.active.unwrap();
                assert_ne!(pane, first);
                assert_eq!(shell.workspace.tab_of(pane), shell.workspace.tab_of(first));
                let state = &shell.workspace.browser_panes[&pane];
                assert!(state.url.is_empty() && state.fresh);
                let page = shell.browser_pages[&pane].0.clone();
                {
                    let page = page.read(cx);
                    // A GPUI page: no Chromium page until it navigates.
                    assert!(page.showing_new_tab());
                    assert!(!page.has_view());
                    let cwd = &shell.session_summaries[&session].cwd;
                    assert_eq!(page.history_target().unwrap().project, path_key(cwd));
                }
                // The address bar can switch to the other page.
                let tabs = shell.open_browser_tabs(pane, cx);
                assert_eq!(tabs.len(), 1);
                assert_eq!(tabs[0].pane, first);
                (pane, page)
            })
            .unwrap();
        page.update(cx, |page, cx| {
            page.open_address("https://a.test/".into(), cx)
        });
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(!page.read(cx).showing_new_tab());
                assert_eq!(shell.workspace.browser_panes[&pane].url, "https://a.test/");
            })
            .unwrap();
    }

    #[gpui::test]
    fn cmd_y_opens_the_history_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (window, _, session) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| shell.open(session, window, cx))
            .unwrap();
        cx.run_until_parked();
        let m = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(window.into(), &format!("{m}-y"));
        cx.run_until_parked();
        let open = window
            .update(cx, |shell, _, _| shell.browser_history_view().is_some())
            .unwrap();
        assert!(open);
        cx.simulate_keystrokes(window.into(), "escape");
        cx.run_until_parked();
        let open = window
            .update(cx, |shell, _, _| shell.browser_history_view().is_some())
            .unwrap();
        assert!(!open);
    }
}
