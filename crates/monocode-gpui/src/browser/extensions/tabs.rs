//! The app's windows and browser pages as Chrome's tabs and windows, per
//! profile (`monocode_core::browser::extensions::tabs::TabModel`), and
//! their changes as `tabs.*` and `windows.*` events.
//!
//! A tab is a browser page (its id stays with the page across views and
//! discards); a window is an app window. Each window has one active tab:
//! of the pages it shows (groups can sit side by side) the one the user
//! focused or brought up last. The model is read from the shells when an
//! extension asks and every [`WATCH`] while instances run.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui::WindowId;
use monocode_core::browser::extensions::tabs::{TabInfo, TabModel, WindowInfo};
use serde_json::{Value, json};

use super::{ProfileKey, instance::epoch_ms, with};

const WATCH: Duration = Duration::from_millis(400);

/// A browser page as the shell sees it.
#[derive(Clone, Debug)]
pub(crate) struct PageTab {
    pub tab: i64,
    pub window: WindowId,
    pub profile: ProfileKey,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub audible: bool,
    pub muted: bool,
    pub discarded: bool,
    pub shown: bool,
    pub last_input: Option<Instant>,
    pub browser_id: Option<i32>,
}

#[derive(Clone, Debug)]
pub(crate) struct WindowTab {
    pub window: WindowId,
    pub focused: bool,
    /// Screen position and size, in points.
    pub bounds: (i32, i32, i32, i32),
}

#[derive(Default)]
pub(crate) struct Tracker {
    models: HashMap<ProfileKey, TabModel>,
    window_ids: HashMap<WindowId, i64>,
    next_window: i64,
    /// When each tab last came into sight, ms since the epoch.
    shown_since: HashMap<i64, f64>,
    shown: HashMap<i64, bool>,
    last_focused: Option<i64>,
}

/// One change, for the instances of a profile: `(event, tab or window,
/// args builder input)`.
pub(crate) enum Change {
    Created(i64),
    Removed(i64, i64),
    Updated(i64, Value),
    Activated(i64, i64),
    FocusChanged(i64),
    WindowCreated(i64),
    WindowRemoved(i64),
}

fn instant_ms(at: Instant) -> f64 {
    epoch_ms() - at.elapsed().as_secs_f64() * 1000.
}

impl Tracker {
    fn window_id(&mut self, w: WindowId) -> i64 {
        if let Some(id) = self.window_ids.get(&w) {
            return *id;
        }
        self.next_window += 1;
        self.window_ids.insert(w, self.next_window);
        self.next_window
    }

    /// Takes the shells' pages and windows; the changes per profile.
    pub fn refresh(
        &mut self,
        pages: Vec<PageTab>,
        windows: Vec<WindowTab>,
    ) -> Vec<(ProfileKey, Change)> {
        let now = epoch_ms();
        let mut infos: Vec<WindowInfo> = Vec::new();
        let mut focused = None;
        for w in &windows {
            let id = self.window_id(w.window);
            if w.focused {
                focused = Some(id);
            }
            infos.push(WindowInfo {
                id,
                focused: w.focused,
                left: w.bounds.0,
                top: w.bounds.1,
                width: w.bounds.2,
                height: w.bounds.3,
            });
        }
        // The window used last stays "last focused" while the app is in
        // the background.
        // (Chrome always has one while a window is open.)
        let last_focused = focused
            .or(self.last_focused)
            .filter(|id| infos.iter().any(|w| w.id == *id))
            .or(infos.first().map(|w| w.id));
        let mut next: HashMap<ProfileKey, TabModel> = HashMap::new();
        for page in pages {
            let window_id = self.window_id(page.window);
            let was_shown = self.shown.insert(page.tab, page.shown).unwrap_or(false);
            if page.shown && !was_shown {
                self.shown_since.insert(page.tab, now);
            }
            let since = self.shown_since.get(&page.tab).copied().unwrap_or(0.);
            let input = page.last_input.map(instant_ms).unwrap_or(0.);
            let model = next
                .entry(page.profile.clone())
                .or_insert_with(|| TabModel {
                    tabs: Vec::new(),
                    windows: infos.clone(),
                    last_focused,
                });
            model.tabs.push(TabInfo {
                id: page.tab,
                window_id,
                url: page.url,
                title: page.title,
                loading: page.loading,
                fav_icon: None,
                audible: page.audible,
                muted: page.muted,
                discarded: page.discarded,
                pinned: false,
                shown: page.shown,
                last_active: since.max(input),
                browser_id: page.browser_id,
            });
        }
        let mut changes = Vec::new();
        let profiles: Vec<ProfileKey> = (self.models.keys().chain(next.keys())).cloned().collect();
        let mut seen = std::collections::HashSet::new();
        for profile in profiles {
            if !seen.insert(profile.clone()) {
                continue;
            }
            let empty = TabModel::default();
            let old = self.models.get(&profile).unwrap_or(&empty);
            let new = next.get(&profile).unwrap_or(&empty);
            for t in &new.tabs {
                match old.tab(t.id) {
                    None => changes.push((profile.clone(), Change::Created(t.id))),
                    Some(o) => {
                        let mut info = serde_json::Map::new();
                        if o.url != t.url {
                            info.insert("url".into(), t.url.clone().into());
                        }
                        if o.title != t.title {
                            info.insert("title".into(), t.title.clone().into());
                        }
                        if o.loading != t.loading {
                            info.insert(
                                "status".into(),
                                (if t.loading { "loading" } else { "complete" }).into(),
                            );
                        }
                        if o.audible != t.audible {
                            info.insert("audible".into(), t.audible.into());
                        }
                        if o.discarded != t.discarded {
                            info.insert("discarded".into(), t.discarded.into());
                        }
                        if o.muted != t.muted {
                            info.insert("mutedInfo".into(), json!({"muted": t.muted}));
                        }
                        if !info.is_empty() {
                            changes.push((
                                profile.clone(),
                                Change::Updated(t.id, Value::Object(info)),
                            ));
                        }
                    }
                }
            }
            for t in &old.tabs {
                if new.tab(t.id).is_none() {
                    changes.push((profile.clone(), Change::Removed(t.id, t.window_id)));
                }
            }
            for w in &new.windows {
                if !old.windows.iter().any(|o| o.id == w.id) && !old.windows.is_empty() {
                    changes.push((profile.clone(), Change::WindowCreated(w.id)));
                }
                let (a, b) = (old.active_in(w.id), new.active_in(w.id));
                if b.is_some() && a != b {
                    changes.push((profile.clone(), Change::Activated(b.unwrap_or(-1), w.id)));
                }
            }
            for w in &old.windows {
                if !new.windows.iter().any(|n| n.id == w.id) {
                    changes.push((profile.clone(), Change::WindowRemoved(w.id)));
                }
            }
            if old.last_focused != new.last_focused && !old.tabs.is_empty() {
                changes.push((
                    profile.clone(),
                    Change::FocusChanged(new.last_focused.unwrap_or(-1)),
                ));
            }
        }
        self.models = next;
        self.last_focused = last_focused;
        let live: std::collections::HashSet<i64> = self
            .models
            .values()
            .flat_map(|m| m.tabs.iter().map(|t| t.id))
            .collect();
        self.shown_since.retain(|t, _| live.contains(t));
        self.shown.retain(|t, _| live.contains(t));
        changes
    }

