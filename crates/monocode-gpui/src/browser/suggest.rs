//! The address bar's suggestions (docs/native/browser/context.md K55):
//! while the user types, the app's open tabs that match and the history's
//! pages by frecency, this project's first. ↑/↓ choose, Enter opens the
//! chosen one (or what was typed), Esc closes the list.

use super::*;
use monocode_core::browser::history::Entry;
use new_tab::OpenTab;

/// Rows at most.
const MAX: usize = 8;
/// Open tabs at most, before the history's pages.
const MAX_TABS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Suggestion {
    /// Switch to this tab.
    Tab(OpenTab),
    /// A page of the history; `elsewhere` when another project visited it.
    Page {
        url: String,
        title: String,
        elsewhere: bool,
    },
}

impl Suggestion {
    fn url(&self) -> &str {
        match self {
            Suggestion::Tab(tab) => &tab.url,
            Suggestion::Page { url, .. } => url,
        }
    }
}

/// The address bar's list.
#[derive(Default)]
pub(crate) struct Suggestions {
    pub(super) items: Vec<Suggestion>,
    pub(super) selected: Option<usize>,
    /// The address field has the keyboard.
    pub(super) focused: bool,
    pub(super) anchor: AnchorSlot,
    pub(super) _focus_out: Option<Subscription>,
}

/// Whether every word of `typed` is in `url` or `title`.
fn matches(typed: &str, url: &str, title: &str) -> bool {
    let (url, title) = (url.to_lowercase(), title.to_lowercase());
    typed
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|word| url.contains(&word) || title.contains(&word))
}

/// The list for `typed`: matching open tabs, then history pages not open
/// already; `project` is this page's project.
pub(super) fn suggestions(
    typed: &str,
    tabs: Vec<OpenTab>,
    pages: Vec<Entry>,
    project: Option<&str>,
) -> Vec<Suggestion> {
    if typed.trim().is_empty() {
        return Vec::new();
    }
    let mut items: Vec<Suggestion> = tabs
        .into_iter()
        .filter(|tab| !tab.url.is_empty() && matches(typed, &tab.url, &tab.title))
        .take(MAX_TABS)
        .map(Suggestion::Tab)
        .collect();
    for page in pages {
        if items.len() >= MAX {
            break;
        }
        if items.iter().any(|item| item.url() == page.url) {
            continue;
        }
        items.push(Suggestion::Page {
            elsewhere: project.is_some_and(|p| p != page.project),
            url: page.url,
            title: page.title,
        });
    }
    items
}

impl BrowserPage {
    /// The address field's text changed: the list follows it while the
    /// user types there.
    pub(super) fn address_changed(&mut self, cx: &mut Context<Self>) {
        let typed = self.address.read(cx).content.to_string();
        if !self.suggest.focused || typed.trim().is_empty() || typed == self.url {
            self.close_suggestions(cx);
            return;
        }
        let tabs = (self.links.as_ref()).map_or_else(Vec::new, |links| (links.open_tabs)(cx));
        let project = self.history_target().map(|t| t.project.clone());
        let pages = {
            let stores = history::stores(cx);
            let mut stores = stores.borrow_mut();
            stores.suggest(
                project.as_deref().unwrap_or(""),
                &typed,
                MAX,
                history::now_ms(),
            )
        };
        self.suggest.items = suggestions(&typed, tabs, pages, project.as_deref());
        self.suggest.selected = None;
        cx.notify();
    }

    pub(super) fn close_suggestions(&mut self, cx: &mut Context<Self>) -> bool {
        if self.suggest.items.is_empty() {
            return false;
        }
        self.suggest.items.clear();
        self.suggest.selected = None;
        cx.notify();
        true
    }

    /// ↑/↓ in the address field: the list's choice, else the new tab page's.
    pub(super) fn step_address(&mut self, step: isize, cx: &mut Context<Self>) -> bool {
        if self.suggest.items.is_empty() {
            return self.step_new_tab(step, cx);
        }
        let len = self.suggest.items.len();
        self.suggest.selected = new_tab::step_selection(self.suggest.selected, step, len);
        cx.notify();
        true
    }

    /// Enter in the address field: the chosen suggestion or new tab row
    /// opens; false leaves the typed text to the field.
    pub(super) fn submit_address(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(item) = (self.suggest.selected).and_then(|at| self.suggest.items.get(at)) {
            let item = item.clone();
            self.pick_suggestion(item, cx);
            return true;
        }
        self.close_suggestions(cx);
        // A new tab row opens only while nothing is typed over it.
        let blank = self.address.read(cx).content.trim().is_empty();
        blank && self.open_new_tab_selection(cx)
    }

    fn pick_suggestion(&mut self, item: Suggestion, cx: &mut Context<Self>) {
        self.close_suggestions(cx);
        match item {
            Suggestion::Tab(tab) => {
                // Back to the address the page is at; the other tab shows.
                let url = self.url.clone();
                self.address.update(cx, |field, cx| field.set_text(url, cx));
                cx.emit(PageRequest::ShowTab(tab.pane));
            }
            Suggestion::Page { url, .. } => {
                self.address
                    .update(cx, |field, cx| field.set_text(url.clone(), cx));
                self.load(url, cx);
            }
        }
    }

