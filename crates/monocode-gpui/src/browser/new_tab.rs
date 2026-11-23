//! The new tab page (docs/native/browser/context.md K66): what a blank tab
//! shows instead of a web page, drawn by GPUI. It lists the project's
//! running dev servers (the local ports its runs and terminals listen on),
//! the pages it opened lately and the ones it visits most. The address
//! field keeps the keyboard; ↑/↓ walk the list and Enter opens.
//!
//! No Chromium page exists for it: one is made only when the tab navigates.
//! Agents see the tab as `about:blank` (shell/browser_agent.rs).

use super::*;
use gpui::SharedString;
use monocode_core::browser::{
    dev_servers::{self, DevServer, Root},
    history::{Entry, Order},
};

/// How often the dev servers are looked for while the page shows.
const PROBE_EVERY: Duration = Duration::from_secs(3);
/// Rows in "Recently opened" and "Most visited".
const ROWS: usize = 6;

/// A tab of the app the address bar can switch to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenTab {
    pub pane: i64,
    pub url: String,
    pub title: String,
}

/// What a page learns from its shell: the other tabs it may switch to and
/// the processes of its project, for its dev servers.
#[derive(Clone)]
pub(crate) struct ShellLinks {
    pub open_tabs: Lookup<OpenTab>,
    pub dev_roots: Lookup<Root>,
}

/// Asks the shell for a list.
pub(crate) type Lookup<T> = Rc<dyn Fn(&App) -> Vec<T>>;

/// The new tab page's state.
#[derive(Default)]
pub(crate) struct NewTab {
    servers: Vec<DevServer>,
    /// Dev servers were looked for at least once.
    looked: bool,
    recent: Vec<Entry>,
    most: Vec<Entry>,
    /// The row ↑/↓ chose, counting servers, then recent, then most visited.
    selected: Option<usize>,
    probe: Option<gpui::Task<()>>,
}

impl NewTab {
    /// Every row's address, in the order they show.
    fn urls(&self) -> Vec<String> {
        (self.servers.iter().map(|s| s.url.clone()))
            .chain(self.recent.iter().map(|e| e.url.clone()))
            .chain(self.most.iter().map(|e| e.url.clone()))
            .collect()
    }
}

/// Most-visited rows that are not already among the recent ones.
fn without(most: Vec<Entry>, recent: &[Entry]) -> Vec<Entry> {
    most.into_iter()
        .filter(|e| !recent.iter().any(|r| r.url == e.url))
        .collect()
}

/// The next row from `at` moving by `step`, wrapping; from none, the first
/// or last.
pub(super) fn step_selection(at: Option<usize>, step: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len = len as isize;
    Some(match at {
        Some(at) => (at as isize + step).rem_euclid(len) as usize,
        None if step < 0 => (len - 1) as usize,
        None => 0,
    })
}

impl BrowserPage {
    /// A blank tab that has no Chromium page: the new tab page shows.
    pub(crate) fn showing_new_tab(&self) -> bool {
        self.url.is_empty() && !self.has_view()
    }

