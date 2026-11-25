//! The Command Bar's rows (docs/native/modes.md §Command Bar): what is typed
//! becomes, in order, a calculator's answer, the commands it names, the
//! address or search it means, the open tabs it matches and the history's
//! pages. The SOCode model resolves addresses and searches
//! (`socode_browser::omnibox`); the shell runs the row picked.

use monocode_core::browser::history::Entry;
use socode_browser::omnibox::{CommandBarSearchPreferences, Resolved, calculator};

/// Rows at most.
const MAX_ROWS: usize = 20;
/// Commands at most.
const MAX_COMMANDS: usize = 5;
/// Open tabs at most: with nothing typed, and with something typed.
const MAX_RECENT_TABS: usize = 5;
const MAX_TABS: usize = 6;
/// History pages at most.
const MAX_PAGES: usize = 4;

/// What the Command Bar can do besides opening pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    NewTab,
    NewFolder,
    RenameTab,
    ResetTabName,
    DuplicateTab,
    TogglePin,
    ToggleFavorite,
    ToggleMute,
    CloseTab,
    CloseTabsBelow,
    CloseOtherTabs,
    ClearToday,
    RestoreTab,
    Undo,
    Redo,
    CopyUrl,
    CopyUrlAsMarkdown,
    Reload,
    ForceReload,
    StopLoading,
    GoBack,
    GoForward,
    NextTab,
    PreviousTab,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    FindInPage,
    /// Developer Tools (⌥⌘I) and SOCode's other DevTools commands
    /// (browser/devtools.rs).
    DevTools,
    InspectElements,
    JavaScriptConsole,
    NetworkInspector,
    ViewSource,
    /// DevTools move to this side.
    DockDevTools(crate::browser::DockSide),
    PrintPage,
    ViewHistory,
    ReplacePinnedUrl,
    ResetPinnedTab,
    CollapseAllFolders,
    ExpandAllFolders,
    ToggleSidebar,
    SendToChat,
    /// Capture (⇧⌘2): an area of the page (docs/native/modes.md §Capture).
    Capture,
    /// The Site Control Center under the address field.
    SiteControls,
    ClearCookiesAndRefresh,
    ClearCacheAndRefresh,
    DevMode,
    /// Settings → Browser, at the link routing rules.
    LinkRouting,
}

/// The tab on screen, as the commands need to know it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ActiveTab {
    /// One of the user's own (not a project's page).
    pub own: bool,
    pub pinned: bool,
    pub favorite: bool,
    /// Away from its pinned address.
    pub away: bool,
    pub named: bool,
    pub muted: bool,
    pub loading: bool,
    /// It has Today tabs under it.
    pub below: bool,
    /// Other Today tabs.
    pub others: bool,
}

/// What the commands depend on.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub active: Option<ActiveTab>,
    pub today: bool,
    pub archive: bool,
    pub undo: bool,
    pub redo: bool,
    pub open_folders: bool,
    pub closed_folders: bool,
    pub sidebar_shown: bool,
}

/// An open tab the bar may switch to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TabCandidate {
    pub pane: i64,
    pub title: String,
    pub url: String,
    /// Where it sits: "Favorites", "Pinned", "Today" or a project's name.
    pub place: String,
    /// Shown last at this time (later first with nothing typed).
    pub last_active: std::time::SystemTime,
}

/// One row of the bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Row {
    /// The answer of a sum typed; Enter copies it.
    Calculator(String),
    Command {
        command: Command,
        label: String,
    },
    /// The address typed, or a search for what was typed.
    Open {
        url: String,
        label: String,
        search: bool,
    },
    Tab(TabCandidate),
    /// A page of the history.
    Page {
        url: String,
        title: String,
    },
    /// The search engine's completion of what was typed (SOCode's remote
    /// suggestions); a search for it.
    Remote {
        query: String,
        url: String,
    },
}

