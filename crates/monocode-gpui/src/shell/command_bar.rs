//! Browser mode's Command Bar (docs/native/modes.md §Command Bar): ⌘T opens
//! it over the page for a new tab, ⌘L and a click on the sidebar's address
//! open it on the sidebar's address field with the page's address, to go
//! elsewhere in the same tab. It lists what `browse_command::rows` finds:
//! commands, the address or search typed, open tabs, history and, as
//! SOCode's, the search engine's completions; a popular domain completes
//! inline as it is typed. In Browser mode it draws in a child window over
//! the live page (`shell/overlay_window.rs`). It also hosts the icon picker
//! (`BarTarget::Icon`), whose search field is the bar's.
use super::browse_organize::{IconCatalog, IconTarget};
use super::*;
use crate::browse_command::{self, ActiveTab, Command, Facts, Row, TabCandidate, Typed};
use crate::modes::{AppMode, BrowseSection};
use gpui::Hsla;
use gpui::Task;
use std::time::Duration;

/// Where the Command Bar's choice goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarTarget {
    /// A new tab (⌘T).
    NewTab,
    /// The tab of this pane, which goes to the address picked (⌘L).
    Current(i64),
    /// A page that joins the page of this pane in a Split View.
    Split(i64),
    /// The icon picker for a tab or folder (Change Icon…).
    Icon(IconTarget),
}

/// The search engine is asked this long after the last key.
const REMOTE_DEBOUNCE: Duration = Duration::from_millis(180);

pub(super) struct CommandBar {
    pub target: BarTarget,
    pub input: Entity<input::Input>,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// What the user typed, while a row's address stands in the field.
    typed: Option<String>,
    /// What the user typed last, without the inline completion.
    entered: String,
    /// The field's text while a popular domain completes what was typed.
    completion: Option<String>,
    /// The search engine's completions of `remote_query`.
    remote: Vec<String>,
    remote_query: String,
    remote_task: Option<Task<()>>,
    /// The note on what the suggestions send, shown.
    pub notice: bool,
    /// The icon picker's catalogue and grid.
    pub icon_catalog: IconCatalog,
    pub icon_scroll: gpui::ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

/// The bar's width over the page, and on the sidebar's address (SOCode's
/// 764 and 420), its header band and corners, and its rows.
const BAR_WIDTH: f32 = 764.;
const ADDRESS_BAR_WIDTH: f32 = 420.;
const BAR_HEADER: f32 = 64.;
const BAR_RADIUS: f32 = 12.;
const ROW_HEIGHT: f32 = 46.;

impl Shell {
    // ---- Opening and closing ----

