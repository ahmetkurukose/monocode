//! Viewport state: closing a tab never deletes or stops a session.
//! A title tab is keyed by one of its sessions; a tab holding more than one
//! pane keeps its split tree in `layouts` and its last focused pane in
//! `focused`. `active` is always the focused session.
use crate::pane_layout::{Edge, PaneNode, SplitDir, leaf};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum AppView {
    Inbox,
    Notes,
    Automations,
    Database,
    Api,
}
impl AppView {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Notes => "Notes",
            Self::Automations => "Automations",
            Self::Database => "Database",
            Self::Api => "API",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Notes => "note",
            Self::Automations => "zap",
            Self::Database => "database",
            Self::Api => "api",
        }
    }
}

/// The title strip has one order, including conversations, app views and Monos.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum TitleTab {
    Session(i64),
    Search,
    App(AppView),
    Mono {
        id: String,
        project: String,
        worktree: String,
    },
    /// Classic with the tree rail: an app view as one project's tab. Kept
    /// out of the saved `title_order`, `pinned_tabs` and `scope_focus`: on
    /// disk these tabs live in `Workspace::project_apps`, because a build
    /// that does not know this variant would fail to read the whole file.
    ProjectApp {
        view: AppView,
        project: String,
    },
}

impl TitleTab {
    pub fn session(&self) -> Option<i64> {
        match self {
            Self::Session(id) => Some(*id),
            Self::Mono { .. } | Self::Search | Self::App(_) | Self::ProjectApp { .. } => None,
        }
    }
}

/// A project app tab where it sat: its `title_order` index, its pin index,
/// or (for `scope_focus`) the scope it was focused in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectAppSlot {
    pub view: Option<AppView>,
    pub project: String,
    pub slot: usize,
    pub scope: String,
}

/// `TitleTab::ProjectApp` entries, saved apart so older builds ignore them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectApps {
    pub tabs: Vec<ProjectAppSlot>,
    pub pinned: Vec<ProjectAppSlot>,
    pub focus: Vec<ProjectAppSlot>,
}

impl ProjectApps {
    fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.pinned.is_empty() && self.focus.is_empty()
    }
}

/// The tabs stacked in one split region (docs/native/tab-groups.md). A group
/// has no id: its visible tab is the member that is a leaf of a layout, and
/// a leaf no group lists is a group of one, so only real stacks are saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TabGroup {
    /// Pane ids in strip order, pinned ones first.
    pub tabs: Vec<i64>,
    pub pinned: Vec<i64>,
    /// The tab the next preview open replaces; never pinned.
    pub preview: Option<i64>,
    /// Most recently shown first. Closing the visible tab shows its head.
    #[serde(skip)]
    pub mru: Vec<i64>,
}

impl TabGroup {
    /// Worth keeping: a stack, or a lone tab that is pinned or a preview.
    fn worth_keeping(&self) -> bool {
        self.tabs.len() > 1
            || (!self.tabs.is_empty() && (!self.pinned.is_empty() || self.preview.is_some()))
    }
}

/// A named section of projects in the original project navigation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ProjectRailGroup {
    pub id: String,
    pub name: String,
    pub collapsed: bool,
    pub color_index: Option<usize>,
    pub custom_color: Option<String>,
    pub mascot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Workspace {
    pub window: crate::window_state::WindowState,
    pub tabs: Vec<i64>,
    pub title_order: Vec<TitleTab>,
    /// Tabs pinned to the strip's left edge as icon cards, in pin order.
    /// Each stays in `title_order` too; this only marks and orders them.
    pub pinned_tabs: Vec<TitleTab>,
    pub scope_focus: HashMap<String, TitleTab>,
    /// Sidebar narrowing belongs to this run. Reopen on the full project,
    /// while saved conversations retain their own execution directories.
    #[serde(skip)]
    pub worktree_focus: HashMap<String, crate::worktree_switcher::WorktreeFocus>,
    /// Directly opened chats join the workspace on screen without changing
    /// their execution directory. Like workspace selection, these pins are local
    /// to the current run and disappear when their tabs close.
    #[serde(skip)]
    pub tab_workspaces: HashMap<i64, String>,
    pub active: Option<i64>,
    pub projects: Vec<String>,
    /// Projects archived from the rail, newest first. Their chats stay in
    /// history; adding the project again takes it out of this list.
    pub archived_projects: Vec<String>,
    pub project: String,
    /// Last focused open session in each project, independent of tab order.
    pub project_focus: HashMap<String, i64>,
    pub recent_files: HashMap<String, Vec<String>>,
    pub collapsed: HashSet<String>,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub compact_session_cards: bool,
    /// The Search field heading the sidebar; ⌘K opens search either way.
    pub sidebar_search_visible: bool,
    pub sidebar_notes_visible: bool,
    pub sidebar_automations_visible: bool,
    pub sidebar_inbox_visible: bool,
    pub sidebar_database_visible: bool,
    pub sidebar_api_visible: bool,
    pub sidebar_activity_visible: bool,
    /// The inset right panel: open state, tab, width and tab order.
    pub right_panel_open: bool,
    pub right_panel_tab: crate::right_panel::RightTab,
    pub right_panel_width: f32,
    pub right_panel_tabs: Vec<crate::right_panel::RightTab>,
    /// Each tab's file/review pane beside its chat.
    pub reviews: std::collections::HashMap<i64, crate::review::ReviewState>,
    /// Independent file panes use negative IDs in the layout tree. Their
    /// originating session supplies project/worktree scope and Add to Chat.
    pub review_owners: HashMap<i64, i64>,
    /// Monos as tabs, by their negative pane IDs. Each keeps the Mono it
    /// shows and the scope (project and worktree) it opened in.
    pub mono_panes: HashMap<i64, TitleTab>,
    /// Terminals pulled out of a dock into a split or onto the title strip,
    /// by their negative pane IDs. Like file panes, each borrows its owner
    /// session's project and worktree scope.
    pub terminal_panes: HashMap<i64, TerminalPane>,
    /// Browser pages (Chromium), by their negative pane IDs. Like terminal
    /// panes, each borrows its owner session's project scope.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        serialize_with = "saved_browser_panes"
    )]
    pub browser_panes: HashMap<i64, BrowserPane>,
    /// App views, Search and Monos as panes, by their negative pane IDs.
    /// They have no owner session. `TitleTab::ProjectApp` never goes here:
    /// see its note on older builds.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub view_panes: HashMap<i64, TitleTab>,
    /// The scope (`Shell::tab_scope`'s key) each view pane opened in; a Mono
    /// pane's scope is in its `TitleTab::Mono`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub view_scopes: HashMap<i64, String>,
    /// Split trees of tabs with more than one pane, by tab key.
    pub layouts: HashMap<i64, PaneNode>,
    /// Hidden tabs stacked under layout leaves; see `TabGroup`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<TabGroup>,
    /// The pane each split tab returns to when it is activated.
    pub focused: HashMap<i64, i64>,
    /// Each project's terminal dock (ProjectTerminalDock), by project path.
    pub docks: HashMap<String, TerminalDock>,
    /// Each project's saved commands, run from the footer's Actions button.
    pub actions: HashMap<String, Vec<ProjectAction>>,
    /// The id of the action each project last ran; the footer shows it.
    pub last_actions: HashMap<String, String>,
    /// Original layout: separate project and session sidebars.
    pub classic_layout: bool,
    /// The new single-sidebar layout can keep its full-height, square frame.
    pub new_layout_boxy: bool,
    pub project_rail_open: bool,
    pub project_rail_monos_pinned: bool,
    pub project_rail_width: f32,
    pub project_rail_pinned: HashSet<String>,
    /// Projects opened in the rail's tree view.
    pub project_rail_expanded: HashSet<String>,
    /// Worktrees closed under their project in the rail's tree view; any other is open,
    /// so a new worktree shows its chats straight away.
    pub project_rail_worktrees_collapsed: HashSet<String>,
    /// Projects put to sleep, with when (ms). A sleeping project keeps its
    /// chats, tabs and terminal list but runs no shells, warm agents or pages
    /// until it is opened again.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub slept_projects: HashMap<String, u64>,
    pub project_rail_groups: Vec<ProjectRailGroup>,
    pub project_rail_assignments: HashMap<String, String>,
    pub classic_sidebar_tabs: HashMap<String, String>,
    pub classic_sidebar_tab_order: Vec<String>,
    pub new_layout_prompt_dismissed: bool,
    /// The rail stops asking for a GitHub star once dismissed.
    pub github_star_prompt_dismissed: bool,
    pub preferences: crate::settings::Preferences,
    pub session_filters: monocode_core::session_filters::Filters,
    pub project_appearance: HashMap<String, monocode_core::project_appearance::ProjectAppearance>,
    /// What each project's terminals start with (`project_path -> setup`):
    /// variables, an environment file, a virtual environment, commands.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub project_terminal_setups: HashMap<String, monocode_core::terminal_setup::TerminalSetup>,
    /// Each project's run configurations, last run and test runner
    /// (`project_path -> runs`); resolved per checkout when something runs.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub project_runs: HashMap<String, monocode_core::code::run::ProjectRuns>,
    /// What the code features keep for each project
    /// (`project_path -> page id -> value`), edited on the pages they add to
    /// the project's settings (`code_shell::settings`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub project_code_settings: HashMap<String, HashMap<String, serde_json::Value>>,
    /// Projects whose browser pages keep their own cookies and storage
    /// instead of the profile all others share
    /// (docs/native/browser/context.md K15).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub browser_isolated_projects: std::collections::BTreeSet<String>,
    /// The right panel's whole tab strip by tab key, feature tabs included,
    /// so one that comes back takes the place it was dragged to.
    /// `right_panel_tabs` keeps the built-in tabs alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub right_panel_strip: Vec<String>,
    /// Last desktop source values, used to reconcile subsequent desktop changes
    /// while retaining deliberate native project and preference edits.
    pub desktop_project_preferences: Option<serde_json::Value>,
    /// Unmodified desktop viewport metadata, including pane types and explicit
    /// tab-group assignments that do not yet have a native representation.
    pub desktop_workspace_snapshot: Option<serde_json::Value>,
    /// Exact source strings retained for preferences with no native equivalent.
    pub desktop_preferences_metadata: Option<serde_json::Value>,
    /// Only filled while saving: see `TitleTab::ProjectApp`.
    #[serde(skip_serializing_if = "ProjectApps::is_empty")]
    pub project_apps: ProjectApps,
}

/// A saved command the footer runs in a terminal with one click.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProjectAction {
    pub id: String,
    pub name: String,
    pub command: String,
    /// One of `ACTION_ICONS`.
    pub icon: String,
}
/// The icons an action can wear, in picker order; the first is the default.
pub(crate) const ACTION_ICONS: [&str; 6] =
    ["play", "test-tube", "hammer", "rocket", "bug", "package"];
impl ProjectAction {
    pub fn icon(&self) -> &'static str {
        ACTION_ICONS
            .iter()
            .find(|icon| **icon == self.icon)
            .copied()
            .unwrap_or(ACTION_ICONS[0])
    }
}

/// Port of src/features/projects/model/projectTerminal.ts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DockSide {
    Top,
    #[default]
    Bottom,
    Left,
    Right,
}
impl DockSide {
    pub fn vertical(self) -> bool {
        matches!(self, DockSide::Top | DockSide::Bottom)
    }
    /// DOCK_SIZE_DEFAULT.
    pub fn default_size(self) -> f32 {
        if self.vertical() { 220. } else { 360. }
    }
    /// clampDockSize: at least a usable strip, at most 70% of the window.
    pub fn clamp(self, value: f32, viewport: (f32, f32)) -> f32 {
        if !value.is_finite() {
            return self.default_size();
        }
        let (min, max) = self.limits(viewport);
        value.round().clamp(min, max)
    }
    pub fn limits(self, viewport: (f32, f32)) -> (f32, f32) {
        let (min, span) = if self.vertical() {
            (88., viewport.1)
        } else {
            (180., viewport.0)
        };
        (min, (span * 0.7).floor().max(min))
    }
}
/// A terminal living in the pane tree. Shells are not restored; the pane
/// reopens a fresh shell in the same directory, as a dock tab does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TerminalPane {
    pub owner: i64,
    pub cwd: String,
}
/// A browser page in the pane tree (docs/native/browser/context.md §4.5).
/// Its address, title, zoom and docked DevTools are saved; the page loads
/// again when shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct BrowserPane {
    pub owner: i64,
    pub url: String,
    pub title: String,
    /// Page zoom in percent; `None` is 100%.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<u16>,
    /// Its docked DevTools are open; they open again when the page shows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub docked_devtools: bool,
    /// The docked DevTools' height in points; `None` is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docked_devtools_height: Option<u32>,
    /// The agent session that opened it (context.md K21); not saved.
    #[serde(skip)]
    pub opened_by: Option<i64>,
    /// The throwaway profile it browses with (K22), shared with the pages it
    /// opens. Such a page is never saved: it goes with the app.
    #[serde(skip)]
    pub ephemeral: Option<String>,
    /// Opened in this run, not restored: its first load is a new visit of
    /// the history (K55); not saved.
    #[serde(skip)]
    pub fresh: bool,
}

/// `browser_panes` as saved: without the pages of throwaway profiles, whose
/// panes then drop out of the layout at restore.
fn saved_browser_panes<S: serde::Serializer>(
    panes: &HashMap<i64, BrowserPane>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_map(panes.iter().filter(|(_, pane)| pane.ephemeral.is_none()))
}

