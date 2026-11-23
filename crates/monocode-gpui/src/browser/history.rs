//! A page's browsing history (docs/native/browser/context.md K55, K65):
//! every web page a tab commits is a visit of the tab's project, kept in its
//! profile's store (`monocode_core::browser::history`). Throwaway profiles
//! (K22) keep nothing, and a restored tab's first load is not a new visit.
//!
//! The stores are one app-wide [`HistoryGlobal`]: `<data>/Browser/History`,
//! or memory when the data mode has no folder.

use super::*;
use monocode_core::browser::history::Stores;
pub(crate) use monocode_core::browser::history::Transition;

struct HistoryGlobal(Rc<RefCell<Stores>>);
impl Global for HistoryGlobal {}

/// Where the stores live, once (the first shell to make a page says).
pub(crate) fn init(dir: Option<PathBuf>, cx: &mut App) {
    if !cx.has_global::<HistoryGlobal>() {
        cx.set_global(HistoryGlobal(Rc::new(RefCell::new(Stores::new(dir)))));
    }
}

/// The app's history stores.
pub(crate) fn stores(cx: &mut App) -> Rc<RefCell<Stores>> {
    init(None, cx);
    cx.global::<HistoryGlobal>().0.clone()
}

/// Now, in the stores' milliseconds.
pub(crate) fn now_ms() -> i64 {
    monocode_core::time::now_ms() as i64
}

/// The store a profile's visits go to; `None` for a throwaway one.
pub(crate) fn store_name(profile: &Profile) -> Option<String> {
    match profile {
        Profile::Default => Some(monocode_core::browser::history::DEFAULT_PROFILE.to_owned()),
        Profile::Isolated(id) => Some(id.clone()),
        Profile::Ephemeral(_) => None,
    }
}

/// Where a page's visits go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PageHistory {
    /// The project's key (`files::path_key` of its folder).
    pub project: String,
    /// The profile's store.
    pub store: String,
}

/// A page's history bookkeeping.
#[derive(Default)]
pub(crate) struct Recorder {
    target: Option<PageHistory>,
    /// How the next committed address came about, when the page knows.
    next: Option<Transition>,
    /// The address a restored page reopens at: its first commit there is
    /// not a new visit.
    restored: Option<String>,
    /// The address last recorded, so one commit reported twice counts once.
    last: Option<String>,
}

impl BrowserPage {
    /// Where the page's visits go, if anywhere. A page made for a tab that
    /// was saved (not opened in this run) is `restored`; one an agent opened
    /// starts with an agent's visit.
    pub(crate) fn set_history(
        &mut self,
        target: Option<PageHistory>,
        restored: bool,
        by_agent: bool,
    ) {
        self.history = Recorder {
            target,
            next: by_agent.then_some(Transition::Agent),
            restored: (restored && !self.url.is_empty()).then(|| self.url.clone()),
            last: None,
        };
    }

    /// Where the page's visits go (tests and the new tab page).
    pub(crate) fn history_target(&self) -> Option<&PageHistory> {
        self.history.target.as_ref()
    }

    /// The next address the page commits came about this way.
    pub(super) fn next_transition(&mut self, transition: Transition) {
        self.history.next = Some(transition);
    }

    /// The page committed `url` (Chromium's address change).
    pub(super) fn record_visit(&mut self, url: &str, cx: &mut App) {
        let Some(target) = self.history.target.clone() else {
            return;
        };
        if self
            .history
            .restored
            .take()
            .is_some_and(|saved| saved == url)
        {
            self.history.last = Some(url.to_owned());
            self.history.next = None;
            return;
        }
        let transition = self.history.next.take().unwrap_or_else(|| {
            if self.active_agent().is_some() {
                Transition::Agent
            } else {
                Transition::Link
            }
        });
        let again = self.history.last.as_deref() == Some(url);
        self.history.last = Some(url.to_owned());
        // A reload is the same visit; so is one commit reported twice.
        if transition == Transition::Reload || (again && transition == Transition::Link) {
            return;
        }
        let stores = stores(cx);
        let now = now_ms();
        let mut stores = stores.borrow_mut();
        // The title comes later: the page's title event, or its load's end
        // (a same-document navigation keeps the title it had).
        let recorded = (stores.store(&target.store, now))
            .and_then(|store| store.record(&target.project, url, "", transition, now));
        if let Err(error) = recorded {
            log::debug!("browser history: {error}");
        }
    }

    /// A load ended: the page has its title, and a reload that committed
    /// nothing new is over.
    pub(super) fn load_ended(&mut self, cx: &mut App) {
        if self.history.next == Some(Transition::Reload) {
            self.history.next = None;
        }
        self.record_title(cx);
    }

    /// The page's title changed, or its load ended.
    pub(super) fn record_title(&self, cx: &mut App) {
        self.update_url_row(cx, |store, project, url, title| {
            store.set_title(project, url, title)
        });
    }

