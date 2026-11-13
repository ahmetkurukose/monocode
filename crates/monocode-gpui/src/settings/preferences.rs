//! App-wide preferences the Settings pages edit. Defaults are the React
//! app's (`settings.ts`, `appearance.ts`, `displayPrefs.ts`, `sounds.ts`).
use monocode_core::provider::Kind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    // General
    pub sounds: bool,
    pub notifications: bool,
    pub project_notifications: serde_json::Value,
    pub notification_projects: serde_json::Value,
    pub file_tabs: String,
    pub file_word_wrap: bool,
    pub file_view_mode: String,
    pub tab_animations: bool,
    pub notes: bool,
    pub quick_composer: bool,
    /// Global search floats as a command panel instead of opening a page.
    pub search_panel: bool,
    pub working_agents: bool,
    pub close_to_tray: bool,
    /// Running an action shows the terminal it runs in.
    pub action_opens_terminal: bool,
    /// Legacy: the new layout opens the terminal as a right panel tab, not a
    /// dock. Read when `terminal_placement` is unset, and kept in step with it.
    pub terminal_in_panel: bool,
    /// Where terminals open: "bottom" (the dock), "panel" (the right panel's
    /// Terminal tab) or "title" (their own title bar tabs). Empty falls back
    /// to `terminal_in_panel`.
    pub terminal_placement: String,
    /// The terminal's text size in points.
    pub terminal_font_size: u32,
    /// Lines of history each new terminal keeps above the screen.
    pub terminal_scrollback: u32,
    /// Selecting text in a terminal copies it.
    pub terminal_copy_on_select: bool,
    // Appearance
    pub theme: String,
    pub wayland_window_decorations: bool,
    pub workspace_theme: String,
    // Retain the legacy inset-only key when reading and saving preferences.
    // Window materials now use the shared opacity and main-pane glass values.
    pub inset_glass: bool,
    pub accent_color: Option<String>,
    pub diff_palette: String,
    pub theme_hue: u32,
    pub theme_saturation: u32,
    pub theme_dark_lightness: u32,
    pub sidebar_opacity: u32,
    // Retain the saved value for compatibility; native window blur is fixed.
    pub sidebar_blur: u32,
    pub body_glass: bool,
    pub chat_background: Option<String>,
    pub chat_background_effect: String,
    pub chat_background_scope: String,
    pub chat_background_empty: u32,
    pub chat_background_session: u32,
    pub collapsed_project_rail: String,
    /// Classic layout's project list: "list" (one row per project) or "tree".
    pub project_rail_view: String,
    /// The apps offered under each project in the tree: "inbox", "notes", "database".
    pub project_apps: Vec<String>,
    /// Which apps `project_apps` was last updated for, so an app added later is
    /// offered once to saved preferences and then left to the user.
    #[serde(default)]
    pub project_apps_rev: u32,
    pub ui_scale: u32,
    pub show_excluded_files: bool,
    // Chat
    pub transcript_layout: String,
    pub anchor_prompts: bool,
    pub paced_reveal: bool,
    /// Tool groups stay closed, even while they run, until clicked open.
    pub collapse_tool_groups: bool,
    /// A finished reply folds its work under "worked for"; off keeps the
    /// text and tool groups inline.
    pub fold_finished_work: bool,
    pub follow_up: String,
    pub model_controls: String,
    pub autocorrect: bool,
    pub format_on_save: bool,
    pub autosave: bool,
    pub diff_view: String,
    pub composer_mascot: bool,
    /// What fills an empty session: "ink", "games" or "none".
    pub empty_session: String,
    // Providers
    pub show_remaining_usage: bool,
    pub mask_emails: bool,
    pub idle_agents: u32,
    pub claude_hooks: bool,
    /// Start Claude Code with its Chrome integration.
    pub claude_chrome: bool,
    /// What agents may do in the browser pane: "any" site, "local"
    /// addresses only, or "off".
    pub browser_agents: String,
    /// Where web links in replies and terminals open: "monocode" (a
    /// browser pane) or "system" (the user's own browser).
    pub browser_links: String,
    /// Minutes a page may be off screen before it is put to sleep; 0 never.
    pub browser_sleep_minutes: u32,
    /// How many pages may be loaded at once before the oldest sleeps.
    pub browser_max_live: u32,
    /// Experimental: report Monocode instead of `sdk-cli` as Claude's entrypoint.
    pub claude_entrypoint: bool,
    pub binary_paths: HashMap<String, String>,
    pub disabled_skill_paths: Vec<String>,
    pub skill_source_paths: Vec<String>,
    pub default_provider: Option<String>,
    pub default_models: HashMap<String, String>,
    pub hidden_providers: Vec<String>,
    pub project_providers: HashMap<String, ProviderDefaults>,
    pub favorite_models: Vec<ModelChoice>,
    pub recent_models: Vec<ModelChoice>,
    pub model_picker_tab: Option<String>,
    /// The last access mode picked in a composer; new chats start with it.
    pub default_access: Option<String>,
    /// Last explicit effort choice for each provider, shared across projects.
    pub provider_efforts: HashMap<String, String>,
    /// The model for session titles, branch names, commit messages and pull
    /// requests; `None` lets each use its own harness's quick model.
    pub background_model: Option<ModelChoice>,
    // Monos
    pub monos: bool,
    pub rail_monos_pinned: bool,
    pub mono_menu_bar_icon: bool,
    // Archive
    pub show_archived: bool,
    // Keybindings: a changed shortcut, or `None` for a disabled one.
    pub keybindings: HashMap<String, Option<String>>,
}

