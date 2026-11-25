//! The app's modes (docs/native/modes.md): Dev, the workspace as it always
//! was, and Browser, an Arc-like browser whose sidebar lists the user's own
//! tabs and the pages of every project. The mode bar above Settings in the
//! project rail switches between them, as do ⌃1–⌃4 and a two-finger swipe
//! over the rail.
//!
//! This module holds what is saved and the arithmetic of moving between
//! modes: where a swipe has taken the rail, where it settles, and the
//! colour Browser mode tints the window's chrome with. The shell's side is
//! `shell/modes.rs` (state changes) and `shell/modes_ui.rs` (drawing).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime};

use socode_browser::folders::{FolderId, PinnedItem};
use socode_browser::{Browser, Section, SpaceId, Tab, TabId, favorites::MAX_FAVORITES};

use gpui::{Hsla, hsla};
use serde::{Deserialize, Serialize};

/// The mode bar holds at most this many buttons.
pub(crate) const MAX_MODES: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AppMode {
    /// Projects, chats, editors and terminals.
    #[default]
    Dev,
    /// The browser, with the user's own tabs and every project's pages.
    Browser,
}

impl AppMode {
    /// Every mode, in the bar's default order.
    pub(crate) const ALL: [AppMode; 2] = [AppMode::Dev, AppMode::Browser];

    pub(crate) fn label(self) -> &'static str {
        match self {
            AppMode::Dev => "Dev",
            AppMode::Browser => "Browser",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            AppMode::Dev => "code",
            AppMode::Browser => "compass",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            AppMode::Dev => "dev",
            AppMode::Browser => "browser",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.id() == id)
    }
}

/// The modes the bar shows, in its order. Hidden modes are left out; at
/// least one stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ModeBar {
    pub shown: Vec<AppMode>,
}

impl Default for ModeBar {
    fn default() -> Self {
        Self {
            shown: AppMode::ALL.to_vec(),
        }
    }
}

impl ModeBar {
    /// Without repeats, at most [`MAX_MODES`], never empty.
    pub(crate) fn normalize(&mut self) {
        let mut seen = Vec::new();
        self.shown.retain(|mode| {
            let fresh = !seen.contains(mode);
            seen.push(*mode);
            fresh
        });
        self.shown.truncate(MAX_MODES);
        if self.shown.is_empty() {
            self.shown.push(AppMode::Dev);
        }
    }

    pub(crate) fn index(&self, mode: AppMode) -> Option<usize> {
        self.shown.iter().position(|m| *m == mode)
    }

    pub(crate) fn hidden(&self) -> Vec<AppMode> {
        AppMode::ALL
            .into_iter()
            .filter(|mode| !self.shown.contains(mode))
            .collect()
    }

    /// Moves `mode` one place left (`-1`) or right (`1`); false at an end.
    pub(crate) fn shift(&mut self, mode: AppMode, step: isize) -> bool {
        let Some(ix) = self.index(mode) else {
            return false;
        };
        let to = ix as isize + step;
        if to < 0 || to as usize >= self.shown.len() {
            return false;
        }
        self.shown.swap(ix, to as usize);
        true
    }

    /// Puts `mode` at `to` (clamped), taking it from where it was.
    pub(crate) fn place(&mut self, mode: AppMode, to: usize) {
        let Some(from) = self.index(mode) else {
            return;
        };
        let mode = self.shown.remove(from);
        let to = to.min(self.shown.len());
        self.shown.insert(to, mode);
    }

    /// Hides `mode` unless it is the last one shown.
    pub(crate) fn hide(&mut self, mode: AppMode) -> bool {
        if self.shown.len() <= 1 || !self.shown.contains(&mode) {
            return false;
        }
        self.shown.retain(|m| *m != mode);
        true
    }

    pub(crate) fn show(&mut self, mode: AppMode) -> bool {
        if self.shown.contains(&mode) || self.shown.len() >= MAX_MODES {
            return false;
        }
        self.shown.push(mode);
        true
    }
}

