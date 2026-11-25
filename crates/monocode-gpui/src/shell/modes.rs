//! The shell's side of the modes (docs/native/modes.md): switching between
//! Dev and Browser (the mode bar, ⌃1–⌃4, a two-finger swipe over the rail),
//! Browser mode's own tabs, and their menus and keys. Drawing is in
//! `modes_ui.rs`; what is saved and the swipe's arithmetic in `crate::modes`.
//!
//! Browser mode's own tabs are browser panes like a project's, so the
//! browser's extensions, sound, memory saver and passwords work for them
//! unchanged. They belong to no session (their owner is 0) and sit in no
//! tab of the workspace: `workspace.browse.tabs` lists them, which keeps
//! them from being pruned (`Workspace::prune_browser_panes`). They browse
//! with the shared default profile and keep their own history.
use super::*;
use crate::browser::PageRequest;
use crate::modes::{AppMode, BrowseSection, BrowseTint, ModeSlide, Swipe};
use gpui::{Div, ScrollDelta, ScrollWheelEvent, Stateful, TouchPhase};
use std::time::{Duration, Instant};

/// The history store's project for Browser mode's own tabs.
pub(crate) const BROWSE_HISTORY_PROJECT: &str = "monocode://browse";
/// The owner of Browser mode's own tabs: no session.
pub(super) const NO_OWNER: i64 = 0;

actions!(
    workspace,
    [
        /// ⌘T in Browser mode.
        BrowseNewTab,
        /// ⌘W in Browser mode.
        BrowseCloseTab,
        /// ⇧⌘T in Browser mode.
        BrowseReopenTab,
        BrowseNextTab,
        BrowsePreviousTab,
        /// ⌘[ and ⌘] in Browser mode: the page's own history.
        BrowseBack,
        BrowseForward,
        /// ⌘D: the tab joins the favourites or leaves them.
        BrowseToggleFavorite,
        /// ⌘L with the page not focused: its address field.
        BrowseFocusAddress,
        /// ⇧⌘C and ⌥D in Browser mode: the tab on screen's link copied,
        /// the tab duplicated.
        BrowseCopyLink,
        BrowseDuplicate,
        /// ⌘Z and ⇧⌘Z in Browser mode, outside the page: the sidebar's
        /// last change undone or redone.
        BrowseUndo,
        BrowseRedo,
        /// The next mode of the bar, wrapping.
        NextMode,
        PreviousMode
    ]
);

/// ⌃1–⌃4: the bar's mode at this place.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct SwitchMode(pub usize);

/// ⌘1–⌘9 in Browser mode: the sidebar's tab at this place (-1: the last).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct BrowseActivateTab(pub isize);

/// The key context the workspace adds while Browser mode is on screen.
pub(crate) const BROWSER_MODE_CONTEXT: &str = "BrowserMode";

/// Bound after the workspace's own keys: at the same depth, later bindings
/// win, so Browser mode's take over ⌘T, ⌘W, ⌘[ … while it is on screen.
pub(super) fn bind(cx: &mut App) {
    let m = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let workspace = Some("Workspace");
    let browse = Some(BROWSER_MODE_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("ctrl-1", SwitchMode(0), workspace),
        KeyBinding::new("ctrl-2", SwitchMode(1), workspace),
        KeyBinding::new("ctrl-3", SwitchMode(2), workspace),
        KeyBinding::new("ctrl-4", SwitchMode(3), workspace),
        KeyBinding::new(&format!("{m}-t"), BrowseNewTab, browse),
        KeyBinding::new(&format!("{m}-w"), BrowseCloseTab, browse),
        KeyBinding::new(&format!("{m}-shift-t"), BrowseReopenTab, browse),
        KeyBinding::new("ctrl-tab", BrowseNextTab, browse),
        KeyBinding::new("ctrl-shift-tab", BrowsePreviousTab, browse),
        KeyBinding::new(&format!("{m}-shift-]"), BrowseNextTab, browse),
        KeyBinding::new(&format!("{m}-shift-["), BrowsePreviousTab, browse),
        KeyBinding::new(&format!("{m}-alt-down"), BrowseNextTab, browse),
        KeyBinding::new(&format!("{m}-alt-up"), BrowsePreviousTab, browse),
        KeyBinding::new(&format!("{m}-["), BrowseBack, browse),
        KeyBinding::new(&format!("{m}-]"), BrowseForward, browse),
        KeyBinding::new(&format!("{m}-d"), BrowseToggleFavorite, browse),
        KeyBinding::new(&format!("{m}-l"), BrowseFocusAddress, browse),
        KeyBinding::new(&format!("{m}-z"), BrowseUndo, browse),
        KeyBinding::new(&format!("{m}-shift-c"), BrowseCopyLink, browse),
        KeyBinding::new("alt-d", BrowseDuplicate, browse),
        KeyBinding::new(&format!("{m}-shift-z"), BrowseRedo, browse),
    ];
    for n in 1..=8 {
        bindings.push(KeyBinding::new(
            &format!("{m}-{n}"),
            BrowseActivateTab(n as isize - 1),
            browse,
        ));
    }
    bindings.push(KeyBinding::new(
        &format!("{m}-9"),
        BrowseActivateTab(-1),
        browse,
    ));
    cx.bind_keys(bindings);
}

/// What a tab row shows of its page.
#[derive(Clone, Debug)]
pub(super) struct BrowseTab {
    pub pane: i64,
    pub title: String,
    pub url: String,
    pub favicon: Option<String>,
    pub loading: bool,
    pub sound: Option<browser_audio::TabSound>,
    /// An agent opened it.
    pub agent: bool,
    /// Its place among the user's own; `None` for a project's page.
    pub section: Option<BrowseSection>,
    /// A favourite or pinned tab away from its pinned address: the address
    /// it belongs at.
    pub home: Option<String>,
    /// Unloaded to save memory or put away: it loads again when shown.
    pub sleeping: bool,
}

/// A project's pages in the sidebar.
pub(super) struct BrowseProject {
    pub path: String,
    pub panes: Vec<i64>,
}

/// After this long without an event, a swipe is over (its end was lost).
const SWIPE_STALE: Duration = Duration::from_millis(350);

impl Shell {
    // ---- Modes ----