pub(crate) const UI_SCALE_PERCENTS: [u32; 17] = [
    50, 60, 70, 80, 85, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200,
];

impl Default for Preferences {
    fn default() -> Self {
        Self {
            sounds: true,
            notifications: false,
            project_notifications: serde_json::json!({}),
            notification_projects: serde_json::json!([]),
            file_tabs: "pane".into(),
            file_word_wrap: true,
            file_view_mode: "preview".into(),
            tab_animations: false,
            notes: true,
            quick_composer: true,
            search_panel: false,
            working_agents: true,
            close_to_tray: true,
            action_opens_terminal: true,
            terminal_in_panel: false,
            terminal_placement: String::new(),
            terminal_font_size: 13,
            terminal_scrollback: 2000,
            terminal_copy_on_select: false,
            theme: "dark".into(),
            wayland_window_decorations: true,
            workspace_theme: "default".into(),
            inset_glass: true,
            accent_color: None,
            diff_palette: "default".into(),
            theme_hue: 240,
            theme_saturation: 0,
            theme_dark_lightness: 9,
            sidebar_opacity: 85,
            sidebar_blur: 24,
            body_glass: true,
            chat_background: None,
            chat_background_effect: "none".into(),
            chat_background_scope: "all".into(),
            chat_background_empty: 24,
            chat_background_session: 24,
            collapsed_project_rail: "compact".into(),
            project_rail_view: "list".into(),
            project_apps: vec![
                "inbox".into(),
                "notes".into(),
                "database".into(),
                "api".into(),
            ],
            project_apps_rev: PROJECT_APPS_REV,
            ui_scale: 100,
            show_excluded_files: false,
            transcript_layout: "chat".into(),
            anchor_prompts: true,
            paced_reveal: true,
            collapse_tool_groups: false,
            fold_finished_work: true,
            follow_up: "steer".into(),
            model_controls: "menu".into(),
            autocorrect: false,
            format_on_save: true,
            autosave: false,
            diff_view: "editor".into(),
            composer_mascot: true,
            empty_session: "ink".into(),
            show_remaining_usage: false,
            mask_emails: true,
            idle_agents: 3,
            claude_hooks: true,
            claude_chrome: false,
            browser_agents: "any".into(),
            browser_links: "monocode".into(),
            browser_sleep_minutes: 15,
            browser_max_live: 4,
            claude_entrypoint: false,
            binary_paths: HashMap::new(),
            disabled_skill_paths: Vec::new(),
            skill_source_paths: Vec::new(),
            default_provider: None,
            default_models: HashMap::new(),
            hidden_providers: Vec::new(),
            project_providers: HashMap::new(),
            favorite_models: Vec::new(),
            recent_models: Vec::new(),
            model_picker_tab: None,
            default_access: None,
            provider_efforts: HashMap::new(),
            background_model: None,
            monos: true,
            rail_monos_pinned: false,
            mono_menu_bar_icon: true,
            show_archived: false,
            keybindings: HashMap::new(),
        }
    }
}