/// The hue Browser mode washes the sidebar and the frame around its page
/// with, as an Arc space's colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BrowseTint {
    #[default]
    Violet,
    Blue,
    Teal,
    Green,
    Amber,
    Rose,
    /// No colour: the chrome stays as in Dev mode.
    Graphite,
}

impl BrowseTint {
    pub(crate) const ALL: [BrowseTint; 7] = [
        BrowseTint::Violet,
        BrowseTint::Blue,
        BrowseTint::Teal,
        BrowseTint::Green,
        BrowseTint::Amber,
        BrowseTint::Rose,
        BrowseTint::Graphite,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            BrowseTint::Violet => "Violet",
            BrowseTint::Blue => "Blue",
            BrowseTint::Teal => "Teal",
            BrowseTint::Green => "Green",
            BrowseTint::Amber => "Amber",
            BrowseTint::Rose => "Rose",
            BrowseTint::Graphite => "Graphite",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            BrowseTint::Violet => "violet",
            BrowseTint::Blue => "blue",
            BrowseTint::Teal => "teal",
            BrowseTint::Green => "green",
            BrowseTint::Amber => "amber",
            BrowseTint::Rose => "rose",
            BrowseTint::Graphite => "graphite",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tint| tint.id() == id)
    }

    /// Hue (0–1) and saturation; `None` for Graphite.
    fn hue(self) -> Option<(f32, f32)> {
        Some(match self {
            BrowseTint::Violet => (262. / 360., 0.62),
            BrowseTint::Blue => (215. / 360., 0.7),
            BrowseTint::Teal => (178. / 360., 0.55),
            BrowseTint::Green => (145. / 360., 0.45),
            BrowseTint::Amber => (36. / 360., 0.75),
            BrowseTint::Rose => (343. / 360., 0.62),
            BrowseTint::Graphite => return None,
        })
    }

    /// The swatch shown in menus: the hue at full strength.
    pub(crate) fn swatch(self) -> Hsla {
        match self.hue() {
            Some((h, s)) => hsla(h, s, 0.6, 1.),
            None => hsla(0., 0., 0.55, 1.),
        }
    }

    /// The wash at the top and bottom of the sidebar, `amount` (0–1) of the
    /// way into Browser mode. Light themes take a lighter, weaker wash.
    pub(crate) fn wash(self, amount: f32, light: bool) -> (Hsla, Hsla) {
        let amount = amount.clamp(0., 1.);
        let Some((h, s)) = self.hue() else {
            return (hsla(0., 0., 0., 0.), hsla(0., 0., 0., 0.));
        };
        let (l, top, bottom) = if light {
            (0.62, 0.16, 0.07)
        } else {
            (0.55, 0.20, 0.08)
        };
        (hsla(h, s, l, top * amount), hsla(h, s, l, bottom * amount))
    }

    /// The selected tab's fill and the mode pill: the hue, faintly.
    pub(crate) fn highlight(self, light: bool) -> Option<Hsla> {
        let (h, s) = self.hue()?;
        Some(if light {
            hsla(h, s, 0.5, 0.16)
        } else {
            hsla(h, s, 0.68, 0.2)
        })
    }
}

/// Where a tab of the user's own sits in the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowseSection {
    /// The icon tiles at the top.
    Favorites,
    /// The rows above the divider: kept, and unloaded rather than closed.
    Pinned,
    /// The rows under "+ New Tab", newest first; Clear closes them.
    Today,
}

/// A row among the pinned tabs, as the sidebar draws it: a tab or a folder,
/// at its depth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PinnedRow {
    Tab {
        pane: i64,
        depth: usize,
    },
    Folder {
        id: u64,
        name: String,
        open: bool,
        depth: usize,
    },
}

/// The pinned tabs as saved before the model, read once to move them over.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum LegacyPinned {
    Tab(i64),
    Folder(LegacyFolder),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LegacyFolder {
    pub name: String,
    pub items: Vec<LegacyPinned>,
}

