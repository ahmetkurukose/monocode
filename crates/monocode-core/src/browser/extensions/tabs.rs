//! `chrome.tabs.Tab` and `chrome.windows.Window` over the app's tab model,
//! and `tabs.query`. The app makes a [`TabModel`] from its windows: every
//! browser page of a profile is a tab, every app window a window. Each
//! window has one active tab, as in Chrome: of the pages the user sees
//! there (a window can show several, in groups side by side), the one they
//! focused or brought up last.

use serde_json::{Value, json};

use super::pattern::{PatternSet, glob};

#[derive(Clone, Debug, PartialEq)]
pub struct TabInfo {
    pub id: i64,
    pub window_id: i64,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub fav_icon: Option<String>,
    pub audible: bool,
    pub muted: bool,
    pub discarded: bool,
    pub pinned: bool,
    /// Shown in its window now (its group's selected tab, on screen).
    pub shown: bool,
    /// When the user last focused or brought up the page (ms since the
    /// epoch, as `lastAccessed`).
    pub last_active: f64,
    /// The page's browser id while it has one.
    pub browser_id: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub id: i64,
    pub focused: bool,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabModel {
    /// In window order, then tab order.
    pub tabs: Vec<TabInfo>,
    pub windows: Vec<WindowInfo>,
    /// The window the user used last (`lastFocusedWindow`).
    pub last_focused: Option<i64>,
}

impl TabModel {
    /// The window's active tab: of its shown tabs the one used last; with
    /// none shown, the one used last at all.
    pub fn active_in(&self, window: i64) -> Option<i64> {
        let pick = |shown_only: bool| {
            (self.tabs.iter())
                .filter(|t| t.window_id == window && (!shown_only || t.shown))
                .max_by(|a, b| {
                    a.last_active
                        .partial_cmp(&b.last_active)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(b.id.cmp(&a.id))
                })
                .map(|t| t.id)
        };
        pick(true).or_else(|| pick(false))
    }

    pub fn is_active(&self, tab: &TabInfo) -> bool {
        self.active_in(tab.window_id) == Some(tab.id)
    }

    pub fn tab(&self, id: i64) -> Option<&TabInfo> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn index(&self, tab: &TabInfo) -> usize {
        (self.tabs.iter())
            .filter(|t| t.window_id == tab.window_id)
            .position(|t| t.id == tab.id)
            .unwrap_or(0)
    }

    /// The window `currentWindow` means for a caller in `caller_window`
    /// (its tab's, or for a background, the last focused).
    pub fn current_window(&self, caller_window: Option<i64>) -> Option<i64> {
        caller_window
            .or(self.last_focused)
            .or(self.windows.first().map(|w| w.id))
    }

    /// `chrome.tabs.Tab`; `url`, `title` and `favIconUrl` only when
    /// `reveal` (the `tabs` permission or a host permission for the tab).
    pub fn tab_json(&self, tab: &TabInfo, reveal: bool) -> Value {
        let active = self.is_active(tab);
        let mut v = json!({
            "id": tab.id,
            "index": self.index(tab),
            "windowId": tab.window_id,
            "openerTabId": Value::Null,
            "active": active,
            "highlighted": active,
            "selected": active,
            "pinned": tab.pinned,
            "audible": tab.audible,
            "discarded": tab.discarded,
            "autoDiscardable": true,
            "frozen": false,
            "mutedInfo": {"muted": tab.muted},
            "incognito": false,
            "status": if tab.loading { "loading" } else { "complete" },
            "groupId": -1,
            "width": self.windows.iter().find(|w| w.id == tab.window_id).map_or(0, |w| w.width),
            "height": self.windows.iter().find(|w| w.id == tab.window_id).map_or(0, |w| w.height),
            "lastAccessed": tab.last_active,
        });
        if reveal {
            v["url"] = tab.url.clone().into();
            v["title"] = tab.title.clone().into();
            if let Some(icon) = &tab.fav_icon {
                v["favIconUrl"] = icon.clone().into();
            }
        }
        if let Value::Object(map) = &mut v {
            map.remove("openerTabId");
        }
        v
    }

    pub fn window_json(
        &self,
        window: &WindowInfo,
        populate: Option<&dyn Fn(&TabInfo) -> bool>,
    ) -> Value {
        let mut v = json!({
            "id": window.id,
            "focused": window.focused,
            "top": window.top,
            "left": window.left,
            "width": window.width,
            "height": window.height,
            "incognito": false,
            "type": "normal",
            "state": "normal",
            "alwaysOnTop": false,
        });
        if let Some(reveal) = populate {
            v["tabs"] = Value::Array(
                (self.tabs.iter())
                    .filter(|t| t.window_id == window.id)
                    .map(|t| self.tab_json(t, reveal(t)))
                    .collect(),
            );
        }
        v
    }