    /// Repaints what moves with the modes (the rail, the mode bar) without
    /// rebuilding the sidebar's structure, for every frame of a slide.
    pub(super) fn notify_mode_chrome(&self, cx: &mut Context<Self>) {
        self.sidebar_frame.update(cx, |_, cx| cx.notify());
        self.projects_frame.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Browser mode is on screen: it is the mode, and Settings are closed.
    pub(super) fn browser_mode(&self) -> bool {
        self.workspace.mode == AppMode::Browser && self.settings.is_none()
    }

    /// Where the rail is between the bar's modes: mid-swipe, mid-slide, or
    /// at rest on the mode.
    pub(super) fn mode_position(&self) -> f32 {
        let bar = &self.workspace.mode_bar;
        if let Some(swipe) = self
            .mode_swipe
            .as_ref()
            .filter(|s| s.horizontal == Some(true))
        {
            return swipe.position(self.mode_swipe_width, bar.shown.len());
        }
        if let Some(slide) = &self.mode_slide {
            return slide.at(Instant::now()).0;
        }
        bar.index(self.workspace.mode).unwrap_or(0) as f32
    }

    /// The rail is between modes: a frame is needed until it settles.
    pub(super) fn mode_moving(&self) -> bool {
        self.mode_slide.is_some()
            || self
                .mode_swipe
                .as_ref()
                .is_some_and(|s| s.horizontal == Some(true))
    }

    /// Ends a finished slide and a swipe whose end never came. Returns
    /// whether the rail still moves.
    pub(super) fn settle_mode_motion(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let now = Instant::now();
        if self.mode_slide.is_some_and(|slide| slide.at(now).1) {
            self.mode_slide = None;
            self.notify_mode_chrome(cx);
        }
        if self.mode_swipe.is_some()
            && self
                .mode_swipe_at
                .is_some_and(|at| now.saturating_duration_since(at) > SWIPE_STALE)
        {
            self.finish_mode_swipe(window, cx);
        }
        self.mode_moving()
    }

    /// How far into Browser mode the window is, 0 to 1, for its tint.
    pub(super) fn browser_presence(&self) -> f32 {
        if self.settings.is_some() {
            return 0.;
        }
        match self.workspace.mode_bar.index(AppMode::Browser) {
            Some(ix) => crate::modes::presence(ix, self.mode_position()),
            None => 0.,
        }
    }

    /// Shows `mode`, sliding the rail there from where it is.
    pub(super) fn set_mode(&mut self, mode: AppMode, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.mode_position();
        self.mode_swipe = None;
        self.switch_mode(mode, from, window, cx);
    }

    fn switch_mode(
        &mut self,
        mode: AppMode,
        from: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(to) = self.workspace.mode_bar.index(mode) else {
            return;
        };
        let to = to as f32;
        // Settings cover every mode: picking one closes them.
        if self.settings.is_some() {
            self.close_settings(window, cx);
        }
        self.mode_slide = (!cx.reduce_motion() && (from - to).abs() > 0.001)
            .then(|| ModeSlide::new(from, to, Instant::now()));
        if self.workspace.mode != mode {
            self.close_menu(cx);
            self.close_preview(cx);
            self.workspace.mode = mode;
            match mode {
                AppMode::Browser => self.enter_browser_mode(window, cx),
                AppMode::Dev => self.focus_workspace(window, cx),
            }
            self.persist(cx);
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// The bar's mode at `ix`, if it has one.
    pub(super) fn switch_mode_at(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(mode) = self.workspace.mode_bar.shown.get(ix).copied() {
            self.set_mode(mode, window, cx);
        }
    }

    /// The bar's next (`1`) or previous (`-1`) mode, wrapping.
    pub(super) fn step_mode(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let shown = &self.workspace.mode_bar.shown;
        let count = shown.len() as isize;
        if count < 2 {
            return;
        }
        let ix = self
            .workspace
            .mode_bar
            .index(self.workspace.mode)
            .unwrap_or(0) as isize;
        let next = (ix + step).rem_euclid(count) as usize;
        self.switch_mode_at(next, window, cx);
    }

    /// Browser mode takes the screen: its page, or a new tab when there is
    /// none at all.
    fn enter_browser_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_app_search(cx);
        self.browse_ensure_active();
        if self.workspace.browse.active.is_none() && self.browse_entries().is_empty() {
            self.browse_new_tab(None, None, true, window, cx);
            return;
        }
        self.focus_browse_page(window, cx);
    }

    /// The keyboard goes to the page on screen, or the workspace without one.
    pub(super) fn focus_browse_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.workspace.browse.active).and_then(|pane| self.browser_page(pane, cx)) {
            Some(page) => page.update(cx, |page, cx| page.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }

    // ---- The swipe ----

    /// A wheel event over the rail. A sideways two-finger swipe moves the
    /// rail between modes and returns true (the event is taken); anything
    /// else is left alone. Mice and momentum after the lift never switch.
    pub(super) fn mode_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.settings.is_some() || self.workspace.mode_bar.shown.len() < 2 {
            self.mode_swipe = None;
            return false;
        }
        let ScrollDelta::Pixels(delta) = event.delta else {
            return false;
        };
        let now = Instant::now();
        let (dx, dy) = (delta.x.as_f32(), delta.y.as_f32());
        match event.touch_phase {
            TouchPhase::Started => {
                // From the mode a slide is on its way to.
                let start = match &self.mode_slide {
                    Some(slide) => slide.target().round().max(0.) as usize,
                    None => self
                        .workspace
                        .mode_bar
                        .index(self.workspace.mode)
                        .unwrap_or(0),
                };
                let mut swipe = Swipe::new(start);
                let took = swipe.travel(dx, dy, now);
                self.mode_swipe = Some(swipe);
                self.mode_swipe_width = width;
                self.mode_swipe_at = Some(now);
                if took {
                    self.mode_slide = None;
                    self.notify_mode_chrome(cx);
                }
                took
            }
            TouchPhase::Moved => {
                let Some(swipe) = self.mode_swipe.as_mut() else {
                    return false;
                };
                let took = swipe.travel(dx, dy, now);
                self.mode_swipe_at = Some(now);
                if took {
                    self.mode_slide = None;
                    self.notify_mode_chrome(cx);
                }
                took
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let sideways =
                    (self.mode_swipe.as_ref()).is_some_and(|s| s.horizontal == Some(true));
                self.finish_mode_swipe(window, cx);
                sideways
            }
        }
    }

    /// The fingers lifted: the rail settles on the nearest mode, or the
    /// next one if the swipe was quick enough.
    fn finish_mode_swipe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode_swipe_at = None;
        let Some(swipe) = self.mode_swipe.take() else {
            return;
        };
        if swipe.horizontal != Some(true) {
            return;
        }
        let count = self.workspace.mode_bar.shown.len();
        let width = self.mode_swipe_width;
        let from = swipe.position(width, count);
        let target = swipe.settle(width, count);
        let mode = self.workspace.mode_bar.shown[target];
        self.switch_mode(mode, from, window, cx);
    }

    // ---- The mode bar ----