    /// Reads the project's history again and looks for its dev servers,
    /// then again every few seconds while the page shows.
    fn refresh_new_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.history_target().cloned() {
            let stores = history::stores(cx);
            let now = history::now_ms();
            let mut stores = stores.borrow_mut();
            let recent = stores.entries(Some(&target.project), "", Order::Recent, ROWS, now);
            let most = stores.entries(Some(&target.project), "", Order::MostVisited, ROWS * 2, now);
            self.new_tab.most = without(most, &recent).into_iter().take(ROWS).collect();
            self.new_tab.recent = recent;
        }
        let roots = (self.links.as_ref()).map_or_else(Vec::new, |links| (links.dev_roots)(cx));
        let found = cx.background_executor().spawn(async move {
            if roots.is_empty() {
                return Vec::new();
            }
            dev_servers::find(&roots, &dev_servers::listeners(), &dev_servers::parents())
        });
        self.new_tab.probe = Some(cx.spawn(async move |this, cx| {
            let servers = found.await;
            let again = this.update(cx, |this, cx| {
                if this.new_tab.servers != servers || !this.new_tab.looked {
                    this.new_tab.servers = servers;
                    this.new_tab.looked = true;
                    this.clamp_new_tab_selection();
                    cx.notify();
                }
                this.showing_new_tab()
            });
            if !again.unwrap_or(false) {
                return;
            }
            cx.background_executor().timer(PROBE_EVERY).await;
            let _ = this.update(cx, |this, cx| {
                if this.showing_new_tab() {
                    this.refresh_new_tab(cx);
                } else {
                    this.new_tab.probe = None;
                }
            });
        }));
    }

    fn clamp_new_tab_selection(&mut self) {
        let len = self.new_tab.urls().len();
        if self.new_tab.selected.is_some_and(|s| s >= len) {
            self.new_tab.selected = len.checked_sub(1);
        }
    }

    /// ↑/↓ in the address field over the new tab page.
    pub(super) fn step_new_tab(&mut self, step: isize, cx: &mut Context<Self>) -> bool {
        if !self.showing_new_tab() {
            return false;
        }
        let len = self.new_tab.urls().len();
        self.new_tab.selected = step_selection(self.new_tab.selected, step, len);
        cx.notify();
        len > 0
    }

    /// Enter over the new tab page: the chosen row opens, when one is.
    pub(super) fn open_new_tab_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(url) = (self.new_tab.selected)
            .filter(|_| self.showing_new_tab())
            .and_then(|at| self.new_tab.urls().into_iter().nth(at))
        else {
            return false;
        };
        self.open_address(url, cx);
        true
    }

    /// Opens `url` as if typed: the address field shows it.
    pub(crate) fn open_address(&mut self, url: String, cx: &mut Context<Self>) {
        self.new_tab.selected = None;
        self.new_tab.probe = None;
        self.address
            .update(cx, |field, cx| field.set_text(url.clone(), cx));
        self.load(url, cx);
    }

    /// The page drawn in a blank tab's body.
    pub(super) fn new_tab_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.new_tab.probe.is_none() {
            self.refresh_new_tab(cx);
        }
        let mut row = 0usize;
        let selected = self.new_tab.selected;
        let mut next = || {
            row += 1;
            selected == Some(row - 1)
        };
        let servers: Vec<AnyElement> = (self.new_tab.servers.clone().into_iter())
            .map(|server| {
                let detail = if server.command.is_empty() {
                    server.name.clone()
                } else {
                    format!("{} · {}", server.name, server.command)
                };
                let label = format!("localhost:{}", server.port);
                self.new_tab_row("play", label, detail, server.url, next(), cx)
            })
            .collect();
        let recent: Vec<AnyElement> = (self.new_tab.recent.clone().into_iter())
            .map(|entry| self.entry_row(entry, next(), cx))
            .collect();
        let most: Vec<AnyElement> = (self.new_tab.most.clone().into_iter())
            .map(|entry| self.entry_row(entry, next(), cx))
            .collect();
        let servers_empty = if self.new_tab.looked {
            "No dev server of this project is running. Start one from Run or a terminal."
        } else {
            "Looking for dev servers…"
        };
        div()
            .id("browser-new-tab")
            .debug_selector(|| "browser-new-tab".into())
            .role(gpui::Role::Region)
            .aria_label("New Tab")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .px_6()
                    .py(px(40.))
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .child(section("Dev servers", servers, servers_empty))
                    .child(section(
                        "Recently opened",
                        recent,
                        "Pages you open in this project show here.",
                    ))
                    .when(!most.is_empty(), |d| {
                        d.child(section("Most visited", most, ""))
                    }),
            )
            .into_any_element()
    }

    fn entry_row(&self, entry: Entry, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let title = if entry.title.trim().is_empty() {
            page_title(&entry.url, "")
        } else {
            entry.title.clone()
        };
        let detail = entry.url.clone();
        self.new_tab_row("globe", title, detail, entry.url, selected, cx)
    }

    fn new_tab_row(
        &self,
        glyph: &'static str,
        label: String,
        detail: String,
        url: String,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("browser-new-tab-{url}"));
        div()
            .id(gpui::ElementId::Name(id))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(label.clone()))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py(px(7.))
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(content(0.08)))
            .hover(|s| s.bg(content(0.06)))
            .child(icon(glyph, 14.).text_color(content(0.45)))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(content(0.9))
                            .child(label),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(content(0.45))
                            .child(detail),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_address(url.clone(), cx)))
            .into_any_element()
    }
}