/// What Browser mode keeps: the model of the user's own tabs (favourites,
/// pinned tabs in folders, Today, the archive and undo; `socode_browser`),
/// the browser pane each of its tabs shows in, and the page on screen.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BrowseState {
    /// The tabs' model, with one Space.
    pub model: Browser,
    /// The browser pane of each model tab, by the tab's id.
    pub panes: BTreeMap<u64, i64>,
    /// The page on screen: one of the model's tabs, or a project's page.
    pub active: Option<i64>,
    pub tint: BrowseTint,
    /// Projects whose pages the sidebar folds away, by path key.
    pub folded: BTreeSet<String>,
    /// Rows chosen together with ⌘- and ⇧-click, for one action on all.
    #[serde(skip)]
    pub selection: Vec<i64>,
    /// Peek: a page a pinned tab's link to another site opened over it,
    /// and that tab's pane. It is no tab of the model until opened as one.
    #[serde(skip)]
    pub peek: Option<(i64, i64)>,
    /// The pages of the open Little windows (`shell/browse_little.rs`):
    /// no tabs of the model until moved into Browser mode.
    #[serde(skip)]
    pub little: Vec<i64>,
    // Saved before the model; moved into it by `normalize`.
    #[serde(skip_serializing)]
    tabs: Vec<i64>,
    #[serde(skip_serializing)]
    favorites: Vec<i64>,
    #[serde(skip_serializing)]
    pinned: Vec<LegacyPinned>,
    #[serde(skip_serializing)]
    home: BTreeMap<i64, String>,
    #[serde(skip_serializing)]
    names: BTreeMap<i64, String>,
}

/// Now, for the model, which takes time as an argument.
pub(crate) fn now() -> SystemTime {
    SystemTime::now()
}

impl BrowseState {
    // ---- Tabs and panes ----

    /// The model tab shown in `pane`.
    pub(crate) fn tab_of(&self, pane: i64) -> Option<TabId> {
        (self.panes.iter())
            .find(|(_, p)| **p == pane)
            .map(|(tab, _)| TabId::from_raw(*tab))
    }

    /// The pane showing the model tab `tab`.
    pub(crate) fn pane_of(&self, tab: TabId) -> Option<i64> {
        self.panes.get(&tab.get()).copied()
    }

    /// `pane` shows a tab of the user's own (or one saved before the model,
    /// not moved into it yet: the restore keeps its pane for the move).
    pub(crate) fn owns(&self, pane: i64) -> bool {
        self.tab_of(pane).is_some()
            || self.tabs.contains(&pane)
            || self.peek.is_some_and(|(peek, _)| peek == pane)
            || self.little.contains(&pane)
    }

    /// The model's tab shown in `pane`.
    pub(crate) fn tab(&self, pane: i64) -> Option<&Tab> {
        let id = self.tab_of(pane)?;
        self.model.all_tabs().find(|tab| tab.id == id)
    }

    fn space_id(&self) -> SpaceId {
        self.model.space().id
    }

    /// The section of a tab of the user's own; `None` for any other page.
    pub(crate) fn section(&self, pane: i64) -> Option<BrowseSection> {
        let id = self.tab_of(pane)?;
        let space = self.model.space();
        if self.model.is_favorite(id) {
            Some(BrowseSection::Favorites)
        } else if space.pinned.iter().any(|tab| tab.id == id) {
            Some(BrowseSection::Pinned)
        } else if space.today.iter().any(|tab| tab.id == id) {
            Some(BrowseSection::Today)
        } else {
            None
        }
    }