    pub(super) fn mode_menu(&self, mode: AppMode) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let bar = &self.workspace.mode_bar;
        let ix = bar.index(mode).unwrap_or(0);
        let mut items: Vec<MenuItem> = Vec::new();
        if self.workspace.mode != mode {
            items.push(
                MenuAction::new("switch", format!("Switch to {}", mode.label()))
                    .icon(mode.icon())
                    .shortcut(format!("⌃{}", ix + 1))
                    .into(),
            );
            items.push(MenuItem::Separator);
        }
        items.extend([
            MenuAction::new("left", "Move Left")
                .icon("arrow-left-2")
                .disabled(ix == 0)
                .into(),
            MenuAction::new("right", "Move Right")
                .icon("arrow-right-2")
                .disabled(ix + 1 >= bar.shown.len())
                .into(),
            MenuAction::new("hide", format!("Hide {}", mode.label()))
                .icon("eye")
                .disabled(bar.shown.len() <= 1)
                .into(),
        ]);
        let hidden = bar.hidden();
        if !hidden.is_empty() {
            items.push(MenuItem::Separator);
            for other in hidden {
                items.push(
                    MenuAction::new(
                        format!("show:{}", other.id()),
                        format!("Show {}", other.label()),
                    )
                    .icon(other.icon())
                    .into(),
                );
            }
        }
        if mode == AppMode::Browser {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Header("Browser color".into()));
            let current = self.workspace.browse.tint;
            for tint in BrowseTint::ALL {
                items.push(
                    MenuAction::new(format!("tint:{}", tint.id()), tint.label())
                        .icon("dot")
                        .icon_color(tint.swatch())
                        .checked(tint == current)
                        .into(),
                );
            }
        }
        (
            format!("{} mode", mode.label()).into(),
            items,
            220.,
            MenuStyle::Explorer,
        )
    }

    pub(super) fn run_mode_menu(
        &mut self,
        mode: AppMode,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bar = &mut self.workspace.mode_bar;
        match action {
            "switch" => return self.set_mode(mode, window, cx),
            "left" => {
                bar.shift(mode, -1);
            }
            "right" => {
                bar.shift(mode, 1);
            }
            "hide" => {
                if bar.hide(mode) && self.workspace.mode == mode {
                    let first = self.workspace.mode_bar.shown[0];
                    self.mode_slide = None;
                    self.set_mode(first, window, cx);
                }
            }
            _ => {
                if let Some(other) = action.strip_prefix("show:").and_then(AppMode::from_id) {
                    bar.show(other);
                } else if let Some(tint) =
                    action.strip_prefix("tint:").and_then(BrowseTint::from_id)
                {
                    self.workspace.browse.tint = tint;
                }
            }
        }
        // The rail rests on the mode's new place.
        self.mode_slide = None;
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// A mode button dropped on another's place.
    pub(super) fn move_mode(&mut self, mode: AppMode, to: usize, cx: &mut Context<Self>) {
        if self.workspace.mode_bar.index(mode) == Some(to) {
            return;
        }
        self.workspace.mode_bar.place(mode, to);
        self.mode_slide = None;
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    // ---- Browser mode's tabs ----

    /// The user's own tabs, then each project's pages, as the sidebar
    /// lists them; folded projects' pages are left out.
    pub(super) fn browse_entries(&self) -> Vec<i64> {
        let mut entries = self.workspace.browse.order();
        for project in self.browse_projects() {
            if !self.browse_folded(&project.path) {
                entries.extend(project.panes);
            }
        }
        entries
    }

    pub(super) fn browse_folded(&self, project: &str) -> bool {
        (self.workspace.browse.folded).contains(&monocode_core::files::path_key(project))
    }

    /// The pages of the window's projects that sit in a tab, by project in
    /// the rail's order, oldest page first.
    pub(super) fn browse_projects(&self) -> Vec<BrowseProject> {
        let mut panes: Vec<(i64, String)> = (self.workspace.browser_panes.iter())
            .filter(|(pane, _)| {
                !self.workspace.browse.owns(**pane) && self.workspace.tab_of(**pane).is_some()
            })
            .filter_map(|(pane, state)| {
                let project = (self.session_summaries.get(&state.owner))
                    .map(|owner| owner.cwd.clone())
                    .or_else(|| state.project.clone())?;
                Some((*pane, project))
            })
            .collect();
        panes.sort_by_key(|(pane, _)| pane.unsigned_abs());
        let rank = |path: &str| {
            let key = monocode_core::files::path_key(path);
            (self.workspace.projects.iter())
                .position(|p| monocode_core::files::path_key(p) == key)
                .unwrap_or(usize::MAX)
        };
        let mut projects: Vec<BrowseProject> = Vec::new();
        for (pane, path) in panes {
            let key = monocode_core::files::path_key(&path);
            match (projects.iter_mut()).find(|p| monocode_core::files::path_key(&p.path) == key) {
                Some(project) => project.panes.push(pane),
                None => projects.push(BrowseProject {
                    path,
                    panes: vec![pane],
                }),
            }
        }
        projects.sort_by_key(|project| rank(&project.path));
        projects
    }

    /// What the sidebar shows of the page of `pane`.
    pub(super) fn browse_tab(&self, pane: i64, cx: &App) -> Option<BrowseTab> {
        let state = self.workspace.browser_panes.get(&pane)?;
        let (url, title, favicon, loading) = match self.browser_pages.get(&pane) {
            Some((page, _)) => {
                let page = page.read(cx);
                (
                    page.url.clone(),
                    page.display_title(),
                    page.favicon.clone().or_else(|| state.favicon.clone()),
                    page.loading(),
                )
            }
            None => (
                state.url.clone(),
                crate::browser::page_title(&state.url, &state.title),
                state.favicon.clone(),
                false,
            ),
        };
        let browse = &self.workspace.browse;
        let title = match browse.name(pane) {
            Some(name) => name.to_string(),
            None if url.is_empty() => "New Tab".to_string(),
            None => title,
        };
        let home = (browse.home(pane))
            .filter(|home| crate::modes::away_from_home(home, &url))
            .map(str::to_string);
        Some(BrowseTab {
            pane,
            title,
            sound: self.browser_tab_sound(pane, cx),
            agent: state.opened_by.is_some(),
            section: browse.section(pane),
            home,
            // A tab of the user's own whose page is not open sleeps.
            sleeping: self.browser_discarded(pane, cx)
                || (browse.owns(pane) && !self.browser_pages.contains_key(&pane)),
            url,
            favicon,
            loading,
        })
    }

    /// A page the sidebar lists: the user's own, or a project's in a tab.
    pub(super) fn browse_exists(&self, pane: i64) -> bool {
        self.workspace.browse.owns(pane)
            || (self.workspace.browser_panes.contains_key(&pane)
                && self.workspace.tab_of(pane).is_some())
    }

    /// The page on screen is one that still exists; else the model's tab
    /// on screen, else the first entry.
    pub(super) fn browse_ensure_active(&mut self) {
        if self
            .workspace
            .browse
            .active
            .is_some_and(|pane| self.browse_exists(pane))
        {
            return;
        }
        let browse = &self.workspace.browse;
        self.workspace.browse.active = (browse.model.active_tab())
            .and_then(|tab| browse.pane_of(tab.id))
            .or_else(|| self.browse_entries().first().copied());
    }

    // ---- The model ----

    /// Every model tab has a pane and every pane of the user's own a model
    /// tab: panes are made for tabs that came (a new tab, Undo, a restore
    /// from the archive) and closed for tabs that went (archived). The page
    /// on screen follows the model's when its own went.
    pub(super) fn browse_reconcile(&mut self, cx: &mut Context<Self>) {
        let tabs: Vec<(u64, String, String)> = (self.workspace.browse.model.all_tabs())
            .map(|tab| {
                let title = tab.page_title.clone().unwrap_or_default();
                (tab.id.get(), tab.url.clone(), title)
            })
            .collect();
        for (id, url, title) in &tabs {
            let known = (self.workspace.browse.panes.get(id))
                .is_some_and(|pane| self.workspace.browser_panes.contains_key(pane));
            if !known {
                let pane = self.create_browser_pane(NO_OWNER, url.clone());
                if let Some(state) = self.workspace.browser_panes.get_mut(&pane) {
                    state.title = title.clone();
                }
                self.workspace.browse.panes.insert(*id, pane);
            }
        }
        let gone: Vec<(u64, i64)> = (self.workspace.browse.panes.iter())
            .filter(|(id, _)| !tabs.iter().any(|(t, _, _)| t == *id))
            .map(|(id, pane)| (*id, *pane))
            .collect();
        for (id, pane) in gone {
            self.workspace.browse.panes.remove(&id);
            self.workspace.browser_panes.remove(&pane);
            self.browser_pages.remove(&pane);
            self.workspace.browse.selection.retain(|p| *p != pane);
            if self.workspace.browse.active == Some(pane) {
                self.workspace.browse.active = None;
            }
        }
        if self.workspace.browse.active.is_none() {
            self.browse_ensure_active();
        }
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// A change to the model, then the panes made whole again.
    pub(super) fn browse_edit<R>(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut socode_browser::Browser, std::time::SystemTime) -> R,
    ) -> R {
        let result = edit(&mut self.workspace.browse.model, crate::modes::now());
        self.browse_reconcile(cx);
        result
    }

    /// The model tab of a pane of the user's own.
    pub(super) fn browse_model_tab(&self, pane: i64) -> Option<socode_browser::TabId> {
        self.workspace.browse.tab_of(pane)
    }

    /// A page of the user's own changed address or title: the model keeps
    /// it, for the archive, pinned addresses and the search.
    pub(super) fn browse_page_changed(&mut self, pane: i64, url: &str, title: &str) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        let model = &mut self.workspace.browse.model;
        if model.all_tabs().any(|t| t.id == tab && t.url != url) && !url.is_empty() {
            model.set_url(tab, url);
        }
        if !title.is_empty()
            && model
                .all_tabs()
                .any(|t| t.id == tab && t.page_title.as_deref() != Some(title))
        {
            model.set_title(tab, title);
        }
    }

    /// A new tab of the user's own, at the top of Today. `url` empty or
    /// none: its address field takes the keyboard. Shown when `foreground`.
    pub(super) fn browse_new_tab(
        &mut self,
        url: Option<String>,
        _after: Option<i64>,
        foreground: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> i64 {
        let previous = self.workspace.browse.model.active_tab().map(|tab| tab.id);
        let tab = self.browse_edit(cx, |model, now| {
            let tab = model.new_tab(url.unwrap_or_default(), now);
            if !foreground && let Some(previous) = previous {
                model.activate_tab(previous, now);
            }
            tab
        });
        let pane = self.workspace.browse.pane_of(tab).unwrap_or_default();
        if foreground {
            self.browse_select(pane, window, cx);
        }
        pane
    }

    /// Shows the page of `pane` (the user's own or a project's) in Browser
    /// mode, with the keyboard.
    pub(super) fn browse_select(&mut self, pane: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.workspace.browser_panes.contains_key(&pane) {
            return;
        }
        self.workspace.browse.active = Some(pane);
        if let Some(tab) = self.browse_model_tab(pane) {
            (self.workspace.browse.model).activate_tab(tab, crate::modes::now());
        }
        if self.workspace.mode != AppMode::Browser {
            self.set_mode(AppMode::Browser, window, cx);
        }
        self.focus_browse_page(window, cx);
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// ⌘W and a row's ✕: a Today tab goes to the archive; a favourite or
    /// pinned tab goes back to its pinned address and unloads, keeping its
    /// place. A project's page closes in its project.
    pub(super) fn browse_close(&mut self, pane: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return self.browse_remove(pane, window, cx);
        };
        let closed = self.browse_edit(cx, |model, now| model.close_tab(tab, now));
        if closed == Some(socode_browser::Closed::Unloaded) {
            self.browse_unload_page(pane);
            // The model moved its focus past the tab.
            if self.workspace.browse.active == Some(pane) {
                let browse = &self.workspace.browse;
                self.workspace.browse.active = (browse.model.active_tab())
                    .and_then(|t| browse.pane_of(t.id))
                    .filter(|p| *p != pane);
                self.browse_ensure_active();
            }
        }
        if self.browser_mode() {
            self.focus_browse_page(window, cx);
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// The page of a pane of the user's own closes; it opens again, at the
    /// model's address, when shown.
    fn browse_unload_page(&mut self, pane: i64) {
        self.browser_pages.remove(&pane);
        if let Some(tab) = self.workspace.browse.tab(pane).cloned()
            && let Some(state) = self.workspace.browser_panes.get_mut(&pane)
        {
            state.url = tab.url;
            state.title = tab.page_title.unwrap_or_default();
        }
    }

    /// A favourite or pinned tab put away without going home (a folder's
    /// Close All Tabs): its page unloads where it is.
    pub(super) fn browse_unload(&mut self, pane: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.browse.active == Some(pane) {
            return self.browse_close(pane, window, cx);
        }
        self.browse_unload_page(pane);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Takes the tab of `pane` out for good: the user's own leaves the
    /// model without going to the archive (Delete Tab); a project's page
    /// closes in its project too.
    pub(super) fn browse_remove(&mut self, pane: i64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.browse_model_tab(pane) {
            self.browse_edit(cx, |model, now| {
                if model.close_tab(tab, now) == Some(socode_browser::Closed::Archived) {
                    model.delete_archived(tab);
                } else {
                    model.remove_pin(tab, now);
                    if model.all_tabs().any(|t| t.id == tab)
                        && model.close_tab(tab, now) == Some(socode_browser::Closed::Archived)
                    {
                        model.delete_archived(tab);
                    }
                }
            });
        } else if self.workspace.browser_panes.contains_key(&pane) {
            let was_active = self.workspace.browse.active == Some(pane);
            self.close_pane(pane, window, cx);
            if was_active {
                self.workspace.browse.active = None;
                self.browse_ensure_active();
            }
        }
        if self.browser_mode() {
            self.focus_browse_page(window, cx);
        }
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// The next (`1`) or previous (`-1`) tab of the sidebar, wrapping.
    pub(super) fn browse_step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.browse_entries();
        if let Some(next) = adjacent(&entries, self.workspace.browse.active.as_ref(), step) {
            self.browse_select(next, window, cx);
        }
    }

    /// ⌘1–⌘9: the sidebar's tab at `ix` (-1: the last).
    pub(super) fn browse_activate(
        &mut self,
        ix: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.browse_entries();
        let pane = if ix < 0 {
            entries.last()
        } else {
            entries.get(ix as usize)
        };
        if let Some(pane) = pane.copied() {
            self.browse_select(pane, window, cx);
        }
    }

    /// ⇧⌘T: the tab archived last comes back at the top of Today.
    pub(super) fn browse_reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.browse_edit(cx, |model, now| model.restore_last_archived(now));
        if let Some(pane) = tab.and_then(|tab| self.workspace.browse.pane_of(tab)) {
            self.browse_select(pane, window, cx);
        }
    }

    /// ⌘Z and ⇧⌘Z in the sidebar: its last change undone or redone.
    pub(super) fn browse_undo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let done = self.browse_edit(cx, |model, now| {
            if redo {
                model.redo_sidebar(now)
            } else {
                model.undo_sidebar(now)
            }
        });
        if done && self.browser_mode() {
            let browse = &self.workspace.browse;
            if let Some(pane) = (browse.model.active_tab()).and_then(|t| browse.pane_of(t.id)) {
                self.browse_select(pane, window, cx);
            }
        }
    }

    /// Today tabs unseen for the archive's interval go to the archive; the
    /// page on screen and pages playing sound stay.
    pub(super) fn browse_archive_idle(&mut self, cx: &mut Context<Self>) {
        let protected: Vec<socode_browser::TabId> = (self.workspace.browse.panes.iter())
            .filter(|(_, pane)| {
                self.workspace.browse.active == Some(**pane)
                    || self.browser_tab_sound(**pane, cx).is_some()
            })
            .map(|(tab, _)| socode_browser::TabId::from_raw(*tab))
            .collect();
        let now = crate::modes::now();
        let archived = (self.workspace.browse.model).archive_idle_except(now, &protected);
        if archived > 0 {
            self.browse_reconcile(cx);
        }
    }

    /// The user's own tab joins the favourites or leaves them for Today.
    pub(super) fn browse_toggle_favorite(&mut self, pane: i64, cx: &mut Context<Self>) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        self.browse_edit(cx, |model, _| {
            if model.is_favorite(tab) {
                model.unfavorite_tab(tab)
            } else {
                model.favorite_tab(tab)
            }
        });
    }

    /// The user's own tab is pinned above the divider or goes back to Today.
    pub(super) fn browse_toggle_pin(&mut self, pane: i64, cx: &mut Context<Self>) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        let section = self.workspace.browse.section(pane);
        self.browse_edit(cx, |model, _| {
            if section == Some(BrowseSection::Favorites) {
                let space = model.space().id;
                model.move_tab(tab, space, socode_browser::Section::Pinned);
            } else {
                model.toggle_pin(tab);
            }
        });
    }

    /// A tab of the user's own goes to `section`, before `before` (dropped
    /// on that tab), else at the section's end (the top, for Today).
    pub(super) fn browse_place(
        &mut self,
        pane: i64,
        section: BrowseSection,
        before: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        let before = before
            .filter(|b| *b != pane)
            .and_then(|b| self.browse_model_tab(b));
        let from = self.workspace.browse.section(pane);
        let first_today = (self.workspace.browse.model.space().today.first()).map(|t| t.id);
        self.browse_edit(cx, |model, _| {
            let space = model.space().id;
            match section {
                BrowseSection::Favorites => {
                    if from != Some(BrowseSection::Favorites) && !model.favorite_tab(tab) {
                        return;
                    }
                    let order: Vec<_> = (model.active_user_profile().favorites.iter())
                        .filter(|t| !model.is_split_secondary(t.id))
                        .map(|t| t.id)
                        .collect();
                    let at = before.and_then(|b| order.iter().position(|t| *t == b));
                    let at = at.unwrap_or(order.len().saturating_sub(1));
                    model.move_favorite(tab, at);
                }
                BrowseSection::Pinned => match before {
                    Some(before) => {
                        if from == Some(BrowseSection::Favorites) {
                            model.move_tab(tab, space, socode_browser::Section::Pinned);
                        }
                        model.move_tab_relative(tab, before, false);
                    }
                    None => {
                        model.move_tab(tab, space, socode_browser::Section::Pinned);
                    }
                },
                BrowseSection::Today => {
                    if from != Some(BrowseSection::Today) {
                        model.move_tab(tab, space, socode_browser::Section::Today);
                    }
                    match before.or(first_today) {
                        Some(before) if before != tab => {
                            model.move_tab_relative(tab, before, false);
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    /// A favourite or pinned tab goes back to the address it is pinned at.
    pub(super) fn browse_back_home(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.browse_model_tab(pane) else {
            return;
        };
        let Some(home) = self.workspace.browse.home(pane).map(str::to_string) else {
            return;
        };
        self.workspace.browse.model.reset_pinned_tab(tab);
        match self.browser_pages.get(&pane) {
            Some((page, _)) => page
                .clone()
                .update(cx, |page, cx| page.open_address(home, cx)),
            None => {
                if let Some(state) = self.workspace.browser_panes.get_mut(&pane) {
                    state.url = home;
                }
            }
        }
        self.browse_select(pane, window, cx);
    }

    /// The page a favourite or pinned tab shows becomes its pinned address.
    pub(super) fn browse_replace_home(&mut self, pane: i64, cx: &mut Context<Self>) {
        if let Some(tab) = self.browse_model_tab(pane) {
            self.browse_edit(cx, |model, _| model.replace_pinned_url(tab));
        }
    }

    /// Clear: the Today tabs go to the archive, but the one on screen.
    pub(super) fn browse_clear(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.browse_edit(cx, |model, now| model.clear_today(now));
    }

    /// The Today tabs under `pane` go to the archive.
    pub(super) fn browse_close_below(
        &mut self,
        pane: i64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let today = self.workspace.browse.today();
        let Some(ix) = today.iter().position(|id| *id == pane) else {
            return;
        };
        let tabs: Vec<_> = (today[ix + 1..].iter())
            .filter_map(|p| self.browse_model_tab(*p))
            .collect();
        self.browse_edit(cx, |model, now| model.archive_sidebar_items(&tabs, now));
    }

    /// A project's pages fold away under its heading or come back.
    pub(super) fn browse_toggle_fold(&mut self, project: &str, cx: &mut Context<Self>) {
        let key = monocode_core::files::path_key(project);
        let folded = &mut self.workspace.browse.folded;
        if !folded.remove(&key) {
            folded.insert(key);
        }
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// The page on screen's history.
    pub(super) fn browse_history_step(&mut self, back: bool, cx: &mut Context<Self>) {
        let Some(page) =
            (self.workspace.browse.active).and_then(|pane| self.browser_page(pane, cx))
        else {
            return;
        };
        page.update(cx, |page, _| {
            if back {
                page.go_back()
            } else {
                page.go_forward()
            }
        });
    }

    /// What a page of the user's own asks for.
    pub(super) fn browse_page_request(
        &mut self,
        pane: i64,
        request: PageRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A Little window's page (shell/browse_little.rs).
        let request = match self.workspace.browse.little.contains(&pane) {
            true => match self.little_page_request(request, pane, cx) {
                Some(request) => request,
                None => return,
            },
            false => request,
        };
        match request {
            PageRequest::Open {
                url,
                foreground,
                popup,
            } => {
                let new = self.browse_new_tab(Some(url), Some(pane), false, window, cx);
                if let Some(id) = popup
                    && let Some(page) = self.browser_page(new, cx)
                {
                    page.update(cx, |page, cx| page.adopt_popup(id, cx));
                }
                if foreground {
                    self.browse_select(new, window, cx);
                }
            }
            PageRequest::Close => self.browse_close(pane, window, cx),
            PageRequest::Grab(result) => self.deliver_browser_grab(pane, result, cx),
            PageRequest::SiteControls(ask) => self.browse_site_ask(pane, ask, window, cx),
            PageRequest::ShowTab(other) => self.browse_select(other, window, cx),
            PageRequest::Peek(url) => self.browse_open_peek(pane, url, window, cx),
        }
    }

    /// The page's address and title go into the focused chat's composer,
    /// and the chat comes forward in Dev mode.
    pub(super) fn browse_send_to_chat(
        &mut self,
        pane: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.browse_tab(pane, cx) else {
            return;
        };
        if tab.url.is_empty() {
            return;
        }
        let Some((id, view)) = (self.browser_grab_target(pane))
            .and_then(|id| Some((id, self.ensure_session(id, cx)?)))
        else {
            self.notification_state
                .browser_grab
                .update(cx, |notice, cx| {
                    notice.show(
                        "There is no chat to send it to. Open a chat first.".into(),
                        cx,
                    )
                });
            cx.notify();
            return;
        };
        let link = if tab.title.is_empty() || tab.title == tab.url {
            tab.url.clone()
        } else {
            format!("{} — {}", tab.title, tab.url)
        };
        view.update(cx, |chat, cx| {
            chat.input.update(cx, |input, cx| {
                let mut text = input.content.to_string();
                if !text.is_empty() && !text.ends_with(char::is_whitespace) {
                    text.push('\n');
                }
                text.push_str(&link);
                input.set_text(text, cx);
            })
        });
        self.set_mode(AppMode::Dev, window, cx);
        self.pending_chat_reveal = Some(id);
        self.pending_composer_focus = Some(id);
        cx.notify();
    }

    /// "Move to ›": the sections it is not in, a new folder and every
    /// folder by its path, as SOCode's.
    fn browse_move_to_items(&self, section: Option<BrowseSection>) -> Vec<MenuAction> {
        let browse = &self.workspace.browse;
        let mut items = Vec::new();
        if section != Some(BrowseSection::Favorites) {
            items.push(
                MenuAction::new("move:favorites", "Favorites").disabled(browse.favorites_full()),
            );
        }
        if section != Some(BrowseSection::Pinned) {
            items.push(MenuAction::new("move:pinned", "Pinned"));
        }
        if section != Some(BrowseSection::Today) {
            items.push(MenuAction::new("move:today", "Today"));
        }
        items.push(MenuAction::new("folder:new", "New Folder…"));
        for (id, path, _) in browse.folder_paths() {
            items.push(MenuAction::new(format!("folder:{id}"), path));
        }
        items
    }

    /// A tab's menu, as SOCode's: copy and share, its name and sound, its
    /// pinned page, duplicate and move, then what takes it out of the
    /// sidebar. A project's page leads to its project instead.
    pub(super) fn browse_tab_menu(
        &self,
        pane: i64,
        cx: &App,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let chosen = self.browse_chosen(pane);
        if chosen.len() > 1 {
            return self.browse_selection_menu(&chosen);
        }
        let browse = &self.workspace.browse;
        let section = browse.section(pane);
        let tab = self.browse_tab(pane, cx);
        let blank = tab.as_ref().is_none_or(|tab| tab.url.is_empty());
        let away = tab.as_ref().is_some_and(|tab| tab.home.is_some());
        let muted = self.browser_tab_sound(pane, cx) == Some(browser_audio::TabSound::Muted);
        let mut items: Vec<MenuItem> = Vec::new();
        // A Split View's row: the split's own items first, as SOCode's.
        if let Some(panes) = self.browse_split_of(pane) {
            let vertical = (browse.tab_of(pane))
                .and_then(|tab| browse.model.split_for(tab))
                .is_some_and(|split| split.axis == socode_browser::splits::Axis::Vertical);
            items.extend([
                MenuAction::new("split-separate", "Separate Tab").into(),
                MenuAction::new("split-separate-all", "Separate All Tabs").into(),
                MenuAction::new("split-add", "Add Tab…")
                    .disabled(panes.len() >= socode_browser::splits::MAX_SPLIT_PANES)
                    .into(),
                MenuAction::new(
                    "split-flip",
                    if vertical {
                        "Convert to Horizontal Split View"
                    } else {
                        "Convert to Vertical Split View"
                    },
                )
                .into(),
                MenuAction::new("split-close", "Close Pane")
                    .shortcut("⌃⇧-")
                    .into(),
                MenuItem::Separator,
            ]);
        }
        items.extend([
            MenuAction::new("copy-link", "Copy Link")
                .shortcut(format!("⇧{MOD}C"))
                .disabled(blank)
                .into(),
            MenuAction::new("share", "Share")
                .submenu(vec![
                    MenuAction::new("copy-markdown", "Copy Link as Markdown").disabled(blank),
                ])
                .into(),
            MenuItem::Separator,
        ]);
        if section.is_some() {
            items.push(MenuAction::new("rename", "Rename…").into());
            if browse.name(pane).is_some() {
                items.push(MenuAction::new("reset-name", "Reset Name").into());
            }
            let target = super::browse_organize::IconTarget::Tab(pane);
            items.push(MenuAction::new("change-icon", "Change Icon…").into());
            if self.browse_custom_icon(target).is_some() {
                items.push(MenuAction::new("reset-icon", "Reset Icon").into());
            }
        }
        items.push(
            MenuAction::new(
                if muted { "unmute" } else { "mute" },
                if muted { "Unmute" } else { "Mute" },
            )
            .disabled(blank)
            .into(),
        );
        if matches!(
            section,
            Some(BrowseSection::Pinned | BrowseSection::Favorites)
        ) {
            items.push(
                MenuAction::new("edit-pinned", "Edit Pinned Page")
                    .submenu(vec![
                        MenuAction::new("replace-home", "Replace Pinned URL with Current")
                            .disabled(!away),
                        MenuAction::new("home", "Back to Pinned URL").disabled(!away),
                    ])
                    .into(),
            );
        }
        items.push(MenuItem::Separator);
        // A page of the user's own joins the page on screen, or is joined.
        if section.is_some() && !blank && self.browse_split_of(pane).is_none() {
            let on_screen = browse.active == Some(pane);
            let split = MenuAction::new(
                "split-add",
                if on_screen {
                    "Add Split View"
                } else {
                    "Open in Split View"
                },
            );
            items.push(
                if on_screen {
                    split.shortcut("⌃⇧=")
                } else {
                    split
                }
                .into(),
            );
        }
        items.push(
            MenuAction::new("duplicate", "Duplicate")
                .shortcut("⌥D")
                .disabled(blank)
                .into(),
        );
        if section.is_some() {
            items.push(
                MenuAction::new("move-to", "Move to")
                    .submenu(self.browse_move_to_items(section))
                    .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            MenuAction::new("send-to-chat", "Send to Chat")
                .disabled(blank)
                .into(),
        );
        if section.is_none() {
            items.push(MenuAction::new("show-in-project", "Show in Dev Mode").into());
        }
        items.push(MenuItem::Separator);
        match section {
            Some(BrowseSection::Favorites) => {
                items.push(MenuAction::new("favorite", "Remove Favorite").into())
            }
            Some(BrowseSection::Pinned) => items.push(MenuAction::new("pin", "Remove Pin").into()),
            Some(BrowseSection::Today) => {
                let today = browse.today();
                let below = (today.iter().position(|id| *id == pane))
                    .is_some_and(|ix| ix + 1 < today.len());
                items.push(MenuAction::new("close", "Archive Tab").into());
                items.push(
                    MenuAction::new("close-below", "Archive Tabs Below")
                        .disabled(!below)
                        .into(),
                );
            }
            None => items.push(MenuAction::new("close", "Close Tab").into()),
        }
        ("Tab".into(), items, 208., MenuStyle::Native)
    }

    pub(super) fn run_browse_tab_menu(
        &mut self,
        pane: i64,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = self.browse_chosen(pane);
        if chosen.len() > 1 {
            return self.run_browse_selection_menu(chosen, action, window, cx);
        }
        let tab = self.browse_tab(pane, cx);
        let url = tab.as_ref().map(|tab| tab.url.clone()).unwrap_or_default();
        match action {
            "rename" => self.browse_rename(
                super::session_groups::RenameTarget::BrowseTab(pane),
                window,
                cx,
            ),
            "reset-name" => self.browse_reset_name(pane, cx),
            "change-icon" => self.browse_change_icon(
                super::browse_organize::IconTarget::Tab(pane),
                window,
                cx,
            ),
            "reset-icon" => {
                self.browse_set_icon(super::browse_organize::IconTarget::Tab(pane), None, cx)
            }
            "split-close" => self.browse_close_split_pane(pane, window, cx),
            "home" => self.browse_back_home(pane, window, cx),
            "replace-home" => self.browse_replace_home(pane, cx),
            "favorite" => self.browse_toggle_favorite(pane, cx),
            "pin" => self.browse_toggle_pin(pane, cx),
            "move:favorites" => self.browse_place(pane, BrowseSection::Favorites, None, cx),
            "move:pinned" => self.browse_place(pane, BrowseSection::Pinned, None, cx),
            "move:today" => self.browse_place(pane, BrowseSection::Today, None, cx),
            "duplicate" => {
                let after = self.workspace.browse.owns(pane).then_some(pane);
                self.browse_new_tab(Some(url), after, true, window, cx);
            }
            "copy-link" => cx.write_to_clipboard(gpui::ClipboardItem::new_string(url)),
            "copy-markdown" => {
                let title = tab.map(|tab| tab.title).unwrap_or_default();
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(markdown_link(&title, &url)))
            }
            "mute" | "unmute" => self.set_browser_muted(pane, action == "mute", cx),
            "send-to-chat" => self.browse_send_to_chat(pane, window, cx),
            "show-in-project" => {
                self.set_mode(AppMode::Dev, window, cx);
                self.open_browser_pane(pane, window, cx);
            }
            "split-add" => match self.workspace.browse.active {
                // The page on screen, or one of its split: pick what joins.
                Some(active) if active == pane || self.browse_split_of(pane).is_some() => self
                    .browse_open_command_bar(
                        super::command_bar::BarTarget::Split(pane),
                        window,
                        cx,
                    ),
                Some(active) => self.browse_add_split(active, pane, window, cx),
                None => {}
            },
            "split-separate-all" => self.browse_separate_all(pane, cx),
            "split-flip" => self.browse_flip_split(pane, cx),
            "split-separate" => self.browse_separate_pane(pane, cx),
            "close" => self.browse_close(pane, window, cx),
            "remove" => self.browse_remove(pane, window, cx),
            "close-below" => self.browse_close_below(pane, window, cx),
            _ if action.starts_with("folder:") => {
                let folder = action["folder:".len()..].parse().ok();
                self.browse_add_to_folder(&[pane], folder, window, cx);
            }
            "close-others" => {
                let others: Vec<i64> = (self.workspace.browse.today().into_iter())
                    .filter(|id| *id != pane)
                    .collect();
                let tabs: Vec<_> = others
                    .iter()
                    .filter_map(|p| self.browse_model_tab(*p))
                    .collect();
                self.browse_edit(cx, |model, now| model.archive_sidebar_items(&tabs, now));
                self.browse_select(pane, window, cx);
            }
            _ => {}
        }
    }

    /// The menu of the sidebar's empty room.
    pub(super) fn browse_sidebar_menu(&self) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let browse = &self.workspace.browse;
        let items: Vec<MenuItem> = vec![
            MenuAction::new("new-tab", "New Tab")
                .shortcut(format!("{MOD}T"))
                .into(),
            MenuAction::new("reopen", "Reopen Closed Tab")
                .shortcut(format!("⇧{MOD}T"))
                .disabled(browse.model.archive.is_empty())
                .into(),
            MenuAction::new("new-folder", "New Folder").into(),
            MenuItem::Separator,
            MenuAction::new("clear", "Clear Today")
                .disabled(browse.today().is_empty())
                .into(),
        ];
        ("Sidebar".into(), items, 232., MenuStyle::Native)
    }

    pub(super) fn run_browse_sidebar_menu(
        &mut self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "new-tab" => {
                self.browse_open_command_bar(super::command_bar::BarTarget::NewTab, window, cx);
            }
            "reopen" => self.browse_reopen(window, cx),
            "new-folder" => {
                self.browse_new_folder(super::browse_organize::PinSpot::End, &[], window, cx);
            }
            "clear" => self.browse_clear(window, cx),
            _ => {}
        }
    }

    /// The actions Browser mode adds to the workspace's element.
    pub(super) fn browse_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, action: &SwitchMode, window, cx| {
            this.switch_mode_at(action.0, window, cx)
        }))
        .on_action(cx.listener(|this, _: &NextMode, window, cx| this.step_mode(1, window, cx)))
        .on_action(cx.listener(|this, _: &PreviousMode, window, cx| this.step_mode(-1, window, cx)))
        .on_action(cx.listener(|this, _: &BrowseNewTab, window, cx| {
            if this.browser_mode() {
                this.browse_open_command_bar(super::command_bar::BarTarget::NewTab, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseCloseTab, window, cx| {
            if this.browser_mode() && this.workspace.browse.peek.is_some() {
                this.browse_close_peek(cx);
                this.focus_browse_page(window, cx);
                return;
            }
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode()) {
                let chosen = this.browse_chosen(pane);
                this.browse_close_chosen(chosen, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseCopyLink, window, cx| {
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode()) {
                this.run_browse_tab_menu(pane, "copy-link", window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseDuplicate, window, cx| {
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode()) {
                this.run_browse_tab_menu(pane, "duplicate", window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseUndo, window, cx| {
            if this.browser_mode() {
                this.browse_undo(false, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseRedo, window, cx| {
            if this.browser_mode() {
                this.browse_undo(true, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseReopenTab, window, cx| {
            if this.browser_mode() {
                this.browse_reopen(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseNextTab, window, cx| {
            if this.browser_mode() {
                this.browse_step(1, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowsePreviousTab, window, cx| {
            if this.browser_mode() {
                this.browse_step(-1, window, cx);
            }
        }))
        .on_action(cx.listener(|this, action: &BrowseActivateTab, window, cx| {
            if this.browser_mode() {
                this.browse_activate(action.0, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseBack, _, cx| {
            if this.browser_mode() {
                this.browse_history_step(true, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseForward, _, cx| {
            if this.browser_mode() {
                this.browse_history_step(false, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseFocusAddress, window, cx| {
            if this.browser_mode() {
                let target = match this.workspace.browse.active {
                    Some(pane) => super::command_bar::BarTarget::Current(pane),
                    None => super::command_bar::BarTarget::NewTab,
                };
                this.browse_open_command_bar(target, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &BrowseToggleFavorite, _, cx| {
            if let Some(pane) = this.workspace.browse.active.filter(|_| this.browser_mode()) {
                this.browse_toggle_favorite(pane, cx);
            }
        }))
    }
}

/// `[title](url)`, with the brackets in the title escaped.
pub(super) fn markdown_link(title: &str, url: &str) -> String {
    let title = if title.is_empty() { url } else { title };
    let title = title.replace('[', "\\[").replace(']', "\\]");
    format!("[{title}]({url})")
}

// ---- Development hook ----

impl Shell {
    /// Development hook, not a user feature: with `MONOCODE_MODES_DEV=<file>`
    /// set, the app reads a command a script writes to the file, then
    /// removes it, and appends what it did to `<file>.log`:
    /// `mode dev|browser`, `tab <url>`, `favorite`, `tint <name>`,
    /// `hold <position>` (holds the rail mid-swipe, as fingers would),
    /// `release`, `compact on|off` and `shot <png>`, a picture of the window
    /// drawn off screen (Chromium's pages are not in it).
    pub(super) fn start_modes_dev_hook(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(file) = std::env::var_os("MONOCODE_MODES_DEV").filter(|f| !f.is_empty()) else {
            return;
        };
        let file = std::path::PathBuf::from(file);
        let log = file.with_extension("log");
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let Ok(command) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let _ = std::fs::remove_file(&file);
                for line in command.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let (verb, arg) = line.split_once(' ').unwrap_or((line, ""));
                    let arg = arg.trim().to_string();
                    if verb == "shot" {
                        cx.background_executor()
                            .timer(Duration::from_millis(500))
                            .await;
                        // GPUI draws off screen only with its test support.
                        #[cfg(feature = "selftest")]
                        let result: Result<(), String> =
                            match cx.update(|window, _| window.render_to_image()) {
                                Ok(Ok(image)) => image.save(&arg).map_err(|e| e.to_string()),
                                Ok(Err(error)) => Err(error.to_string()),
                                Err(_) => Err("the window closed".into()),
                            };
                        #[cfg(not(feature = "selftest"))]
                        let result: Result<(), String> =
                            Err("build with --features selftest for pictures".into());
                        append(&log, &format!("shot {arg}: {result:?}"));
                        continue;
                    }
                    let done = this.update_in(cx, |this, window, cx| {
                        this.run_modes_dev_command(verb, &arg, window, cx)
                    });
                    append(&log, &format!("{line}: {done:?}"));
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                }
            }
        })
        .detach();

        fn append(log: &std::path::Path, line: &str) {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn run_modes_dev_command(
        &mut self,
        verb: &str,
        arg: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match verb {
            "mode" => {
                let mode = AppMode::from_id(arg).ok_or("unknown mode")?;
                self.set_mode(mode, window, cx);
            }
            "tab" => {
                self.browse_new_tab(Some(arg.to_string()), None, true, window, cx);
            }
            "favorite" => {
                let pane = self.workspace.browse.active.ok_or("no tab")?;
                self.browse_toggle_favorite(pane, cx);
            }
            "pin" => {
                let pane = self.workspace.browse.active.ok_or("no tab")?;
                self.browse_toggle_pin(pane, cx);
            }
            // The tab on screen goes into the folder named `arg`, made if new.
            "folder" => {
                let pane = self.workspace.browse.active.ok_or("no tab")?;
                let existing = (self.workspace.browse.folder_paths().into_iter())
                    .find(|(_, path, _)| path == arg)
                    .map(|(id, _, _)| id);
                match existing {
                    Some(id) => self.browse_add_to_folder(&[pane], Some(id), window, cx),
                    None => {
                        let id = self
                            .browse_new_folder(
                                super::browse_organize::PinSpot::End,
                                &[pane],
                                window,
                                cx,
                            )
                            .ok_or("no folder")?;
                        self.finish_rename(false, window, cx);
                        self.finish_browse_rename(
                            &super::session_groups::RenameTarget::BrowseFolder(id),
                            arg,
                            cx,
                        );
                    }
                }
            }
            // The Command Bar for a new tab, `arg` typed in it.
            "bar" => {
                self.browse_open_command_bar(super::command_bar::BarTarget::NewTab, window, cx);
                if let Some(bar) = &self.command_bar {
                    let input = bar.input.clone();
                    input.update(cx, |field, cx| field.set_text(arg.to_string(), cx));
                }
            }
            "name" => {
                let pane = self.workspace.browse.active.ok_or("no tab")?;
                self.finish_browse_rename(
                    &super::session_groups::RenameTarget::BrowseTab(pane),
                    arg,
                    cx,
                );
            }
            "tint" => {
                self.workspace.browse.tint = BrowseTint::from_id(arg).ok_or("unknown tint")?;
                self.invalidate_chrome(cx);
            }
            "hold" => {
                let position: f32 = arg.parse().map_err(|_| "not a number")?;
                let start = self
                    .workspace
                    .mode_bar
                    .index(self.workspace.mode)
                    .unwrap_or(0);
                let width = self.project_rail_drawn_width() - 1.;
                let mut swipe = Swipe::new(start);
                let now = Instant::now();
                swipe.travel(-(position - start as f32) * width, 0., now);
                swipe.travel(0., 0., now);
                self.mode_swipe = Some(swipe);
                self.mode_swipe_width = width;
                // Held: never stale while the picture is taken.
                self.mode_swipe_at = Some(now + Duration::from_secs(3600));
                self.mode_slide = None;
                self.invalidate_chrome(cx);
            }
            "release" => {
                self.mode_swipe_at = None;
                self.finish_mode_swipe(window, cx);
            }
            "compact" => {
                self.workspace.project_rail_open = arg != "on";
                self.invalidate_chrome(cx);
            }
            "group" => {
                self.new_browser_pane(Some(arg.to_string()), window, cx);
            }
            _ => return Err(format!("unknown command {verb}")),
        }
        cx.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn browse_window(cx: &mut TestAppContext) -> (gpui::WindowHandle<Shell>, i64) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, session) = super::super::tests::window(cx);
        // The original layout, whose project rail holds the mode bar.
        window
            .update(cx, |shell, _, cx| {
                shell.workspace.classic_layout = true;
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        cx.run_until_parked();
        (window, session)
    }

    #[gpui::test]
    fn browser_mode_starts_with_a_tab_of_its_own_and_dev_mode_keeps_its_workspace(
        cx: &mut TestAppContext,
    ) {
        let (window, session) = browse_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.set_mode(AppMode::Browser, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let (pane, active_pane) = window
            .update(cx, |shell, _, _| {
                assert!(shell.browser_mode());
                let browse = &shell.workspace.browse;
                assert_eq!(browse.today().len(), 1, "a first tab of its own");
                (browse.today()[0], shell.workspace.active)
            })
            .unwrap();
        window
            .update(cx, |shell, _, _| {
                let state = &shell.workspace.browser_panes[&pane];
                assert_eq!(state.owner, NO_OWNER);
                assert!(
                    shell.workspace.tab_of(pane).is_none(),
                    "in no tab of the workspace"
                );
                assert_eq!(shell.workspace.browse.active, Some(pane));
            })
            .unwrap();
        // Saving keeps it; Dev mode is as it was.
        window
            .update(cx, |shell, window, cx| {
                shell.persist(cx);
                assert!(shell.workspace.browser_panes.contains_key(&pane));
                shell.set_mode(AppMode::Dev, window, cx);
                assert!(!shell.browser_mode());
                assert_eq!(shell.workspace.active, active_pane);
            })
            .unwrap();
    }

    #[gpui::test]
    fn own_tabs_survive_a_restore_and_project_pages_are_listed_by_project(cx: &mut TestAppContext) {
        let (window, session) = browse_window(cx);
        let (own, project_page) = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("https://project.test/".into()), window, cx);
                let project_page = shell.workspace.active.unwrap();
                shell.set_mode(AppMode::Browser, window, cx);
                let own =
                    shell.browse_new_tab(Some("https://own.test/".into()), None, true, window, cx);
                (own, project_page)
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let projects = shell.browse_projects();
                assert_eq!(projects.len(), 1);
                assert_eq!(projects[0].panes, vec![project_page]);
                let entries = shell.browse_entries();
                assert_eq!(entries.first(), Some(&own));
                assert!(entries.contains(&project_page));
                // The saved workspace restores Browser mode and its tab.
                let saved = serde_json::to_string(&shell.workspace).unwrap();
                let sessions: Vec<(i64, String)> = shell
                    .session_summaries
                    .values()
                    .map(|s| (s.id, s.cwd.clone()))
                    .collect();
                let restored =
                    crate::workspace::Workspace::restore(Some(&saved), &sessions, "/tmp");
                assert_eq!(restored.mode, AppMode::Browser);
                assert!(restored.browse.owns(own));
                assert!(restored.browser_panes.contains_key(&own));
                assert_eq!(restored.browser_panes[&own].url, "https://own.test/");
            })
            .unwrap();
    }

    #[gpui::test]
    fn closing_tabs_moves_down_the_list_and_reopen_brings_one_back(cx: &mut TestAppContext) {
        let (window, _) = browse_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.set_mode(AppMode::Browser, window, cx);
                let first = shell.workspace.browse.today()[0];
                let a =
                    shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx);
                let b =
                    shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx);
                // New tabs go on top.
                assert_eq!(shell.browse_entries(), vec![b, a, first]);
                shell.browse_close(b, window, cx);
                assert_eq!(shell.workspace.browse.active, Some(a));
                assert!(!shell.workspace.browser_panes.contains_key(&b));
                shell.browse_reopen(window, cx);
                let reopened = shell.workspace.browse.active.unwrap();
                assert_eq!(
                    shell.workspace.browser_panes[&reopened].url,
                    "https://b.test/"
                );
                // Favourites lead the list; ⌘1 is the first of them.
                shell.browse_toggle_favorite(first, cx);
                assert_eq!(shell.browse_entries()[0], first);
                shell.browse_activate(0, window, cx);
                assert_eq!(shell.workspace.browse.active, Some(first));
                shell.browse_step(-1, window, cx);
                assert_eq!(shell.workspace.browse.active, Some(a), "wraps to the last");
            })
            .unwrap();
    }

    #[gpui::test]
    fn pinned_tabs_go_home_and_clear_archives_today_until_undone(cx: &mut TestAppContext) {
        let (window, _) = browse_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.set_mode(AppMode::Browser, window, cx);
                let first = shell.workspace.browse.today()[0];
                let pinned =
                    shell.browse_new_tab(Some("https://p.test/".into()), None, true, window, cx);
                shell.browse_toggle_pin(pinned, cx);
                assert_eq!(shell.workspace.browse.pinned_tabs(), vec![pinned]);
                assert_eq!(shell.workspace.browse.home(pinned), Some("https://p.test/"));
                let a =
                    shell.browse_new_tab(Some("https://a.test/".into()), None, true, window, cx);
                let b =
                    shell.browse_new_tab(Some("https://b.test/".into()), None, true, window, cx);
                assert_eq!(shell.workspace.browse.today(), vec![b, a, first]);
                // A link a pinned tab opens goes to the top of Today, behind.
                let opened = shell.browse_new_tab(
                    Some("https://link.test/".into()),
                    Some(pinned),
                    false,
                    window,
                    cx,
                );
                assert_eq!(shell.workspace.browse.today()[0], opened);
                assert_eq!(shell.workspace.browse.active, Some(b));
                // Away from its address, ⌘W takes a pinned tab home and
                // unloads it; it keeps its place.
                shell.browse_select(pinned, window, cx);
                // As a page's navigation does: the model hears it.
                shell.browse_page_changed(pinned, "https://p.test/away", "Away");
                let tab = shell.workspace.browse.tab(pinned).unwrap();
                assert_eq!(tab.url, "https://p.test/away");
                assert_eq!(tab.pinned_url.as_deref(), Some("https://p.test/"));
                shell.browse_close(pinned, window, cx);
                assert!(shell.workspace.browse.owns(pinned));
                assert_ne!(shell.workspace.browse.active, Some(pinned));
                assert_eq!(
                    shell.workspace.browser_panes[&pinned].url,
                    "https://p.test/"
                );
                // Clear archives Today but the tab on screen; ⌘Z brings
                // them back.
                shell.browse_select(opened, window, cx);
                shell.browse_clear(window, cx);
                assert_eq!(shell.workspace.browse.today(), vec![opened]);
                assert!(!shell.workspace.browser_panes.contains_key(&a));
                shell.browse_undo(false, window, cx);
                assert_eq!(shell.workspace.browse.today().len(), 4);
                // ⇧⌘T brings back the last tab archived.
                let back = shell.workspace.browse.today()[1];
                shell.browse_close(back, window, cx);
                assert_eq!(shell.workspace.browse.today().len(), 3);
                shell.browse_reopen(window, cx);
                assert_eq!(shell.workspace.browse.today().len(), 4);
                // Dropped on a pinned row, a Today tab is pinned before it.
                let a = shell.workspace.browse.today()[2];
                shell.browse_place(a, BrowseSection::Pinned, Some(pinned), cx);
                assert_eq!(shell.workspace.browse.pinned_tabs(), vec![a, pinned]);
                // Its menu offers its pinned address, and no Close Tabs Below.
                let (_, items, _, _) = shell.browse_tab_menu(a, cx);
                let ids: Vec<String> = items
                    .iter()
                    .flat_map(|item| match item {
                        MenuItem::Action(action) => std::iter::once(action.id.to_string())
                            .chain(action.submenu.iter().map(|sub| sub.id.to_string()))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .collect();
                // "Edit Pinned Page ›" holds it; "Remove Pin" ends the menu.
                assert!(ids.contains(&"home".to_string()), "{ids:?}");
                assert!(ids.contains(&"pin".to_string()), "{ids:?}");
                assert!(!ids.contains(&"close-below".to_string()), "{ids:?}");
                assert_eq!(
                    markdown_link("A [b]", "https://a.test/"),
                    "[A \\[b\\]](https://a.test/)"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn tabs_go_into_folders_take_names_and_act_together(cx: &mut TestAppContext) {
        let (window, _) = browse_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.set_mode(AppMode::Browser, window, cx);
                let a =
                    shell.browse_new_tab(Some("https://x.test/a".into()), None, true, window, cx);
                let b =
                    shell.browse_new_tab(Some("https://x.test/b".into()), None, true, window, cx);
                let c =
                    shell.browse_new_tab(Some("https://y.test/".into()), None, true, window, cx);
                // ⌘-click chooses a second row; the menu acts on both.
                shell.browse_select(a, window, cx);
                let cmd = gpui::Modifiers {
                    platform: true,
                    ..Default::default()
                };
                shell.browse_click(b, cmd, window, cx);
                assert_eq!(shell.browse_chosen(a), vec![b, a]);
                let (title, _, _, _) = shell.browse_tab_menu(a, cx);
                assert_eq!(title.as_ref(), "2 Tabs");
                // Into a new folder, named for their site, its name being typed.
                shell.run_browse_tab_menu(a, "folder:new", window, cx);
                let (id, path, _) = shell.workspace.browse.folder_paths()[0].clone();
                assert_eq!(path, "x.test");
                assert_eq!(shell.workspace.browse.folder_tabs(id), vec![b, a]);
                assert!(shell.renaming.is_some());
                shell.finish_rename(false, window, cx);
                assert!(shell.workspace.browse.selection.is_empty());
                // A plain click lets a choice go.
                shell.browse_click(c, gpui::Modifiers::default(), window, cx);
                assert_eq!(shell.browse_chosen(c), vec![c]);
                // A tab's own name stands over its page's title until reset.
                shell.finish_browse_rename(
                    &super::super::session_groups::RenameTarget::BrowseTab(c),
                    "Mine",
                    cx,
                );
                assert_eq!(shell.browse_tab(c, cx).unwrap().title, "Mine");
                shell.run_browse_tab_menu(c, "reset-name", window, cx);
                assert_ne!(shell.browse_tab(c, cx).unwrap().title, "Mine");
                // Its menu offers the folder by name; picking it moves the tab in.
                let (_, items, _, _) = shell.browse_tab_menu(c, cx);
                let submenu = items
                    .iter()
                    .find_map(|item| match item {
                        MenuItem::Action(action) if action.id.as_ref() == "move-to" => {
                            Some(action.submenu.clone())
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(submenu.iter().any(|item| item.label.as_ref() == "x.test"));
                shell.run_browse_tab_menu(c, &format!("folder:{id}"), window, cx);
                assert_eq!(shell.workspace.browse.folder_tabs(id), vec![b, a, c]);
                // Deleting the folder keeps its tabs pinned.
                shell.run_browse_folder_menu(id, "delete", window, cx);
                assert_eq!(shell.workspace.browse.pinned_tabs(), vec![b, a, c]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_bar_hides_and_moves_modes_and_ctrl_keys_follow_its_order(cx: &mut TestAppContext) {
        let (window, _) = browse_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.run_mode_menu(AppMode::Browser, "left", window, cx);
                assert_eq!(
                    shell.workspace.mode_bar.shown,
                    vec![AppMode::Browser, AppMode::Dev]
                );
                shell.switch_mode_at(0, window, cx);
                assert_eq!(shell.workspace.mode, AppMode::Browser);
                // Hiding the mode on screen goes to the first one left.
                shell.run_mode_menu(AppMode::Browser, "hide", window, cx);
                assert_eq!(shell.workspace.mode_bar.shown, vec![AppMode::Dev]);
                assert_eq!(shell.workspace.mode, AppMode::Dev);
                shell.run_mode_menu(AppMode::Dev, "show:browser", window, cx);
                shell.run_mode_menu(AppMode::Browser, "tint:teal", window, cx);
                assert_eq!(shell.workspace.browse.tint, BrowseTint::Teal);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_sideways_swipe_over_the_rail_switches_and_a_vertical_one_scrolls(cx: &mut TestAppContext) {
        let (window, _) = browse_window(cx);
        let wheel = |dx: f32, dy: f32, phase: TouchPhase| ScrollWheelEvent {
            position: gpui::point(px(40.), px(300.)),
            delta: ScrollDelta::Pixels(gpui::point(px(dx), px(dy))),
            modifiers: gpui::Modifiers::default(),
            touch_phase: phase,
        };
        window
            .update(cx, |shell, window, cx| {
                let width = 220.;
                assert!(!shell.mode_wheel(&wheel(0., 0., TouchPhase::Started), width, window, cx));
                assert!(!shell.mode_wheel(&wheel(-2., 30., TouchPhase::Moved), width, window, cx));
                assert!(!shell.mode_wheel(&wheel(-2., 30., TouchPhase::Moved), width, window, cx));
                shell.mode_wheel(&wheel(0., 0., TouchPhase::Ended), width, window, cx);
                assert_eq!(shell.workspace.mode, AppMode::Dev);
                // Momentum after the lift does nothing.
                assert!(!shell.mode_wheel(&wheel(-40., 0., TouchPhase::Moved), width, window, cx));
                shell.mode_wheel(&wheel(0., 0., TouchPhase::Started), width, window, cx);
                for _ in 0..6 {
                    assert!(
                        shell.mode_wheel(&wheel(-20., 0., TouchPhase::Moved), width, window, cx)
                            || shell.mode_swipe.as_ref().unwrap().horizontal.is_none()
                    );
                }
                assert!(shell.mode_position() > 0.3);
                assert!(shell.mode_wheel(&wheel(0., 0., TouchPhase::Ended), width, window, cx));
                assert_eq!(shell.workspace.mode, AppMode::Browser);
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-sidebar").is_some());
        assert!(visual.debug_bounds("mode-bar").is_some());
    }
}