    /// The page named its icon.
    pub(super) fn record_favicon(&self, favicon: &str, cx: &mut App) {
        let favicon = favicon.to_owned();
        self.update_url_row(cx, move |store, project, url, _| {
            store.set_favicon(project, url, &favicon)
        });
    }

    fn update_url_row(
        &self,
        cx: &mut App,
        f: impl FnOnce(&monocode_core::browser::history::Store, &str, &str, &str) -> Result<(), String>,
    ) {
        let Some(target) = self.history.target.clone() else {
            return;
        };
        if self.history.last.as_deref() != Some(self.url.as_str()) {
            return;
        }
        let stores = stores(cx);
        let now = now_ms();
        let mut stores = stores.borrow_mut();
        let updated = (stores.store(&target.store, now))
            .and_then(|store| f(store, &target.project, &self.url, &self.title));
        if let Err(error) = updated {
            log::debug!("browser history: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::browser::history::{Order, Query};

    fn page(url: &str, cx: &mut gpui::TestAppContext) -> Entity<BrowserPage> {
        cx.new(|cx| BrowserPage::new(url.into(), String::new(), 100, "/tmp".into(), cx))
    }

    fn target() -> PageHistory {
        PageHistory {
            project: "/p".into(),
            store: "default".into(),
        }
    }

    fn visits(cx: &mut gpui::TestAppContext) -> Vec<(String, String, Transition)> {
        cx.update(|cx| {
            let stores = stores(cx);
            let mut stores = stores.borrow_mut();
            let mut visits = stores.visits(&Query::default(), now_ms());
            visits.reverse();
            visits
                .into_iter()
                .map(|v| (v.url, v.title, v.transition))
                .collect()
        })
    }

    fn event(
        page: &Entity<BrowserPage>,
        event: monocode_cef::BrowserEvent,
        cx: &mut gpui::TestAppContext,
    ) {
        page.update(cx, |page, cx| page.on_event(event, cx));
    }

    #[gpui::test]
    fn a_page_records_its_visits_with_title_and_how_they_came(cx: &mut gpui::TestAppContext) {
        use monocode_cef::BrowserEvent::*;
        let page = page("", cx);
        page.update(cx, |page, _| page.set_history(Some(target()), false, false));
        // Typed in the address bar.
        page.update(cx, |page, cx| page.load("https://a.test/".into(), cx));
        event(&page, Address("https://a.test/".into()), cx);
        event(&page, Title("A".into()), cx);
        event(&page, Favicon("https://a.test/icon.png".into()), cx);
        // The same commit reported again, and a reload: one visit.
        event(&page, Address("https://a.test/".into()), cx);
        page.update(cx, |page, _| page.reload(false));
        event(&page, Address("https://a.test/".into()), cx);
        // A link, then an agent's navigation.
        event(&page, Address("https://a.test/b".into()), cx);
        page.update(cx, |page, cx| page.agent_used(7, "Agent".into(), cx));
        event(&page, Address("https://c.test/".into()), cx);
        // Not web pages.
        event(&page, Address("about:blank".into()), cx);
        event(&page, Address("data:text/html,x".into()), cx);
        assert_eq!(
            visits(cx),
            [
                ("https://a.test/".into(), "A".into(), Transition::Typed),
                ("https://a.test/b".into(), String::new(), Transition::Link),
                ("https://c.test/".into(), String::new(), Transition::Agent),
            ]
        );
        cx.update(|cx| {
            let stores = stores(cx);
            let entry = stores
                .borrow_mut()
                .entries(Some("/p"), "a.test/", Order::Recent, 10, now_ms())
                .into_iter()
                .find(|e| e.url == "https://a.test/")
                .unwrap();
            assert_eq!(entry.favicon.as_deref(), Some("https://a.test/icon.png"));
        });
    }

    #[gpui::test]
    fn restored_and_throwaway_pages_add_no_visit(cx: &mut gpui::TestAppContext) {
        use monocode_cef::BrowserEvent::*;
        // A tab saved at a.test loads there again: not a visit; where it
        // goes from there is.
        let restored = page("https://a.test/", cx);
        restored.update(cx, |page, _| page.set_history(Some(target()), true, false));
        event(&restored, Address("https://a.test/".into()), cx);
        event(&restored, Address("https://a.test/next".into()), cx);
        // An isolated agent's page records nothing.
        let throwaway = page("https://b.test/", cx);
        assert_eq!(store_name(&Profile::Ephemeral("agent-1".into())), None);
        throwaway.update(cx, |page, _| page.set_history(None, false, true));
        event(&throwaway, Address("https://b.test/".into()), cx);
        assert_eq!(
            visits(cx),
            [(
                "https://a.test/next".into(),
                String::new(),
                Transition::Link
            )]
        );
    }
}