fn section(title: &'static str, rows: Vec<AnyElement>, empty: &'static str) -> impl IntoElement {
    let is_empty = rows.is_empty();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .px_3()
                .pb_1()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(content(0.45))
                .child(title),
        )
        .children(rows)
        .when(is_empty && !empty.is_empty(), |d| {
            d.child(
                div()
                    .px_3()
                    .text_size(px(12.))
                    .text_color(content(0.4))
                    .child(empty),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrows_walk_the_rows_and_wrap() {
        assert_eq!(step_selection(None, 1, 3), Some(0));
        assert_eq!(step_selection(None, -1, 3), Some(2));
        assert_eq!(step_selection(Some(2), 1, 3), Some(0));
        assert_eq!(step_selection(Some(0), -1, 3), Some(2));
        assert_eq!(step_selection(Some(1), 1, 0), None);
    }

    #[test]
    fn most_visited_leaves_out_what_recent_shows() {
        let entry = |url: &str| Entry {
            url: url.into(),
            title: String::new(),
            favicon: None,
            project: "/p".into(),
            visit_count: 1,
            typed_count: 0,
            last_visit: 0,
        };
        let most = without(vec![entry("a"), entry("b")], &[entry("a")]);
        assert_eq!(most, [entry("b")]);
    }

    #[gpui::test]
    fn a_blank_tab_shows_the_new_tab_page_without_a_chromium_page(cx: &mut gpui::TestAppContext) {
        use monocode_core::browser::history::Transition;
        let page =
            cx.new(|cx| BrowserPage::new(String::new(), String::new(), 100, "/tmp".into(), cx));
        cx.update(|cx| {
            let stores = history::stores(cx);
            let mut stores = stores.borrow_mut();
            let store = stores.store("default", history::now_ms()).unwrap();
            let now = history::now_ms();
            for (url, at) in [
                ("https://often.test/", now - 10),
                ("https://often.test/", now - 9),
                ("https://last.test/", now),
            ] {
                store.record("/p", url, "", Transition::Link, at).unwrap();
            }
            store
                .record("/q", "https://other.test/", "", Transition::Link, now)
                .unwrap();
        });
        page.update(cx, |page, cx| {
            page.set_history(
                Some(history::PageHistory {
                    project: "/p".into(),
                    store: "default".into(),
                }),
                false,
                false,
            );
            page.links = Some(ShellLinks {
                open_tabs: Rc::new(|_| Vec::new()),
                dev_roots: Rc::new(|_| Vec::new()),
            });
            assert!(page.showing_new_tab());
            page.refresh_new_tab(cx);
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            assert!(page.new_tab.looked);
            assert!(page.new_tab.servers.is_empty());
            // This project's pages only.
            assert_eq!(
                page.new_tab.urls(),
                ["https://last.test/", "https://often.test/"]
            );
            assert!(page.step_new_tab(1, cx));
            assert!(page.step_new_tab(1, cx));
            assert_eq!(page.new_tab.selected, Some(1));
            assert!(page.open_new_tab_selection(cx));
            assert_eq!(page.url, "https://often.test/");
            assert!(!page.showing_new_tab());
            // Nothing made a Chromium page yet: the next paint does.
            assert!(!page.has_view());
        });
    }
}
