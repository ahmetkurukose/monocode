//! Port of src/features/settings/model/settings.ts: the pages, their rail
//! groups and the index Settings search jumps through.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Section {
    #[default]
    General,
    Connections,
    Integrations,
    Appearance,
    Terminal,
    Keybindings,
    Browser,
    Chat,
    Providers,
    Mcp,
    Skills,
    Monos,
    Inbox,
    Archive,
    Worktrees,
}

pub(crate) const GROUPS: [(&str, &[Section]); 3] = [
    (
        "App",
        &[
            Section::General,
            Section::Connections,
            Section::Integrations,
            Section::Appearance,
            Section::Terminal,
            Section::Keybindings,
            Section::Browser,
        ],
    ),
    (
        "Agents",
        &[
            Section::Chat,
            Section::Providers,
            Section::Mcp,
            Section::Skills,
            Section::Monos,
        ],
    ),
    (
        "Workspace",
        &[Section::Inbox, Section::Archive, Section::Worktrees],
    ),
];

impl Section {
    pub(crate) const ALL: [Section; 15] = [
        Section::General,
        Section::Connections,
        Section::Integrations,
        Section::Appearance,
        Section::Terminal,
        Section::Keybindings,
        Section::Browser,
        Section::Chat,
        Section::Providers,
        Section::Mcp,
        Section::Skills,
        Section::Monos,
        Section::Inbox,
        Section::Archive,
        Section::Worktrees,
    ];
    pub(crate) fn id(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Connections => "connections",
            Section::Integrations => "integrations",
            Section::Appearance => "appearance",
            Section::Terminal => "terminal",
            Section::Keybindings => "keybindings",
            Section::Browser => "browser",
            Section::Chat => "chat",
            Section::Providers => "providers",
            Section::Mcp => "mcp",
            Section::Skills => "skills",
            Section::Monos => "monos",
            Section::Inbox => "inbox",
            Section::Archive => "archive",
            Section::Worktrees => "worktrees",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Connections => "Connections",
            Section::Integrations => "Integrations",
            Section::Appearance => "Appearance",
            Section::Terminal => "Terminal",
            Section::Keybindings => "Keybindings",
            Section::Browser => "Browser",
            Section::Chat => "Chat",
            Section::Providers => "Providers",
            Section::Mcp => "MCP",
            Section::Skills => "Skills",
            Section::Monos => "Monos",
            Section::Inbox => "Inbox",
            Section::Archive => "Archive",
            Section::Worktrees => "Worktrees",
        }
    }
    pub(crate) fn description(self) -> &'static str {
        match self {
            Section::General => {
                "The build you are running, how MonoCode reaches you, and the panels it shows."
            }
            Section::Connections => "Connect your machines and run agents remotely through SSH.",
            Section::Integrations => {
                "Work trackers and code hosts the Inbox reads, and the board each project works from."
            }
            Section::Appearance => {
                "Theme, tint, translucency, workspace layout, and conversation backgrounds."
            }
            Section::Terminal => {
                "The size of the text in terminals, and what selecting in them does."
            }
            Section::Keybindings => {
                "Every shortcut the workspace handles, from the app menu and the key handler."
            }
            Section::Browser => {
                "Links, power and memory, site permissions, importing, saved passwords, and Chrome Web Store extensions."
            }
            Section::Chat => {
                "How transcripts read, what the composer does with a follow-up, how files save, and how diffs open."
            }
            Section::Providers => {
                "Provider accounts, agent CLIs MonoCode can drive, and the model new sessions start with."
            }
            Section::Mcp => "Find MCP servers across providers and manage their connections.",
            Section::Skills => {
                "Discover and manage file skills from project, personal, and harness folders."
            }
            Section::Monos => "The resident agent beside your tabs, and which projects have one.",
            Section::Inbox => "Notification preferences for each project's Inbox.",
            Section::Archive => "Projects and conversations you have archived.",
            Section::Worktrees => "Manage additional worktrees for each project.",
        }
    }
    fn keywords(self) -> &'static str {
        match self {
            Section::General => "version update sounds notifications notes rail",
            Section::Connections => "ssh remote host machine server environment always on",
            Section::Integrations => {
                "monday tracker board item github gitlab linear jira atlassian azure devops connect token integration"
            }
            Section::Appearance => {
                "theme dark light color accent glass blur zoom scale wallpaper rail sidebar"
            }
            Section::Terminal => "shell font size text zoom select copy clipboard selection iterm",
            Section::Keybindings => "shortcut hotkey keyboard binding",
            Section::Browser => {
                "web chromium links cdp battery memory site permissions import password login sign in credentials vault keychain touch id autofill saved never extension add-on plugin chrome web store proton pass install update"
            }
            Section::Chat => {
                "transcript composer prompt message diff review layout format save editor"
            }
            Section::Providers => {
                "account sign in login model harness claude codex gemini cli default hooks"
            }
            Section::Mcp => {
                "tools servers connections oauth authenticate login claude codex cursor opencode"
            }
            Section::Skills => "skill instructions prompt",
            Section::Monos => "mono resident agent mascot claim project title bar",
            Section::Inbox => "notifications mute resume sounds banners reminders",
            Section::Archive => "archived restore delete hidden",
            Section::Worktrees => "git branch worktree working copy project create delete",
        }
    }
    /// SettingsRail.tsx's SECTION_ICONS.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Section::General => "sliders",
            Section::Connections => "internet",
            Section::Integrations => "kanban",
            Section::Appearance => "palette",
            Section::Terminal => "terminal",
            Section::Keybindings => "keyboard",
            Section::Browser => "app-window",
            Section::Chat => "message-square",
            Section::Providers => "bot",
            Section::Mcp => "globe",
            Section::Skills => "sparkles",
            Section::Monos => "ghost",
            Section::Inbox => "inbox-hi",
            Section::Archive => "archive",
            Section::Worktrees => "folder-tree",
        }
    }
}