    pub fn model(&self, profile: &ProfileKey) -> TabModel {
        self.models
            .get(profile)
            .cloned()
            .unwrap_or_else(|| TabModel {
                tabs: Vec::new(),
                windows: self
                    .models
                    .values()
                    .next()
                    .map(|m| m.windows.clone())
                    .unwrap_or_default(),
                last_focused: self.last_focused,
            })
    }

    pub fn tab_of_browser(&self, profile: &ProfileKey, browser_id: i32) -> Option<i64> {
        let model = self.models.get(profile)?;
        model
            .tabs
            .iter()
            .find(|t| t.browser_id == Some(browser_id))
            .map(|t| t.id)
    }

    pub fn tab_json(&self, profile: &ProfileKey, tab: i64, reveal: bool) -> Option<Value> {
        let model = self.models.get(profile)?;
        Some(model.tab_json(model.tab(tab)?, reveal))
    }

    pub fn window_of(&self, profile: &ProfileKey, tab: i64) -> Option<i64> {
        self.models.get(profile)?.tab(tab).map(|t| t.window_id)
    }

    pub fn dev_json(&self) -> Value {
        json!(
            self.models
                .iter()
                .map(|(p, m)| json!({
                    "profile": super::profile_folder(p),
                    "lastFocused": m.last_focused,
                    "tabs": m.tabs.iter().map(|t| json!({
                        "id": t.id, "window": t.window_id, "url": t.url, "shown": t.shown,
                        "active": m.is_active(t), "lastActive": t.last_active,
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        )
    }
}

/// Reads the shells now, and tells the instances what changed.
pub(crate) fn refresh_now() {
    let Some(app) = with(|rt| rt.app.clone()) else {
        return;
    };
    let (pages, windows) = app.update(crate::shell::browser_extensions::extension_tabs);
    let Some(changes) = with(|rt| rt.tabs.refresh(pages, windows)) else {
        return;
    };
    for (profile, change) in changes {
        emit(&profile, change);
    }
}

/// One change as each instance of the profile sees it (URLs only where
/// it has the `tabs` permission or host access).
fn emit(profile: &ProfileKey, change: Change) {
    let targets: Vec<(super::InstanceKey, bool, bool)> = with(|rt| {
        (rt.instances.values())
            .filter(|i| &i.key.profile == profile)
            .map(|i| {
                (
                    i.key.clone(),
                    i.ext.has("tabs"),
                    i.ext.manifest.host_permissions.is_empty(),
                )
            })
            .collect()
    })
    .unwrap_or_default();
    for (key, tabs_permission, _) in targets {
        let reveal = |tab: i64| {
            with(|rt| {
                let model = rt.tabs.models.get(profile)?;
                let t = model.tab(tab)?;
                let ext = rt.exts.get(&key.id)?;
                let reveal = tabs_permission || ext.hosts.matches(&t.url);
                Some(model.tab_json(t, reveal))
            })
            .flatten()
        };
        let (name, args) = match &change {
            Change::Created(tab) => ("tabs.onCreated", json!([reveal(*tab)])),
            Change::Removed(tab, window) => (
                "tabs.onRemoved",
                json!([tab, {"windowId": window, "isWindowClosing": false}]),
            ),
            Change::Updated(tab, info) => {
                let tab_json = reveal(*tab);
                let mut info = info.clone();
                if tab_json.as_ref().is_some_and(|t| t.get("url").is_none())
                    && let Value::Object(m) = &mut info
                {
                    m.remove("url");
                    m.remove("title");
                }
                if info.as_object().is_some_and(|m| m.is_empty()) {
                    continue;
                }
                ("tabs.onUpdated", json!([tab, info, tab_json]))
            }
            Change::Activated(tab, window) => (
                "tabs.onActivated",
                json!([{"tabId": tab, "windowId": window}]),
            ),
            Change::FocusChanged(window) => ("windows.onFocusChanged", json!([window])),
            Change::WindowCreated(window) => ("windows.onCreated", json!([{"id": window}])),
            Change::WindowRemoved(window) => ("windows.onRemoved", json!([window])),
        };
        super::instance::broadcast(&key, name, args, false);
    }
}

pub(super) fn start_watching() {
    super::spawn(async move {
        loop {
            super::sleep(WATCH).await;
            // As Chrome starts its extensions with the browser: the shared
            // profile's run once Chromium does (isolated profiles' with
            // their first page).
            if super::super::engine_started() {
                let missing: Vec<super::InstanceKey> = with(|rt| {
                    (rt.exts.keys())
                        .map(|id| super::InstanceKey {
                            profile: None,
                            id: id.clone(),
                        })
                        .filter(|k| !rt.instances.contains_key(k))
                        .collect()
                })
                .unwrap_or_default();
                for key in missing {
                    super::instance::ensure(&key);
                }
            }
            if with(|rt| !rt.instances.is_empty()) == Some(true) {
                refresh_now();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(tab: i64, window: WindowId, url: &str, shown: bool) -> PageTab {
        PageTab {
            tab,
            window,
            profile: None,
            url: url.into(),
            title: String::new(),
            loading: false,
            audible: false,
            muted: false,
            discarded: false,
            shown,
            last_input: None,
            browser_id: Some(tab as i32),
        }
    }

    fn window(window: WindowId, focused: bool) -> WindowTab {
        WindowTab {
            window,
            focused,
            bounds: (0, 0, 1000, 800),
        }
    }

    #[test]
    fn switching_tabs_and_groups_moves_the_active_tab() {
        let w = WindowId::from(1u64);
        let mut t = Tracker::default();
        let created = t.refresh(
            vec![
                page(1, w, "https://a/", true),
                page(2, w, "https://b/", false),
            ],
            vec![window(w, true)],
        );
        assert_eq!(
            created
                .iter()
                .filter(|(_, c)| matches!(c, Change::Created(_)))
                .count(),
            2
        );
        assert_eq!(t.model(&None).active_in(1), Some(1));
        // The user switches to tab 2 in the same group.
        std::thread::sleep(Duration::from_millis(3));
        let changes = t.refresh(
            vec![
                page(1, w, "https://a/", false),
                page(2, w, "https://b/", true),
            ],
            vec![window(w, true)],
        );
        assert!(
            changes
                .iter()
                .any(|(_, c)| matches!(c, Change::Activated(2, 1)))
        );
        assert_eq!(t.model(&None).active_in(1), Some(2));
        // A second group shows tab 1 beside tab 2: the one brought up last.
        std::thread::sleep(Duration::from_millis(3));
        t.refresh(
            vec![
                page(1, w, "https://a/", true),
                page(2, w, "https://b/", true),
            ],
            vec![window(w, true)],
        );
        assert_eq!(t.model(&None).active_in(1), Some(1));
        // Clicking into tab 2 makes it the active one.
        let mut p2 = page(2, w, "https://b/2", true);
        p2.last_input = Some(Instant::now());
        let changes = t.refresh(
            vec![page(1, w, "https://a/", true), p2],
            vec![window(w, true)],
        );
        assert_eq!(t.model(&None).active_in(1), Some(2));
        assert!(
            changes.iter().any(
                |(_, c)| matches!(c, Change::Updated(2, info) if info["url"] == "https://b/2")
            )
        );
        // Closing tab 1.
        let changes = t.refresh(vec![page(2, w, "https://b/2", true)], vec![window(w, true)]);
        assert!(
            changes
                .iter()
                .any(|(_, c)| matches!(c, Change::Removed(1, 1)))
        );
        assert_eq!(t.tab_of_browser(&None, 2), Some(2));
    }
}