/// 1: the API app.
const PROJECT_APPS_REV: u32 = 1;

impl Preferences {
    /// Offers apps added since these preferences were saved. Runs once per
    /// revision, so switching one off afterwards sticks.
    pub(crate) fn migrate(&mut self) {
        if self.project_apps_rev < 1 {
            self.set_project_app_shown("api", true);
        }
        self.project_apps_rev = PROJECT_APPS_REV;
    }

    /// Unknown values fall back to the plain list.
    pub(crate) fn project_rail_tree(&self) -> bool {
        self.project_rail_view == "tree"
    }

    /// Whether the tree offers this app ("inbox", "notes", "database") under each project.
    pub(crate) fn project_app_shown(&self, app: &str) -> bool {
        self.project_apps.iter().any(|a| a == app)
    }

    pub(crate) fn set_project_app_shown(&mut self, app: &str, on: bool) {
        self.project_apps.retain(|a| a != app);
        if on {
            self.project_apps.push(app.to_owned());
        }
        // Keep one order on disk whatever order they were switched in.
        let rank = |a: &String| {
            ["inbox", "notes", "database", "api"]
                .iter()
                .position(|x| x == a)
        };
        self.project_apps.sort_by_key(rank);
    }

    /// "bottom", "panel" or "title".
    pub(crate) fn terminal_placement(&self) -> &str {
        match self.terminal_placement.as_str() {
            placement @ ("bottom" | "panel" | "title") => placement,
            _ if self.terminal_in_panel => "panel",
            _ => "bottom",
        }
    }

    pub(crate) fn set_terminal_placement(&mut self, placement: &str) {
        self.terminal_placement = placement.to_owned();
        self.terminal_in_panel = placement == "panel";
    }

    pub(crate) fn apply_workspace_theme(&mut self, id: String) {
        let color: gpui::Hsla = gpui::rgb(super::palette(&id, false).surface).into();
        self.theme_hue = (color.h * 360.).round() as u32;
        self.theme_saturation = (color.s * 100.).round() as u32;
        self.theme_dark_lightness = ((color.l * 100.).round() as u32).min(30);
        self.workspace_theme = id;
    }

    /// The color settings a project copies when it gets its own theme.
    pub(crate) fn project_theme(&self) -> monocode_core::project_appearance::ProjectTheme {
        monocode_core::project_appearance::ProjectTheme {
            workspace_theme: self.workspace_theme.clone(),
            accent_color: self.accent_color.clone(),
            hue: self.theme_hue,
            saturation: self.theme_saturation,
            dark_lightness: self.theme_dark_lightness,
        }
    }

    pub(crate) fn wayland_decorations(&self, compositor: &str) -> Option<gpui::WindowDecorations> {
        (compositor == "Wayland").then_some(if self.wayland_window_decorations {
            gpui::WindowDecorations::Server
        } else {
            gpui::WindowDecorations::Client
        })
    }