    /// `tabs.query(info)` for a caller in `caller_window`: the matching
    /// tabs, in order. URL and title filters only see the tabs `reveal`
    /// allows (Chrome: they need the `tabs` permission or host access).
    pub fn query(
        &self,
        info: &Value,
        caller_window: Option<i64>,
        reveal: &dyn Fn(&TabInfo) -> bool,
    ) -> Vec<&TabInfo> {
        let current = self.current_window(caller_window);
        let urls: Option<PatternSet> = match &info["url"] {
            Value::String(s) => Some(PatternSet::parse([s.as_str()])),
            Value::Array(a) => Some(PatternSet::parse(a.iter().filter_map(Value::as_str))),
            _ => None,
        };
        let window_id = info["windowId"].as_i64();
        (self.tabs.iter())
            .filter(|t| {
                let active = self.is_active(t);
                let flag =
                    |name: &str, value: bool| info[name].as_bool().is_none_or(|w| w == value);
                flag("active", active)
                    && flag("highlighted", active)
                    && flag("pinned", t.pinned)
                    && flag("audible", t.audible)
                    && flag("discarded", t.discarded)
                    && flag("muted", t.muted)
                    && flag("currentWindow", Some(t.window_id) == current)
                    && flag("lastFocusedWindow", Some(t.window_id) == self.last_focused)
                    && match window_id {
                        // WINDOW_ID_CURRENT.
                        Some(-2) => Some(t.window_id) == current,
                        Some(w) => w == t.window_id,
                        None => true,
                    }
                    && info["status"]
                        .as_str()
                        .is_none_or(|s| s == if t.loading { "loading" } else { "complete" })
                    && info["index"]
                        .as_u64()
                        .is_none_or(|i| i as usize == self.index(t))
                    && info["windowType"].as_str().is_none_or(|w| w == "normal")
                    && info["groupId"].as_i64().is_none_or(|g| g == -1)
                    && urls.as_ref().is_none_or(|u| reveal(t) && u.matches(&t.url))
                    && info["title"]
                        .as_str()
                        .is_none_or(|g| reveal(t) && glob(g, &t.title))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: i64, window: i64, url: &str, shown: bool, last: f64) -> TabInfo {
        TabInfo {
            id,
            window_id: window,
            url: url.into(),
            title: format!("Tab {id}"),
            loading: false,
            fav_icon: None,
            audible: false,
            muted: false,
            discarded: false,
            pinned: false,
            shown,
            last_active: last,
            browser_id: Some(id as i32),
        }
    }

    fn window(id: i64, focused: bool) -> WindowInfo {
        WindowInfo {
            id,
            focused,
            left: 0,
            top: 0,
            width: 1200,
            height: 800,
        }
    }

    fn model() -> TabModel {
        TabModel {
            tabs: vec![
                tab(1, 1, "https://a.com/", true, 10.),
                // Shown beside tab 1, focused after it.
                tab(2, 1, "https://b.com/login", true, 20.),
                // Used last, but hidden behind another tab of its group.
                tab(3, 1, "https://c.com/", false, 30.),
                tab(4, 2, "https://d.com/", false, 5.),
            ],
            windows: vec![window(1, true), window(2, false)],
            last_focused: Some(1),
        }
    }

    fn ids(tabs: Vec<&TabInfo>) -> Vec<i64> {
        tabs.into_iter().map(|t| t.id).collect()
    }

    #[test]
    fn each_window_has_one_active_tab() {
        let m = model();
        assert_eq!(m.active_in(1), Some(2));
        // A window that shows nothing: the tab used last.
        assert_eq!(m.active_in(2), Some(4));
        let all = |_: &TabInfo| true;
        let active = json!({"active": true, "currentWindow": true});
        assert_eq!(ids(m.query(&active, None, &all)), vec![2]);
        assert_eq!(ids(m.query(&active, Some(2), &all)), vec![4]);
        assert_eq!(
            ids(m.query(
                &json!({"active": true, "lastFocusedWindow": true}),
                None,
                &all
            )),
            vec![2]
        );
        assert_eq!(
            ids(m.query(&json!({"active": true}), None, &all)),
            vec![2, 4]
        );
        assert_eq!(
            ids(m.query(&json!({"windowId": -2}), None, &all)),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn switching_tabs_moves_the_active_tab() {
        let mut m = model();
        // The user brings tab 3 up in its group: shown, and the latest.
        m.tabs[2].shown = true;
        m.tabs[2].last_active = 40.;
        assert_eq!(m.active_in(1), Some(3));
        // Focusing tab 1 later wins.
        m.tabs[0].last_active = 50.;
        assert_eq!(m.active_in(1), Some(1));
    }

    #[test]
    fn url_and_title_filters_need_access() {
        let m = model();
        let all = |_: &TabInfo| true;
        let none = |_: &TabInfo| false;
        let q = json!({"url": "https://*.b.com/*"});
        assert_eq!(ids(m.query(&q, None, &all)), vec![2]);
        assert!(m.query(&q, None, &none).is_empty());
        assert_eq!(
            ids(m.query(
                &json!({"url": ["https://a.com/*", "https://d.com/*"]}),
                None,
                &all
            )),
            vec![1, 4]
        );
        assert_eq!(
            ids(m.query(&json!({"title": "Tab ?"}), None, &all)),
            vec![1, 2, 3, 4]
        );
        let t = m.tab_json(m.tab(2).unwrap(), false);
        assert!(t.get("url").is_none());
        assert_eq!(t["active"], true);
        assert_eq!(t["index"], 1);
        let t = m.tab_json(m.tab(2).unwrap(), true);
        assert_eq!(t["url"], "https://b.com/login");
        let w = m.window_json(&m.windows[0], Some(&all));
        assert_eq!(w["tabs"].as_array().unwrap().len(), 3);
    }
}