/// SETTINGS_INDEX: (id, section, label, keywords).
pub(crate) const INDEX: &[(&str, Section, &str, &str)] = &[
    (
        "terminal-font-size",
        Section::Terminal,
        "Terminal font size",
        "text zoom smaller larger shell grid",
    ),
    (
        "terminal-scrollback",
        Section::Terminal,
        "Terminal scrollback",
        "history lines buffer log output memory",
    ),
    (
        "terminal-copy-on-select",
        Section::Terminal,
        "Copy on select",
        "selection clipboard copy highlight iterm",
    ),
    (
        "remote-machines",
        Section::Connections,
        "Your machines",
        "ssh remote connect host server environment",
    ),
    (
        "mobile-access",
        Section::Connections,
        "Your phones",
        "phone iphone android mobile app pair qr code device revoke remove testflight",
    ),
    (
        "mobile-remote",
        Section::Connections,
        "Connect away from home",
        "router port upnp nat remote address public hostname vpn wifi",
    ),
    (
        "mcp-servers",
        Section::Mcp,
        "MCP servers",
        "claude tools connections oauth authenticate login add remove",
    ),
    (
        "monos-enabled",
        Section::Monos,
        "Show monos",
        "mono agent rail hide",
    ),
    (
        "rail-monos-pinned",
        Section::Monos,
        "Pin monos to the icon rail",
        "mono rail compact collapsed icons top divider project picker",
    ),
    (
        "mono-list",
        Section::Monos,
        "Your monos",
        "mono reset soul name projects sessions sidebar visibility hidden show",
    ),
    (
        "project-worktrees",
        Section::Worktrees,
        "Project worktrees",
        "git branch working copy create delete manage",
    ),
    (
        "worktree-location",
        Section::Worktrees,
        "Worktree location",
        "folder root directory workspaces where path",
    ),
    (
        "worktree-branch-prefix",
        Section::Worktrees,
        "Branch prefix",
        "git user name custom prefix branch naming",
    ),
    (
        "worktree-default-base",
        Section::Worktrees,
        "Default base",
        "start from origin main branch base worktree",
    ),
    (
        "worktree-show-external",
        Section::Worktrees,
        "Show other worktrees",
        "external claude worktrees outside rail list",
    ),
    (
        "update",
        Section::General,
        "Version",
        "update upgrade release what's new build changelog",
    ),
    (
        "sounds",
        Section::General,
        "Sounds",
        "audio cue chime mute volume",
    ),
    (
        "notifications",
        Section::General,
        "Notifications",
        "notify alert toast permission reminder background",
    ),
    (
        "notes",
        Section::General,
        "Notes",
        "notebook markdown rail scratchpad",
    ),
    (
        "quick-composer",
        Section::General,
        "Quick composer",
        "spotlight global shortcut hotkey floating prompt anywhere",
    ),
    (
        "working-agents",
        Section::General,
        "Working agents",
        "live running sessions rail card",
    ),
    (
        "action-opens-terminal",
        Section::General,
        "Show terminal for actions",
        "actions commands run script dock background",
    ),
    (
        "terminal-placement",
        Section::General,
        "Terminal",
        "terminal placement dock bottom side right panel title bar tab shell focused layout",
    ),
    (
        "browser-all-links",
        Section::Browser,
        "Open all links in Monocode",
        "browser web links localhost dev server system default chromium shift click",
    ),
    (
        "browser-external-cdp",
        Section::Browser,
        "Allow external CDP connections",
        "browser chromium devtools protocol playwright dev-browser endpoint copy automation",
    ),
    (
        "browser-battery-saver",
        Section::Browser,
        "Browser battery saver",
        "browser chromium energy power battery low frame rate fps throttle freeze background tabs",
    ),
    (
        "browser-memory-saver",
        Section::Browser,
        "Browser memory saver",
        "browser chromium memory ram discard sleep inactive background tabs unload",
    ),
    (
        "browser-offer-passwords",
        Section::Browser,
        "Offer to save passwords",
        "browser password manager save sign in login autofill suggest 1password",
    ),
    (
        "browser-site-permissions",
        Section::Browser,
        "Site permissions",
        "browser website camera microphone location geolocation notifications clipboard midi allow block reset privacy",
    ),
    (
        "browser-import",
        Section::Browser,
        "Import from another browser",
        "browser chrome arc brave import cookies passwords logins sign in migrate bring",
    ),
    (
        "browser-saved-passwords",
        Section::Browser,
        "Saved passwords",
        "browser password login credentials vault keychain touch id reveal copy edit delete",
    ),
    (
        "browser-never-saved",
        Section::Browser,
        "Never saved",
        "browser password sites never save blocked",
    ),
    (
        "browser-extensions-add",
        Section::Browser,
        "Add extension",
        "browser extension add-on plugin chrome web store proton pass password manager install",
    ),
    (
        "browser-extensions-installed",
        Section::Browser,
        "Installed extensions",
        "browser extension add-on plugin enable disable update remove",
    ),
    (
        "file-word-wrap",
        Section::General,
        "Word wrap",
        "files editor long lines horizontal scroll wrapping",
    ),
    (
        "close-session-tab",
        Section::General,
        "Closing a session tab",
        "close tab archive keep sidebar ask confirm session remove",
    ),
    (
        "tab-animations",
        Section::General,
        "Tab animations",
        "motion open close resize transition",
    ),
    (
        "theme",
        Section::Appearance,
        "Theme",
        "dark light system appearance mode",
    ),
    (
        "wayland-window-decorations",
        Section::Appearance,
        "Wayland window decorations",
        "wayland linux compositor title bar titlebar border decorations minimize maximize close",
    ),
    (
        "workspace-theme",
        Section::Appearance,
        "Workspace theme",
        "workspace theme color colour text accent syntax scheme preset harbor moss ember orchid",
    ),
    (
        "accent-color",
        Section::Appearance,
        "Accent color",
        "highlight bubble send button tint",
    ),
    (
        "diff-colors",
        Section::Appearance,
        "Diff colors",
        "colorblind color blind accessibility added removed red green blue orange high contrast changes",
    ),
    ("hue", Section::Appearance, "Hue", "tint color chrome"),
    (
        "saturation",
        Section::Appearance,
        "Saturation",
        "tint color neutral grey gray",
    ),
    (
        "dark-lightness",
        Section::Appearance,
        "Dark-mode lightness",
        "black brightness contrast background",
    ),
    (
        "sidebar-opacity",
        Section::Appearance,
        "Sidebar opacity",
        "glass translucent transparency vibrancy",
    ),
    (
        "main-pane-glass",
        Section::Appearance,
        "Main pane glass",
        "translucent transparency body window",
    ),
    (
        "inset-layout",
        Section::Appearance,
        "Layout",
        "classic focused new preview sidebar navigation tabs inset workspace card rounded floating linear frame",
    ),
    (
        "interface-scale",
        Section::Appearance,
        "Interface scale",
        "zoom font size bigger smaller ui",
    ),
    (
        "collapsed-project-rail",
        Section::Appearance,
        "Collapsed project rail",
        "sidebar compact icons hidden navigation layout",
    ),
    (
        "show-excluded-files",
        Section::Appearance,
        "Show excluded files",
        "explorer gitignore ignored hidden files tree",
    ),
    (
        "chat-background",
        Section::Appearance,
        "Chat background",
        "wallpaper image picture opacity backdrop",
    ),
    (
        "transcript-layout",
        Section::Chat,
        "Transcript layout",
        "full width chat bubble message",
    ),
    (
        "anchor-prompts",
        Section::Chat,
        "Anchor prompts to top",
        "scroll position sticky message",
    ),
    (
        "paced-reveal",
        Section::Chat,
        "Smooth text reveal",
        "streaming pacing immediate latency performance animation",
    ),
    (
        "collapse-tool-groups",
        Section::Chat,
        "Collapse tool groups",
        "tool calls commands steps expand closed mobile compact activity",
    ),
    (
        "fold-finished-work",
        Section::Chat,
        "Fold finished work",
        "worked for collapse tool calls reply inline expanded history turns",
    ),
    (
        "follow-up",
        Section::Chat,
        "Follow-up behavior",
        "queue steer interrupt send while running",
    ),
    (
        "model-controls",
        Section::Chat,
        "Model controls",
        "effort thinking reasoning fast service tier model picker composer",
    ),
    (
        "composer-autocorrect",
        Section::Chat,
        "Autocorrect",
        "spelling spell check autocorrect typo macos composer",
    ),
    (
        "composer-mascot",
        Section::Chat,
        "Composer mascot",
        "runner animation coin fun",
    ),
    (
        "format-on-save",
        Section::Chat,
        "Format on save",
        "prettier quotes editor save format",
    ),
    (
        "diff-view",
        Section::Chat,
        "Diff view",
        "unified editor review changes working tree",
    ),
    (
        "empty-session",
        Section::Chat,
        "Empty session",
        "ink dither trails ripple games pacman snake arcade fun",
    ),
    (
        "agent-clis",
        Section::Providers,
        "Agent CLIs",
        "codex opencode cursor grok pi omp fx hermes antigravity devin binary path",
    ),
    (
        "background-model",
        Section::Providers,
        "Background model",
        "title generation commit message branch worktree name pull request haiku fast small",
    ),
    (
        "provider-accounts",
        Section::Providers,
        "Provider accounts",
        "account sign in login rename remove delete credentials profile usage limit quota exhausted",
    ),
    (
        "show-remaining-usage",
        Section::Providers,
        "Show remaining usage",
        "usage limit meter bar left used quota percent",
    ),
    (
        "mask-emails",
        Section::Providers,
        "Mask account emails",
        "email privacy blur hide screenshot account",
    ),
    (
        "claude-hooks",
        Section::Providers,
        "Claude Code hooks",
        "pretooluse settings.json block command notification",
    ),
    (
        "claude-chrome",
        Section::Providers,
        "Claude in Chrome",
        "browser extension chrome integration web automation",
    ),
    (
        "claude-entrypoint",
        Section::Providers,
        "Identify as Monocode to Claude",
        "entrypoint sdk-cli label usage limit 5-hour window rate experimental",
    ),
    (
        "idle-agents",
        Section::Providers,
        "Keep idle agents ready",
        "memory ram cpu performance process warm park background",
    ),
    (
        "project-notifications",
        Section::Inbox,
        "Project notifications",
        "mute resume sounds banners reminders categories",
    ),
    (
        "monday",
        Section::Integrations,
        "monday.com",
        "monday board item tracker api token personal connect",
    ),
    (
        "monday-boards",
        Section::Integrations,
        "Boards in Inbox",
        "monday follow boards list refresh",
    ),
    (
        "monday-project",
        Section::Integrations,
        "Project board",
        "monday bind board status column people column map write back pull request integrations.json team",
    ),
    (
        "github",
        Section::Integrations,
        "GitHub",
        "gh cli connect pull request sign in",
    ),
    (
        "gitlab",
        Section::Integrations,
        "GitLab",
        "token self-managed merge request connect",
    ),
    (
        "azuredevops",
        Section::Integrations,
        "ADO",
        "azure devops boards repos pull request pat organization connect",
    ),
    (
        "jira",
        Section::Integrations,
        "Jira",
        "atlassian cloud site email api token issues projects connect",
    ),
    (
        "linear",
        Section::Integrations,
        "Linear",
        "api key issues teams connect",
    ),
    (
        "show-archived",
        Section::Archive,
        "Show archived in the sidebar",
        "hidden conversations list",
    ),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SearchResult {
    pub section: Section,
    pub setting: Option<&'static str>,
    pub label: &'static str,
}

fn score(needle: &str, label: &str, keywords: &str) -> Option<f32> {
    let lower = label.to_lowercase();
    if lower.starts_with(needle) {
        Some(0.)
    } else if lower.contains(needle) {
        Some(1.)
    } else if keywords.to_lowercase().contains(needle) {
        Some(2.)
    } else {
        None
    }
}

/// searchSettings: individual settings first, then whole sections.
pub(crate) fn search(query: &str, compositor: &str) -> Vec<SearchResult> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f32, SearchResult)> = Vec::new();
    for &(id, section, label, keywords) in INDEX {
        if id == "wayland-window-decorations" && compositor != "Wayland" {
            continue;
        }
        if let Some(s) = score(&needle, label, keywords) {
            scored.push((
                s,
                SearchResult {
                    section,
                    setting: Some(id),
                    label,
                },
            ));
        }
    }
    for section in Section::ALL {
        let keywords = format!("{} {}", section.description(), section.keywords());
        if let Some(s) = score(&needle, section.label(), &keywords) {
            scored.push((
                s + 0.5,
                SearchResult {
                    section,
                    setting: None,
                    label: section.label(),
                },
            ));
        }
    }
    scored.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase()))
    });
    scored.into_iter().take(8).map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wayland_decorations_are_searchable_only_on_wayland() {
        for query in ["Wayland", "window decorations", "compositor", "title bar"] {
            let results = search(query, "Wayland");
            let result = results
                .iter()
                .find(|r| r.setting == Some("wayland-window-decorations"))
                .unwrap();
            assert_eq!(result.section, Section::Appearance);
            for compositor in ["X11", "Headless", ""] {
                assert!(
                    search(query, compositor)
                        .iter()
                        .all(|r| r.setting != Some("wayland-window-decorations"))
                );
            }
        }
    }

    #[test]
    fn rows_win_their_own_name_and_shared_appearance_rows_are_searchable() {
        let results = search("theme", "Wayland");
        assert_eq!(results[0].setting, Some("theme"));
        assert_eq!(results[1].setting, Some("workspace-theme"));
        assert!(
            search("hue", "Wayland")
                .iter()
                .any(|r| r.setting == Some("hue"))
        );
        assert!(
            search("workspace theme", "Wayland")
                .iter()
                .any(|r| r.setting == Some("workspace-theme"))
        );
        let pages = search("archive", "Wayland");
        assert!(
            pages
                .iter()
                .any(|r| r.section == Section::Archive && r.setting.is_none())
        );
    }
}