impl Row {
    /// The address the row shows in the field while it is chosen.
    pub(crate) fn preview(&self) -> Option<&str> {
        match self {
            Row::Open {
                url, search: false, ..
            }
            | Row::Page { url, .. } => Some(url),
            Row::Tab(tab) => Some(&tab.url),
            _ => None,
        }
    }
}

impl Command {
    /// The commands offered, with their label and the names that find them,
    /// as far as `facts` allow.
    fn offered(facts: &Facts) -> Vec<(Command, &'static str, &'static str)> {
        let tab = facts.active.as_ref();
        let own = tab.is_some_and(|t| t.own);
        let pinned = tab.is_some_and(|t| t.pinned);
        let favorite = tab.is_some_and(|t| t.favorite);
        let muted = tab.is_some_and(|t| t.muted);
        let any = tab.is_some();
        let list: [(Command, &str, &str, bool); 43] = [
            (Command::NewTab, "New Tab", "new tab|open tab", true),
            (
                Command::NewFolder,
                "New Folder…",
                "new folder|create folder|add folder",
                true,
            ),
            (
                Command::RenameTab,
                "Rename Current Tab…",
                "rename current tab|rename tab",
                own,
            ),
            (
                Command::ResetTabName,
                "Reset Tab Name",
                "reset tab name|reset name",
                tab.is_some_and(|t| t.named),
            ),
            (
                Command::DuplicateTab,
                "Duplicate Current Tab",
                "duplicate current tab|duplicate tab",
                any,
            ),
            (
                Command::TogglePin,
                if pinned { "Unpin Tab" } else { "Pin Tab" },
                if pinned {
                    "unpin tab|move to today"
                } else {
                    "pin tab|move to pinned"
                },
                own && !favorite,
            ),
            (
                Command::ToggleFavorite,
                if favorite {
                    "Unfavorite Tab"
                } else {
                    "Favorite Tab"
                },
                if favorite {
                    "unfavorite tab|remove from favorites"
                } else {
                    "favorite tab|add to favorites"
                },
                own,
            ),
            (
                Command::ToggleMute,
                if muted { "Unmute Tab" } else { "Mute Tab" },
                if muted {
                    "unmute tab|unmute current tab"
                } else {
                    "mute tab|mute current tab"
                },
                any,
            ),
            (
                Command::CloseTab,
                if pinned || favorite {
                    "Close Tab"
                } else {
                    "Archive Tab"
                },
                "close tab|archive tab",
                any,
            ),
            (
                Command::CloseTabsBelow,
                "Archive Tabs Below",
                "archive tabs below|close tabs below",
                tab.is_some_and(|t| t.below),
            ),
            (
                Command::CloseOtherTabs,
                "Archive Other Tabs",
                "archive other tabs|close other tabs",
                tab.is_some_and(|t| t.others),
            ),
            (
                Command::ClearToday,
                "Clear Today",
                "clear today|clear unpinned tabs|archive today tabs",
                facts.today,
            ),
            (
                Command::RestoreTab,
                "Restore Last Closed Tab",
                "restore last closed tab|reopen closed tab|unarchive tab",
                facts.archive,
            ),
            (Command::Undo, "Undo", "undo", facts.undo),
            (Command::Redo, "Redo", "redo", facts.redo),
            (Command::CopyUrl, "Copy URL", "copy url|copy link", any),
            (
                Command::CopyUrlAsMarkdown,
                "Copy URL as Markdown",
                "copy url as markdown|copy link as markdown",
                any,
            ),
            (
                Command::Reload,
                "Refresh the Page",
                "refresh page|reload tab|reload",
                any,
            ),
            (
                Command::ForceReload,
                "Force Refresh the Page",
                "force refresh page|reload without cache|hard reload",
                any,
            ),
            (
                Command::StopLoading,
                "Stop Loading",
                "stop loading|stop page",
                tab.is_some_and(|t| t.loading),
            ),
            (Command::GoBack, "Go Back", "go back|back", any),
            (Command::GoForward, "Go Forward", "go forward|forward", any),
            (Command::NextTab, "Next Tab", "next tab", any),
            (Command::PreviousTab, "Previous Tab", "previous tab", any),
            (Command::ZoomIn, "Zoom In", "zoom in|increase zoom", any),
            (Command::ZoomOut, "Zoom Out", "zoom out|decrease zoom", any),
            (
                Command::ResetZoom,
                "Zoom to Actual Size",
                "zoom actual size|reset zoom|100%",
                any,
            ),
            (
                Command::FindInPage,
                "Find in Page…",
                "find in page|search page",
                any,
            ),
            (
                Command::DevTools,
                "Developer Tools",
                "developer tools|devtools",
                any,
            ),
            (Command::PrintPage, "Print Page", "print|print page", any),
            (
                Command::ViewHistory,
                "View History",
                "view history|open history|browsing history",
                true,
            ),
            (
                Command::ReplacePinnedUrl,
                "Replace Pinned URL with Current",
                "replace pinned url with current|update pinned url",
                tab.is_some_and(|t| t.away),
            ),
            (
                Command::ResetPinnedTab,
                "Reset Tab to Pinned URL",
                "reset tab to pinned url|back to pinned url|reset pinned tab",
                tab.is_some_and(|t| t.away),
            ),
            (
                Command::CollapseAllFolders,
                "Collapse All Folders",
                "collapse all folders",
                facts.open_folders,
            ),
            (
                Command::ExpandAllFolders,
                "Expand All Folders",
                "expand all folders",
                facts.closed_folders,
            ),
            (
                Command::ToggleSidebar,
                if facts.sidebar_shown {
                    "Hide Sidebar"
                } else {
                    "Show Sidebar"
                },
                "toggle sidebar|hide sidebar|show sidebar|sidebar",
                true,
            ),
            (
                Command::SendToChat,
                "Send to Chat",
                "send to chat|send page to chat|ask agent",
                any,
            ),
            (
                Command::Capture,
                "Capture…",
                "capture|screenshot|capture area|take screenshot",
                any,
            ),
            (
                Command::SiteControls,
                "Site Controls",
                "site controls|site settings|site information|permissions",
                any,
            ),
            (
                Command::ClearCookiesAndRefresh,
                "Clear Cookies and Refresh",
                "clear cookies and refresh|clear cookies|delete cookies",
                any,
            ),
            (
                Command::ClearCacheAndRefresh,
                "Clear Cache and Refresh",
                "clear cache and refresh|clear cache|empty cache",
                any,
            ),
            (
                Command::DevMode,
                "Switch to Dev Mode",
                "switch to dev mode|dev mode|code",
                true,
            ),
            (
                Command::LinkRouting,
                "Link Routing…",
                "link routing|air traffic control|routing rules|open links in",
                true,
            ),
        ];
        (list.into_iter())
            .chain(devtools_commands(any))
            .filter(|(.., available)| *available)
            .map(|(command, label, names, _)| (command, label, names))
            .collect()
    }
}