    /// The list under the address field, while there is one.
    pub(super) fn suggestions_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.suggest.items.is_empty() {
            return None;
        }
        let width = (self.suggest.anchor.get()).map_or(480., |b| f32::from(b.size.width));
        let weak = cx.weak_entity();
        let rows: Vec<AnyElement> = (self.suggest.items.iter().enumerate())
            .map(|(ix, item)| self.suggestion_row(ix, item.clone(), cx))
            .collect();
        Some(
            popover(
                "browser-suggestions",
                Anchor::Slot(self.suggest.anchor.clone()),
            )
            .align(Align::Start)
            .width(width.max(320.))
            .padding(4.)
            .without_animation()
            .on_dismiss(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.close_suggestions(cx);
                });
            })
            .child(
                div()
                    .id("browser-suggestions-list")
                    .debug_selector(|| "browser-suggestions".into())
                    .role(gpui::Role::ListBox)
                    .aria_label("Suggestions")
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .into_any_element(),
        )
    }

    fn suggestion_row(&self, ix: usize, item: Suggestion, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.suggest.selected == Some(ix);
        let (glyph, title, url, note) = match &item {
            Suggestion::Tab(tab) => (
                "app-window",
                page_title(&tab.url, &tab.title),
                tab.url.clone(),
                Some("Switch to tab"),
            ),
            Suggestion::Page {
                url,
                title,
                elsewhere,
            } => (
                "clock",
                page_title(url, title),
                url.clone(),
                elsewhere.then_some("Other project"),
            ),
        };
        div()
            .id(("browser-suggestion", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(gpui::SharedString::from(title.clone()))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(28.))
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(content(0.1)))
            .hover(|s| s.bg(content(0.07)))
            .child(icon(glyph, 13.).text_color(content(0.45)))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(260.))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(content(0.9))
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(url),
            )
            .children(note.map(|note| {
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(content(0.45))
                    .child(note)
            }))
            // On press: the field keeps the keyboard, and the list closes
            // with it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.pick_suggestion(item.clone(), cx);
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, project: &str) -> Entry {
        Entry {
            url: url.into(),
            title: String::new(),
            favicon: None,
            project: project.into(),
            visit_count: 1,
            typed_count: 0,
            last_visit: 0,
        }
    }

    fn tab(pane: i64, url: &str, title: &str) -> OpenTab {
        OpenTab {
            pane,
            url: url.into(),
            title: title.into(),
        }
    }

    #[test]
    fn open_tabs_come_first_then_history_pages_not_open_already() {
        let tabs = vec![
            tab(-1, "http://localhost:5173/", "Vite App"),
            tab(-2, "https://docs.rs/", "Docs"),
            tab(-3, "", ""),
        ];
        let pages = vec![
            entry("http://localhost:5173/", "/p"),
            entry("http://localhost:5173/login", "/p"),
            entry("http://localhost:8080/", "/q"),
        ];
        let items = suggestions("localhost", tabs.clone(), pages, Some("/p"));
        assert_eq!(
            items,
            [
                Suggestion::Tab(tabs[0].clone()),
                Suggestion::Page {
                    url: "http://localhost:5173/login".into(),
                    title: String::new(),
                    elsewhere: false,
                },
                Suggestion::Page {
                    url: "http://localhost:8080/".into(),
                    title: String::new(),
                    elsewhere: true,
                },
            ]
        );
        // A tab matches by its title too, any case.
        let by_title = suggestions("vite APP", tabs, Vec::new(), Some("/p"));
        assert_eq!(by_title.len(), 1);
        assert!(suggestions(" ", Vec::new(), vec![entry("a", "/p")], None).is_empty());
        let many: Vec<Entry> = (0..20)
            .map(|i| entry(&format!("https://{i}.test/"), "/p"))
            .collect();
        assert_eq!(suggestions("test", Vec::new(), many, None).len(), MAX);
    }

    #[gpui::test]
    fn typing_lists_suggestions_and_enter_opens_the_chosen_one(cx: &mut gpui::TestAppContext) {
        use monocode_core::browser::history::Transition;
        let page =
            cx.new(|cx| BrowserPage::new(String::new(), String::new(), 100, "/tmp".into(), cx));
        cx.update(|cx| {
            let stores = history::stores(cx);
            let now = history::now_ms();
            let mut stores = stores.borrow_mut();
            let store = stores.store("default", now).unwrap();
            store
                .record("/p", "https://github.com/me", "", Transition::Typed, now)
                .unwrap();
        });
        let requests = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let requests = requests.clone();
            cx.subscribe(&page, move |_, request: &PageRequest, _| {
                if let PageRequest::ShowTab(pane) = request {
                    requests.borrow_mut().push(*pane);
                }
            })
            .detach();
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
            page.links = Some(new_tab::ShellLinks {
                open_tabs: Rc::new(|_| vec![tab(-9, "https://github.com/other", "GitHub")]),
                dev_roots: Rc::new(|_| Vec::new()),
            });
            page.suggest.focused = true;
            page.address
                .update(cx, |field, cx| field.set_text("git".into(), cx));
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            assert_eq!(page.suggest.items.len(), 2, "{:?}", page.suggest.items);
            assert!(page.step_address(1, cx));
            assert!(page.step_address(1, cx));
            assert_eq!(page.suggest.selected, Some(1));
            assert!(page.submit_address(cx));
            assert_eq!(page.url, "https://github.com/me");
            assert!(page.suggest.items.is_empty());
            // A tab suggestion asks the shell to show that tab.
            page.address
                .update(cx, |field, cx| field.set_text("other".into(), cx));
        });
        cx.run_until_parked();
        page.update(cx, |page, cx| {
            assert!(page.step_address(1, cx));
            assert!(page.submit_address(cx));
            assert_eq!(page.url, "https://github.com/me");
        });
        cx.run_until_parked();
        assert_eq!(*requests.borrow(), [-9]);
    }
}