/// A project's dock: its place, size and the terminals it holds. Shells are
/// not restored; the dock reopens the same tabs in their directories.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub(crate) struct TerminalDock {
    pub side: DockSide,
    pub size: f32,
    pub open: bool,
    /// Each terminal's working directory, in tab order.
    pub terminals: Vec<String>,
    pub active: usize,
    /// Which tabs are a run or a feature's view rather than a shell, by
    /// position. They end with the app, so they are never saved. Tabs past
    /// its end are shells.
    #[serde(skip)]
    pub transient: Vec<bool>,
}
impl Default for TerminalDock {
    fn default() -> Self {
        Self {
            side: DockSide::Bottom,
            size: DockSide::Bottom.default_size(),
            open: true,
            terminals: Vec::new(),
            active: 0,
            transient: Vec::new(),
        }
    }
}
/// Only the shells are saved: a restart reopens those, and a run or a debug
/// session is over by then.
impl Serialize for TerminalDock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let shells: Vec<&String> = (self.terminals.iter().enumerate())
            .filter(|(ix, _)| !self.is_transient(*ix))
            .map(|(_, cwd)| cwd)
            .collect();
        let before = (0..self.active.min(self.terminals.len()))
            .filter(|ix| !self.is_transient(*ix))
            .count();
        let mut dock = serializer.serialize_struct("TerminalDock", 5)?;
        dock.serialize_field("side", &self.side)?;
        dock.serialize_field("size", &self.size)?;
        dock.serialize_field("open", &self.open)?;
        dock.serialize_field("active", &before.min(shells.len().saturating_sub(1)))?;
        dock.serialize_field("terminals", &shells)?;
        dock.end()
    }
}
impl TerminalDock {
    /// addTerminalToDock: the new terminal is active and the dock opens.
    pub fn add(&mut self, cwd: String) -> usize {
        self.terminals.push(cwd);
        self.active = self.terminals.len() - 1;
        self.open = true;
        self.active
    }
    /// A run's or a feature's tab for the checkout in `cwd`: active like a
    /// new terminal, and left out when the dock is saved.
    pub fn add_transient(&mut self, cwd: String) -> usize {
        let ix = self.add(cwd);
        self.transient.resize(ix, false);
        self.transient.push(true);
        ix
    }
    pub fn is_transient(&self, ix: usize) -> bool {
        self.transient.get(ix).copied().unwrap_or(false)
    }
    /// closeTerminalInDock: the next tab takes over a closed active one.
    /// False once the last terminal is gone.
    pub fn close(&mut self, ix: usize) -> bool {
        if ix >= self.terminals.len() {
            return true;
        }
        if ix < self.transient.len() {
            self.transient.remove(ix);
        }
        self.terminals.remove(ix);
        if self.terminals.is_empty() {
            self.active = 0;
            return false;
        }
        if self.active > ix || self.active >= self.terminals.len() {
            self.active = self.active.saturating_sub(1).min(self.terminals.len() - 1);
        }
        true
    }
    /// onReorderTerminals: the active terminal stays active wherever it lands.
    pub fn reorder(&mut self, from: usize, to: usize) {
        let len = self.terminals.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let active = self.active;
        let cwd = self.terminals.remove(from);
        self.terminals.insert(to, cwd);
        if self.transient.iter().any(|transient| *transient) {
            self.transient.resize(len, false);
            let transient = self.transient.remove(from);
            self.transient.insert(to, transient);
        }
        self.active = if active == from {
            to
        } else if from < active && active <= to {
            active - 1
        } else if to <= active && active < from {
            active + 1
        } else {
            active
        };
    }
    /// onCloseOtherProjectTerminals: `keep` is left, and active.
    pub fn keep_only(&mut self, keep: usize) {
        if keep < self.terminals.len() {
            self.transient = vec![self.is_transient(keep)];
            self.terminals = vec![self.terminals.swap_remove(keep)];
            self.active = 0;
        }
    }
    pub fn set_side(&mut self, side: DockSide) {
        if side != self.side {
            // A dock moved across axes starts from that axis's default.
            if side.vertical() != self.side.vertical() {
                self.size = side.default_size();
            }
            self.side = side;
        }
    }
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            window: Default::default(),
            tabs: Vec::new(),
            title_order: Vec::new(),
            pinned_tabs: Vec::new(),
            scope_focus: HashMap::new(),
            project_apps: ProjectApps::default(),
            worktree_focus: HashMap::new(),
            tab_workspaces: HashMap::new(),
            active: None,
            projects: Vec::new(),
            archived_projects: Vec::new(),
            project: String::new(),
            project_focus: HashMap::new(),
            recent_files: HashMap::new(),
            collapsed: HashSet::new(),
            sidebar_visible: true,
            sidebar_width: 260.,
            compact_session_cards: true,
            sidebar_search_visible: true,
            sidebar_notes_visible: true,
            sidebar_automations_visible: true,
            sidebar_inbox_visible: true,
            sidebar_database_visible: true,
            sidebar_api_visible: true,
            sidebar_activity_visible: true,
            right_panel_open: false,
            right_panel_tab: Default::default(),
            right_panel_width: crate::right_panel::WIDTH_DEFAULT,
            right_panel_tabs: crate::right_panel::RightTab::normalize(&[]),
            reviews: Default::default(),
            review_owners: Default::default(),
            terminal_panes: Default::default(),
            browser_panes: Default::default(),
            mono_panes: Default::default(),
            view_panes: Default::default(),
            view_scopes: Default::default(),
            layouts: Default::default(),
            groups: Vec::new(),
            focused: Default::default(),
            docks: Default::default(),
            actions: HashMap::new(),
            last_actions: HashMap::new(),
            classic_layout: false,
            // Fresh installs start on the boxy frame; saved choices are kept.
            new_layout_boxy: true,
            project_rail_open: true,
            project_rail_monos_pinned: false,
            project_rail_width: 220.,
            project_rail_pinned: HashSet::new(),
            project_rail_expanded: HashSet::new(),
            project_rail_worktrees_collapsed: HashSet::new(),
            slept_projects: HashMap::new(),
            project_rail_groups: Vec::new(),
            project_rail_assignments: HashMap::new(),
            classic_sidebar_tabs: HashMap::new(),
            classic_sidebar_tab_order: Vec::new(),
            new_layout_prompt_dismissed: false,
            github_star_prompt_dismissed: false,
            preferences: crate::settings::Preferences::default(),
            session_filters: Default::default(),
            project_appearance: Default::default(),
            project_terminal_setups: Default::default(),
            project_runs: Default::default(),
            project_code_settings: Default::default(),
            browser_isolated_projects: Default::default(),
            right_panel_strip: Vec::new(),
            desktop_project_preferences: None,
            desktop_workspace_snapshot: None,
            desktop_preferences_metadata: None,
        }
    }
}
impl Workspace {
    /// Seed original-rail concepts from the exact desktop preference strings.
    /// Native edits then live in their own persisted fields.
    pub(crate) fn restore_project_rail_metadata(&mut self) {
        let Some(metadata) = self.desktop_preferences_metadata.as_ref() else {
            return;
        };
        let read = |key: &str| -> Option<serde_json::Value> {
            metadata
                .get(key)?
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
        };
        if let Some(value) = read("monocode.railMonosPinned") {
            self.project_rail_monos_pinned = value.as_bool().unwrap_or(false);
        }
        if self.classic_sidebar_tab_order.is_empty()
            && let Some(value) = read("monocode.sidebarTabOrder")
        {
            self.classic_sidebar_tab_order = serde_json::from_value::<Vec<String>>(value)
                .unwrap_or_default()
                .into_iter()
                .filter(|tab| ["sessions", "files", "changes"].contains(&tab.as_str()))
                .collect();
        }
        if self.classic_sidebar_tabs.is_empty()
            && let Some(value) = read("monocode.projectSidebarTabs.v1")
        {
            self.classic_sidebar_tabs = serde_json::from_value::<HashMap<String, String>>(value)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, tab)| ["sessions", "files", "changes"].contains(&tab.as_str()))
                .collect();
        }
        if let Some(value) = read("monocode.projectRailPinned") {
            self.project_rail_pinned = serde_json::from_value(value).unwrap_or_default();
        }
        if let Some(value) = read("monocode.projectGroups") {
            self.project_rail_groups = serde_json::from_value(value).unwrap_or_default();
        }
        if let Some(value) = read("monocode.projectGroupAssignments") {
            self.project_rail_assignments = serde_json::from_value(value).unwrap_or_default();
        }
        if let Some(value) = read("monocode.projectRailOrder")
            && let Ok(order) = serde_json::from_value::<Vec<String>>(value)
        {
            let mut projects: Vec<_> = order
                .into_iter()
                .filter(|p| self.projects.contains(p))
                .collect();
            projects.extend(
                self.projects
                    .iter()
                    .filter(|p| !projects.contains(p))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            self.projects = projects;
        }
    }

    pub fn boxy_layout(&self) -> bool {
        self.classic_layout || self.new_layout_boxy
    }

    /// The saved state a fresh install starts from: the Classic layout and
    /// the fresh-install preferences. It names no project, so the startup
    /// project is still found as usual.
    pub fn fresh_install_json() -> String {
        serde_json::json!({
            "classic_layout": true,
            "new_layout_boxy": false,
            "preferences": crate::settings::Preferences::fresh_install(),
        })
        .to_string()
    }

    pub fn restore(saved: Option<&str>, sessions: &[(i64, String)], cwd: &str) -> Self {
        let mut state = saved
            .and_then(|s| serde_json::from_str::<Self>(s).ok())
            .unwrap_or_default();
        // Before the project rail returned, "classic" meant the boxy version
        // of the single-sidebar layout. Keep that appearance for existing users.
        if let Some(saved) = saved.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) {
            if saved.get("new_layout_boxy").is_none() {
                state.new_layout_boxy = state.classic_layout;
                state.classic_layout = false;
            }
            // Keep the native empty-session choice when upgrading from the
            // old games toggle. An explicitly saved new choice always wins.
            let preferences = &saved["preferences"];
            if preferences.get("empty_session").is_none()
                && let Some(games) = preferences["empty_session_games"].as_bool()
            {
                state.preferences.empty_session = if games { "games" } else { "none" }.into();
            }
        }
        let has_rail_state = saved
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .is_some_and(|value| value.get("project_rail_pinned").is_some());
        if !has_rail_state {
            state.restore_project_rail_metadata();
        }
        // Hidden project navigation is retired; collapsed always means icons.
        state.preferences.collapsed_project_rail = "compact".into();
        state.project_rail_width = if state.project_rail_width.is_finite() {
            state.project_rail_width.clamp(176., 360.)
        } else {
            220.
        };
        if state.project.is_empty() {
            state.project = cwd.into();
        }
        for appearance in state.project_appearance.values_mut() {
            appearance.normalize();
        }
        let mut known: HashSet<_> = sessions.iter().map(|s| s.0).collect();
        state.rehome_terminal_owned_files(sessions);
        state.review_owners.retain(|id, owner| {
            *id < 0
                && known.contains(owner)
                && state.reviews.get(id).is_some_and(|r| !r.tabs.is_empty())
        });
        known.extend(state.review_owners.keys().copied());
        let owners = &state.review_owners;
        state.mono_panes.retain(|id, mono| {
            *id < 0 && !owners.contains_key(id) && matches!(mono, TitleTab::Mono { .. })
        });
        known.extend(state.mono_panes.keys().copied());
        let (owners, monos) = (&state.review_owners, &state.mono_panes);
        state.terminal_panes.retain(|id, pane| {
            *id < 0
                && !owners.contains_key(id)
                && !monos.contains_key(id)
                && sessions.iter().any(|(known, _)| *known == pane.owner)
        });
        known.extend(state.terminal_panes.keys().copied());
        let (owners, monos, terminals) = (
            &state.review_owners,
            &state.mono_panes,
            &state.terminal_panes,
        );
        state.browser_panes.retain(|id, pane| {
            *id < 0
                && !owners.contains_key(id)
                && !monos.contains_key(id)
                && !terminals.contains_key(id)
                && sessions.iter().any(|(known, _)| *known == pane.owner)
        });
        known.extend(state.browser_panes.keys().copied());
        let (owners, monos, terminals, browsers) = (
            &state.review_owners,
            &state.mono_panes,
            &state.terminal_panes,
            &state.browser_panes,
        );
        state.view_panes.retain(|id, view| {
            *id < 0
                && !owners.contains_key(id)
                && !monos.contains_key(id)
                && !terminals.contains_key(id)
                && !browsers.contains_key(id)
                && !matches!(view, TitleTab::Session(_) | TitleTab::ProjectApp { .. })
        });
        known.extend(state.view_panes.keys().copied());
        let views = &state.view_panes;
        state.view_scopes.retain(|id, _| views.contains_key(id));
        // A visible tab that is gone hands its leaf to the next known hidden
        // tab of its group, so the rest of the stack survives.
        let saved_leaves: HashSet<i64> = (state.tabs.iter())
            .flat_map(|tab| {
                (state.layouts.get(tab).cloned())
                    .unwrap_or_else(|| leaf(*tab))
                    .leaf_ids()
            })
            .collect();
        let mut stand_ins: Vec<(i64, i64)> = Vec::new();
        for group in &mut state.groups {
            let gone: Vec<i64> = (group.tabs.iter().copied())
                .filter(|id| saved_leaves.contains(id) && !known.contains(id))
                .collect();
            group.tabs.retain(|id| known.contains(id));
            let hidden: Vec<i64> = (group.tabs.iter().copied())
                .filter(|id| !saved_leaves.contains(id))
                .filter(|id| !stand_ins.iter().any(|(_, used)| used == id))
                .collect();
            stand_ins.extend(gone.into_iter().zip(hidden));
        }
        let mut seen = HashSet::new();
        let mut layouts = std::mem::take(&mut state.layouts);
        let mut focused = std::mem::take(&mut state.focused);
        for tab in std::mem::take(&mut state.tabs) {
            let mut layout = layouts.remove(&tab).unwrap_or_else(|| leaf(tab));
            for (from, to) in &stand_ins {
                if layout.contains(*from) {
                    layout = layout.replace_leaf(*from, *to);
                }
            }
            let Some(layout) = layout.retain(&mut |id| known.contains(&id) && seen.insert(id))
            else {
                continue;
            };
            let key = if layout.contains(tab) {
                tab
            } else {
                layout.first_leaf()
            };
            state.tabs.push(key);
            for entry in state
                .title_order
                .iter_mut()
                .chain(state.scope_focus.values_mut())
                .chain(state.pinned_tabs.iter_mut())
            {
                if *entry == TitleTab::Session(tab) {
                    *entry = TitleTab::Session(key);
                }
            }
            if layout.is_split() {
                if let Some(pane) = focused.remove(&tab).filter(|id| layout.contains(*id)) {
                    state.focused.insert(key, pane);
                }
                state.layouts.insert(key, layout);
            }
        }
        state.normalize_groups();
        state.settle_mono_panes();
        state.prune_terminal_panes();
        state.prune_browser_panes();
        state.prune_view_panes();
        state.normalize_title_order();
        if state
            .desktop_project_preferences
            .as_ref()
            .and_then(|source| source.get("projects"))
            .is_some()
            && state.active.is_some_and(|id| {
                sessions.iter().any(|(known, path)| {
                    *known == state.session_for_pane(id) && !state.projects.contains(path)
                })
            })
        {
            // An old native sidecar could focus an internal session folder.
            // It must not restore that folder as a remembered user project.
            state.active = None;
        }
        if state.active.is_none_or(|id| state.tab_of(id).is_none()) {
            state.active = state
                .project_focus
                .get(&state.project)
                .copied()
                .filter(|id| {
                    state.tab_of(*id).is_some()
                        && sessions.iter().any(|(known, path)| {
                            *known == state.session_for_pane(*id) && *path == state.project
                        })
                })
                .or_else(|| {
                    state
                        .tabs
                        .iter()
                        .rev()
                        .map(|tab| state.tab_focus(*tab))
                        .find(|id| {
                            sessions.iter().any(|(known, path)| {
                                *known == state.session_for_pane(*id) && *path == state.project
                            })
                        })
                });
        }
        // Focus is always on screen: a hidden tab gives way to its group's.
        state.active = state.active.map(|id| state.visible_of(id).unwrap_or(id));
        if saved.is_none()
            && let Some((id, _)) = sessions.iter().find(|(_, path)| path == cwd)
        {
            state.open(*id);
        }
        state.sidebar_width = state.sidebar_width.clamp(200., 560.);
        state.right_panel_width = state
            .right_panel_width
            .clamp(crate::right_panel::WIDTH_MIN, crate::right_panel::WIDTH_MAX)
            .round();
        state.right_panel_tabs = crate::right_panel::RightTab::normalize(&state.right_panel_tabs);
        state
            .reviews
            .retain(|id, review| known.contains(id) && !review.tabs.is_empty());
        state.docks.retain(|_, dock| !dock.terminals.is_empty());
        for dock in state.docks.values_mut() {
            dock.active = dock.active.min(dock.terminals.len() - 1);
            dock.size = dock.side.clamp(dock.size, (4000., 4000.));
        }
        state.add_project(cwd);
        let open: HashSet<_> = state.open_sessions().into_iter().collect();
        let focus = std::mem::take(&mut state.project_focus);
        let owner = |id: i64| state.session_for_pane(id);
        let focus: HashMap<_, _> = focus
            .into_iter()
            .filter(|(project, id)| {
                open.contains(id)
                    && sessions
                        .iter()
                        .any(|(known, path)| *known == owner(*id) && path == project)
            })
            .collect();
        state.project_focus = focus;
        if let Some((_, project)) = sessions.iter().find(|(id, _)| {
            state
                .active
                .is_some_and(|pane| state.session_for_pane(pane) == *id)
        }) {
            state.project = project.clone();
            state
                .project_focus
                .insert(project.clone(), state.active.unwrap());
        }
        state.add_project(&state.project.clone());
        state.preferences.migrate();
        // Only listed local projects can sleep; anything else would hide work.
        let projects: HashSet<String> = state.projects.iter().cloned().collect();
        state
            .slept_projects
            .retain(|project, _| projects.contains(project) && !project.starts_with("remote://"));
        state.project_terminal_setups.retain(|project, setup| {
            *setup = std::mem::take(setup).normalized();
            !project.trim().is_empty() && !setup.is_empty()
        });
        state.project_runs.retain(|project, runs| {
            *runs = std::mem::take(runs).normalized();
            !project.trim().is_empty() && !runs.is_empty()
        });
        state
            .project_code_settings
            .retain(|project, pages| !project.trim().is_empty() && !pages.is_empty());
        state.restore_project_apps();
        state
    }

    /// The JSON to save. Project app tabs are moved into `project_apps` for
    /// the write and put back where they were afterwards.
    pub fn saved_json(&mut self) -> String {
        let mut apps = ProjectApps::default();
        let slot = |tab: &TitleTab, slot: usize, scope: &str| match tab {
            TitleTab::ProjectApp { view, project } => Some(ProjectAppSlot {
                view: Some(*view),
                project: project.clone(),
                slot,
                scope: scope.into(),
            }),
            _ => None,
        };
        apps.tabs = (self.title_order.iter().enumerate())
            .filter_map(|(ix, tab)| slot(tab, ix, ""))
            .collect();
        apps.pinned = (self.pinned_tabs.iter().enumerate())
            .filter_map(|(ix, tab)| slot(tab, ix, ""))
            .collect();
        apps.focus = (self.scope_focus.iter())
            .filter_map(|(scope, tab)| slot(tab, 0, scope))
            .collect();
        let project_app = |tab: &TitleTab| matches!(tab, TitleTab::ProjectApp { .. });
        self.title_order.retain(|tab| !project_app(tab));
        self.pinned_tabs.retain(|tab| !project_app(tab));
        self.scope_focus.retain(|_, tab| !project_app(tab));
        self.project_apps = apps;
        let json = serde_json::to_string(self).unwrap();
        self.restore_project_apps();
        json
    }

    /// Puts saved project app tabs back, dropping removed projects' tabs.
    fn restore_project_apps(&mut self) {
        let apps = std::mem::take(&mut self.project_apps);
        let tab = |entry: &ProjectAppSlot| {
            self.projects
                .contains(&entry.project)
                .then_some(TitleTab::ProjectApp {
                    view: entry.view?,
                    project: entry.project.clone(),
                })
        };
        let tabs: Vec<_> = (apps.tabs.iter())
            .filter_map(|entry| Some((entry.slot, tab(entry)?)))
            .collect();
        let pinned: Vec<_> = (apps.pinned.iter())
            .filter_map(|entry| Some((entry.slot, tab(entry)?)))
            .collect();
        let focus: Vec<_> = (apps.focus.iter())
            .filter_map(|entry| Some((entry.scope.clone(), tab(entry)?)))
            .collect();
        for (slot, tab) in tabs {
            if !self.title_order.contains(&tab) {
                let slot = slot.min(self.title_order.len());
                self.title_order.insert(slot, tab);
            }
        }
        for (slot, tab) in pinned {
            if self.title_order.contains(&tab) && !self.pinned_tabs.contains(&tab) {
                let slot = slot.min(self.pinned_tabs.len());
                self.pinned_tabs.insert(slot, tab);
            }
        }
        for (scope, tab) in focus {
            if self.title_order.contains(&tab) {
                self.scope_focus.entry(scope).or_insert(tab);
            }
        }
    }
    pub fn add_project(&mut self, path: &str) {
        self.archived_projects.retain(|p| p != path);
        if !self.projects.iter().any(|p| p == path) {
            self.projects.push(path.into());
        }
    }
    /// The rail group a project is listed under. Assignments to deleted
    /// groups leave the project in the ungrouped list.
    pub fn project_rail_group(&self, path: &str) -> Option<&str> {
        let id = self.project_rail_assignments.get(path)?;
        self.project_rail_groups
            .iter()
            .any(|group| &group.id == id)
            .then_some(id.as_str())
    }
    /// Whether two projects sit in the same rail section and reorder together.
    pub fn project_rail_peers(&self, a: &str, b: &str) -> bool {
        let pinned = self.project_rail_pinned.contains(a);
        pinned == self.project_rail_pinned.contains(b)
            && (pinned || self.project_rail_group(a) == self.project_rail_group(b))
    }
    /// Projects in the order the current layout shows them. The rail lists
    /// pinned projects, then each group's members, then the rest.
    pub fn visible_project_order(&self) -> Vec<String> {
        if !self.classic_layout {
            return self.projects.clone();
        }
        let unpinned = |p: &&String| !self.project_rail_pinned.contains(*p);
        let mut order: Vec<String> = self
            .projects
            .iter()
            .filter(|p| self.project_rail_pinned.contains(*p))
            .cloned()
            .collect();
        for group in &self.project_rail_groups {
            order.extend(
                self.projects
                    .iter()
                    .filter(unpinned)
                    .filter(|p| self.project_rail_group(p) == Some(group.id.as_str()))
                    .cloned(),
            );
        }
        order.extend(
            self.projects
                .iter()
                .filter(unpinned)
                .filter(|p| self.project_rail_group(p).is_none())
                .cloned(),
        );
        order
    }
    /// The projects that reorder alongside `path`, in the order shown: the
    /// whole list, or just its rail section.
    pub fn project_peers(&self, path: &str) -> Vec<String> {
        self.visible_project_order()
            .into_iter()
            .filter(|p| !self.classic_layout || self.project_rail_peers(p, path))
            .collect()
    }
    /// Moves a project to `to` in the saved order. Returns whether it moved.
    pub fn move_project(&mut self, path: &str, to: usize) -> bool {
        let Some(from) = self.projects.iter().position(|p| p == path) else {
            return false;
        };
        let to = to.min(self.projects.len() - 1);
        if from == to {
            return false;
        }
        let project = self.projects.remove(from);
        self.projects.insert(to, project);
        true
    }
    /// Moves a project one place along what the layout shows, staying inside
    /// its rail section. Returns whether it moved.
    pub fn step_project_order(&mut self, path: &str, step: isize) -> bool {
        let peers = self.project_peers(path);
        let Some(ix) = peers.iter().position(|p| p == path) else {
            return false;
        };
        let Some(neighbour) = ix
            .checked_add_signed(step.signum())
            .and_then(|ix| peers.get(ix))
        else {
            return false;
        };
        let to = self.projects.iter().position(|p| p == neighbour).unwrap();
        self.move_project(path, to)
    }
    /// Moves a project to either end of its section: `step < 0` for the start.
    pub fn move_project_to_end(&mut self, path: &str, step: isize) -> bool {
        let peers = self.project_peers(path);
        let target = if step < 0 {
            peers.first()
        } else {
            peers.last()
        };
        let Some(to) = target.and_then(|t| self.projects.iter().position(|p| p == t)) else {
            return false;
        };
        self.move_project(path, to)
    }
    /// Older versions recorded a file opened from a terminal as owned by that
    /// terminal pane, which restore cannot resolve to a chat, so the file tab
    /// vanished. Give each such file to the terminal's session while it is
    /// known, else to a session of the file's project.
    fn rehome_terminal_owned_files(&mut self, sessions: &[(i64, String)]) {
        let owned: Vec<(i64, i64)> = self
            .review_owners
            .iter()
            .filter(|(_, owner)| **owner < 0)
            .map(|(pane, owner)| (*pane, *owner))
            .collect();
        for (pane, owner) in owned {
            let session = self
                .terminal_panes
                .get(&owner)
                .map(|t| t.owner)
                .filter(|owner| sessions.iter().any(|(id, _)| id == owner))
                .or_else(|| {
                    let cwd = match self.reviews.get(&pane)?.tabs.first()? {
                        crate::review::ReviewTab::File { cwd, .. } => cwd.clone(),
                        _ => return None,
                    };
                    sessions
                        .iter()
                        .find(|(_, session_cwd)| *session_cwd == cwd)
                        .map(|(id, _)| *id)
                });
            if let Some(session) = session {
                self.review_owners.insert(pane, session);
            }
        }
    }
    pub fn session_for_pane(&self, id: i64) -> i64 {
        self.review_owners
            .get(&id)
            .copied()
            .or_else(|| self.terminal_panes.get(&id).map(|t| t.owner))
            .or_else(|| self.browser_panes.get(&id).map(|b| b.owner))
            .unwrap_or(id)
    }
    /// A file, terminal or browser pane standing in for its owner session.
    pub fn is_detached_pane(&self, id: i64) -> bool {
        self.review_owners.contains_key(&id)
            || self.terminal_panes.contains_key(&id)
            || self.browser_panes.contains_key(&id)
    }
    /// File, Mono, terminal, browser and view panes share the negative IDs
    /// below every session.
    pub fn next_pane_id(&self) -> i64 {
        self.review_owners
            .keys()
            .chain(self.mono_panes.keys())
            .chain(self.view_panes.keys())
            .chain(self.terminal_panes.keys())
            .chain(self.browser_panes.keys())
            .copied()
            .min()
            .unwrap_or(0)
            .min(0)
            - 1
    }
    #[cfg(test)]
    pub fn mono_pane(&self, mono: &str) -> Option<i64> {
        self.mono_panes.iter().find_map(|(pane, tab)| {
            matches!(tab, TitleTab::Mono { id, .. } if id == mono).then_some(*pane)
        })
    }
    /// Monos and view panes have no owner session, so they never scope or
    /// key a tab while a conversation's pane is there to.
    fn is_ownerless(&self, id: i64) -> bool {
        self.mono_panes.contains_key(&id) || self.view_panes.contains_key(&id)
    }
    /// The pane that scopes a tab to a project: its focus, unless that is a
    /// Mono or view pane, which works across projects.
    pub fn scope_pane(&self, tab: i64) -> i64 {
        let focus = self.tab_focus(tab);
        if !self.is_ownerless(focus) {
            return focus;
        }
        self.layout(tab)
            .leaf_ids()
            .into_iter()
            .find(|id| !self.is_ownerless(*id))
            .unwrap_or(focus)
    }
    /// Focus a session, opening it as its own tab unless a tab holds it.
    pub fn open(&mut self, id: i64) {
        if self.leaf_tab(id).is_none() && self.show(id) {
            return;
        }
        match self.tab_of(id) {
            Some(tab) => {
                if self.layouts.contains_key(&tab) {
                    self.focused.insert(tab, id);
                }
            }
            None => {
                self.tabs.push(id);
                self.title_order.push(TitleTab::Session(id));
            }
        }
        self.active = Some(id);
    }
    /// Stacks `tab` in `after`'s group right after it, as a page opened
    /// from another sits beside its opener (docs/native/tab-groups.md). It
    /// stays on screen when it was.
    pub fn place_tab_after(&mut self, tab: i64, after: i64) {
        if tab == after || self.tab_of(after).is_none() {
            return;
        }
        let group = self.group_tabs(after);
        let Some(at) = group.iter().position(|t| *t == after) else {
            return;
        };
        let before = group
            .iter()
            .position(|t| *t == tab)
            .is_some_and(|ix| ix < at);
        let to = if before { at } else { at + 1 };
        let show = self.active == Some(tab);
        self.add_to_group(tab, after, Some(to), show);
    }
    /// Close the whole title tab holding `id`; its sessions keep running.
    pub fn close(&mut self, id: i64) {
        let Some(tab) = self.tab_of(id) else {
            return;
        };
        let ix = self.tabs.iter().position(|t| *t == tab).unwrap_or(0);
        let layout = self.layout(tab);
        let leaves = layout.leaf_ids();
        let members: HashSet<i64> = leaves.iter().flat_map(|id| self.group_tabs(*id)).collect();
        // Hidden tabs close with the title tab that shows their group.
        self.groups
            .retain(|group| !group.tabs.iter().any(|id| leaves.contains(id)));
        self.tabs.remove(ix);
        self.tab_workspaces.remove(&tab);
        self.title_order
            .retain(|entry| *entry != TitleTab::Session(tab));
        self.scope_focus
            .retain(|_, entry| *entry != TitleTab::Session(tab));
        self.layouts.remove(&tab);
        self.focused.remove(&tab);
        self.project_focus.retain(|_, id| !members.contains(id));
        if self
            .active
            .is_some_and(|active| self.tab_of(active).is_none())
        {
            self.active = self
                .tabs
                .get(ix)
                .or_else(|| self.tabs.last())
                .map(|tab| self.tab_focus(*tab));
        }
    }
    /// The title tab holding a session, through its group's visible tab
    /// when it is a hidden one.
    pub fn tab_of(&self, id: i64) -> Option<i64> {
        self.leaf_tab(id).or_else(|| {
            let visible = self.visible_of(id).filter(|visible| *visible != id)?;
            self.leaf_tab(visible)
        })
    }
    /// The title tab whose layout shows `id` as a leaf.
    fn leaf_tab(&self, id: i64) -> Option<i64> {
        self.tabs.iter().copied().find(|tab| {
            *tab == id
                || self
                    .layouts
                    .get(tab)
                    .is_some_and(|layout| layout.contains(id))
        })
    }
    pub fn active_tab(&self) -> Option<i64> {
        self.active.and_then(|id| self.tab_of(id))
    }
    pub fn layout(&self, tab: i64) -> PaneNode {
        self.layouts.get(&tab).cloned().unwrap_or_else(|| leaf(tab))
    }
    /// Every session shown in some tab, hidden group tabs included, each
    /// group in strip order.
    pub fn open_sessions(&self) -> Vec<i64> {
        let mut seen = HashSet::new();
        let mut open = Vec::new();
        for tab in &self.tabs {
            for id in self.layout(*tab).leaf_ids() {
                for member in self.group_tabs(id) {
                    if seen.insert(member) {
                        open.push(member);
                    }
                }
            }
        }
        open
    }
    /// The pane a tab activates.
    pub fn tab_focus(&self, tab: i64) -> i64 {
        self.focused
            .get(&tab)
            .copied()
            .filter(|id| self.layouts.get(&tab).is_some_and(|l| l.contains(*id)))
            .unwrap_or(tab)
    }
    /// Store a tab's tree, rekeying it when its key session left and
    /// dropping the tree once a single pane remains.
    fn set_layout(&mut self, tab: i64, layout: PaneNode) -> i64 {
        let Some(ix) = self.tabs.iter().position(|t| *t == tab) else {
            return tab;
        };
        let focus = self.focused.remove(&tab);
        self.layouts.remove(&tab);
        // A tab is keyed by a conversation when it holds one, never a Mono.
        let key = if layout.contains(tab) && !self.is_ownerless(tab) {
            tab
        } else {
            let leaves = layout.leaf_ids();
            leaves
                .iter()
                .copied()
                .find(|id| !self.is_ownerless(*id))
                .unwrap_or(leaves[0])
        };
        self.tabs[ix] = key;
        if key != tab
            && let Some(scope) = self.tab_workspaces.remove(&tab)
        {
            self.tab_workspaces.insert(key, scope);
        }
        for entry in self
            .title_order
            .iter_mut()
            .chain(self.scope_focus.values_mut())
            .chain(self.pinned_tabs.iter_mut())
        {
            if *entry == TitleTab::Session(tab) {
                *entry = TitleTab::Session(key);
            }
        }
        if layout.is_split() {
            if let Some(focus) = focus.filter(|id| layout.contains(*id)) {
                self.focused.insert(key, focus);
            }
            self.layouts.insert(key, layout);
        }
        key
    }
    /// Take a session out of whichever tab holds it. A hidden group tab
    /// just leaves its group; a visible one hands its leaf to the group's
    /// most recently used tab. Otherwise the leaf goes: a tab left empty
    /// closes, and the active session moves to a neighbouring pane.
    fn take_out(&mut self, id: i64) {
        let Some(tab) = self.tab_of(id) else {
            return;
        };
        self.project_focus.retain(|_, focus| *focus != id);
        if self.leaf_tab(id).is_none() {
            self.leave_group(id);
            return;
        }
        if let Some(next) = self.group_mru_next(id) {
            let was_focused = self.focused.get(&tab) == Some(&id);
            let key = self.set_layout(tab, self.layout(tab).replace_leaf(id, next));
            self.leave_group(id);
            self.touch(next);
            if self.active == Some(id) {
                self.active = Some(next);
            }
            if was_focused {
                self.focus_in_tab(key, next);
            }
            return;
        }
        self.leave_group(id);
        let layout = self.layout(tab);
        match layout.remove(id) {
            Some(rest) => {
                let next = layout.sibling(id).unwrap_or_else(|| rest.first_leaf());
                let key = self.set_layout(tab, rest);
                if self.active == Some(id) {
                    self.active = Some(next);
                }
                if self.layouts.contains_key(&key) && self.focused.get(&key) == Some(&id) {
                    self.focused.insert(key, next);
                }
            }
            None => self.close(id),
        }
    }
    /// Close one pane of a split tab, or one tab of a group. False when it
    /// is the tab's only pane and its group's only tab.
    pub fn close_pane(&mut self, id: i64) -> bool {
        let Some(tab) = self.tab_of(id) else {
            return false;
        };
        if !self.layout(tab).is_split() && self.group_tabs(id).len() <= 1 {
            return false;
        }
        self.take_out(id);
        if let Some(active) = self.active
            && let Some(tab) = self.tab_of(active)
            && self.layouts.contains_key(&tab)
        {
            self.focused.insert(tab, active);
        }
        true
    }
    /// cmd-d: a new session beside the focused pane.
    pub fn split(&mut self, new: i64, dir: SplitDir) {
        let Some(focused) = self.active.and_then(|id| self.visible_of(id)) else {
            self.open(new);
            return;
        };
        let Some(tab) = self.tab_of(focused) else {
            self.open(new);
            return;
        };
        self.take_out(new);
        let tab = self.tab_of(focused).unwrap_or(tab);
        let key = self.set_layout(tab, self.layout(tab).split_pane(focused, dir, new));
        self.focused.insert(key, new);
        self.active = Some(new);
    }
    /// A pane, tab-strip session or sidebar session dropped on a pane edge.
    pub fn place(&mut self, id: i64, target: i64, edge: Edge) {
        let Some(target) = self.visible_of(target) else {
            return;
        };
        if id == target {
            return;
        }
        // A tab that shares a group leaves it for a leaf of its own.
        let same = self.leaf_tab(id).is_some()
            && self.leaf_tab(id) == self.leaf_tab(target)
            && self.group_tabs(id).len() <= 1;
        if !same {
            self.take_out(id);
        }
        let Some(tab) = self.tab_of(target) else {
            return;
        };
        let key = self.set_layout(tab, self.layout(tab).place_pane(id, target, edge));
        self.focused.insert(key, id);
        self.active = Some(id);
    }
    /// Mono panes no tab holds any more, forgotten.
    pub fn settle_mono_panes(&mut self) -> Vec<i64> {
        let open: HashSet<_> = self.open_sessions().into_iter().collect();
        let gone: Vec<_> = (self.mono_panes.keys().copied())
            .filter(|id| !open.contains(id))
            .collect();
        for id in &gone {
            self.mono_panes.remove(id);
        }
        gone
    }
    /// Terminal panes no tab holds any more, forgotten. Their shells stop
    /// with them, so the caller drops their views.
    pub fn prune_terminal_panes(&mut self) -> Vec<i64> {
        let open: HashSet<_> = self.open_sessions().into_iter().collect();
        let gone: Vec<_> = self
            .terminal_panes
            .keys()
            .copied()
            .filter(|id| !open.contains(id))
            .collect();
        for id in &gone {
            self.terminal_panes.remove(id);
        }
        gone
    }
    /// Makes `pane` the one its split tab comes back to.
    pub fn focus_in_tab(&mut self, tab: i64, pane: i64) {
        if self.layouts.contains_key(&tab) {
            self.focused.insert(tab, pane);
        }
    }
    /// Browser panes no tab holds any more, forgotten. Their pages close
    /// with them, so the caller drops their views.
    pub fn prune_browser_panes(&mut self) -> Vec<i64> {
        let open: HashSet<_> = self.open_sessions().into_iter().collect();
        let gone: Vec<_> = (self.browser_panes.keys().copied())
            .filter(|id| !open.contains(id))
            .collect();
        for id in &gone {
            self.browser_panes.remove(id);
        }
        gone
    }
    /// Pane `id` becomes its own title tab at `to`: how workspaces saved
    /// before tab groups held several title tabs of one scope.
    #[cfg(test)]
    pub fn detach(&mut self, id: i64, to: usize) {
        if !self
            .tab_of(id)
            .is_some_and(|tab| self.layout(tab).is_split() || self.group_tabs(id).len() > 1)
        {
            return;
        }
        let scope = self
            .tab_of(id)
            .and_then(|tab| self.tab_workspaces.get(&tab))
            .cloned();
        self.take_out(id);
        self.tabs.insert(to.min(self.tabs.len()), id);
        if let Some(scope) = scope {
            self.tab_workspaces.insert(id, scope);
        }
        let next = self
            .tabs
            .iter()
            .skip(to + 1)
            .find_map(|next| {
                self.title_order
                    .iter()
                    .position(|tab| *tab == TitleTab::Session(*next))
            })
            .unwrap_or(self.title_order.len());
        self.title_order.insert(next, TitleTab::Session(id));
        self.active = Some(id);
    }
    /// Show `to` in `from`'s pane (Session: Next/Previous in tab).
    pub fn swap_session(&mut self, from: i64, to: i64) {
        if self.tab_of(to).is_some() {
            return;
        }
        let Some(tab) = self.tab_of(from) else {
            return;
        };
        // `to` takes `from`'s slot in its group.
        for group in &mut self.groups {
            for id in group
                .tabs
                .iter_mut()
                .chain(group.pinned.iter_mut())
                .chain(group.mru.iter_mut())
                .chain(group.preview.iter_mut())
            {
                if *id == from {
                    *id = to;
                }
            }
        }
        if self.leaf_tab(from).is_none() {
            return;
        }
        let key = self.set_layout(tab, self.layout(tab).replace_leaf(from, to));
        if self.layouts.contains_key(&key) {
            self.focused.insert(key, to);
        }
    }
    /// cmd-opt-arrows within the active tab.
    pub fn neighbor(&self, dir: Edge) -> Option<i64> {
        let active = self.active?;
        self.layout(self.tab_of(active)?).neighbor(active, dir)
    }
    pub fn place_sash(&mut self, tab: i64, path: &[usize], index: usize, boundary: f32) {
        if let Some(layout) = self.layouts.get_mut(&tab) {
            layout.place_sash(path, index, boundary);
        }
    }
    /// A split tab's panes share its room equally.
    pub fn equalize(&mut self, tab: i64) {
        if let Some(layout) = self.layouts.get_mut(&tab) {
            layout.equalize();
        }
    }
    pub fn sash_range(&self, tab: i64, path: &[usize], index: usize) -> Option<(f32, f32, f32)> {
        self.layouts.get(&tab)?.sash_range(path, index)
    }
    pub fn normalize_title_order(&mut self) {
        self.tab_workspaces.retain(|tab, _| self.tabs.contains(tab));
        let mut seen = HashSet::new();
        self.title_order.retain(|tab| {
            tab.session().is_none_or(|id| self.tabs.contains(&id)) && seen.insert(tab.clone())
        });
        for id in &self.tabs {
            let tab = TitleTab::Session(*id);
            if seen.insert(tab.clone()) {
                self.title_order.push(tab);
            }
        }
        self.prune_pinned_tabs();
    }
    /// Pins follow their tabs: a tab that left the strip is no longer pinned.
    pub fn prune_pinned_tabs(&mut self) {
        let mut seen = HashSet::new();
        let order = &self.title_order;
        self.pinned_tabs
            .retain(|tab| order.contains(tab) && seen.insert(tab.clone()));
    }
}