/// SOCode's DevTools commands besides Developer Tools, for a tab on screen
/// when `any`.
fn devtools_commands(any: bool) -> [(Command, &'static str, &'static str, bool); 8] {
    use crate::browser::DockSide;
    [
        (
            Command::InspectElements,
            "Inspect Elements",
            "inspect elements|element inspector|inspect",
            any,
        ),
        (
            Command::JavaScriptConsole,
            "JavaScript Console",
            "javascript console|console",
            any,
        ),
        (
            Command::NetworkInspector,
            "Network Inspector",
            "network inspector|network",
            any,
        ),
        (
            Command::ViewSource,
            "View Source",
            "view source|page source|show source",
            any,
        ),
        (
            Command::DockDevTools(DockSide::Right),
            "Dock DevTools to Right",
            "dock devtools to right|dock to right|devtools right",
            any,
        ),
        (
            Command::DockDevTools(DockSide::Left),
            "Dock DevTools to Left",
            "dock devtools to left|dock to left|devtools left",
            any,
        ),
        (
            Command::DockDevTools(DockSide::Bottom),
            "Dock DevTools to Bottom",
            "dock devtools to bottom|dock to bottom|devtools bottom",
            any,
        ),
        (
            Command::DockDevTools(DockSide::Undocked),
            "Undock DevTools into Separate Window",
            "undock devtools into separate window|undock devtools|devtools window",
            any,
        ),
    ]
}