    fn panes_of<'a>(&self, tabs: impl Iterator<Item = &'a Tab>) -> Vec<i64> {
        tabs.filter(|tab| !self.model.is_split_secondary(tab.id))
            .filter_map(|tab| self.pane_of(tab.id))
            .collect()
    }

    /// The favourites, in the grid's order.
    pub(crate) fn favorite_panes(&self) -> Vec<i64> {
        self.panes_of(self.model.active_user_profile().favorites.iter())
    }

    /// The pinned rows on screen: closed folders hide their tabs, but the
    /// one on screen.
    pub(crate) fn pinned_rows(&self) -> Vec<PinnedRow> {
        self.rows(false)
    }

    fn rows(&self, hidden: bool) -> Vec<PinnedRow> {
        (self.model.pinned_rows(self.space_id(), hidden).into_iter())
            .filter_map(|row| match row.item {
                PinnedItem::Tab(tab) => Some(PinnedRow::Tab {
                    pane: self.pane_of(tab)?,
                    depth: row.depth,
                }),
                PinnedItem::Split(split) => {
                    let anchor = (self.model.space().splits.iter())
                        .find(|s| s.id == split)
                        .map(|s| s.anchor)?;
                    Some(PinnedRow::Tab {
                        pane: self.pane_of(anchor)?,
                        depth: row.depth,
                    })
                }
                PinnedItem::Folder(id) => {
                    let folder = self.model.folder(id)?;
                    Some(PinnedRow::Folder {
                        id: id.get(),
                        name: folder.name.clone(),
                        open: !folder.collapsed,
                        depth: row.depth,
                    })
                }
            })
            .collect()
    }

    /// Every pinned tab, folders opened, in order.
    pub(crate) fn pinned_tabs(&self) -> Vec<i64> {
        (self.rows(true).into_iter())
            .filter_map(|row| match row {
                PinnedRow::Tab { pane, .. } => Some(pane),
                PinnedRow::Folder { .. } => None,
            })
            .collect()
    }

    pub(crate) fn is_pinned(&self, pane: i64) -> bool {
        self.section(pane) == Some(BrowseSection::Pinned)
    }

    /// The Today tabs, newest first.
    pub(crate) fn today(&self) -> Vec<i64> {
        self.panes_of(self.model.space().today.iter())
    }

    /// The order the sidebar lists the user's pages in: favourites, pinned
    /// tabs, then Today.
    pub(crate) fn order(&self) -> Vec<i64> {
        let mut order = self.favorite_panes();
        order.extend(self.pinned_tabs());
        order.extend(self.today());
        order
    }

    /// The grid has no room for another.
    pub(crate) fn favorites_full(&self) -> bool {
        self.model.favorite_count() >= MAX_FAVORITES
    }

    /// The name the user gave the tab of `pane`.
    pub(crate) fn name(&self, pane: i64) -> Option<&str> {
        self.tab(pane)?.custom_title.as_deref()
    }

    /// The address the tab of `pane` is pinned at, if it is pinned.
    pub(crate) fn home(&self, pane: i64) -> Option<&str> {
        self.tab(pane)?.pinned_url.as_deref()
    }

    // ---- Folders ----

    pub(crate) fn folder(&self, id: u64) -> Option<&socode_browser::folders::Folder> {
        self.model.folder(FolderId::from_raw(id))
    }

    /// The tabs in the folder `id`, its folders' too.
    pub(crate) fn folder_tabs(&self, id: u64) -> Vec<i64> {
        (self.model.folder_tabs(FolderId::from_raw(id)).into_iter())
            .filter_map(|tab| self.pane_of(tab))
            .collect()
    }

    /// Every folder with its path and depth, for menus.
    pub(crate) fn folder_paths(&self) -> Vec<(u64, String, usize)> {
        (self.rows(true).into_iter())
            .filter_map(|row| match row {
                PinnedRow::Folder { id, depth, .. } => {
                    Some((id, self.model.folder_path(FolderId::from_raw(id)), depth))
                }
                PinnedRow::Tab { .. } => None,
            })
            .collect()
    }

    /// The folder holding `pane` directly, if any.
    pub(crate) fn parent_folder(&self, pane: i64) -> Option<u64> {
        let tab = self.tab_of(pane)?;
        self.model.folder_ancestors(tab).last().map(|f| f.get())
    }

    // ---- Keeping it whole ----

    /// Forgets the panes `exists` does not know, and moves a sidebar saved
    /// before the model into it (`page` gives a pane's address and title).
    pub(crate) fn normalize(
        &mut self,
        exists: impl Fn(i64) -> bool,
        page: impl Fn(i64) -> Option<(String, String)>,
    ) {
        if !self.tabs.is_empty() {
            self.migrate(&exists, &page);
        }
        let model = &self.model;
        self.panes
            .retain(|tab, pane| exists(*pane) && model.all_tabs().any(|t| t.id.get() == *tab));
        let owned: Vec<i64> = self.panes.values().copied().collect();
        self.selection.retain(|pane| owned.contains(pane));
        if self.active.is_some_and(|pane| !exists(pane)) {
            self.active = None;
        }
    }

    /// The sidebar as saved before the model becomes model tabs, each kept
    /// on its pane: Today, pinned tabs in their folders, favourites, names
    /// and pinned addresses.
    fn migrate(
        &mut self,
        exists: &impl Fn(i64) -> bool,
        page: &impl Fn(i64) -> Option<(String, String)>,
    ) {
        let tabs = std::mem::take(&mut self.tabs);
        let favorites = std::mem::take(&mut self.favorites);
        let pinned = std::mem::take(&mut self.pinned);
        let home = std::mem::take(&mut self.home);
        let names = std::mem::take(&mut self.names);
        let now = now();
        let space = self.space_id();
        let mut new = |state: &mut Self, pane: i64| -> Option<TabId> {
            if !exists(pane) || state.panes.values().any(|p| *p == pane) {
                return None;
            }
            let (url, title) = page(pane).unwrap_or_default();
            let tab = state.model.new_tab(url, now);
            if !title.is_empty() {
                state.model.set_title(tab, title);
            }
            if let Some(name) = names.get(&pane) {
                state.model.rename_tab(tab, name);
            }
            state.panes.insert(tab.get(), pane);
            Some(tab)
        };
        fn pin_level(
            state: &mut BrowseState,
            items: &[LegacyPinned],
            parent: Option<FolderId>,
            space: SpaceId,
            new: &mut dyn FnMut(&mut BrowseState, i64) -> Option<TabId>,
        ) {
            for item in items {
                match item {
                    LegacyPinned::Tab(pane) => {
                        if let Some(tab) = new(state, *pane) {
                            state.model.move_tab(tab, space, Section::Pinned);
                            if let Some(parent) = parent {
                                state.model.move_item_to_folder(tab, parent);
                            }
                        }
                    }
                    LegacyPinned::Folder(folder) => {
                        let name = if folder.name.trim().is_empty() {
                            "Folder"
                        } else {
                            folder.name.as_str()
                        };
                        if let Some(id) = state.model.create_folder(space, parent, name) {
                            pin_level(state, &folder.items, Some(id), space, new);
                        }
                    }
                }
            }
        }
        // Today, oldest first: each new tab goes on top.
        let today: Vec<i64> = (tabs.iter().rev().copied())
            .filter(|pane| !favorites.contains(pane))
            .collect();
        let mut pinned_panes = Vec::new();
        fn collect(items: &[LegacyPinned], out: &mut Vec<i64>) {
            for item in items {
                match item {
                    LegacyPinned::Tab(pane) => out.push(*pane),
                    LegacyPinned::Folder(folder) => collect(&folder.items, out),
                }
            }
        }
        collect(&pinned, &mut pinned_panes);
        for pane in today.into_iter().filter(|p| !pinned_panes.contains(p)) {
            new(self, pane);
        }
        pin_level(self, &pinned, None, space, &mut new);
        for pane in favorites {
            if let Some(tab) = new(self, pane) {
                self.model.favorite_tab(tab);
            }
        }
        for (pane, url) in home {
            if let Some(tab) = self.tab_of(pane)
                && let Some(entry) = self.model.tab_mut(tab)
            {
                entry.pinned_url = Some(url);
            }
        }
        self.model.clear_sidebar_history();
    }
}