    /// SettingsView's Restore defaults: the Appearance page only.
    pub(crate) fn restore_appearance(&mut self) {
        let d = Self::default();
        self.theme = d.theme;
        self.wayland_window_decorations = d.wayland_window_decorations;
        self.accent_color = d.accent_color;
        self.workspace_theme = d.workspace_theme;
        self.sidebar_opacity = d.sidebar_opacity;
        self.sidebar_blur = d.sidebar_blur;
        self.theme_hue = d.theme_hue;
        self.theme_saturation = d.theme_saturation;
        self.theme_dark_lightness = d.theme_dark_lightness;
        self.body_glass = d.body_glass;
        self.inset_glass = d.inset_glass;
        self.show_excluded_files = d.show_excluded_files;
        self.chat_background_empty = d.chat_background_empty;
        self.chat_background_session = d.chat_background_session;
        self.chat_background_scope = d.chat_background_scope;
        self.diff_palette = d.diff_palette;
        self.chat_background_effect = d.chat_background_effect;
        self.chat_background = d.chat_background;
        self.ui_scale = d.ui_scale;
        self.collapsed_project_rail = d.collapsed_project_rail;
        self.project_rail_view = d.project_rail_view;
        self.project_apps = d.project_apps;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProviderDefaults {
    pub provider: Option<String>,
    pub models: HashMap<String, String>,
    pub hidden: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelChoice {
    pub provider: String,
    pub model: Option<String>,
}

impl gpui::Global for Preferences {}
impl Preferences {
    /// The background model picked in Settings, if any.
    pub(crate) fn background_text_model(&self) -> Option<(Kind, Option<String>)> {
        let choice = self.background_model.as_ref()?;
        Some((Kind::parse(&choice.provider)?, choice.model.clone()))
    }
    pub(crate) fn hidden_for(&self, project: &str) -> Vec<String> {
        let mut hidden = self.hidden_providers.clone();
        if let Some(settings) = self.project_providers.get(project) {
            hidden.extend(settings.hidden.clone());
        }
        hidden.sort();
        hidden.dedup();
        hidden
    }
}

#[cfg(test)]
mod decoration_tests {
    use super::Preferences;

    #[test]
    fn decoration_requests_are_scoped_to_the_wayland_backend() {
        for enabled in [true, false] {
            let prefs = Preferences {
                wayland_window_decorations: enabled,
                ..Default::default()
            };
            for compositor in ["X11", "Headless", ""] {
                assert_eq!(prefs.wayland_decorations(compositor), None);
            }
            assert_eq!(
                prefs.wayland_decorations("Wayland"),
                Some(if enabled {
                    gpui::WindowDecorations::Server
                } else {
                    gpui::WindowDecorations::Client
                })
            );
        }
    }

    #[test]
    fn decorations_default_persist_and_restore() {
        let mut prefs: Preferences = serde_json::from_str("{}").unwrap();
        assert!(prefs.wayland_window_decorations);
        prefs.wayland_window_decorations = false;
        let saved = serde_json::to_string(&prefs).unwrap();
        let mut restored: Preferences = serde_json::from_str(&saved).unwrap();
        assert!(!restored.wayland_window_decorations);
        restored.restore_appearance();
        assert!(restored.wayland_window_decorations);
    }

    #[test]
    fn saved_preferences_are_offered_the_api_app_once() {
        let mut saved: Preferences =
            serde_json::from_str(r#"{"project_apps":["inbox","notes","database"]}"#).unwrap();
        assert!(!saved.project_app_shown("api"));
        saved.migrate();
        assert_eq!(saved.project_apps, ["inbox", "notes", "database", "api"]);
        // Switching it off afterwards sticks across the next load.
        saved.set_project_app_shown("api", false);
        let mut again: Preferences =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        again.migrate();
        assert!(!again.project_app_shown("api"));
        // New preferences already have it and are not touched.
        let mut fresh = Preferences::default();
        fresh.migrate();
        assert!(fresh.project_app_shown("api"));
    }
}