/// How well `query` names a command: 0 for its exact name, then the fewer
/// words left over the better; `None` when it does not name it. Every typed
/// word must start the name's word at its place.
pub(crate) fn command_match_score(query: &str, names: &str) -> Option<usize> {
    let words: Vec<_> = query.split_whitespace().collect();
    names
        .split('|')
        .filter_map(|name| {
            let name = name.trim().trim_end_matches('…').to_lowercase();
            if name == query {
                return Some(0);
            }
            let parts: Vec<_> = name.split_whitespace().collect();
            (!words.is_empty()
                && words.len() <= parts.len()
                && words
                    .iter()
                    .zip(&parts)
                    .all(|(word, part)| part.starts_with(word)))
            .then(|| 1 + parts.len() - words.len())
        })
        .min()
}

/// Whether every word of `query` is in one of `values`.
fn matches(query: &str, values: &[&str]) -> bool {
    let values: Vec<String> = values.iter().map(|v| v.to_lowercase()).collect();
    query
        .split_whitespace()
        .all(|word| values.iter().any(|value| value.contains(word)))
}

/// What the user typed, as the bar reads it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Typed<'a> {
    /// The field's text, a domain completed inline included.
    pub text: &'a str,
    /// What was typed before the inline completion, while there is one.
    pub prefix: Option<&'a str>,
}

impl<'a> From<&'a str> for Typed<'a> {
    fn from(text: &'a str) -> Self {
        Typed { text, prefix: None }
    }
}

/// The bar's rows for `typed`. `tabs` are the open tabs but the one on
/// screen, in the sidebar's order; `pages` the history's matches, best
/// first; `remote` the search engine's completions. `address` turns typed
/// text into an address the app opens, when the model's resolver takes it
/// for a search (`localhost:3000`). A domain completed inline opens first;
/// a search for what was typed follows it, as SOCode's.
pub(crate) fn rows<'a>(
    typed: impl Into<Typed<'a>>,
    facts: &Facts,
    mut tabs: Vec<TabCandidate>,
    pages: Vec<Entry>,
    remote: &[String],
    search: &CommandBarSearchPreferences,
    address: impl Fn(&str) -> Option<String>,
) -> Vec<Row> {
    let Typed {
        text: typed,
        prefix,
    } = typed.into();
    let query = prefix.unwrap_or(typed).trim().to_lowercase();
    let mut rows = Vec::new();
    if let Some(answer) = calculator::answer(typed) {
        rows.push(Row::Calculator(answer));
    }
    if !query.is_empty() {
        let mut commands: Vec<(usize, Row)> = (Command::offered(facts).into_iter())
            .filter_map(|(command, label, names)| {
                let score = (command_match_score(&query, label).into_iter())
                    .chain(command_match_score(&query, names))
                    .min()?;
                Some((
                    score,
                    Row::Command {
                        command,
                        label: label.to_string(),
                    },
                ))
            })
            .collect();
        commands.sort_by_key(|(score, _)| *score);
        rows.extend(commands.into_iter().take(MAX_COMMANDS).map(|(_, row)| row));
        match search.resolve(typed) {
            Some(Resolved::Url(url)) => rows.push(Row::Open {
                label: url.clone(),
                url,
                search: false,
            }),
            Some(Resolved::Search { query: words, url }) => {
                // The app's own reading first: a local address is no search.
                match address(typed.trim()).filter(|a| !a.contains(&url) && looks_local(a)) {
                    Some(local) => rows.push(Row::Open {
                        label: local.clone(),
                        url: local,
                        search: false,
                    }),
                    None => rows.push(Row::Open {
                        label: format!("{} — Search with {}", words, search.default_name()),
                        url,
                        search: true,
                    }),
                }
            }
            None => {}
        }
        if let Some(prefix) = prefix.filter(|p| !p.trim().is_empty()) {
            let url = search.search(prefix.trim()).url().to_string();
            rows.push(Row::Open {
                label: format!("{} — Search with {}", prefix.trim(), search.default_name()),
                url,
                search: true,
            });
        }
    }
    // Open tabs: the latest shown with nothing typed, else those matching.
    if query.is_empty() {
        tabs.sort_by_key(|tab| std::cmp::Reverse(tab.last_active));
        rows.extend(tabs.into_iter().take(MAX_RECENT_TABS).map(Row::Tab));
    } else {
        let open: Vec<String> = tabs.iter().map(|t| t.url.clone()).collect();
        rows.extend(
            (tabs.into_iter())
                .filter(|tab| matches(&query, &[&tab.title, &tab.url]))
                .take(MAX_TABS)
                .map(Row::Tab),
        );
        // History pages not open already.
        rows.extend(
            (pages.into_iter())
                .filter(|page| !open.contains(&page.url))
                .filter(|page| matches(&query, &[&page.title, &page.url]))
                .take(MAX_PAGES)
                .map(|page| Row::Page {
                    url: page.url,
                    title: page.title,
                }),
        );
    }
    // The search engine's completions last, as SOCode's.
    if !query.is_empty() {
        rows.extend(remote.iter().map(|query| Row::Remote {
            url: search.search(query).url().to_string(),
            query: query.clone(),
        }));
    }
    rows.truncate(MAX_ROWS);
    rows
}