/// Tab groups (docs/native/tab-groups.md).
impl Workspace {
    /// The stored group listing `id`. A leaf no group lists has none.
    pub fn group_of(&self, id: i64) -> Option<usize> {
        self.groups
            .iter()
            .position(|group| group.tabs.contains(&id))
    }
    /// The tabs of `id`'s group in strip order: just `id` for a leaf of
    /// its own, nothing when it is not open.
    pub fn group_tabs(&self, id: i64) -> Vec<i64> {
        match self.group_of(id) {
            Some(ix) if self.visible_of(id).is_some() => self.groups[ix].tabs.clone(),
            _ if self.leaf_tab(id).is_some() => vec![id],
            _ => Vec::new(),
        }
    }
    /// The tab of `id`'s group that is on screen: `id` itself for a leaf.
    pub fn visible_of(&self, id: i64) -> Option<i64> {
        if self.leaf_tab(id).is_some() {
            return Some(id);
        }
        let group = &self.groups[self.group_of(id)?];
        (group.tabs.iter().copied())
            .find(|member| *member != id && self.leaf_tab(*member).is_some())
    }
    /// The tab that takes over when `id` closes: the group's most recently
    /// used other tab, else its right neighbour, else its left one.
    pub fn group_mru_next(&self, id: i64) -> Option<i64> {
        let group = &self.groups[self.group_of(id)?];
        if let Some(next) = (group.mru.iter()).find(|m| **m != id && group.tabs.contains(m)) {
            return Some(*next);
        }
        let ix = group.tabs.iter().position(|m| *m == id)?;
        (group.tabs.get(ix + 1).copied()).or_else(|| ix.checked_sub(1).map(|ix| group.tabs[ix]))
    }
    /// Bring `id` on screen and focus it. A hidden tab takes its group's
    /// leaf; the title tab is rekeyed when that leaf was its key.
    pub fn show(&mut self, id: i64) -> bool {
        let Some(visible) = self.visible_of(id) else {
            return false;
        };
        let Some(tab) = self.leaf_tab(visible) else {
            return false;
        };
        if visible != id {
            self.set_layout(tab, self.layout(tab).replace_leaf(visible, id));
            self.touch(visible);
        }
        if let Some(tab) = self.leaf_tab(id) {
            self.focus_in_tab(tab, id);
        }
        self.touch(id);
        self.active = Some(id);
        true
    }
    /// Close one tab: a hidden one leaves its group, a visible one hands
    /// over to its group's MRU tab, and a group's last tab closes its pane
    /// (or the whole title tab when that was its only pane).
    #[cfg(test)]
    pub fn close_in_group(&mut self, id: i64) -> bool {
        if self.close_pane(id) {
            return true;
        }
        if self.tab_of(id).is_none() {
            return false;
        }
        self.close(id);
        true
    }
    /// Stack `id` in `beside`'s group at `index`, else right after the
    /// visible tab, never before the pinned ones. An open `id` moves here
    /// from wherever it was; one already in this group is only reordered.
    pub fn add_to_group(&mut self, id: i64, beside: i64, index: Option<usize>, show: bool) -> bool {
        let Some(visible) = self.visible_of(beside) else {
            return false;
        };
        if self.group_tabs(beside).contains(&id) {
            if let Some(to) = index {
                self.move_in_group(id, to);
            }
            if show {
                self.show(id);
            }
            return true;
        }
        self.take_out(id);
        let Some(visible) = self.visible_of(visible) else {
            return false;
        };
        let ix = self.ensure_group(visible);
        let group = &mut self.groups[ix];
        let after = group.tabs.iter().position(|m| *m == visible).unwrap_or(0) + 1;
        let at = index
            .unwrap_or(after)
            .clamp(group.pinned.len(), group.tabs.len());
        group.tabs.insert(at, id);
        if !group.mru.contains(&visible) {
            group.mru.insert(0, visible);
        }
        group.mru.push(id);
        if show {
            self.show(id);
        }
        true
    }
    /// Reorder `id` within its group; pinned tabs stay ahead of the rest.
    pub fn move_in_group(&mut self, id: i64, to: usize) -> bool {
        let Some(ix) = self.group_of(id) else {
            return false;
        };
        let group = &mut self.groups[ix];
        let from = group.tabs.iter().position(|m| *m == id).unwrap();
        let pinned = group.pinned.len();
        let to = if group.pinned.contains(&id) {
            to.min(pinned - 1)
        } else {
            to.clamp(pinned, group.tabs.len() - 1)
        };
        if from == to {
            return false;
        }
        group.tabs.remove(from);
        group.tabs.insert(to, id);
        let tabs = group.tabs.clone();
        group
            .pinned
            .sort_by_key(|m| tabs.iter().position(|t| t == m));
        true
    }
    /// Pin `id` at the end of its group's pinned tabs, or unpin it to just
    /// after them. A pinned tab is never the preview.
    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> bool {
        let Some(visible) = self.visible_of(id) else {
            return false;
        };
        let ix = self.ensure_group(visible);
        let group = &mut self.groups[ix];
        if group.pinned.contains(&id) == pinned {
            self.prune_groups();
            return false;
        }
        group.tabs.retain(|m| *m != id);
        if pinned {
            group.tabs.insert(group.pinned.len(), id);
            group.pinned.push(id);
            if group.preview == Some(id) {
                group.preview = None;
            }
        } else {
            group.pinned.retain(|m| *m != id);
            group.tabs.insert(group.pinned.len(), id);
        }
        self.prune_groups();
        true
    }
    /// Mark the preview tab of `member`'s group (`None` clears it). Only an
    /// unpinned tab of that group can be the preview.
    pub fn set_preview(&mut self, member: i64, preview: Option<i64>) -> bool {
        let Some(visible) = self.visible_of(member) else {
            return false;
        };
        if let Some(preview) = preview
            && !self.group_tabs(member).contains(&preview)
        {
            return false;
        }
        let ix = self.ensure_group(visible);
        let group = &mut self.groups[ix];
        if preview.is_some_and(|p| group.pinned.contains(&p)) {
            self.prune_groups();
            return false;
        }
        let changed = group.preview != preview;
        group.preview = preview;
        self.prune_groups();
        changed
    }
    /// The preview tab of `id`'s group.
    pub fn preview_of_group(&self, id: i64) -> Option<i64> {
        self.groups[self.group_of(id)?].preview
    }
    /// Fold a whole title tab into `target`'s group: each of its panes,
    /// with its own group's hidden tabs, is stacked there in order. No pane
    /// closes; the source title tab is gone.
    pub fn merge_tab_into(&mut self, source: i64, target: i64) -> bool {
        let Some(visible) = self.visible_of(target) else {
            return false;
        };
        if !self.tabs.contains(&source) || self.tab_of(visible) == Some(source) {
            return false;
        }
        let leaves = self.layout(source).leaf_ids();
        let incoming: Vec<i64> = leaves.iter().flat_map(|id| self.group_tabs(*id)).collect();
        self.groups
            .retain(|group| !group.tabs.iter().any(|id| leaves.contains(id)));
        self.tabs.retain(|t| *t != source);
        self.tab_workspaces.remove(&source);
        self.title_order
            .retain(|entry| *entry != TitleTab::Session(source));
        self.scope_focus
            .retain(|_, entry| *entry != TitleTab::Session(source));
        self.pinned_tabs
            .retain(|entry| *entry != TitleTab::Session(source));
        self.layouts.remove(&source);
        self.focused.remove(&source);
        let ix = self.ensure_group(visible);
        let group = &mut self.groups[ix];
        for id in incoming {
            if !group.tabs.contains(&id) {
                group.tabs.push(id);
            }
        }
        if self.active.is_some_and(|id| self.leaf_tab(id).is_none()) {
            self.active = Some(visible);
        }
        self.normalize_groups();
        true
    }
    /// Keep groups coherent: members are open panes listed once, each group
    /// has exactly one visible tab, pinned tabs lead and the preview is an
    /// unpinned member. Lone plain tabs are not stored.
    pub fn normalize_groups(&mut self) {
        let leaves: HashSet<i64> = (self.tabs.iter())
            .flat_map(|tab| self.layout(*tab).leaf_ids())
            .collect();
        let pane = |id: &i64| {
            self.review_owners.contains_key(id)
                || self.mono_panes.contains_key(id)
                || self.view_panes.contains_key(id)
                || self.terminal_panes.contains_key(id)
                || self.browser_panes.contains_key(id)
        };
        let mut seen = HashSet::new();
        let mut groups = Vec::new();
        for mut group in std::mem::take(&mut self.groups) {
            group
                .tabs
                .retain(|id| (leaves.contains(id) || *id > 0 || pane(id)) && seen.insert(*id));
            let visible: Vec<i64> = (group.tabs.iter().copied())
                .filter(|id| leaves.contains(id))
                .collect();
            // A group with no tab on screen has nowhere to be shown.
            let Some(first) = visible.first().copied() else {
                continue;
            };
            // Two visible tabs are two groups; the hidden ones stay with the first.
            for other in visible.into_iter().skip(1) {
                group.tabs.retain(|id| *id != other);
                groups.push(TabGroup {
                    tabs: vec![other],
                    pinned: (group.pinned.contains(&other).then_some(other))
                        .into_iter()
                        .collect(),
                    preview: (group.preview == Some(other)).then_some(other),
                    mru: Vec::new(),
                });
            }
            debug_assert!(group.tabs.contains(&first));
            let tabs = group.tabs.clone();
            group.pinned = (tabs.iter().copied())
                .filter(|id| group.pinned.contains(id))
                .collect();
            let pinned = group.pinned.clone();
            group.tabs = (pinned.iter().copied())
                .chain(tabs.into_iter().filter(|id| !pinned.contains(id)))
                .collect();
            group.preview = group
                .preview
                .filter(|id| group.tabs.contains(id) && !pinned.contains(id));
            let tabs = group.tabs.clone();
            let mut mru_seen = HashSet::new();
            group
                .mru
                .retain(|id| tabs.contains(id) && mru_seen.insert(*id));
            groups.push(group);
        }
        self.groups = groups;
        self.prune_groups();
    }
    /// View panes no tab holds any more, forgotten.
    pub fn prune_view_panes(&mut self) -> Vec<i64> {
        let open: HashSet<_> = self.open_sessions().into_iter().collect();
        let gone: Vec<_> = (self.view_panes.keys().copied())
            .filter(|id| !open.contains(id))
            .collect();
        for id in &gone {
            self.view_panes.remove(id);
            self.view_scopes.remove(id);
        }
        gone
    }
    /// The group around a visible tab, created as a group of one if needed.
    fn ensure_group(&mut self, visible: i64) -> usize {
        self.group_of(visible).unwrap_or_else(|| {
            self.groups.push(TabGroup {
                tabs: vec![visible],
                ..Default::default()
            });
            self.groups.len() - 1
        })
    }
    /// Drop `id` from its group, its pin, preview and recency included.
    fn leave_group(&mut self, id: i64) {
        if let Some(ix) = self.group_of(id) {
            let group = &mut self.groups[ix];
            group.tabs.retain(|m| *m != id);
            group.pinned.retain(|m| *m != id);
            group.mru.retain(|m| *m != id);
            if group.preview == Some(id) {
                group.preview = None;
            }
            self.prune_groups();
        }
    }
    /// `id` becomes its group's most recently used tab.
    fn touch(&mut self, id: i64) {
        if let Some(ix) = self.group_of(id) {
            let mru = &mut self.groups[ix].mru;
            mru.retain(|m| *m != id);
            mru.insert(0, id);
        }
    }
    fn prune_groups(&mut self) {
        self.groups.retain(TabGroup::worth_keeping);
    }
}