/// Whether a pinned tab or favourite at `url` is away from its pinned
/// address `home`; a trailing slash or fragment does not count.
pub(crate) fn away_from_home(home: &str, url: &str) -> bool {
    fn bare(url: &str) -> &str {
        let url = url.split('#').next().unwrap_or(url);
        url.trim_end_matches('/')
    }
    !url.is_empty() && bare(home) != bare(url)
}

// ---- Moving between modes ----

/// How long a switch slides when nothing holds it.
pub(crate) const SLIDE: Duration = Duration::from_millis(320);

/// Ease out: fast start, gentle landing.
fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// The rail sliding to a mode: positions are indices into the bar, so 0.5
/// is halfway between its first two modes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ModeSlide {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl ModeSlide {
    pub(crate) fn new(from: f32, to: f32, now: Instant) -> Self {
        // A shorter way takes a shorter time, never under a third.
        let distance = (to - from).abs().min(1.);
        let duration = SLIDE.mul_f32(0.35 + 0.65 * distance);
        Self {
            from,
            to,
            start: now,
            duration,
        }
    }

    /// Where the slide is at `now`, and whether it is over.
    pub(crate) fn at(&self, now: Instant) -> (f32, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f32()
            / self.duration.as_secs_f32().max(0.001);
        if t >= 1. {
            (self.to, true)
        } else {
            (self.from + (self.to - self.from) * ease_out(t), false)
        }
    }

    pub(crate) fn target(&self) -> f32 {
        self.to
    }
}