    /// Opens the Command Bar for `target`: empty for a new tab, with the
    /// page's address chosen for the tab on screen.
    pub(super) fn browse_open_command_bar(
        &mut self,
        target: BarTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match target {
            BarTarget::NewTab | BarTarget::Split(_) | BarTarget::Icon(_) => String::new(),
            BarTarget::Current(pane) => self
                .browse_tab(pane, cx)
                .map(|tab| tab.url)
                .unwrap_or_default(),
        };
        let icon = matches!(target, BarTarget::Icon(_));
        let input = cx.new(|cx| {
            let (label, size) = if icon {
                ("Search Emoji", 12.5)
            } else {
                ("Search or Enter URL…", 18.)
            };
            let mut input = input::Input::field(label, label, content(0.35), cx);
            input.set_font_size(size);
            input
        });
        let len = text.len();
        let text_for_entered = text.clone();
        input.update(cx, |field, cx| {
            field.set_text(text, cx);
            field.select_range(0..len, cx);
        });
        let subscriptions = vec![cx.subscribe(&input, |this, _, _: &input::Changed, cx| {
            this.browse_command_bar_changed(cx)
        })];
        let focus = input.read(cx).focus_handle(cx);
        self.command_bar = Some(CommandBar {
            target,
            input,
            rows: Vec::new(),
            selected: 0,
            typed: None,
            entered: text_for_entered,
            completion: None,
            remote: Vec::new(),
            remote_query: String::new(),
            remote_task: None,
            notice: false,
            icon_catalog: IconCatalog::default(),
            icon_scroll: gpui::ScrollHandle::new(),
            _subscriptions: subscriptions,
        });
        self.browse_command_bar_changed(cx);
        window.focus(&focus, cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Closes the Command Bar; the page gets the keyboard back.
    pub(super) fn browse_close_command_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_bar.take().is_none() {
            return;
        }
        if self.browser_mode() {
            self.focus_browse_page(window, cx);
        }
        self.invalidate_chrome(cx);
        cx.notify();
    }

    // ---- Rows ----

    /// The text changed: the rows follow it, the first one chosen.
    fn browse_command_bar_changed(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &self.command_bar else {
            return;
        };
        let typed = bar.input.read(cx).content.to_string();
        // A row's address stands in the field while it is chosen: that is
        // no typing; nor is the inline completion coming back.
        if (bar.typed.is_some()
            && (bar.rows.get(bar.selected)).and_then(Row::preview) == Some(&typed))
            || bar.completion.as_deref() == Some(typed.as_str())
        {
            return;
        }
        let target = bar.target;
        if let BarTarget::Icon(_) = target {
            if let Some(bar) = &mut self.command_bar {
                bar.rows.clear();
                bar.icon_scroll.set_offset(gpui::point(px(0.), px(0.)));
            }
            cx.notify();
            return;
        }
        // A popular domain completes a fresh insertion at the end, chosen
        // so the next key replaces it, as SOCode's ("git" → github.com).
        let grew = typed.len() > bar.entered.len()
            && typed
                .to_lowercase()
                .starts_with(&bar.entered.to_lowercase());
        let completed = (grew && !matches!(target, BarTarget::Split(_)))
            .then(|| socode_browser::omnibox::top_domain(&typed))
            .flatten()
            .map(|(domain, _)| domain)
            .filter(|domain| domain.len() > typed.len())
            .map(|domain| format!("{typed}{}", &domain[typed.len()..]));
        let input = bar.input.clone();
        if let Some(bar) = &mut self.command_bar {
            bar.entered = typed.clone();
            bar.completion = completed.clone();
        }
        if let Some(full) = &completed {
            let (start, full) = (typed.len(), full.clone());
            input.update(cx, |field, cx| {
                let end = full.len();
                field.set_text(full, cx);
                field.select_range(start..end, cx);
            });
        }
        let prefix = completed.is_some().then_some(typed.as_str());
        let typed = completed.clone().unwrap_or_else(|| typed.clone());
        // Opened on a page's own address, the field lists nothing until
        // the user types.
        let current_url = match target {
            BarTarget::Current(pane) => self.browse_tab(pane, cx).map(|t| t.url),
            BarTarget::NewTab | BarTarget::Split(_) | BarTarget::Icon(_) => None,
        };
        let query = if current_url.as_deref() == Some(typed.as_str()) {
            ""
        } else {
            typed.as_str()
        };
        self.browse_ask_search_engine(prefix.unwrap_or(query), cx);
        let remote = self.browse_remote_rows(prefix.unwrap_or(query));
        // Joining a Split View, no commands: tabs and addresses only, and
        // not the pages already in it.
        let mut facts = self.browse_command_facts(cx);
        let mut tabs = self.browse_command_tabs(cx);
        if let BarTarget::Split(anchor) = target {
            facts = Facts::default();
            let inside = self.browse_split_of(anchor).unwrap_or_else(|| vec![anchor]);
            tabs.retain(|tab| !inside.contains(&tab.pane) && self.workspace.browse.owns(tab.pane));
        }
        let rows = browse_command::rows(
            Typed {
                text: query,
                prefix,
            },
            &facts,
            tabs,
            self.browse_command_pages(prefix.unwrap_or(query), cx),
            &remote,
            &self
                .workspace
                .browse
                .model
                .active_user_profile()
                .settings
                .search,
            crate::browser::address,
        );
        if let Some(bar) = &mut self.command_bar {
            bar.rows = rows;
            bar.selected = 0;
            bar.typed = None;
        }
        cx.notify();
    }

    /// The search engine's completions of `query`, once they came, but
    /// those the user removed; none while suggestions are off.
    fn browse_remote_rows(&self, query: &str) -> Vec<String> {
        let Some(bar) = &self.command_bar else {
            return Vec::new();
        };
        let search = &self
            .workspace
            .browse
            .model
            .active_user_profile()
            .settings
            .search;
        let Some(engine) = search.suggestion_engine().filter(|_| search.suggestions) else {
            return Vec::new();
        };
        if bar.remote_query != query.trim() {
            return Vec::new();
        }
        (bar.remote.iter())
            .filter(|q| !search.suggestion_dismissed(engine, q))
            .cloned()
            .collect()
    }

    /// Asks the search engine for completions of `query`, a moment after
    /// the last key, as SOCode's: only for a search (not an address or a
    /// path), only while the profile's suggestions are on with an engine
    /// that has them, never in tests. The answer is dropped when the field
    /// moved on.
    fn browse_ask_search_engine(&mut self, query: &str, cx: &mut Context<Self>) {
        let search = &self
            .workspace
            .browse
            .model
            .active_user_profile()
            .settings
            .search;
        let engine = search.suggestion_engine().filter(|_| search.suggestions);
        let query = query.trim().to_string();
        let Some(bar) = &mut self.command_bar else {
            return;
        };
        if bar.remote_query == query {
            return;
        }
        bar.remote.clear();
        bar.remote_query = query.clone();
        bar.remote_task = None;
        let wanted = matches!(bar.target, BarTarget::NewTab | BarTarget::Current(_))
            && (2..=512).contains(&query.len())
            && !query.contains(['/', '@', ':', '\\'])
            && matches!(
                socode_browser::omnibox::resolve(&query),
                Some(socode_browser::omnibox::Resolved::Search { .. })
            );
        let Some(engine) = engine.filter(|_| wanted && !cfg!(test)) else {
            return;
        };
        let url = engine.suggest_url(&query);
        let input = bar.input.clone();
        bar.remote_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REMOTE_DEBOUNCE).await;
            let asked = query.clone();
            let rows = cx
                .background_spawn(
                    async move { monocode_core::browser::suggest::fetch(&url, &asked) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(bar) = &mut this.command_bar else {
                    return;
                };
                if bar.input != input || bar.remote_query != query || rows.is_empty() {
                    return;
                }
                bar.remote = rows;
                // The rows follow, the chosen one kept.
                let selected = bar.selected;
                this.browse_refresh_rows(selected, cx);
            });
        }));
    }

    /// The rows again for the field as it is (the search engine answered),
    /// keeping the chosen row.
    fn browse_refresh_rows(&mut self, selected: usize, cx: &mut Context<Self>) {
        let Some(bar) = &self.command_bar else {
            return;
        };
        let target = bar.target;
        // While a row's address stands in the field, what was typed.
        let text = (bar.typed.clone())
            .or_else(|| bar.completion.clone())
            .unwrap_or_else(|| bar.input.read(cx).content.to_string());
        let entered = bar.entered.clone();
        let prefix = bar.completion.is_some().then_some(entered.as_str());
        let current_url = match target {
            BarTarget::Current(pane) => self.browse_tab(pane, cx).map(|t| t.url),
            _ => None,
        };
        let query = if current_url.as_deref() == Some(text.as_str()) {
            ""
        } else {
            text.as_str()
        };
        let remote = self.browse_remote_rows(prefix.unwrap_or(query));
        let rows = browse_command::rows(
            Typed {
                text: query,
                prefix,
            },
            &self.browse_command_facts(cx),
            self.browse_command_tabs(cx),
            self.browse_command_pages(prefix.unwrap_or(query), cx),
            &remote,
            &self
                .workspace
                .browse
                .model
                .active_user_profile()
                .settings
                .search,
            crate::browser::address,
        );
        if let Some(bar) = &mut self.command_bar {
            bar.selected = selected.min(rows.len().saturating_sub(1));
            bar.rows = rows;
        }
        cx.notify();
    }

    /// What the commands depend on, now.
    fn browse_command_facts(&self, cx: &App) -> Facts {
        let browse = &self.workspace.browse;
        let active = browse.active.and_then(|pane| {
            let tab = self.browse_tab(pane, cx)?;
            let today = browse.today();
            let ix = today.iter().position(|p| *p == pane);
            Some(ActiveTab {
                own: tab.section.is_some(),
                pinned: tab.section == Some(BrowseSection::Pinned),
                favorite: tab.section == Some(BrowseSection::Favorites),
                away: tab.home.is_some(),
                named: browse.name(pane).is_some(),
                muted: tab.sound == Some(browser_audio::TabSound::Muted),
                loading: tab.loading,
                below: ix.is_some_and(|ix| ix + 1 < today.len()),
                others: ix.is_some() && today.len() > 1,
            })
        });
        let space = browse.model.space();
        Facts {
            active,
            today: !space.today.is_empty(),
            archive: !browse.model.archive.is_empty(),
            undo: browse.model.sidebar_undo_action().is_some(),
            redo: browse.model.sidebar_redo_action().is_some(),
            open_folders: space.folders.iter().any(|f| !f.collapsed),
            closed_folders: space.folders.iter().any(|f| f.collapsed),
            sidebar_shown: !self.browse_rail_compact(),
        }
    }

    /// The open tabs the bar may switch to: the user's own and the
    /// projects' pages, but the one on screen.
    fn browse_command_tabs(&self, cx: &App) -> Vec<TabCandidate> {
        let browse = &self.workspace.browse;
        let place = |pane: i64| match browse.section(pane) {
            Some(BrowseSection::Favorites) => "Favorites".to_string(),
            Some(BrowseSection::Pinned) => "Pinned".to_string(),
            Some(BrowseSection::Today) => "Today".to_string(),
            None => String::new(),
        };
        let last_active = |pane: i64| {
            browse
                .tab(pane)
                .map_or(std::time::UNIX_EPOCH, |tab| tab.last_active)
        };
        let mut out: Vec<TabCandidate> = (browse.order().into_iter())
            .filter(|pane| Some(*pane) != browse.active)
            .filter_map(|pane| {
                let tab = self.browse_tab(pane, cx)?;
                Some(TabCandidate {
                    pane,
                    title: tab.title,
                    url: tab.url,
                    place: place(pane),
                    last_active: last_active(pane),
                })
            })
            .collect();
        for project in self.browse_projects() {
            let name = self.project_label(&project.path);
            for pane in project.panes {
                if Some(pane) == browse.active {
                    continue;
                }
                if let Some(tab) = self.browse_tab(pane, cx) {
                    out.push(TabCandidate {
                        pane,
                        title: tab.title,
                        url: tab.url,
                        place: name.to_string(),
                        last_active: std::time::UNIX_EPOCH,
                    });
                }
            }
        }
        out
    }

    /// The history's pages for `query`, Browser mode's own first.
    fn browse_command_pages(
        &self,
        query: &str,
        cx: &mut App,
    ) -> Vec<monocode_core::browser::history::Entry> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let stores = crate::browser::history::stores(cx);
        let mut stores = stores.borrow_mut();
        stores.suggest(
            super::modes::BROWSE_HISTORY_PROJECT,
            query,
            8,
            crate::browser::history::now_ms(),
        )
    }

    /// ↑/↓: the chosen row moves, and its address stands in the field.
    fn browse_command_bar_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.command_bar else {
            return;
        };
        if bar.rows.is_empty() {
            return;
        }
        let count = bar.rows.len() as isize;
        bar.selected = (bar.selected as isize + step).rem_euclid(count) as usize;
        let typed = bar
            .typed
            .clone()
            .unwrap_or_else(|| bar.input.read(cx).content.to_string());
        let text = bar.rows[bar.selected]
            .preview()
            .map(str::to_string)
            .unwrap_or_else(|| typed.clone());
        bar.typed = Some(typed);
        let input = bar.input.clone();
        input.update(cx, |field, cx| {
            let len = text.len();
            field.set_text(text, cx);
            field.select_range(len..len, cx);
        });
        cx.notify();
    }

    // ---- Running a row ----

    /// Enter: the chosen row; with none, a blank tab for a new tab (its page
    /// lists the project's dev servers).
    fn browse_command_bar_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = &self.command_bar else {
            return;
        };
        let target = bar.target;
        let row = bar.rows.get(bar.selected).cloned();
        let text = bar.input.read(cx).content.to_string();
        let empty = text.trim().is_empty();
        // The icon picker: Enter gives the first emoji found.
        if let BarTarget::Icon(icon_target) = target {
            let first = super::browse_organize::picker_entries(bar.icon_catalog, &text)
                .into_iter()
                .next();
            if let Some(first) = first {
                self.browse_set_icon(icon_target, Some(&first.value), cx);
            }
            return self.browse_close_command_bar(window, cx);
        }
        match row {
            Some(row) => self.browse_run_row(target, row, window, cx),
            None if empty && target == BarTarget::NewTab => {
                self.browse_close_command_bar(window, cx);
                self.browse_new_tab(None, None, true, window, cx);
            }
            None => self.browse_close_command_bar(window, cx),
        }
    }

    #[cfg(test)]
    pub(super) fn browse_command_bar_submit_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_command_bar_submit(window, cx)
    }

    pub(super) fn browse_run_row(
        &mut self,
        target: BarTarget,
        row: Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_close_command_bar(window, cx);
        match row {
            Row::Calculator(answer) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(answer))
            }
            Row::Open { url, .. } | Row::Page { url, .. } | Row::Remote { url, .. } => {
                self.browse_open_url(target, url, window, cx)
            }
            Row::Tab(tab) => match target {
                BarTarget::Split(anchor) => self.browse_add_split(anchor, tab.pane, window, cx),
                _ => self.browse_select(tab.pane, window, cx),
            },
            Row::Command { command, .. } => self.browse_run_command(command, window, cx),
        }
    }

    /// `url` in a new tab, or in the tab of `target`.
    fn browse_open_url(
        &mut self,
        target: BarTarget,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            BarTarget::NewTab => {
                self.browse_new_tab(Some(url), None, true, window, cx);
            }
            BarTarget::Split(anchor) => self.browse_add_split_url(anchor, url, window, cx),
            BarTarget::Icon(_) => {}
            BarTarget::Current(pane) => {
                match self.browser_page(pane, cx) {
                    Some(page) => page.update(cx, |page, cx| page.open_address(url, cx)),
                    None => {
                        if let Some(state) = self.workspace.browser_panes.get_mut(&pane) {
                            state.url = url;
                        }
                    }
                }
                self.browse_select(pane, window, cx);
            }
        }
    }

    /// A command, on the tab on screen where it needs one.
    fn browse_run_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.workspace.browse.active;
        let page = active.and_then(|pane| self.browser_page(pane, cx));
        // Page actions go to the page, which takes the keyboard first.
        let page_action = |this: &mut Self,
                           action: Box<dyn gpui::Action>,
                           window: &mut Window,
                           cx: &mut Context<Self>| {
            this.focus_browse_page(window, cx);
            window.dispatch_action(action, cx);
        };
        match command {
            Command::NewTab => {
                self.browse_open_command_bar(BarTarget::NewTab, window, cx);
            }
            Command::NewFolder => {
                self.browse_new_folder(super::browse_organize::PinSpot::End, &[], window, cx);
            }
            Command::RenameTab => {
                if let Some(pane) = active {
                    self.browse_rename(
                        super::session_groups::RenameTarget::BrowseTab(pane),
                        window,
                        cx,
                    );
                }
            }
            Command::ResetTabName => {
                if let Some(pane) = active {
                    self.browse_reset_name(pane, cx);
                }
            }
            Command::ResetPinnedTab => {
                if let Some(pane) = active {
                    self.browse_back_home(pane, window, cx);
                }
            }
            Command::ReplacePinnedUrl => {
                if let Some(pane) = active {
                    self.browse_replace_home(pane, cx);
                }
            }
            Command::DuplicateTab => {
                if let Some(pane) = active {
                    self.run_browse_tab_menu(pane, "duplicate", window, cx);
                }
            }
            Command::TogglePin => {
                if let Some(pane) = active {
                    self.browse_toggle_pin(pane, cx);
                }
            }
            Command::ToggleFavorite => {
                if let Some(pane) = active {
                    self.browse_toggle_favorite(pane, cx);
                }
            }
            Command::ToggleMute => {
                if let Some(pane) = active {
                    let muted =
                        self.browser_tab_sound(pane, cx) == Some(browser_audio::TabSound::Muted);
                    self.set_browser_muted(pane, !muted, cx);
                }
            }
            Command::CloseTab => {
                if let Some(pane) = active {
                    self.browse_close(pane, window, cx);
                }
            }
            Command::CloseTabsBelow => {
                if let Some(pane) = active {
                    self.browse_close_below(pane, window, cx);
                }
            }
            Command::CloseOtherTabs => {
                if let Some(pane) = active {
                    self.run_browse_tab_menu(pane, "close-others", window, cx);
                }
            }
            Command::ClearToday => self.browse_clear(window, cx),
            Command::RestoreTab => self.browse_reopen(window, cx),
            Command::Undo => self.browse_undo(false, window, cx),
            Command::Redo => self.browse_undo(true, window, cx),
            Command::CopyUrl => {
                if let Some(pane) = active {
                    self.run_browse_tab_menu(pane, "copy-link", window, cx);
                }
            }
            Command::CopyUrlAsMarkdown => {
                if let Some(pane) = active {
                    self.run_browse_tab_menu(pane, "copy-markdown", window, cx);
                }
            }
            Command::Reload => {
                if let Some(page) = page {
                    page.update(cx, |page, _| page.reload_or_stop());
                }
            }
            Command::StopLoading => {
                if let Some(page) = page.filter(|p| p.read(cx).loading()) {
                    page.update(cx, |page, _| page.reload_or_stop());
                }
            }
            Command::ForceReload => page_action(
                self,
                Box::new(crate::browser::ReloadIgnoringCache),
                window,
                cx,
            ),
            Command::ZoomIn => page_action(self, Box::new(crate::browser::ZoomIn), window, cx),
            Command::ZoomOut => page_action(self, Box::new(crate::browser::ZoomOut), window, cx),
            Command::ResetZoom => {
                page_action(self, Box::new(crate::browser::ResetZoom), window, cx)
            }
            Command::FindInPage => page_action(self, Box::new(crate::browser::Find), window, cx),
            Command::PrintPage => {
                if let Some(page) = page {
                    page.read(cx).print();
                }
            }
            Command::DevTools
            | Command::InspectElements
            | Command::JavaScriptConsole
            | Command::NetworkInspector => {
                use crate::browser::DevToolsCommand as D;
                let devtools = match command {
                    Command::InspectElements => D::Elements,
                    Command::JavaScriptConsole => D::Console,
                    Command::NetworkInspector => D::Network,
                    _ => D::Toggle,
                };
                if let Some(page) = page {
                    page.update(cx, |page, cx| page.run_dev_tools(devtools, cx));
                }
            }
            Command::DockDevTools(side) => {
                if let Some(page) = page {
                    page.update(cx, |page, cx| page.dock_dev_tools(side, cx));
                }
            }
            Command::ViewSource => {
                if let Some(page) = page {
                    page.update(cx, |page, cx| page.view_source(cx));
                }
            }
            Command::GoBack => self.browse_history_step(true, cx),
            Command::GoForward => self.browse_history_step(false, cx),
            Command::NextTab => self.browse_step(1, window, cx),
            Command::PreviousTab => self.browse_step(-1, window, cx),
            Command::ViewHistory => window.dispatch_action(
                Box::new(crate::browser::history_view::ShowBrowserHistory),
                cx,
            ),
            Command::CollapseAllFolders | Command::ExpandAllFolders => {
                let collapsed = command == Command::CollapseAllFolders;
                self.browse_edit(cx, |model, _| {
                    let space = model.space().id;
                    model.set_all_folders_collapsed(space, collapsed)
                });
            }
            Command::ToggleSidebar => self.toggle_primary_sidebar(window, cx),
            Command::SendToChat => {
                if let Some(pane) = active {
                    self.browse_send_to_chat(pane, window, cx);
                }
            }
            Command::Capture => self.browse_capture(cx),
            Command::SiteControls => self.browse_site_controls(cx),
            Command::ClearCookiesAndRefresh => {
                self.browse_clear_site(crate::browser::ClearSiteData::Cookies, cx)
            }
            Command::ClearCacheAndRefresh => {
                self.browse_clear_site(crate::browser::ClearSiteData::Cache, cx)
            }
            Command::DevMode => self.set_mode(AppMode::Dev, window, cx),
            Command::LinkRouting => {
                self.open_settings(crate::settings::Section::Browser, window, cx)
            }
        }
    }

    // ---- Drawing ----

    /// The Command Bar over Browser mode's body, with a veil that closes it
    /// on a click; or the icon picker. `None` while it is closed.
    pub(super) fn browse_command_bar_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = self.command_bar.as_ref()?;
        if let BarTarget::Icon(target) = bar.target {
            let picker =
                self.browse_icon_picker(target, bar.icon_catalog, &bar.input, &bar.icon_scroll, cx);
            return Some(self.browse_bar_veil(picker, cx));
        }
        let on_address = matches!(bar.target, BarTarget::Current(_)) && !self.browse_rail_compact();
        let width = if on_address {
            ADDRESS_BAR_WIDTH
        } else {
            BAR_WIDTH
        };
        let light = crate::theme::is_light();
        let ink = if light { 0. } else { 1. };
        let rows: Vec<AnyElement> = (bar.rows.iter().enumerate())
            .map(|(ix, row)| self.browse_command_row(ix, row, ix == bar.selected, cx))
            .collect();
        let search = &self
            .workspace
            .browse
            .model
            .active_user_profile()
            .settings
            .search;
        let suggests = search.suggestions
            && search.suggestion_engine().is_some()
            && matches!(bar.target, BarTarget::NewTab | BarTarget::Current(_));
        // The note on what suggestions send shows the first time they
        // come, and on the info button.
        let remote_shown = bar.rows.iter().any(|row| matches!(row, Row::Remote { .. }));
        let notice = suggests && (bar.notice || (remote_shown && !search.notice_dismissed));
        let engine = search.default_name().to_string();
        // The header's icon: the chosen page's, the tab's own, or a glass.
        let typed = !bar.input.read(cx).content.trim().is_empty();
        let lead = match (bar.rows.get(bar.selected), bar.target) {
            (Some(Row::Tab(tab)), _) if typed => self.browse_row_favicon(tab.pane, &tab.url, cx),
            (Some(Row::Command { .. }), _) if typed => icon("command-compact", 16.)
                .text_color(content(0.75))
                .into_any_element(),
            (_, BarTarget::Current(pane)) => {
                let url = self.browse_tab(pane, cx).map(|t| t.url).unwrap_or_default();
                self.browse_row_favicon(pane, &url, cx)
            }
            _ => icon("search", 16.)
                .text_color(content(0.75))
                .into_any_element(),
        };
        let info = div()
            .id("command-bar-info")
            .debug_selector(|| "command-bar-info".into())
            .role(Role::Button)
            .aria_label("About Suggestions")
            .cursor_pointer()
            .size(px(24.))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(content(0.08))
            .text_color(content(0.55))
            .hover(|s| s.bg(content(0.14)))
            .child(icon("alert-circle", 15.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(bar) = &mut this.command_bar {
                    bar.notice = !bar.notice;
                }
                cx.notify();
            }));
        let card = div()
            .id("command-bar")
            .debug_selector(|| "command-bar".into())
            .key_context("CommandBar")
            .relative()
            .w(px(width))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded(px(BAR_RADIUS))
            .bg(crate::theme::floating_fill())
            .border_1()
            .border_color(if light { white(0.8) } else { black(0.9) })
            .shadow(vec![
                BoxShadow::new(px(0.), px(18.), gpui::rgba(0x00000052).into()).blur_radius(px(48.)),
            ])
            .text_color(content(0.92))
            // In the main window (tests) the page under it draws its last
            // picture; in Browser mode's child window the page stays live.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(crate::browser::cover_element()),
            )
            // The double edge, as SOCode's.
            .child(
                div()
                    .absolute()
                    .inset(px(1.))
                    .rounded(px(BAR_RADIUS - 1.))
                    .border_1()
                    .border_color(gpui::hsla(0., 0., ink, if light { 0.08 } else { 0.2 })),
            )
            .capture_action(cx.listener(|this, _: &input::Up, _, cx| {
                cx.stop_propagation();
                this.browse_command_bar_step(-1, cx);
            }))
            .capture_action(cx.listener(|this, _: &input::Down, _, cx| {
                cx.stop_propagation();
                this.browse_command_bar_step(1, cx);
            }))
            // Drawn in Browser mode's child window, the bar acts in the
            // main one, where the pages and their focus are.
            .capture_action(cx.listener(|this, _: &input::Submit, _, cx| {
                cx.stop_propagation();
                this.in_main_window(cx, |this, window, cx| {
                    this.browse_command_bar_submit(window, cx)
                });
            }))
            .capture_action(cx.listener(|this, _: &DismissMenu, _, cx| {
                cx.stop_propagation();
                this.in_main_window(cx, |this, window, cx| {
                    this.browse_close_command_bar(window, cx)
                });
            }))
            // The child window has no Workspace keys: Esc is read here.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.in_main_window(cx, |this, window, cx| {
                        this.browse_close_command_bar(window, cx)
                    });
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(BAR_HEADER - 1.))
                    .flex_shrink_0()
                    .pl(px(22.))
                    .pr(px(21.))
                    .flex()
                    .items_center()
                    .gap(px(13.))
                    .child(
                        div()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(lead),
                    )
                    .child(
                        div()
                            .debug_selector(|| "command-bar-input".into())
                            .flex_1()
                            .min_w_0()
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(px(28.))
                            .child(bar.input.clone()),
                    )
                    .when(suggests && !notice, |d| d.child(info)),
            )
            .when(!bar.rows.is_empty(), |d| {
                d.child(div().h(px(1.)).bg(content(0.1))).child(
                    div()
                        .id("command-bar-rows")
                        .max_h(px(ROW_HEIGHT * 9. + 16.))
                        .overflow_y_scroll()
                        .pt(px(6.))
                        .pb(px(if notice { 2. } else { 10. }))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .children(rows),
                )
            })
            .when(notice, |d| {
                d.child(self.browse_suggestions_notice(&engine, cx))
            });
        // Over the page, about a third of the way down; on the sidebar's
        // address, where the field is.
        let place = if on_address {
            div()
                .absolute()
                .top(px(TITLE_BAR_HEIGHT + 4.))
                .left(px(6.))
                .child(card)
        } else {
            div()
                .absolute()
                .top(gpui::relative(0.5))
                .mt(px(-169.))
                .left(px(self.browse_rail_width()))
                .right_0()
                .flex()
                .justify_center()
                .px_4()
                .child(card)
        };
        Some(self.browse_bar_veil(place.into_any_element(), cx))
    }

    /// The room around the bar: a press there closes it.
    fn browse_bar_veil(&self, child: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("command-bar-layer")
            .debug_selector(|| "command-bar-layer".into())
            .absolute()
            .inset_0()
            // Fully clear windows let presses through to the window below.
            .bg(gpui::rgba(0x00000003))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.in_main_window(cx, |this, window, cx| {
                        this.browse_close_command_bar(window, cx)
                    })
                }),
            )
            .child(child)
            .into_any_element()
    }

    /// What the search suggestions send, with the way to their settings,
    /// as SOCode's note under the bar.
    fn browse_suggestions_notice(&self, engine: &str, cx: &mut Context<Self>) -> AnyElement {
        let button = |id: &'static str, label: &'static str, width: f32| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(Role::Button)
                .cursor_pointer()
                .w(px(width))
                .h(px(32.))
                .flex_shrink_0()
                .rounded(px(6.))
                .border_1()
                .border_color(content(0.12))
                .bg(content(0.06))
                .hover(|s| s.bg(content(0.12)))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .child(label)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        div()
            .debug_selector(|| "command-bar-notice".into())
            .h(px(60.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(15.))
            .pl(px(15.))
            .pr(px(9.))
            .border_t_1()
            .border_color(content(0.08))
            .rounded_b(px(BAR_RADIUS))
            .child(icon("alert-circle", 14.).text_color(content(0.5)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child("We talk to your search engine to help us suggest results."),
                    )
                    .child(div().truncate().text_color(content(0.5)).child(format!(
                        "What you type goes to {engine}, without cookies or the page."
                    ))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .flex_shrink_0()
                    .child(
                        button("command-bar-search-settings", "Search Settings…", 132.).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.in_main_window(cx, |this, window, cx| {
                                    this.browse_close_command_bar(window, cx);
                                    this.open_settings(
                                        crate::settings::Section::Browser,
                                        window,
                                        cx,
                                    )
                                })
                            }),
                        ),
                    )
                    .child(
                        button("command-bar-notice-dismiss", "Dismiss", 72.).on_click(cx.listener(
                            |this, _, _, cx| {
                                let search = &mut this
                                    .workspace
                                    .browse
                                    .model
                                    .active_user_profile_mut()
                                    .settings
                                    .search;
                                search.notice_dismissed = true;
                                if let Some(bar) = &mut this.command_bar {
                                    bar.notice = false;
                                }
                                this.persist(cx);
                                cx.notify();
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    fn browse_command_row(
        &self,
        ix: usize,
        row: &Row,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let light = crate::theme::is_light();
        let ink = if light { 0. } else { 1. };
        // What the field's own page is called, for its address's row.
        let current = match self.command_bar.as_ref().map(|bar| bar.target) {
            Some(BarTarget::Current(pane)) => self.browse_tab(pane, cx),
            _ => None,
        };
        let page_row = matches!(
            row,
            Row::Tab(_)
                | Row::Page { .. }
                | Row::Open { .. }
                | Row::Remote { .. }
                | Row::Command { .. }
        );
        let search_row = matches!(
            row,
            Row::Open { search: true, .. } | Row::Remote { .. } | Row::Calculator(_)
        );
        let command = matches!(row, Row::Command { .. });
        let history = matches!(row, Row::Page { .. });
        let glyph = |path: &'static str| icon(path, 14.).into_any_element();
        let (tile, title, faded, detail): (AnyElement, String, Option<String>, String) = match row {
            Row::Calculator(answer) => (glyph("copy"), format!("= {answer}"), None, "Copy".into()),
            Row::Command { label, .. } => {
                (glyph("command-compact"), label.clone(), None, String::new())
            }
            Row::Open {
                label,
                search: true,
                ..
            } => (
                glyph("search"),
                label
                    .split(" — Search with ")
                    .next()
                    .unwrap_or(label)
                    .to_string(),
                None,
                String::new(),
            ),
            Row::Open { url, .. } => {
                let shown = display_url(url);
                let name = match &current {
                    Some(tab) if tab.url == *url => Some(tab.title.clone()),
                    _ => (socode_browser::omnibox::TOP_DOMAINS.iter())
                        .find(|(domain, _)| shown == *domain)
                        .map(|(_, name)| format!("{name} — {shown}")),
                };
                (glyph("globe"), name.unwrap_or(shown), None, String::new())
            }
            Row::Remote { query, .. } => (glyph("search"), query.clone(), None, String::new()),
            Row::Tab(tab) => (
                self.browse_row_favicon(tab.pane, &tab.url, cx),
                tab.title.clone(),
                None,
                if tab.place.is_empty() || tab.place == "Today" {
                    "Switch to Tab".to_string()
                } else {
                    tab.place.clone()
                },
            ),
            Row::Page { url, title } => (
                glyph("clock"),
                if title.is_empty() {
                    display_url(url)
                } else {
                    title.clone()
                },
                (!title.is_empty()).then(|| format!(" — {}", display_url(url))),
                String::new(),
            ),
        };
        let shortcut = match row {
            Row::Command {
                command: Command::Undo,
                ..
            } => Some(format!("{MOD}Z")),
            Row::Command {
                command: Command::Redo,
                ..
            } => Some(format!("⇧{MOD}Z")),
            _ => None,
        };
        let selection = if page_row {
            self.browse_command_page_selection()
        } else {
            self.browse_command_selection()
        };
        let arrow = !search_row && !command;
        let group: SharedString = format!("command-bar-row-{ix}").into();
        let remote = match row {
            Row::Remote { query, .. } => Some(query.clone()),
            _ => None,
        };
        div()
            .id(("command-bar-row", ix))
            .debug_selector(move || format!("command-bar-row-{ix}"))
            .group(group.clone())
            .role(Role::ListBoxOption)
            .aria_selected(selected)
            .cursor_pointer()
            .flex_shrink_0()
            .h(px(ROW_HEIGHT))
            .mx(px(8.))
            .pl(px(10.))
            .pr(px(11.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .gap(px(9.))
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if selected {
                gpui::white()
            } else {
                gpui::hsla(0., 0., ink, 0.82)
            })
            .when(selected, |d| d.bg(selection))
            .when(!selected, |d| {
                d.hover(move |s| s.bg(gpui::hsla(0., 0., ink, 0.05)))
            })
            // Pages show their icon; commands a glyph on a faint tile, white
            // on the chosen row.
            .child(
                div()
                    .size(px(24.))
                    .flex_shrink_0()
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(command && selected, |d| {
                        d.bg(gpui::white()).text_color(gpui::hsla(0., 0., 0.12, 1.))
                    })
                    .when(command && !selected, |d| {
                        d.bg(gpui::hsla(0., 0., ink, 0.07))
                            .text_color(gpui::hsla(0., 0., ink, 0.7))
                    })
                    .when(!command, |d| {
                        d.text_color(if selected {
                            white(0.85)
                        } else {
                            gpui::hsla(0., 0., ink, 0.7)
                        })
                    })
                    .child(tile),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .child(div().min_w_0().truncate().child(title))
                    .children(faded.map(|faded| {
                        div()
                            .flex_shrink_0()
                            .max_w(px(320.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .opacity(if selected { 0.65 } else { 0.4 })
                            .child(faded)
                    })),
            )
            .when(!detail.is_empty(), |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(260.))
                        .truncate()
                        .text_color(if selected {
                            white(0.95)
                        } else {
                            gpui::hsla(0., 0., ink, 0.4)
                        })
                        .child(detail),
                )
            })
            .children(shortcut.map(|keys| {
                div()
                    .h(px(24.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .when(selected, |d| d.bg(gpui::white()).text_color(selection))
                    .child(keys)
            }))
            // A search engine's completion can be put away.
            .children(remote.map(|query| {
                div()
                    .id(("command-bar-row-remove", ix))
                    .debug_selector(move || format!("command-bar-row-remove-{ix}"))
                    .role(Role::Button)
                    .aria_label("Remove Suggestion")
                    .px_2()
                    .rounded_md()
                    .invisible()
                    .group_hover(group.clone(), |s| s.visible())
                    .hover(move |s| s.bg(gpui::hsla(0., 0., ink, 0.12)))
                    .child(icon("x", 12.))
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let search = &mut this
                            .workspace
                            .browse
                            .model
                            .active_user_profile_mut()
                            .settings
                            .search;
                        if let Some(engine) = search.suggestion_engine() {
                            search.dismiss_suggestion(engine, &query);
                        }
                        let selected = this.command_bar.as_ref().map_or(0, |bar| bar.selected);
                        this.browse_refresh_rows(selected, cx);
                        this.persist(cx);
                    }))
            }))
            .when(arrow, |d| {
                d.child(
                    div()
                        .size(px(24.))
                        .flex_shrink_0()
                        .rounded(px(5.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(history && !selected, |d| {
                            d.invisible().group_hover(group.clone(), |s| s.visible())
                        })
                        .bg(if selected {
                            gpui::white()
                        } else {
                            gpui::hsla(0., 0., ink, 0.06)
                        })
                        .text_color(if selected {
                            selection
                        } else {
                            gpui::hsla(0., 0., ink, 0.6)
                        })
                        .child(icon("arrow-right-2", 14.)),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let Some(bar) = &this.command_bar else {
                        return;
                    };
                    let target = bar.target;
                    if let Some(row) = bar.rows.get(ix).cloned() {
                        this.in_main_window(cx, move |this, window, cx| {
                            this.browse_run_row(target, row, window, cx)
                        });
                    }
                }),
            )
            .into_any_element()
    }

    /// The chosen command's fill: the Browser colour, faintly.
    fn browse_command_selection(&self) -> Hsla {
        let light = crate::theme::is_light();
        match self.workspace.browse.tint.highlight(light) {
            Some(tint) => tint,
            None => content(0.1),
        }
    }

    /// The chosen page's, search's or action's fill, as SOCode's blue.
    fn browse_command_page_selection(&self) -> Hsla {
        gpui::rgb(if crate::theme::is_light() {
            0x8f95ff
        } else {
            0x464ddc
        })
        .into()
    }

    /// A tab row's icon in the bar: the page's favicon, or a globe.
    fn browse_row_favicon(&self, pane: i64, url: &str, cx: &mut Context<Self>) -> AnyElement {
        let favicon = (self.workspace.browser_panes.get(&pane))
            .and_then(|state| state.favicon.clone())
            .and_then(|f| crate::favicons::icon(&f, cx));
        match favicon {
            Some(image) => gpui::img(image)
                .size(px(16.))
                .rounded(px(3.))
                .into_any_element(),
            None if url.is_empty() => icon("plus", 15.)
                .text_color(content(0.5))
                .into_any_element(),
            None => icon("globe", 15.)
                .text_color(content(0.5))
                .into_any_element(),
        }
    }
}

/// An address as the bar shows it: no scheme, `www.` or final slash.
fn display_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn type_in(shell: &mut Shell, text: &str, cx: &mut Context<Shell>) {
        let input = shell.command_bar.as_ref().unwrap().input.clone();
        input.update(cx, |field, cx| field.set_text(text.into(), cx));
    }

    #[gpui::test]
    fn the_bar_opens_addresses_in_a_new_tab_or_the_one_on_screen(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                // ⌘T: what is typed opens in a new tab at the top of Today.
                shell.browse_open_command_bar(BarTarget::NewTab, window, cx);
                type_in(shell, "example.com", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                let bar = shell.command_bar.as_ref().unwrap();
                assert!(
                    matches!(&bar.rows[0], Row::Open { url, search: false, .. } if url.contains("example.com")),
                    "{:?}",
                    bar.rows
                );
                shell.browse_command_bar_submit(window, cx);
                assert!(shell.command_bar.is_none());
                let new = shell.workspace.browse.active.unwrap();
                assert_eq!(shell.workspace.browse.today()[0], new);
                assert!(shell.workspace.browser_panes[&new].url.contains("example.com"));
                // ⌘L: the tab on screen goes elsewhere; no tab is added.
                shell.browse_open_command_bar(BarTarget::Current(new), window, cx);
                // Its own address lists nothing to do yet.
                assert!(shell.command_bar.as_ref().unwrap().rows.iter().all(|r| matches!(r, Row::Tab(_))));
                type_in(shell, "rust-lang.org", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                let count = shell.workspace.browse.order().len();
                shell.browse_command_bar_submit(window, cx);
                assert_eq!(shell.workspace.browse.order().len(), count);
                let active = shell.workspace.browse.active.unwrap();
                assert!(
                    shell
                        .browse_tab(active, cx)
                        .unwrap()
                        .url
                        .contains("rust-lang.org")
                );
                // A command runs: Clear Today archives all but the tab on screen.
                shell.browse_open_command_bar(BarTarget::NewTab, window, cx);
                type_in(shell, "clear today", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                let bar = shell.command_bar.as_ref().unwrap();
                assert!(matches!(
                    &bar.rows[0],
                    Row::Command {
                        command: Command::ClearToday,
                        ..
                    }
                ));
                shell.browse_command_bar_submit(window, cx);
                assert_eq!(shell.workspace.browse.today().len(), 1);
                // Esc closes it; nothing happens.
                shell.browse_open_command_bar(BarTarget::NewTab, window, cx);
                shell.browse_close_command_bar(window, cx);
                assert!(shell.command_bar.is_none());
                assert_eq!(shell.workspace.browse.today().len(), 1);
                // Empty, Enter opens a blank tab (its page lists dev servers).
                shell.browse_open_command_bar(BarTarget::NewTab, window, cx);
                if !shell.command_bar.as_ref().unwrap().rows.is_empty() {
                    shell.command_bar.as_mut().unwrap().rows.clear();
                }
                shell.browse_command_bar_submit(window, cx);
                let blank = shell.workspace.browse.active.unwrap();
                assert!(shell.workspace.browser_panes[&blank].url.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn domains_complete_inline_and_the_search_engines_completions_follow(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.workspace.classic_layout = true;
                shell.set_mode(AppMode::Browser, window, cx);
                shell.browse_open_command_bar(BarTarget::NewTab, window, cx);
                type_in(shell, "gi", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                // "gi" completes to github.com, the rest chosen.
                let bar = shell.command_bar.as_ref().unwrap();
                assert_eq!(bar.input.read(cx).content.as_ref(), "github.com");
                assert!(
                    matches!(&bar.rows[0], Row::Open { url, search: false, .. } if url.contains("github.com")),
                    "{:?}",
                    bar.rows
                );
                assert!(
                    matches!(&bar.rows[1], Row::Open { search: true, label, .. } if label.starts_with("gi —")),
                    "{:?}",
                    bar.rows
                );
                // Typing on keeps completing; deleting does not.
                type_in(shell, "git", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let bar = shell.command_bar.as_ref().unwrap();
                assert_eq!(bar.input.read(cx).content.as_ref(), "github.com");
                type_in(shell, "gi", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let bar = shell.command_bar.as_ref().unwrap();
                assert_eq!(bar.input.read(cx).content.as_ref(), "gi");
                // The search engine answers: its completions come last, with
                // the note on what they send until it is dismissed.
                type_in(shell, "rust lang", cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let bar = shell.command_bar.as_mut().unwrap();
                assert_eq!(bar.remote_query, "rust lang");
                bar.remote = vec!["rust language book".into()];
                shell.browse_refresh_rows(0, cx);
                let bar = shell.command_bar.as_ref().unwrap();
                assert!(
                    matches!(bar.rows.last(), Some(Row::Remote { query, .. }) if query == "rust language book"),
                    "{:?}",
                    bar.rows
                );
            })
            .unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("command-bar-notice").is_some());
        let dismiss = visual
            .debug_bounds("command-bar-notice-dismiss")
            .expect("Dismiss");
        visual.simulate_click(dismiss.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("command-bar-notice").is_none());
        let info = visual
            .debug_bounds("command-bar-info")
            .expect("the info button");
        visual.simulate_click(info.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("command-bar-notice").is_some());
        window
            .update(cx, |shell, _, _| {
                let search = &shell
                    .workspace
                    .browse
                    .model
                    .active_user_profile()
                    .settings
                    .search;
                assert!(search.notice_dismissed);
            })
            .unwrap();
    }
}