/// Port of src/features/workspace/model/tabVisitHistory.ts. Closed tabs are
/// pruned rather than reopened by Back, and a new visit discards Forward.
#[derive(Default)]
pub(crate) struct VisitHistory {
    pub back: Vec<i64>,
    pub forward: Vec<i64>,
    current: Option<i64>,
}
impl VisitHistory {
    fn push(&mut self, id: i64) {
        if self.back.last() != Some(&id) {
            self.back.push(id);
        }
        if self.back.len() > 50 {
            self.back.remove(0);
        }
    }
    pub fn record(&mut self, id: i64) {
        if self.current == Some(id) {
            return;
        }
        if let Some(current) = self.current {
            self.push(current);
        }
        self.current = Some(id);
        self.forward.clear();
    }
    pub fn step(&mut self, direction: isize) -> Option<i64> {
        let target = if direction < 0 {
            let target = self.back.pop()?;
            if let Some(current) = self.current {
                self.forward.insert(0, current);
            }
            target
        } else {
            if self.forward.is_empty() {
                return None;
            }
            let target = self.forward.remove(0);
            if let Some(current) = self.current {
                self.push(current);
            }
            target
        };
        self.current = Some(target);
        Some(target)
    }
    pub fn prune(&mut self, tabs: &[i64], active: Option<i64>) {
        self.back.retain(|id| tabs.contains(id));
        self.forward.retain(|id| tabs.contains(id));
        if self.current.is_none_or(|id| !tabs.contains(&id)) {
            self.current = active;
        }
        while self
            .back
            .last()
            .copied()
            .is_some_and(|id| Some(id) == self.current)
        {
            self.back.pop();
        }
        while self
            .forward
            .first()
            .copied()
            .is_some_and(|id| Some(id) == self.current)
        {
            self.forward.remove(0);
        }
    }
}
pub(crate) fn project_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_empty_session_choices_survive_upgrade_and_new_choices_take_precedence() {
        for (preferences, expected) in [
            (serde_json::json!({}), "ink"),
            (serde_json::json!({"empty_session_games": false}), "none"),
            (serde_json::json!({"empty_session_games": true}), "games"),
            (
                serde_json::json!({"empty_session_games": false, "empty_session": "games"}),
                "games",
            ),
            (
                serde_json::json!({"empty_session_games": true, "empty_session": "none"}),
                "none",
            ),
        ] {
            let saved =
                serde_json::json!({"preferences": preferences, "sidebar_width": 280.}).to_string();
            let state = super::Workspace::restore(Some(&saved), &[], "/project");
            assert_eq!(state.preferences.empty_session, expected);
            assert_eq!(state.sidebar_width, 280.);
            let saved = serde_json::to_string(&state).unwrap();
            assert!(!saved.contains("empty_session_games"));
            let reopened = super::Workspace::restore(Some(&saved), &[], "/project");
            assert_eq!(reopened.preferences.empty_session, expected);
        }
    }

    #[test]
    fn rail_moves_stay_inside_their_section_and_follow_the_drawn_order() {
        let mut state = super::Workspace {
            classic_layout: true,
            projects: ["/a", "/b", "/c", "/d", "/e", "/f"]
                .map(str::to_owned)
                .to_vec(),
            project_rail_pinned: ["/e".to_owned()].into(),
            project_rail_groups: vec![super::ProjectRailGroup {
                id: "g".into(),
                name: "Group".into(),
                ..Default::default()
            }],
            project_rail_assignments: [("/b", "g"), ("/d", "g"), ("/f", "deleted")]
                .map(|(p, g)| (p.to_owned(), g.to_owned()))
                .into(),
            ..Default::default()
        };
        // Pinned, then the group, then the rest. A deleted group's member
        // falls back to the ungrouped list.
        assert_eq!(
            state.visible_project_order(),
            ["/e", "/b", "/d", "/a", "/c", "/f"]
        );
        assert_eq!(state.project_peers("/f"), ["/a", "/c", "/f"]);
        assert!(state.step_project_order("/f", -1));
        assert_eq!(state.project_peers("/a"), ["/a", "/f", "/c"]);
        assert!(!state.step_project_order("/b", -1), "/b leads its group");
        assert!(state.move_project_to_end("/b", 1));
        assert_eq!(state.project_peers("/d"), ["/d", "/b"]);
        assert!(!state.step_project_order("/e", 1), "/e is the only pin");
        // The projects row shows the saved order as it is.
        state.classic_layout = false;
        assert_eq!(state.visible_project_order(), state.projects);
        assert!(state.move_project_to_end("/e", -1));
        assert_eq!(state.projects[0], "/e");
    }

    #[test]
    fn legacy_boxy_layout_and_hidden_rail_migrate_without_changing_appearance() {
        let state = super::Workspace::restore(
            Some(r#"{"classic_layout":true,"preferences":{"collapsed_project_rail":"hidden"}}"#),
            &[],
            "/work",
        );
        assert!(!state.classic_layout);
        assert!(state.new_layout_boxy);
        assert!(state.boxy_layout());
        assert_eq!(state.preferences.collapsed_project_rail, "compact");
        let saved = serde_json::to_string(&state).unwrap();
        let mut state = super::Workspace::restore(Some(&saved), &[], "/work");
        state.classic_layout = true;
        state.project_rail_open = false;
        state.project_rail_width = 248.;
        let saved = serde_json::to_string(&state).unwrap();
        let state = super::Workspace::restore(Some(&saved), &[], "/work");
        assert!(state.classic_layout && state.new_layout_boxy);
        assert!(!state.project_rail_open);
        assert_eq!(state.project_rail_width, 248.);
    }

    use super::*;
    #[test]
    fn navigation_prunes_closed_tabs_and_a_new_visit_discards_forward() {
        let mut h = VisitHistory::default();
        h.record(1);
        h.record(2);
        h.record(3);
        assert_eq!(h.step(-1), Some(2));
        h.prune(&[1, 3], Some(3));
        assert_eq!(h.step(-1), Some(1));
        assert_eq!(h.step(1), Some(3));
        h.step(-1);
        h.record(4);
        assert!(h.forward.is_empty());
        for id in 5..100 {
            h.record(id);
        }
        assert_eq!(h.back.len(), 50);
    }
    /// A Mono split beside `target`, as a Mono pane.
    fn place_mono(s: &mut Workspace, mono: TitleTab, target: i64, edge: Edge) -> i64 {
        let pane = s.next_pane_id();
        s.title_order.retain(|entry| *entry != mono);
        s.scope_focus.retain(|_, entry| *entry != mono);
        s.mono_panes.insert(pane, mono);
        s.place(pane, target, edge);
        pane
    }
    fn mono(id: &str) -> TitleTab {
        TitleTab::Mono {
            id: id.into(),
            project: "/work".into(),
            worktree: "/work".into(),
        }
    }
    #[test]
    fn a_mono_pane_stays_a_pane_and_a_closed_tab_forgets_it() {
        let mut s = Workspace::default();
        s.open(1);
        s.open(2);
        let pane = place_mono(&mut s, mono("m"), 1, Edge::Right);
        assert!(pane < 0);
        assert_eq!(s.layout(1).leaf_ids(), vec![1, pane]);
        assert_eq!(s.active, Some(pane));
        assert_eq!(s.scope_pane(1), 1, "a Mono never scopes its tab");
        assert_eq!(s.mono_pane("m"), Some(pane));
        assert!(s.settle_mono_panes().is_empty());
        // Restarting keeps the Mono in its split.
        let saved = serde_json::to_string(&s).unwrap();
        let restored = Workspace::restore(
            Some(&saved),
            &[(1, "/work".into()), (2, "/work".into())],
            "/work",
        );
        assert_eq!(restored.layout(1).leaf_ids(), vec![1, pane]);
        assert_eq!(restored.mono_pane("m"), Some(pane));
        // Alone in its tab it is still a pane: a Mono is an ordinary tab.
        s.close_pane(1);
        assert!(s.settle_mono_panes().is_empty());
        assert_eq!(s.mono_pane("m"), Some(pane));
        // A closed tab forgets its Mono pane.
        s.close(pane);
        assert_eq!(s.settle_mono_panes(), [pane]);
        assert!(s.mono_panes.is_empty());
    }
    #[test]
    fn close_and_reopen_keep_session_identity() {
        let mut s = Workspace::default();
        s.open(1);
        s.open(2);
        s.open(3);
        s.open(2);
        assert_eq!(s.tabs, vec![1, 2, 3]);
        s.close(1);
        assert_eq!(s.active, Some(2));
        s.close(2);
        assert_eq!(s.active, Some(3));
        s.open(2);
        s.open(3);
        assert_eq!(s.active, Some(3));
        s.close(3);
        s.close(2);
        assert_eq!(s.active, None);
        s.open(1);
        assert_eq!(s.tabs, vec![1]);
    }
    #[test]
    fn split_tabs_place_detach_and_close_panes_without_losing_sessions() {
        let mut s = Workspace::default();
        s.open(1);
        s.open(2);
        s.open(1);
        s.split(3, SplitDir::Right);
        s.tab_workspaces.insert(1, "/selected".into());
        s.tab_workspaces.insert(2, "/other".into());
        assert_eq!(s.tabs, [1, 2]);
        assert_eq!(s.layout(1).leaf_ids(), [1, 3]);
        assert_eq!((s.active, s.active_tab()), (Some(3), Some(1)));
        // Another tab's session joins this one; its old tab closes.
        s.place(2, 1, Edge::Bottom);
        assert_eq!(s.tabs, [1]);
        assert!(!s.tab_workspaces.contains_key(&2));
        assert_eq!(s.layout(1).leaf_ids(), [1, 2, 3]);
        assert_eq!(s.open_sessions(), [1, 2, 3]);
        // Closing the key pane rekeys the tab and keeps its place.
        s.open(4);
        s.open(1);
        assert!(s.close_pane(1));
        assert_eq!(s.tabs, [2, 4]);
        assert_eq!(s.tab_workspaces[&2], "/selected");
        assert!(!s.tab_workspaces.contains_key(&1));
        assert_eq!(s.active, Some(2));
        assert_eq!(s.tab_focus(2), 2);
        s.open(3);
        s.open(s.tab_focus(4));
        assert_eq!(s.active, Some(4));
        s.open(s.tab_focus(2));
        assert_eq!(s.active, Some(3));
        // A pane dragged to the strip becomes a tab where it was dropped.
        s.detach(3, 0);
        assert_eq!(s.tabs, [3, 2, 4]);
        assert_eq!(s.tab_workspaces[&3], "/selected");
        assert!(!s.layouts.contains_key(&2));
        assert!(!s.close_pane(3));
    }
    /// A title tab of 1 whose group stacks 4 and 5 hidden behind it.
    fn stacked() -> Workspace {
        let mut s = Workspace::default();
        s.open(1);
        assert!(s.add_to_group(4, 1, None, false));
        assert!(s.add_to_group(5, 4, None, false));
        s
    }
    #[test]
    fn a_hidden_tab_is_open_through_its_group_but_not_a_leaf() {
        let s = stacked();
        assert_eq!(s.group_tabs(5), [1, 5, 4]);
        assert_eq!(s.layout(1).leaf_ids(), [1]);
        assert_eq!((s.tab_of(4), s.tab_of(5)), (Some(1), Some(1)));
        assert_eq!(s.visible_of(4), Some(1));
        assert_eq!(s.open_sessions(), [1, 5, 4]);
        assert_eq!(s.active, Some(1));
        assert_eq!(s.tabs, [1]);
        // Without groups a leaf is a group of one.
        let mut plain = Workspace::default();
        plain.open(7);
        assert_eq!(plain.group_tabs(7), [7]);
        assert!(plain.group_tabs(8).is_empty());
        assert!(plain.groups.is_empty());
    }
    #[test]
    fn showing_a_hidden_tab_swaps_the_leaf_and_rekeys_the_title_tab() {
        let mut s = stacked();
        s.tab_workspaces.insert(1, "/w".into());
        assert!(s.show(4));
        assert_eq!(s.tabs, [4], "a single-pane tab is keyed by what it shows");
        assert_eq!(s.title_order, [TitleTab::Session(4)]);
        assert_eq!(s.tab_workspaces[&4], "/w");
        assert_eq!(s.layout(4).leaf_ids(), [4]);
        assert_eq!((s.active, s.tab_of(1)), (Some(4), Some(4)));
        // In a split, the key pane gives way and the tab stays keyed.
        s.split(2, SplitDir::Right);
        assert_eq!(s.layout(4).leaf_ids(), [4, 2]);
        assert!(s.show(5));
        let tab = s.tab_of(2).unwrap();
        assert!(s.tabs == [tab] && s.layout(tab).contains(tab));
        assert_eq!(s.layout(tab).leaf_ids(), [5, 2]);
        assert_eq!((s.active, s.tab_focus(tab)), (Some(5), 5));
        assert_eq!(s.open_sessions(), [1, 5, 4, 2]);
        // Opening a hidden tab shows it.
        s.open(1);
        assert_eq!(s.layout(s.tab_of(2).unwrap()).leaf_ids(), [1, 2]);
        assert_eq!(s.active, Some(1));
    }
    #[test]
    fn closing_a_visible_tab_shows_the_most_recently_used_one() {
        let mut s = stacked();
        s.add_to_group(6, 1, None, false);
        assert_eq!(s.group_tabs(1), [1, 6, 5, 4]);
        s.show(4);
        s.show(1);
        // Recency is 1, 4, …: 4 takes over, not the neighbour 6.
        assert!(s.close_in_group(1));
        assert_eq!(s.tabs, [4]);
        assert_eq!(s.active, Some(4));
        assert_eq!(s.group_tabs(4), [6, 5, 4]);
        // A hidden tab just leaves.
        assert!(s.close_in_group(5));
        assert_eq!(s.group_tabs(4), [6, 4]);
        assert_eq!(s.tabs, [4]);
        // With no recency left, the neighbour takes over.
        assert!(s.close_in_group(4));
        assert_eq!((s.tabs.clone(), s.active), (vec![6], Some(6)));
        assert!(s.groups.is_empty(), "a lone plain tab is not stored");
        // The last tab closes the title tab as before.
        assert!(s.close_in_group(6));
        assert!(s.tabs.is_empty());
        assert!(!s.close_in_group(6));
    }
    #[test]
    fn closing_a_title_tab_drops_its_groups_hidden_tabs_included() {
        let mut s = stacked();
        s.open(2);
        s.close(5);
        assert_eq!(s.tabs, [2]);
        assert!(s.groups.is_empty());
        assert!(s.tab_of(4).is_none());
        assert_eq!(s.open_sessions(), [2]);
    }
    #[test]
    fn adding_an_open_tab_to_a_group_moves_it() {
        let mut s = stacked();
        s.open(2);
        s.open(3);
        // A whole other title tab's only pane joins: its tab closes.
        assert!(s.add_to_group(2, 4, None, true));
        assert_eq!(s.tabs, [2, 3], "shown, 2 keys the tab");
        assert_eq!(s.group_tabs(1), [1, 2, 5, 4]);
        assert_eq!(s.active, Some(2));
        assert_eq!(s.layout(2).leaf_ids(), [2]);
        // A hidden tab of another group moves without a copy left behind.
        s.open(3);
        assert!(s.add_to_group(5, 3, Some(0), false));
        assert_eq!(s.group_tabs(3), [5, 3]);
        assert_eq!(s.group_tabs(2), [1, 2, 4]);
        assert_eq!(s.open_sessions().len(), 5);
        // Within its group it is only reordered.
        assert!(s.add_to_group(4, 2, Some(0), false));
        assert_eq!(s.group_tabs(2), [4, 1, 2]);
    }
    #[test]
    fn pinned_tabs_lead_their_group_and_a_preview_is_never_pinned() {
        let mut s = stacked();
        assert!(s.set_preview(1, Some(4)));
        assert_eq!(s.preview_of_group(5), Some(4));
        assert!(s.set_pinned(4, true));
        assert_eq!(s.group_tabs(1), [4, 1, 5]);
        assert_eq!(s.preview_of_group(1), None);
        assert!(!s.set_preview(1, Some(4)));
        // New and moved tabs stay after the pinned ones.
        assert!(s.move_in_group(5, 0));
        assert_eq!(s.group_tabs(1), [4, 5, 1]);
        assert!(!s.move_in_group(4, 2), "a pinned tab stays among the pins");
        s.add_to_group(6, 1, Some(0), false);
        assert_eq!(s.group_tabs(1), [4, 6, 5, 1]);
        assert!(s.set_pinned(4, false));
        assert_eq!(s.group_tabs(1), [4, 6, 5, 1]);
        assert!(s.set_pinned(5, true));
        assert_eq!(s.group_tabs(1), [5, 4, 6, 1]);
        // A lone tab keeps its pin as a group of one.
        let mut lone = Workspace::default();
        lone.open(9);
        assert!(lone.set_pinned(9, true));
        assert_eq!(lone.groups.len(), 1);
        assert!(lone.set_pinned(9, false));
        assert!(lone.groups.is_empty());
    }
    #[test]
    fn a_hidden_tab_dropped_on_an_edge_gets_its_own_pane() {
        let mut s = stacked();
        s.place(4, 1, Edge::Right);
        assert_eq!(s.layout(1).leaf_ids(), [1, 4]);
        assert_eq!(s.group_tabs(1), [1, 5]);
        assert_eq!(s.group_tabs(4), [4]);
        assert_eq!(s.active, Some(4));
        // The visible tab of a stack leaves too; its group shows the next.
        s.show(1);
        s.place(1, 4, Edge::Bottom);
        let tab = s.tab_of(4).unwrap();
        assert_eq!(s.layout(tab).leaf_ids(), [5, 4, 1]);
        assert!(s.groups.is_empty());
        // A pane of its own dropped beside its neighbour just moves.
        s.place(1, 5, Edge::Left);
        assert_eq!(s.layout(s.tab_of(4).unwrap()).leaf_ids(), [1, 5, 4]);
    }
    #[test]
    fn split_detach_and_swap_keep_groups_coherent() {
        let mut s = stacked();
        // A hidden tab split beside the focused pane leaves its group.
        s.split(5, SplitDir::Right);
        assert_eq!(s.layout(1).leaf_ids(), [1, 5]);
        assert_eq!(s.group_tabs(1), [1, 4]);
        // A hidden tab dragged to the strip becomes its own title tab.
        s.detach(4, 1);
        assert_eq!(s.tabs, [1, 4]);
        assert!(s.groups.is_empty());
        // Swapping a visible tab hands its slot in the group to the new one.
        s.add_to_group(6, 1, None, false);
        s.swap_session(1, 7);
        assert_eq!(s.group_tabs(7), [7, 6]);
        assert!(s.tab_of(1).is_none());
        s.swap_session(6, 8);
        assert_eq!(s.group_tabs(7), [7, 8]);
        // A Mono hidden in a group stays there.
        let pane = s.next_pane_id();
        s.mono_panes.insert(pane, mono("m"));
        s.add_to_group(pane, 4, None, true);
        assert!(s.settle_mono_panes().is_empty());
        assert_eq!(s.group_tabs(4), [4, pane]);
    }
    #[test]
    fn merging_a_title_tab_stacks_all_its_panes_in_the_target_group() {
        let mut s = stacked();
        s.open(2);
        s.split(3, SplitDir::Down);
        s.add_to_group(6, 3, None, false);
        s.tab_workspaces.insert(2, "/w".into());
        s.pinned_tabs.push(TitleTab::Session(2));
        assert!(s.merge_tab_into(2, 5));
        assert_eq!(s.tabs, [1]);
        assert_eq!(s.title_order, [TitleTab::Session(1)]);
        assert!(s.pinned_tabs.is_empty() && s.tab_workspaces.is_empty());
        assert!(!s.layouts.contains_key(&2) && !s.focused.contains_key(&2));
        assert_eq!(s.group_tabs(1), [1, 5, 4, 2, 3, 6]);
        assert_eq!(s.layout(1).leaf_ids(), [1]);
        assert_eq!(s.active, Some(1));
        assert!(!s.merge_tab_into(1, 4), "a tab cannot fold into itself");
    }
    #[test]
    fn groups_pins_and_previews_survive_a_restart() {
        let mut s = stacked();
        s.open(2);
        s.split(3, SplitDir::Right);
        s.add_to_group(-2, 3, None, false);
        s.view_panes.insert(-2, TitleTab::App(AppView::Notes));
        s.set_pinned(5, true);
        s.set_preview(1, Some(4));
        let json = s.saved_json();
        let sessions: Vec<_> = (1..=5).map(|id| (id, "/w".into())).collect();
        let r = Workspace::restore(Some(&json), &sessions, "/w");
        let strip = |s: &Workspace| {
            (s.groups.iter())
                .map(|g| (g.tabs.clone(), g.pinned.clone(), g.preview))
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(&r), strip(&s));
        assert_eq!(r.view_panes[&-2], TitleTab::App(AppView::Notes));
        assert_eq!(r.tab_of(4), Some(1));
        assert_eq!(r.next_pane_id(), -3);
        // A file without groups or view panes writes neither.
        let plain = serde_json::to_string(&Workspace::default()).unwrap();
        assert!(!plain.contains("\"groups\"") && !plain.contains("view_panes"));
    }
    #[test]
    fn restore_and_normalize_drop_dangling_group_tabs() {
        let mut s = stacked();
        s.groups[0].tabs.extend([-9, 4]);
        s.groups[0].preview = Some(8);
        s.groups.push(TabGroup {
            tabs: vec![12, 13],
            ..Default::default()
        });
        s.view_panes.insert(-3, TitleTab::Search);
        s.normalize_groups();
        assert_eq!(s.groups.len(), 1, "a group with nothing on screen goes");
        assert_eq!(s.group_tabs(1), [1, 5, 4]);
        assert_eq!(s.preview_of_group(1), None);
        // A gone visible tab hands its leaf to a known hidden one.
        let json = serde_json::to_string(&s).unwrap();
        let sessions = vec![(4, "/w".into()), (5, "/w".into())];
        let r = Workspace::restore(Some(&json), &sessions, "/w");
        assert_eq!(r.tabs, [5]);
        assert_eq!(r.group_tabs(4), [5, 4]);
        assert!(
            r.view_panes.is_empty(),
            "a view pane no tab holds is dropped"
        );
        // Two visible tabs in one group split into two groups.
        let mut s = Workspace::default();
        s.open(1);
        s.open(2);
        s.groups.push(TabGroup {
            tabs: vec![1, 3, 2],
            ..Default::default()
        });
        s.normalize_groups();
        assert_eq!(s.group_tabs(1), [1, 3]);
        assert_eq!(s.group_tabs(2), [2]);
    }
    #[test]
    fn restore_keeps_split_tabs_and_prunes_their_missing_panes() {
        let mut s = Workspace::default();
        s.open(1);
        s.split(2, SplitDir::Right);
        s.split(3, SplitDir::Down);
        s.open(9);
        s.open(2);
        let json = serde_json::to_string(&s).unwrap();
        let sessions: Vec<_> = [2, 3].iter().map(|id| (*id, "/w".into())).collect();
        let s = Workspace::restore(Some(&json), &sessions, "/w");
        assert_eq!(s.tabs, [2]);
        assert_eq!(s.layout(2).leaf_ids(), [2, 3]);
        assert_eq!(s.active, Some(2));
        assert_eq!(s.tab_focus(2), 2);
    }
    #[test]
    fn terminal_docks_add_close_move_and_clamp_like_project_terminal() {
        let mut dock = TerminalDock::default();
        assert_eq!((dock.side, dock.size), (DockSide::Bottom, 220.));
        dock.add("/w/a".into());
        dock.add("/w/b".into());
        dock.add("/w/c".into());
        assert_eq!(dock.active, 2);
        dock.active = 1;
        assert!(dock.close(1));
        assert_eq!((dock.terminals.len(), dock.active), (2, 1));
        assert!(dock.close(0));
        assert_eq!(dock.active, 0);
        assert!(!dock.close(0));
        dock.set_side(DockSide::Right);
        assert_eq!(dock.size, 360.);
        dock.size = 500.;
        dock.set_side(DockSide::Left);
        assert_eq!(dock.size, 500.);
        assert_eq!(DockSide::Bottom.clamp(10., (1280., 800.)), 88.);
        assert_eq!(DockSide::Bottom.clamp(900., (1280., 800.)), 560.);
        assert_eq!(DockSide::Left.clamp(f32::NAN, (1280., 800.)), 360.);
        let mut s = Workspace::default();
        s.docks.insert("/w".into(), dock.clone());
        let mut kept = dock.clone();
        kept.add("/w".into());
        s.docks.insert("/k".into(), kept);
        let json = serde_json::to_string(&s).unwrap();
        let s = Workspace::restore(Some(&json), &[], "/w");
        assert!(!s.docks.contains_key("/w"));
        assert_eq!(s.docks["/k"].terminals, ["/w"]);
    }
    #[test]
    fn terminal_docks_reorder_keeping_the_active_terminal_and_close_others() {
        let mut dock = TerminalDock::default();
        for cwd in ["/a", "/b", "/c", "/d"] {
            dock.add(cwd.into());
        }
        dock.active = 1;
        dock.reorder(1, 3);
        assert_eq!(dock.terminals, ["/a", "/c", "/d", "/b"]);
        assert_eq!(dock.active, 3);
        dock.reorder(0, 2);
        assert_eq!(dock.terminals, ["/c", "/d", "/a", "/b"]);
        assert_eq!(dock.active, 3);
        dock.reorder(3, 0);
        assert_eq!(dock.terminals, ["/b", "/c", "/d", "/a"]);
        assert_eq!(dock.active, 0);
        dock.active = 2;
        dock.reorder(3, 1);
        assert_eq!(dock.terminals, ["/b", "/a", "/c", "/d"]);
        assert_eq!(dock.active, 3);
        dock.keep_only(2);
        assert_eq!(
            (dock.terminals.clone(), dock.active),
            (vec!["/c".to_string()], 0)
        );
    }
    #[test]
    fn project_app_tabs_are_saved_apart_and_come_back_in_place() {
        let sessions = vec![(1, "/a".into()), (2, "/a".into())];
        let notes = TitleTab::ProjectApp {
            view: AppView::Notes,
            project: "/a".into(),
        };
        let gone = TitleTab::ProjectApp {
            view: AppView::Inbox,
            project: "/removed".into(),
        };
        let mut state = Workspace::default();
        state.open(1);
        state.open(2);
        state.projects = vec!["/a".into()];
        state.project = "/a".into();
        state.title_order = vec![
            TitleTab::Session(1),
            notes.clone(),
            TitleTab::Session(2),
            gone.clone(),
        ];
        state.pinned_tabs = vec![TitleTab::Session(2), notes.clone()];
        state.scope_focus.insert("/a\0/a".into(), notes.clone());
        let json = state.saved_json();
        // A build without the variant must still read the file.
        assert!(!json.contains("ProjectApp"), "{json}");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["title_order"].as_array().unwrap().len(), 2);
        // Saving leaves the live state as it was, removed project included.
        assert_eq!(state.title_order[1], notes);
        assert_eq!(state.pinned_tabs, vec![TitleTab::Session(2), notes.clone()]);
        assert!(state.project_apps.tabs.is_empty());
        let restored = Workspace::restore(Some(&json), &sessions, "/a");
        assert_eq!(
            restored.title_order,
            vec![TitleTab::Session(1), notes.clone(), TitleTab::Session(2)]
        );
        assert_eq!(
            restored.pinned_tabs,
            vec![TitleTab::Session(2), notes.clone()]
        );
        assert_eq!(restored.scope_focus.get("/a\0/a"), Some(&notes));
        assert!(restored.project_apps.tabs.is_empty());
        // A file without app tabs has no `project_apps` at all.
        let mut plain = Workspace::default();
        let plain: serde_json::Value = serde_json::from_str(&plain.saved_json()).unwrap();
        assert!(plain.get("project_apps").is_none());
    }
    #[test]
    fn restore_keeps_project_selection_and_prunes_stale_project_focus() {
        let sessions = vec![(3, "/b".into()), (2, "/a".into()), (1, "/a".into())];
        let mut state = Workspace::default();
        for id in [1, 2, 3] {
            state.open(id);
        }
        state.project = "/a".into();
        state.active = Some(99);
        state.project_focus.insert("/a".into(), 1);
        state.project_focus.insert("/b".into(), 99);
        state.worktree_focus.insert(
            "/a".into(),
            crate::worktree_switcher::WorktreeFocus {
                path: "/a-tree".into(),
                branch: Some("feature".into()),
            },
        );
        state.tab_workspaces.insert(1, "/a-tree".into());
        let mut saved = serde_json::to_value(&state).unwrap();
        assert!(saved.get("worktree_focus").is_none());
        assert!(saved.get("tab_workspaces").is_none());
        // Older native sidecars saved this transient filter. Ignore it too.
        saved["worktree_focus"] = serde_json::json!({
            "/a": {"path":"/a-tree", "branch":"feature"}
        });
        let json = serde_json::to_string(&saved).unwrap();
        let restored = Workspace::restore(Some(&json), &sessions, "/b");
        assert_eq!(restored.active, Some(1));
        assert_eq!(restored.project, "/a");
        assert!(restored.worktree_focus.is_empty());
        assert!(restored.tab_workspaces.is_empty());
        assert!(!restored.project_focus.contains_key("/b"));
        state.active = Some(3);
        let json = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&json), &sessions, "/a");
        assert_eq!(restored.active, Some(3));
        assert_eq!(restored.project, "/b");
        let fresh = Workspace::restore(None, &sessions, "/a");
        assert_eq!(fresh.active, Some(2));
        assert_eq!(fresh.project, "/a");
        assert!(fresh.new_layout_boxy);
        let installed = Workspace::restore(Some(&Workspace::fresh_install_json()), &sessions, "/a");
        assert_eq!(installed.project, "/a");
        assert!(installed.classic_layout && !installed.new_layout_boxy);
        assert_eq!(
            installed.preferences,
            crate::settings::Preferences::fresh_install()
        );
        let rounded = Workspace::restore(Some(r#"{"new_layout_boxy":false}"#), &sessions, "/a");
        assert!(!rounded.new_layout_boxy);
    }
    #[test]
    fn restoring_history_keeps_only_remembered_or_selected_projects() {
        let saved = serde_json::json!({
            "project": "/code/app",
            "projects": ["/code/app", "/code/other"],
            "tabs": [2],
            "active": 2,
            "desktop_project_preferences": {"projects": ["/code/app", "/code/other"]}
        })
        .to_string();
        let sessions = vec![
            (1, "/code/app".into()),
            (2, "/tmp/internal-mono-chat".into()),
            (3, "/private/var/folders/temporary".into()),
        ];
        let restored = Workspace::restore(Some(&saved), &sessions, "/code/app");
        assert_eq!(restored.projects, ["/code/app", "/code/other"]);
        assert_eq!(restored.project, "/code/app");
        assert_eq!(restored.active, None);
        let serialized = serde_json::to_value(restored).unwrap();
        assert_eq!(
            serialized["desktop_project_preferences"]["projects"],
            serde_json::json!(["/code/app", "/code/other"])
        );
    }
    #[test]
    fn restore_prunes_stale_tabs_and_preserves_an_intentionally_empty_workspace() {
        let mut s = Workspace::default();
        s.open(1);
        s.open(8);
        s.tabs.push(1);
        s.sidebar_width = 900.;
        let json = serde_json::to_string(&s).unwrap();
        let s = Workspace::restore(Some(&json), &[(1, "/work/a".into())], "/work/b");
        assert_eq!(s.tabs, vec![1]);
        assert_eq!(s.active, None);
        assert_eq!(s.project, "/work/b");
        assert_eq!(s.sidebar_width, 560.);
        assert_eq!(s.projects, vec!["/work/b"]);
        let s = Workspace::restore(Some("{}"), &[(1, "/work/a".into())], "/work/a");
        assert!(s.tabs.is_empty());
        assert!(
            s.compact_session_cards,
            "older workspaces retain compact cards"
        );
        assert!(
            s.sidebar_activity_visible,
            "older workspaces keep the activity card visible"
        );
    }
    #[test]
    fn desktop_snapshot_translation_deserializes_and_retains_unported_metadata() {
        let source = serde_json::json!({
            "projectCwd":"/work", "activeTabId":"tab",
            "tabs":[{"id":"tab", "focusedId":"b", "groupId":"assigned", "editorPanes":[], "layout":{"type":"split", "dir":"down", "children":[{"type":"leaf","id":"a"},{"type":"leaf","id":"b"}], "sizes":[0.4,0.6]}}],
            "projectTerminals":[{"projectPath":"/work", "side":"bottom", "size":240, "open":false, "pane":{"activeFileId":"term", "files":[{"id":"term", "terminal":true, "cwd":"/work/sub"}]}}]
        });
        let mapped = monocode_core::desktop_preferences::DesktopPreferences::workspace_snapshot(
            &source,
            |id| match id {
                "a" => Some(1),
                "b" => Some(2),
                _ => None,
            },
        );
        let saved = mapped.to_string();
        let restored = Workspace::restore(
            Some(&saved),
            &[(1, "/work".into()), (2, "/work".into())],
            "/work",
        );
        assert_eq!(restored.tabs, [1]);
        assert_eq!(restored.active, Some(2));
        assert!(restored.layouts[&1].is_split());
        assert_eq!(restored.focused[&1], 2);
        assert_eq!(restored.docks["/work"].terminals, ["/work/sub"]);
        assert!(!restored.docks["/work"].open);
        assert_eq!(restored.desktop_workspace_snapshot.as_ref(), Some(&source));
    }

    #[test]
    fn files_recorded_as_owned_by_a_terminal_pane_are_given_to_a_session_on_restore() {
        let mut state = Workspace::default();
        state.add_project("/p");
        state.tabs = vec![1, -2, -3, -5];
        state.title_order = vec![
            TitleTab::Session(1),
            TitleTab::Session(-2),
            TitleTab::Session(-3),
            TitleTab::Session(-5),
        ];
        state.terminal_panes.insert(
            -1,
            TerminalPane {
                owner: 1,
                cwd: "/p".into(),
            },
        );
        let file = |path: &str| crate::review::ReviewState {
            tabs: vec![crate::review::ReviewTab::File {
                cwd: "/p".into(),
                path: path.into(),
            }],
            chat_hidden: true,
            ..Default::default()
        };
        // -2: its terminal is still there. -3: the terminal is long gone. -5: a session owner.
        state.review_owners = [(-2, -1), (-3, -9), (-5, 1)].into();
        state.reviews = [(-2, file("/p/a")), (-3, file("/p/b")), (-5, file("/p/c"))].into();
        let saved = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&saved), &[(1, "/p".into())], "/p");
        assert_eq!(restored.review_owners.get(&-2), Some(&1));
        assert_eq!(
            restored.review_owners.get(&-3),
            Some(&1),
            "a gone terminal falls back to a session of the file's project"
        );
        assert_eq!(restored.review_owners.get(&-5), Some(&1));
        assert!(restored.tab_of(-2).is_some() && restored.tab_of(-3).is_some());
    }

    #[test]
    fn a_projects_isolated_browser_profile_round_trips() {
        let mut state = Workspace::default();
        state.add_project("/p");
        state.browser_isolated_projects.insert("/p".into());
        let saved = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&saved), &[(1, "/p".into())], "/p");
        assert!(restored.browser_isolated_projects.contains("/p"));
        // Nothing is written while every project shares the default profile.
        let plain = serde_json::to_string(&Workspace::default()).unwrap();
        assert!(!plain.contains("browser_isolated_projects"));
    }

    #[test]
    fn a_page_opened_from_another_sits_right_after_its_tab() {
        let mut state = Workspace::default();
        state.open(1);
        for id in [2, 3] {
            assert!(state.add_to_group(id, 1, None, false));
        }
        state.show(1);
        // Opened as a tab of its own, it joins its opener's group beside it.
        state.open(-1);
        state.place_tab_after(-1, 1);
        assert_eq!(state.group_tabs(1), [1, -1, 3, 2]);
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.active, Some(-1));
        // Already in the group, it is only reordered; an unknown opener
        // moves nothing.
        state.place_tab_after(-1, 3);
        assert_eq!(state.group_tabs(1), [1, 3, -1, 2]);
        state.place_tab_after(-1, 99);
        assert_eq!(state.group_tabs(1), [1, 3, -1, 2]);
    }

    #[test]
    fn a_projects_terminal_setup_round_trips_and_an_empty_one_is_dropped() {
        use monocode_core::terminal_setup::{TerminalSetup, Venv};
        let mut state = Workspace::default();
        state.add_project("/p");
        let setup = TerminalSetup {
            environment: "  MODE=dev\n".into(),
            venv: Venv::Auto,
            agents: false,
            ..Default::default()
        };
        state.project_terminal_setups.insert("/p".into(), setup);
        state.project_terminal_setups.insert(
            "/empty".into(),
            TerminalSetup {
                venv: Venv::Path("  ".into()),
                ..Default::default()
            },
        );
        let saved = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&saved), &[(1, "/p".into())], "/p");
        let kept = &restored.project_terminal_setups["/p"];
        assert_eq!(kept.environment, "MODE=dev");
        assert_eq!(kept.venv, Venv::Auto);
        assert!(!kept.agents);
        assert!(!restored.project_terminal_setups.contains_key("/empty"));
        // Nothing is written for a workspace without setups.
        let plain = serde_json::to_string(&Workspace::default()).unwrap();
        assert!(!plain.contains("project_terminal_setups"));
    }

    #[test]
    fn a_projects_run_configurations_round_trip_and_an_empty_entry_is_dropped() {
        use monocode_core::code::run::{ProjectRuns, RunConfig, TestRunner};
        let mut state = Workspace::default();
        state.add_project("/p");
        let mut runs = ProjectRuns::default();
        let id = runs.add(RunConfig {
            name: " Serve ".into(),
            command: "npm start".into(),
            ..Default::default()
        });
        runs.test_runner = TestRunner::Unittest;
        state.project_runs.insert("/p".into(), runs);
        let mut blank = ProjectRuns::default();
        blank.add(RunConfig::default());
        state.project_runs.insert("/blank".into(), blank);
        let saved = serde_json::to_string(&state).unwrap();
        let restored = Workspace::restore(Some(&saved), &[(1, "/p".into())], "/p");
        let kept = &restored.project_runs["/p"];
        assert_eq!(kept.get(&id).unwrap().name, "Serve");
        assert_eq!(kept.test_runner, TestRunner::Unittest);
        assert!(!restored.project_runs.contains_key("/blank"));
        // Nothing is written for a workspace without run configurations.
        let plain = serde_json::to_string(&Workspace::default()).unwrap();
        assert!(!plain.contains("project_runs"));
    }

    #[test]
    fn a_page_saved_by_the_webkit_browser_reopens_in_chromium() {
        // Workspaces saved while Monocode had its WebKit browser keep pages in
        // `browser_panes`, with settings Chromium pages do not have, and start
        // pages in `project_browser_urls`. The page keeps its place, address
        // and title; the rest is dropped.
        let mut state = Workspace::default();
        state.add_project("/p");
        state.terminal_panes.insert(
            -5,
            TerminalPane {
                owner: 1,
                cwd: "/p".into(),
            },
        );
        state.tabs = vec![1];
        state.layouts.insert(
            1,
            PaneNode::Split {
                dir: SplitDir::Right,
                children: vec![
                    PaneNode::Leaf { id: 1 },
                    PaneNode::Leaf { id: -4 },
                    PaneNode::Leaf { id: -5 },
                ],
                sizes: vec![0.4, 0.3, 0.3],
            },
        );
        state.focused.insert(1, -4);
        state.active = Some(-4);
        let mut saved = serde_json::to_value(&state).unwrap();
        saved["browser_panes"] = serde_json::json!({
            "-4": {
                "owner": 1,
                "url": "http://localhost:5173",
                "title": "Vite",
                "user_agent": null,
                "viewport": [390, 844],
                "keep_awake": true,
                "zoom_percent": 125,
                "reload_on_save": true
            }
        });
        saved["project_browser_urls"] = serde_json::json!({ "/p": "localhost:5173" });
        let restored = Workspace::restore(Some(&saved.to_string()), &[(1, "/p".into())], "/p");
        assert_eq!(restored.tabs, vec![1]);
        assert_eq!(restored.layout(1).leaf_ids(), vec![1, -4, -5]);
        assert!(restored.terminal_panes.contains_key(&-5));
        assert_eq!(
            restored.browser_panes[&-4],
            BrowserPane {
                owner: 1,
                url: "http://localhost:5173".into(),
                title: "Vite".into(),
                ..Default::default()
            }
        );
        assert_eq!(restored.focused.get(&1), Some(&-4));
        assert!(restored.projects.contains(&"/p".to_string()));
        let again = serde_json::to_string(&restored).unwrap();
        for gone in [
            "project_browser_urls",
            "keep_awake",
            "zoom_percent",
            "user_agent",
        ] {
            assert!(!again.contains(gone), "{gone}: {again}");
        }
    }
}