/// A two-finger swipe over the rail, from the touch to the lift. Its axis
/// is settled by the first few points of travel: a vertical one is left to
/// the list under it, a sideways one moves between modes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Swipe {
    /// The mode index the swipe started on.
    pub start: usize,
    /// Travel so far, in points (positive: fingers to the right).
    dx: f32,
    dy: f32,
    /// `Some(true)` once it is sideways, `Some(false)` once vertical.
    pub horizontal: Option<bool>,
    /// Recent travel, for the speed at the lift.
    samples: Vec<(Instant, f32)>,
}

/// Travel before a swipe's axis is decided.
const AXIS_SLOP: f32 = 6.;
/// Of the rail's width, how far a slow swipe must go to switch.
const COMMIT_DISTANCE: f32 = 0.28;
/// Points per second at the lift that switch even a short swipe.
const FLICK_SPEED: f32 = 380.;
/// How far past the first or last mode the rail stretches, as a fraction
/// of its width at most.
const STRETCH: f32 = 0.14;

impl Swipe {
    pub(crate) fn new(start: usize) -> Self {
        Self {
            start,
            dx: 0.,
            dy: 0.,
            horizontal: None,
            samples: Vec::new(),
        }
    }

    /// Adds a step of travel. Returns whether the swipe is sideways, so
    /// the event is the mode bar's and not the list's.
    pub(crate) fn travel(&mut self, dx: f32, dy: f32, now: Instant) -> bool {
        match self.horizontal {
            Some(false) => return false,
            Some(true) => {}
            None => {
                self.dy += dy;
                let sideways = self.dx + dx;
                if sideways.abs() + self.dy.abs() < AXIS_SLOP {
                    // Too little to tell; count it toward the swipe.
                    self.dx = sideways;
                    return false;
                }
                let horizontal = sideways.abs() > self.dy.abs() * 1.3;
                self.horizontal = Some(horizontal);
                if !horizontal {
                    return false;
                }
            }
        }
        self.dx += dx;
        self.samples.push((now, dx));
        self.samples
            .retain(|(at, _)| now.saturating_duration_since(*at) < Duration::from_millis(90));
        true
    }

    /// The bar position the swipe has dragged the rail to, `width` points
    /// per mode over `count` modes, stretching past the ends.
    pub(crate) fn position(&self, width: f32, count: usize) -> f32 {
        if self.horizontal != Some(true) || count == 0 {
            return self.start as f32;
        }
        let raw = self.start as f32 - self.dx / width.max(1.);
        stretch(raw, (count - 1) as f32)
    }

    /// Fingers' speed at the lift, in points per second (positive: right).
    fn speed(&self) -> f32 {
        let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
            return 0.;
        };
        let span = last.0.saturating_duration_since(first.0).as_secs_f32();
        let travel: f32 = self.samples.iter().map(|(_, dx)| dx).sum();
        if span < 0.008 {
            // One sample: its step over a frame.
            return travel / 0.016;
        }
        travel / span
    }

    /// The mode index the rail settles on when the fingers lift.
    pub(crate) fn settle(&self, width: f32, count: usize) -> usize {
        if self.horizontal != Some(true) || count == 0 {
            return self.start.min(count.saturating_sub(1));
        }
        let last = (count - 1) as isize;
        let moved = -self.dx / width.max(1.);
        let speed = -self.speed();
        let start = self.start as isize;
        let target = if moved.abs() >= 1. {
            // Dragged past a whole mode: the nearest one.
            (self.start as f32 + moved).round() as isize
        } else if moved > COMMIT_DISTANCE || (speed > FLICK_SPEED && moved > 0.02) {
            start + 1
        } else if moved < -COMMIT_DISTANCE || (speed < -FLICK_SPEED && moved < -0.02) {
            start - 1
        } else {
            start
        };
        target.clamp(0, last) as usize
    }
}