/// A loopback or local-network address, which no search engine knows.
fn looks_local(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or(rest);
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.starts_with("127.")
        || host.starts_with("192.168.")
        || host.starts_with("10.")
        || host == "0.0.0.0"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(pane: i64, title: &str, url: &str, secs: u64) -> TabCandidate {
        TabCandidate {
            pane,
            title: title.into(),
            url: url.into(),
            place: "Today".into(),
            last_active: std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs),
        }
    }

    fn entry(url: &str, title: &str) -> Entry {
        Entry {
            url: url.into(),
            title: title.into(),
            favicon: None,
            project: String::new(),
            visit_count: 1,
            typed_count: 0,
            last_visit: 0,
        }
    }

    fn facts() -> Facts {
        Facts {
            active: Some(ActiveTab {
                own: true,
                others: true,
                ..Default::default()
            }),
            today: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_devtools_commands_are_socodes() {
        let labels = |typed: &str| -> Vec<String> {
            rows(
                typed,
                &facts(),
                vec![],
                vec![],
                &[],
                &CommandBarSearchPreferences::default(),
                |_| None,
            )
            .into_iter()
            .filter_map(|row| match row {
                Row::Command { label, .. } => Some(label),
                _ => None,
            })
            .collect()
        };
        for (typed, label) in [
            ("devtools", "Developer Tools"),
            ("developer tools", "Developer Tools"),
            ("inspect", "Inspect Elements"),
            ("console", "JavaScript Console"),
            ("network", "Network Inspector"),
            ("view source", "View Source"),
            ("dock to left", "Dock DevTools to Left"),
            ("undock", "Undock DevTools into Separate Window"),
        ] {
            assert_eq!(
                labels(typed).first().map(String::as_str),
                Some(label),
                "{typed}"
            );
        }
        // Without a tab on screen there is nothing to inspect.
        let none = Facts::default();
        let offered = Command::offered(&none);
        assert!(!offered.iter().any(|(command, ..)| matches!(
            command,
            Command::DevTools | Command::InspectElements | Command::DockDevTools(_)
        )));
    }

    #[test]
    fn exact_command_names_win_over_prefixes() {
        assert_eq!(
            command_match_score("pin tab", "pin tab|move to pinned"),
            Some(0)
        );
        assert_eq!(command_match_score("pin", "pin tab"), Some(2));
        assert_eq!(command_match_score("cl to", "clear today"), Some(1));
        assert_eq!(command_match_score("today", "clear today"), None);
    }

    #[test]
    fn typed_text_lists_commands_then_the_search_then_tabs_and_pages() {
        let search = CommandBarSearchPreferences::default();
        let tabs = vec![
            tab(-1, "Pinball news", "https://pin.test/", 1),
            tab(-2, "Other", "https://other.test/", 2),
        ];
        let pages = vec![
            entry("https://pin.test/", "Pinball news"),
            entry("https://pinterest.com/", "Pinterest"),
        ];
        let rows = rows("pin", &facts(), tabs, pages, &[], &search, |_| None);
        assert!(
            matches!(
                &rows[0],
                Row::Command {
                    command: Command::TogglePin,
                    ..
                }
            ),
            "{rows:?}"
        );
        let open = rows
            .iter()
            .position(|r| matches!(r, Row::Open { search: true, .. }));
        let tab = rows
            .iter()
            .position(|r| matches!(r, Row::Tab(t) if t.pane == -1));
        let page = rows
            .iter()
            .position(|r| matches!(r, Row::Page { url, .. } if url.contains("pinterest")));
        assert!(open < tab && tab < page, "{rows:?}");
        // An open page is not offered again from the history.
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Page { url, .. } if url == "https://pin.test/"))
                .count(),
            0
        );
    }

    #[test]
    fn nothing_typed_lists_the_tabs_shown_last() {
        let search = CommandBarSearchPreferences::default();
        let tabs = (0..8)
            .map(|i| tab(-i, "T", "https://t.test/", i as u64))
            .collect();
        let rows = rows("", &facts(), tabs, Vec::new(), &[], &search, |_| None);
        assert_eq!(rows.len(), MAX_RECENT_TABS);
        assert!(matches!(&rows[0], Row::Tab(t) if t.pane == -7));
    }

    #[test]
    fn addresses_sums_and_local_servers_resolve() {
        let search = CommandBarSearchPreferences::default();
        let rows1 = rows(
            "example.com",
            &Facts::default(),
            Vec::new(),
            Vec::new(),
            &[],
            &search,
            |_| None,
        );
        assert!(
            matches!(&rows1[0], Row::Open { url, search: false, .. } if url.contains("example.com")),
            "{rows1:?}"
        );
        let sum = rows(
            "2+3*4",
            &Facts::default(),
            Vec::new(),
            Vec::new(),
            &[],
            &search,
            |_| None,
        );
        assert_eq!(sum[0], Row::Calculator("14".into()));
        let local = rows(
            "localhost:5173",
            &Facts::default(),
            Vec::new(),
            Vec::new(),
            &[],
            &search,
            |typed| Some(format!("http://{typed}/")),
        );
        assert!(
            matches!(&local[0], Row::Open { url, search: false, .. } if url.starts_with("http://localhost:5173")),
            "{local:?}"
        );
        // Commands a tab needs are not offered without one.
        let none = rows(
            "pin tab",
            &Facts::default(),
            Vec::new(),
            Vec::new(),
            &[],
            &search,
            |_| None,
        );
        assert!(!none.iter().any(|r| matches!(r, Row::Command { .. })));
    }

    #[test]
    fn a_completed_domain_opens_first_then_the_search_then_remote_completions() {
        let search = CommandBarSearchPreferences::default();
        let typed = Typed {
            text: "github.com",
            prefix: Some("git"),
        };
        let remote = vec!["git rebase".to_string()];
        let rows = rows(
            typed,
            &Facts::default(),
            Vec::new(),
            Vec::new(),
            &remote,
            &search,
            |_| None,
        );
        assert!(
            matches!(&rows[0], Row::Open { url, search: false, .. } if url.contains("github.com")),
            "{rows:?}"
        );
        assert!(
            matches!(&rows[1], Row::Open { url, search: true, .. } if url.contains("q=git")),
            "{rows:?}"
        );
        assert!(
            matches!(rows.last(), Some(Row::Remote { query, url }) if query == "git rebase" && url.contains("git+rebase") || url.contains("git%20rebase")),
            "{rows:?}"
        );
    }
}