/// `raw` held to `0..=last`, giving way a little past either end.
fn stretch(raw: f32, last: f32) -> f32 {
    let give = |over: f32| STRETCH * (1. - 1. / (over * 3. + 1.));
    if raw < 0. {
        -give(-raw)
    } else if raw > last {
        last + give(raw - last)
    } else {
        raw
    }
}

/// How much of mode `index` shows at bar position `position`: 1 when it is
/// the one in place, falling to 0 a whole mode away.
pub(crate) fn presence(index: usize, position: f32) -> f32 {
    (1. - (index as f32 - position).abs()).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_keeps_at_least_one_mode_and_no_repeats() {
        let mut bar = ModeBar {
            shown: vec![AppMode::Browser, AppMode::Browser, AppMode::Dev],
        };
        bar.normalize();
        assert_eq!(bar.shown, vec![AppMode::Browser, AppMode::Dev]);
        assert!(bar.hide(AppMode::Browser));
        assert!(!bar.hide(AppMode::Dev), "the last mode stays");
        assert_eq!(bar.hidden(), vec![AppMode::Browser]);
        assert!(bar.show(AppMode::Browser));
        assert_eq!(bar.shown, vec![AppMode::Dev, AppMode::Browser]);
        assert!(bar.shift(AppMode::Browser, -1));
        assert_eq!(bar.shown, vec![AppMode::Browser, AppMode::Dev]);
        assert!(!bar.shift(AppMode::Browser, -1));
        bar.place(AppMode::Browser, 9);
        assert_eq!(bar.shown, vec![AppMode::Dev, AppMode::Browser]);
        let mut empty = ModeBar { shown: Vec::new() };
        empty.normalize();
        assert_eq!(empty.shown, vec![AppMode::Dev]);
    }

    #[test]
    fn a_saved_bar_and_browse_state_read_back() {
        let bar: ModeBar = serde_json::from_str(r#"{"shown":["browser","dev"]}"#).unwrap();
        assert_eq!(bar.shown, vec![AppMode::Browser, AppMode::Dev]);
        let mut state = BrowseState {
            tint: BrowseTint::Teal,
            active: Some(-3),
            ..Default::default()
        };
        let tab = state.model.new_tab("https://a.test/", now());
        state.panes.insert(tab.get(), -3);
        let json = serde_json::to_string(&state).unwrap();
        let back = serde_json::from_str::<BrowseState>(&json).unwrap();
        assert_eq!(back.today(), vec![-3]);
        assert_eq!(back.tint, BrowseTint::Teal);
        assert_eq!(back.active, Some(-3));
        // Older saves have none of it.
        let empty = serde_json::from_str::<BrowseState>("{}").unwrap();
        assert!(empty.order().is_empty());
    }

    #[test]
    fn a_sidebar_saved_before_the_model_moves_into_it_on_its_panes() {
        let saved = r#"{
            "tabs": [-1, -2, -3, -4, -5],
            "favorites": [-5],
            "pinned": [-4, {"id": 1, "name": "Work", "open": true, "items": [-3]}],
            "home": {"-4": "https://home.test/"},
            "names": {"-1": "Mine"},
            "active": -2
        }"#;
        let mut state: BrowseState = serde_json::from_str(saved).unwrap();
        assert!(state.owns(-1), "kept for the move");
        let url = |pane: i64| Some((format!("https://p{}.test/", -pane), format!("P{}", -pane)));
        state.normalize(|pane| pane != -9, url);
        assert_eq!(state.today(), vec![-1, -2], "in the saved order");
        assert_eq!(state.favorite_panes(), vec![-5]);
        assert_eq!(state.pinned_tabs(), vec![-4, -3]);
        assert_eq!(state.folder_paths().len(), 1);
        assert_eq!(state.folder_paths()[0].1, "Work");
        assert_eq!(state.parent_folder(-3), Some(state.folder_paths()[0].0));
        assert_eq!(state.name(-1), Some("Mine"));
        assert_eq!(state.home(-4), Some("https://home.test/"));
        assert_eq!(state.tab(-2).unwrap().url, "https://p2.test/");
        assert_eq!(state.active, Some(-2));
        // Saved again, only the model remains; nothing moves twice.
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("\"tabs\""), "{json}");
        let mut again: BrowseState = serde_json::from_str(&json).unwrap();
        again.normalize(|_| true, url);
        assert_eq!(again.order(), state.order());
        // A pane that is gone loses its tab's pane, not the tab.
        again.normalize(|pane| pane != -1, url);
        assert!(!again.owns(-1));
        assert_eq!(again.model.space().today.len(), 2);
    }

    fn swipe(steps: &[(f32, f32)], gap_ms: u64) -> Swipe {
        let start = Instant::now();
        let mut swipe = Swipe::new(0);
        for (ix, (dx, dy)) in steps.iter().enumerate() {
            swipe.travel(*dx, *dy, start + Duration::from_millis(gap_ms * ix as u64));
        }
        swipe
    }

    #[test]
    fn a_sideways_swipe_moves_the_rail_and_a_vertical_one_does_not() {
        // Fingers to the left: toward the next mode.
        let s = swipe(&[(-4., 0.5), (-10., 1.), (-30., 0.), (-40., 0.)], 30);
        assert_eq!(s.horizontal, Some(true));
        let p = s.position(220., 2);
        assert!(p > 0.3 && p < 0.4, "{p}");
        assert_eq!(s.settle(220., 2), 1);
        let v = swipe(&[(-1., 8.), (-30., 40.)], 30);
        assert_eq!(v.horizontal, Some(false));
        assert_eq!(v.position(220., 2), 0.);
        assert_eq!(v.settle(220., 2), 0);
    }

    #[test]
    fn a_short_slow_swipe_springs_back_and_a_flick_switches() {
        let slow = swipe(&[(-3., 0.), (-4., 0.), (-3., 0.), (-2., 0.), (-1., 0.)], 60);
        assert_eq!(slow.settle(220., 2), 0);
        let flick = swipe(&[(-4., 0.), (-9., 0.), (-14., 0.)], 8);
        assert_eq!(flick.settle(220., 2), 1);
    }

    #[test]
    fn the_rail_stretches_only_a_little_past_the_ends() {
        // Fingers to the right on the first mode: nothing before it.
        let s = swipe(&[(10., 0.), (200., 0.), (400., 0.)], 30);
        let p = s.position(220., 2);
        assert!(p < 0. && p > -STRETCH, "{p}");
        assert_eq!(s.settle(220., 2), 0);
    }

    #[test]
    fn a_slide_eases_to_its_target() {
        let now = Instant::now();
        let slide = ModeSlide::new(0., 1., now);
        assert_eq!(slide.at(now).0, 0.);
        let (mid, done) = slide.at(now + Duration::from_millis(100));
        assert!(mid > 0.5 && !done, "{mid}");
        assert_eq!(slide.at(now + SLIDE), (1., true));
        assert_eq!(presence(1, 0.25), 0.25);
        assert_eq!(presence(0, 0.25), 0.75);
        assert_eq!(presence(1, 3.), 0.);
    }

    #[test]
    fn graphite_leaves_the_chrome_alone() {
        let (top, bottom) = BrowseTint::Graphite.wash(1., false);
        assert_eq!((top.a, bottom.a), (0., 0.));
        let (top, _) = BrowseTint::Violet.wash(0.5, false);
        assert!(top.a > 0. && top.a < 0.2);
        assert!(BrowseTint::Graphite.highlight(true).is_none());
    }
}
