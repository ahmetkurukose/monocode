//! The shell's side of Search Everywhere (⇧⇧) and the command palette (⌘P):
//! the double-Shift gesture, the sources each modal reads, the action
//! catalog both offer, and what choosing a row does.
use super::*;
use crate::{
    command_palette::{self, CommandPalette},
    everywhere::{self, ActionItem, Everywhere, Tab},
};
use gpui::{Action, KeyContext, Modifiers};
use std::{rc::Rc, time::Instant};

/// Two taps of Shift alone, each shorter than this and this close together.
const TAP_MS: u128 = 350;

/// Tells a double tap of Shift from Shift used for typing: any other key,
/// modifier or click in between breaks the gesture.
#[derive(Default)]
pub(super) struct ShiftTaps {
    down: Option<Instant>,
    last_tap: Option<Instant>,
    shift: bool,
    /// When a release was last counted, so the same one is not counted again
    /// when it arrives a second way.
    counted: Option<Instant>,
}
impl ShiftTaps {
    /// Whether this change completes a double tap.
    pub(super) fn modifiers(&mut self, m: &Modifiers, now: Instant) -> bool {
        let alone = m.shift && !m.control && !m.alt && !m.platform && !m.function;
        let was = std::mem::replace(&mut self.shift, m.shift);
        if m.shift && !alone {
            self.reset();
            return false;
        }
        if alone && !was {
            self.down = Some(now);
            return false;
        }
        if !m.shift && was {
            self.counted = Some(now);
            let Some(down) = self.down.take() else {
                return false;
            };
            if now.duration_since(down).as_millis() > TAP_MS {
                self.last_tap = None;
                return false;
            }
            if self
                .last_tap
                .is_some_and(|last| now.duration_since(last).as_millis() <= TAP_MS)
            {
                self.last_tap = None;
                return true;
            }
            self.last_tap = Some(now);
        }
        false
    }
    /// Shift was pressed and let go with nothing else in between, as the
    /// window reports it to keystroke observers. The modifier changes above
    /// only reach the workspace along the focused element's path; this one
    /// arrives wherever the focus is, so a tap still counts when they did not.
    pub(super) fn released(&mut self, now: Instant) -> bool {
        if (self.counted).is_some_and(|at| now.duration_since(at).as_millis() < 30) {
            return false;
        }
        if !self.shift {
            // The press never reached us: how long it was held is unknown.
            self.shift = true;
            self.down = Some(now);
        }
        self.modifiers(&Modifiers::default(), now)
    }
    /// A key or a click came between: no tap counts.
    pub(super) fn reset(&mut self) {
        self.down = None;
        self.last_tap = None;
    }
}

pub(super) struct EverywhereOpen {
    pub(super) view: Entity<Everywhere>,
    restore: Option<FocusHandle>,
    _events: Subscription,
}
pub(super) struct PaletteOpen {
    picker: Entity<CommandPalette>,
    restore: Option<FocusHandle>,
    _events: Subscription,
}
#[cfg(any(test, feature = "selftest"))]
impl PaletteOpen {
    pub(super) fn picker(&self) -> &Entity<CommandPalette> {
        &self.picker
    }
}

/// `binding`'s keys as the menus show them: `⌘⇧P`, `Ctrl+Shift+P`.
fn keys_label(binding: &KeyBinding) -> SharedString {
    binding
        .keystrokes()
        .iter()
        .map(|k| crate::settings::shortcut_label(&k.unparse()))
        .collect::<Vec<_>>()
        .join(" ")
        .into()
}

impl Shell {
    // ---- the gesture ---------------------------------------------------

    pub(super) fn shift_changed(
        &mut self,
        modifiers: &Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shift_taps.modifiers(modifiers, Instant::now()) {
            self.double_shift(window, cx);
        }
    }

    /// A lone Shift was released, seen by the window's keystroke observer.
    pub(super) fn shift_released(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shift_taps.released(Instant::now()) {
            self.double_shift(window, cx);
        }
    }

    fn double_shift(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.everywhere.is_some() {
            self.close_everywhere(window, cx);
        } else {
            self.open_everywhere(None, None, window, cx);
        }
    }

    // ---- the action catalog --------------------------------------------

    /// Everything the command palette and the Actions tab offer, with the
    /// keys bound to each right now.
    pub(super) fn action_catalog(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<ActionItem> {
        let context = KeyContext::parse("Workspace").ok();
        let keys = |action: &dyn Action| -> Option<SharedString> {
            let context = context.clone()?;
            window
                .highest_precedence_binding_for_action_in_context(action, context)
                .map(|b| keys_label(&b))
        };
        let mut items: Vec<ActionItem> = vec![];
        let mut add = |group: &'static str,
                       title: &'static str,
                       glyph: &'static str,
                       make: fn() -> Box<dyn Action>| {
            let action = make();
            items.push(ActionItem {
                id: format!("{group}: {title}").into(),
                group: group.into(),
                title: title.into(),
                keys: keys(&*action),
                note: None,
                glyph,
                run: Rc::new(move |window, cx| window.dispatch_action(make(), cx)),
            });
        };
        add("Search", "Search Everywhere", "search", || {
            Box::new(SearchEverywhere)
        });
        add("Search", "Go to File", "file", || Box::new(GoToFile));
        add("Search", "Find in Files", "search", || {
            Box::new(FindInFiles)
        });
        add("Search", "Find in Files Panel", "folder-open", || {
            Box::new(SearchFiles)
        });
        add("Search", "Search Agent Conversations", "bot", || {
            Box::new(SearchAgents)
        });
        add("Search", "Switch Project", "folder", || {
            Box::new(SwitchProject)
        });
        add("Agent", "New Conversation", "sparkles", || {
            Box::new(NewCodex)
        });
        add("Agent", "New Claude Conversation", "sparkles", || {
            Box::new(NewClaude)
        });
        add("Agent", "Switch Model", "bot", || Box::new(SwitchModel));
        add("Agent", "Archive Conversation", "archive", || {
            Box::new(ArchiveSession)
        });
        add("Agent", "Previous Conversation", "chevron-up", || {
            Box::new(PreviousSession)
        });
        add("Agent", "Next Conversation", "chevron-down-2", || {
            Box::new(NextSession)
        });
        add("Project", "Open Project…", "folder-add", || {
            Box::new(OpenProject)
        });
        add("Project", "Previous Project", "arrow-left", || {
            Box::new(PreviousProject)
        });
        add("Project", "Next Project", "arrow-right-2", || {
            Box::new(NextProject)
        });
        add("Project", "Sleep This Project", "moon", || {
            Box::new(SleepProject)
        });
        add("Project", "Sleep Other Projects", "moon", || {
            Box::new(SleepOtherProjects)
        });
        add("Tab", "Close Tab", "x", || Box::new(CloseTab));
        add("Tab", "Close Other Tabs", "x", || Box::new(CloseOtherTabs));
        add("Tab", "Close All Tabs", "x", || Box::new(CloseAllTabs));
        add("Tab", "Reopen Closed Tab", "rotate-ccw", || {
            Box::new(ReopenClosedTab)
        });
        add("Tab", "Next Tab", "arrow-right-2", || Box::new(NextTab));
        add("Tab", "Previous Tab", "arrow-left", || {
            Box::new(PreviousTab)
        });
        add("Tab", "Go Back", "arrow-left-2", || Box::new(VisitBack));
        add("Tab", "Go Forward", "arrow-right-2", || {
            Box::new(VisitForward)
        });
        add("Pane", "Split Right", "panel-right", || {
            Box::new(SplitRight)
        });
        add("Pane", "Split Down", "panel-bottom", || Box::new(SplitDown));
        add("Pane", "Focus Left Pane", "panel-left", || {
            Box::new(FocusPaneLeft)
        });
        add("Pane", "Focus Right Pane", "panel-right", || {
            Box::new(FocusPaneRight)
        });
        add("Pane", "Focus Pane Above", "panel-top", || {
            Box::new(FocusPaneUp)
        });
        add("Pane", "Focus Pane Below", "panel-bottom", || {
            Box::new(FocusPaneDown)
        });
        add("Terminal", "Toggle Terminal", "terminal", || {
            Box::new(ToggleTerminal)
        });
        add("Terminal", "New Terminal", "terminal", || {
            Box::new(NewTerminal)
        });
        add("Terminal", "Split Terminal", "terminal", || {
            Box::new(SplitTerminal)
        });
        add("Git", "Commit…", "git-commit", || Box::new(GitCommit));
        add("Git", "New Pull Request…", "git-pull-request", || {
            Box::new(GitNewPullRequest)
        });
        add("Git", "Update Project", "refresh-cw", || {
            Box::new(GitUpdate)
        });
        add("Git", "Update Project (Rebase)", "refresh-cw", || {
            Box::new(GitUpdateRebase)
        });
        add("Git", "Fetch", "arrow-down-circle", || Box::new(GitFetch));
        add("Git", "Pull", "arrow-down-circle", || Box::new(GitPull));
        add("Git", "Push", "cloud-upload", || Box::new(GitPush));
        add("Git", "Branches…", "git-branch", || Box::new(GitBranches));
        add("Git", "Stash Changes", "archive", || Box::new(GitStash));
        add("Git", "Unstash Changes", "archive", || Box::new(GitUnstash));
        add("View", "Browser History", "clock", || {
            Box::new(crate::browser::history_view::ShowBrowserHistory)
        });
        add("View", "Toggle Sidebar", "panel-left", || {
            Box::new(ToggleSidebar)
        });
        add("View", "Toggle Files and Changes", "panel-right", || {
            Box::new(ToggleRightPanel)
        });
        add("View", "Zoom In", "add-01", || Box::new(ZoomIn));
        add("View", "Zoom Out", "minus-sign", || Box::new(ZoomOut));
        add("View", "Reset Zoom", "rotate-ccw", || Box::new(ResetZoom));
        add("View", "Sidebar Appearance…", "palette", || {
            Box::new(SidebarAppearance)
        });
        add("App", "Settings", "sliders", || Box::new(OpenSettings));
        add("App", "Open Inbox", "inbox-hi", || Box::new(OpenInbox));
        add("App", "Open Notes", "sticky-note", || Box::new(OpenNotes));
        add("App", "Check for Updates", "arrow-down-circle", || {
            Box::new(CheckForUpdates)
        });
        add("App", "Reload", "refresh-cw", || Box::new(Reload));
        add("App", "About Monocode", "sparkles", || Box::new(About));
        add("Window", "Minimize", "minus-sign", || {
            Box::new(MinimizeWindow)
        });
        add("Window", "Zoom", "window-maximize", || Box::new(ZoomWindow));

        // The composer's modes act on the conversation in front.
        let weak = cx.weak_entity();
        for (mode, title) in [
            ("plan", "Toggle Plan Mode"),
            ("operator", "Toggle Operator"),
            ("orchestrator", "Toggle Orchestrator"),
        ] {
            let weak = weak.clone();
            items.push(ActionItem {
                id: format!("Agent: {title}").into(),
                group: "Agent".into(),
                title: title.into(),
                keys: None,
                note: None,
                glyph: "ai-idea",
                run: Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if let Some(view) =
                            this.workspace.active.and_then(|id| this.sessions.get(&id))
                        {
                            view.update(cx, |v, cx| v.toggle_mode(mode, cx));
                        }
                    });
                }),
            });
        }
        // Settings' import rows, opened and in view (K61).
        if cfg!(target_os = "macos") {
            let weak = weak.clone();
            items.push(ActionItem {
                id: "Browser: Import from Another Browser…".into(),
                group: "Browser".into(),
                title: "Import from Another Browser…".into(),
                keys: None,
                note: Some("Chrome, Arc or Brave: cookies and passwords".into()),
                glyph: "globe",
                run: Rc::new(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.open_settings(crate::settings::Section::Browser, window, cx);
                        if let Some(settings) = this.settings.clone() {
                            settings.update(cx, |view, cx| view.open_browser_import(cx));
                        }
                    });
                }),
            });
        }
        // What the code features offer right now (run configurations…).
        let scope = crate::code_shell::context::scope(self.working_copy(), cx);
        for command in crate::code_shell::commands::commands(&scope, cx) {
            let group = command
                .title
                .split_once(':')
                .map(|(group, _)| group.trim().to_owned())
                .unwrap_or_else(|| "Code".into());
            items.push(ActionItem {
                id: command.title.clone(),
                group: group.into(),
                title: command.title.clone(),
                keys: None,
                note: (!command.subtitle.is_empty()).then(|| command.subtitle.clone()),
                glyph: "play",
                run: command.run.clone(),
            });
        }
        items.iter_mut().for_each(|item| {
            if item.id.as_ref() == "Search: Search Everywhere" {
                item.keys = Some("⇧⇧".into());
            }
        });
        items
    }

    // ---- Search Everywhere ---------------------------------------------

    fn everywhere_sources(&self, window: &Window, cx: &mut Context<Self>) -> everywhere::Sources {
        let conversations = self
            .sessions
            .values()
            .cloned()
            .chain(
                self.monos
                    .as_ref()
                    .map(|m| m.read(cx).search_conversations())
                    .unwrap_or_default(),
            )
            .collect::<Vec<_>>();
        let mut summaries: HashMap<_, _> = self
            .order
            .iter()
            .map(|&id| (id, self.session_metadata(id, cx).summary.clone()))
            .collect();
        summaries.extend(
            conversations
                .iter()
                .map(|s| (s.read(cx).summary.id, s.read(cx).summary.clone())),
        );
        let live = conversations
            .iter()
            .filter(|s| {
                s.read(cx).history_loaded() && s.read(cx).transcript.read(cx).history_complete()
            })
            .map(|s| {
                (
                    s.read(cx).summary.id,
                    Arc::new(s.read(cx).transcript.read(cx).history_rows().to_vec()),
                )
            })
            .collect();
        let cwd = self.working_copy().to_owned();
        everywhere::Sources {
            recent_files: (self.workspace.recent_files.get(&cwd).cloned()).unwrap_or_default(),
            cwd,
            store: self.store.clone(),
            summaries: summaries.into_values().collect(),
            live,
            projects: self.workspace.projects.clone(),
            logos: self
                .workspace
                .project_appearance
                .iter()
                .filter_map(|(path, a)| a.logo.clone().map(|logo| (path.clone(), logo)))
                .collect(),
            actions: self.action_catalog(window, cx),
            include_ignored: crate::settings::show_excluded_files(cx),
        }
    }

    /// Opens Search Everywhere on `tab` (the last one used when `None`),
    /// with `query` typed (the last query, selected, when `None`).
    pub(crate) fn open_everywhere(
        &mut self,
        tab: Option<Tab>,
        query: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(open) = &self.everywhere {
            let view = open.view.clone();
            view.update(cx, |view, cx| {
                if let Some(tab) = tab {
                    view.select_tab(tab, cx);
                }
                if let Some(query) = query {
                    view.set_query(query, cx);
                }
            });
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        let restore = self
            .palette
            .as_ref()
            .and_then(|o| o.restore.clone())
            .or_else(|| window.focused(cx));
        self.palette = None;
        self.close_menu(cx);
        self.close_preview(cx);
        if self.app_search.as_ref().is_some_and(|s| s.panel) {
            self.close_app_search(window, cx);
        }
        let (saved_query, saved_tab) = self.saved_everywhere.clone().unwrap_or_default();
        let sources = self.everywhere_sources(window, cx);
        let tab = tab.unwrap_or(saved_tab);
        let view = cx.new(|cx| Everywhere::new(sources, tab, cx));
        let query = query.unwrap_or(saved_query);
        if !query.is_empty() {
            view.update(cx, |view, cx| view.set_query(query, cx));
        }
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &everywhere::Event, window, cx| {
                use everywhere::Event as E;
                if matches!(event, E::Close) {
                    this.close_everywhere(window, cx);
                    return;
                }
                this.close_everywhere(window, cx);
                match event {
                    E::Close => {}
                    E::OpenFile { path, at } => {
                        // A file from another open project opens in that project.
                        let cwd = this.switch_to_file_project(path, window, cx);
                        let at = at.map(|(line, column)| (line as usize, column as usize));
                        this.open_file_at(cwd, path.clone(), at, window, cx);
                    }
                    E::Conversation(id) => this.open_search_session(*id, None, window, cx),
                    E::Message {
                        session,
                        row,
                        query,
                    } => {
                        this.open_search_session(*session, Some((*row, query.clone())), window, cx)
                    }
                    E::Project(path) => this.select_project(path.clone(), window, cx),
                    E::Run(run) => {
                        let run = run.clone();
                        window.defer(cx, move |window, cx| run(window, cx));
                    }
                    E::AskAgent(prompt) => {
                        this.next_session_draft = Some(prompt.clone());
                        this.new_chat(cx);
                    }
                    E::FindInPanel(query) => this.find_in_panel(query.clone(), window, cx),
                }
            },
        );
        window.focus(&view.focus_handle(cx), cx);
        self.everywhere = Some(EverywhereOpen {
            view,
            restore,
            _events: events,
        });
        cx.notify();
    }

    pub(crate) fn close_everywhere(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.everywhere.take() {
            self.saved_everywhere = Some(open.view.read(cx).draft(cx));
            if let Some(restore) = open.restore {
                window.focus(&restore, cx);
            } else {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    /// Types `query` into the Explorer's search panel, where results stay
    /// beside the files with include and exclude filters.
    fn find_in_panel(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        window.dispatch_action(Box::new(SearchFiles), cx);
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let _ = weak.update(cx, |this, cx| {
                if let Some(panel) = &this.right_panel {
                    let explorer = panel.read(cx).explorer.clone();
                    explorer.update(cx, |explorer, cx| explorer.search_for(query, window, cx));
                }
            });
        });
    }

    pub(super) fn everywhere_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.everywhere.as_ref()?;
        let viewport = window.viewport_size();
        let width =
            everywhere::modal_width(viewport.width.as_f32(), open.view.read(cx).wants_wide(cx));
        let anchor = Anchor::Point(gpui::point(
            px((viewport.width.as_f32() - width) / 2.),
            px((viewport.height.as_f32() * 0.12).max(28.)),
        ));
        let weak = cx.weak_entity();
        Some(
            div()
                .absolute()
                .size_full()
                .child(crate::modal::scrim("everywhere-scrim"))
                .child(
                    popover("everywhere-modal", anchor)
                        .modal_surface(self.workspace.boxy_layout())
                        .width(width)
                        .gap(0.)
                        .priority(80)
                        .on_dismiss(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.close_everywhere(window, cx));
                        })
                        .child(open.view.clone()),
                )
                .into_any_element(),
        )
    }

    pub(super) fn sync_everywhere_session(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(open) = &self.everywhere else {
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let session = session.read(cx);
        let transcript = session.transcript.read(cx);
        if !transcript.history_complete() {
            return;
        }
        let summary = session.summary.clone();
        let rows = Arc::new(transcript.history_rows().to_vec());
        open.view
            .update(cx, |v, cx| v.update_session(summary, rows, cx));
    }

    // ---- the command palette -------------------------------------------

    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = &self.palette {
            window.focus(&open.picker.focus_handle(cx), cx);
            return;
        }
        let restore = self
            .everywhere
            .as_ref()
            .and_then(|o| o.restore.clone())
            .or_else(|| window.focused(cx));
        if let Some(open) = self.everywhere.take() {
            self.saved_everywhere = Some(open.view.read(cx).draft(cx));
        }
        self.close_menu(cx);
        self.close_preview(cx);
        if self.app_search.as_ref().is_some_and(|s| s.panel) {
            self.close_app_search(window, cx);
        }
        let actions = self.action_catalog(window, cx);
        let picker = cx.new(|cx| CommandPalette::new(actions, cx));
        let events = cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &command_palette::Event, window, cx| {
                this.close_palette(window, cx);
                match event {
                    command_palette::Event::Run(run) => {
                        let run = run.clone();
                        window.defer(cx, move |window, cx| run(window, cx));
                    }
                    command_palette::Event::SearchEverywhere(query) => {
                        this.open_everywhere(Some(Tab::All), Some(query.clone()), window, cx)
                    }
                    command_palette::Event::Dismiss => {}
                }
            },
        );
        window.focus(&picker.focus_handle(cx), cx);
        self.palette = Some(PaletteOpen {
            picker,
            restore,
            _events: events,
        });
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.palette.take() {
            if let Some(restore) = open.restore {
                window.focus(&restore, cx);
            } else {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    /// Types into the open palette's search box.
    #[cfg(test)]
    pub(super) fn set_palette_query(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(open) = &self.palette {
            let search = open.picker.read(cx).search.clone();
            search.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    /// Types into the open Search Everywhere.
    #[cfg(test)]
    pub(super) fn set_everywhere_query(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(open) = &self.everywhere {
            let input = open.view.read(cx).input.clone();
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
    }

    pub(super) fn palette_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.palette.as_ref()?;
        let viewport = window.viewport_size();
        let width = 600f32.min(viewport.width.as_f32() - 32.);
        let anchor = Anchor::Point(gpui::point(
            px((viewport.width.as_f32() - width) / 2.),
            px((viewport.height.as_f32() * 0.14).max(32.)),
        ));
        let weak = cx.weak_entity();
        Some(
            div()
                .absolute()
                .size_full()
                .child(crate::modal::scrim("command-palette-scrim"))
                .child(
                    popover("command-palette-modal", anchor)
                        .modal_surface(self.workspace.boxy_layout())
                        .width(width)
                        .gap(0.)
                        .priority(80)
                        .on_dismiss(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| this.close_palette(window, cx));
                        })
                        .child(open.picker.clone()),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::time::Duration;

    fn shift(on: bool) -> Modifiers {
        Modifiers {
            shift: on,
            ..Default::default()
        }
    }

    #[test]
    fn two_quick_shift_taps_trigger_and_typing_does_not() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut taps = ShiftTaps::default();
        assert!(!taps.modifiers(&shift(true), at(0)));
        assert!(!taps.modifiers(&shift(false), at(60)));
        assert!(!taps.modifiers(&shift(true), at(150)));
        assert!(taps.modifiers(&shift(false), at(210)), "a double tap");

        // Shift held to type a capital: a key comes between.
        let mut taps = ShiftTaps::default();
        taps.modifiers(&shift(true), at(0));
        taps.reset();
        taps.modifiers(&shift(false), at(60));
        taps.modifiers(&shift(true), at(120));
        assert!(!taps.modifiers(&shift(false), at(180)));

        // Too slow between taps.
        let mut taps = ShiftTaps::default();
        taps.modifiers(&shift(true), at(0));
        taps.modifiers(&shift(false), at(50));
        taps.modifiers(&shift(true), at(700));
        assert!(!taps.modifiers(&shift(false), at(750)));

        // Releases that arrive without their presses still make a double tap,
        // and one that arrives both ways is counted once.
        let mut taps = ShiftTaps::default();
        assert!(!taps.released(at(60)));
        assert!(taps.released(at(210)));
        let mut taps = ShiftTaps::default();
        taps.modifiers(&shift(true), at(0));
        assert!(!taps.modifiers(&shift(false), at(60)));
        assert!(!taps.released(at(61)), "the same release again");
        taps.modifiers(&shift(true), at(150));
        assert!(taps.modifiers(&shift(false), at(210)));
        assert!(!taps.released(at(211)));

        // A held Shift is not a tap.
        let mut taps = ShiftTaps::default();
        taps.modifiers(&shift(true), at(0));
        taps.modifiers(&shift(false), at(900));
        taps.modifiers(&shift(true), at(950));
        assert!(!taps.modifiers(&shift(false), at(1000)));

        // Shift with another modifier (⌘⇧P) is not a tap.
        let mut taps = ShiftTaps::default();
        taps.modifiers(&shift(true), at(0));
        taps.modifiers(&shift(false), at(40));
        let both = Modifiers {
            shift: true,
            platform: true,
            ..Default::default()
        };
        taps.modifiers(&both, at(100));
        assert!(!taps.modifiers(&shift(false), at(140)));
    }

    #[gpui::test]
    fn double_shift_opens_search_everywhere_and_again_closes_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            crate::popover::bind(cx);
        });
        let (window, _, active) = crate::shell::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                let input = shell.sessions[&active].read(cx).input.clone();
                input.update(cx, |i, cx| i.set_text("keep this draft".into(), cx));
                w.focus(&input.focus_handle(cx), cx);
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        for _ in 0..2 {
            visual.simulate_modifiers_change(shift(true));
            visual.simulate_modifiers_change(shift(false));
        }
        visual.run_until_parked();
        window
            .update(visual, |shell, _, _| assert!(shell.everywhere.is_some()))
            .unwrap();
        for _ in 0..2 {
            visual.simulate_modifiers_change(shift(true));
            visual.simulate_modifiers_change(shift(false));
        }
        visual.run_until_parked();
        window
            .update(visual, |shell, w, cx| {
                assert!(shell.everywhere.is_none());
                let input = &shell.sessions[&active].read(cx).input;
                assert_eq!(input.read(cx).content, "keep this draft");
                assert!(input.focus_handle(cx).is_focused(w));
            })
            .unwrap();
    }

    #[gpui::test]
    fn double_shift_opens_search_everywhere_from_every_app_view(cx: &mut TestAppContext) {
        use super::super::app_views::AppView;
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            crate::popover::bind(cx);
        });
        let (window, _, _) = crate::shell::tests::window(cx);
        let db = tempfile::tempdir().unwrap();
        for view in [AppView::Inbox, AppView::Database, AppView::Notes] {
            window
                .update(cx, |shell, w, cx| {
                    shell.database_dir = db.path().to_owned();
                    shell.open_app(view, w, cx);
                })
                .unwrap();
            let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
            visual.run_until_parked();
            for _ in 0..2 {
                visual.simulate_modifiers_change(shift(true));
                visual.simulate_modifiers_change(shift(false));
            }
            visual.run_until_parked();
            window
                .update(visual, |shell, w, cx| {
                    assert!(shell.everywhere.is_some(), "from the {view:?} view");
                    shell.close_everywhere(w, cx);
                    // A click on something that takes no focus leaves the
                    // window with none.
                    w.blur(cx);
                })
                .unwrap();
            visual.run_until_parked();
            for _ in 0..2 {
                visual.simulate_modifiers_change(shift(true));
                visual.simulate_modifiers_change(shift(false));
            }
            visual.run_until_parked();
            window
                .update(visual, |shell, w, cx| {
                    assert!(
                        shell.everywhere.is_some(),
                        "from the {view:?} view with nothing focused"
                    );
                    shell.close_everywhere(w, cx);
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn cmd_p_opens_the_command_palette_and_cmd_k_search_everywhere(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            crate::popover::bind(cx);
        });
        let (window, _, active) = crate::shell::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                let input = shell.sessions[&active].read(cx).input.clone();
                input.update(cx, |i, cx| i.set_text("keep this draft".into(), cx));
                w.focus(&input.focus_handle(cx), cx);
            })
            .unwrap();
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(window.into(), &format!("{modifier}-p"));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let open = shell.palette.as_ref().expect("the palette is open");
                let rows = open.picker.read(cx).row_labels();
                assert!(rows.iter().any(|(title, _)| title == "Search Everywhere"));
                assert!(shell.everywhere.is_none());
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "escape");
        cx.run_until_parked();
        window
            .update(cx, |shell, w, cx| {
                assert!(shell.palette.is_none());
                let input = &shell.sessions[&active].read(cx).input;
                assert_eq!(input.read(cx).content, "keep this draft");
                assert!(input.focus_handle(cx).is_focused(w));
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), &format!("{modifier}-k"));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert!(shell.everywhere.is_some());
                assert!(shell.palette.is_none());
            })
            .unwrap();
        // ⌘P from Search Everywhere swaps one modal for the other.
        cx.simulate_keystrokes(window.into(), &format!("{modifier}-p"));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert!(shell.palette.is_some());
                assert!(shell.everywhere.is_none());
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_palette_command_runs_its_action(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            crate::popover::bind(cx);
        });
        let (window, _, _) = crate::shell::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_palette(w, cx);
                shell.set_palette_query("toggle sidebar".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let before = window
            // In Classic, Toggle Sidebar shows and hides the project rail.
            .update(cx, |shell, _, _| shell.workspace.project_rail_open)
            .unwrap();
        window
            .update(cx, |shell, _, cx| {
                let picker = shell.palette.as_ref().unwrap().picker.clone();
                let rows = picker.read(cx).row_labels();
                assert_eq!(rows[0].0, "Toggle Sidebar", "{rows:?}");
                picker.update(cx, |p, cx| p.pick_row(0, cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.palette.is_none());
                assert_ne!(shell.workspace.project_rail_open, before);
                let history = cx.global::<everywhere::ActionHistory>();
                assert_eq!(history.0[0].as_ref(), "View: Toggle Sidebar");
            })
            .unwrap();
    }

    fn project_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "--quiet"]);
        for file in files {
            let full = dir.path().join(file);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, "x = 1\nthe needle is here\n").unwrap();
        }
        git(&["add", "."]);
        dir
    }

    #[gpui::test]
    fn search_everywhere_finds_files_and_contents_and_opens_them(cx: &mut TestAppContext) {
        let dir = project_with(&["app/utils.py", "app/models.py", "docs/notes.md"]);
        let cwd = dir.path().to_string_lossy().to_string();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.workspace.project = cwd.clone();
                shell.open_everywhere(Some(Tab::Files), Some(String::new()), w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                shell.set_everywhere_query("models".into(), cx)
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let view = &shell.everywhere.as_ref().unwrap().view;
                let rows = view.read(cx).row_labels();
                assert_eq!(
                    rows.first().map(|r| r.0.as_str()),
                    Some("models.py"),
                    "{rows:?}"
                );
            })
            .unwrap();
        // Content tab: inside the files, opened at the match.
        window
            .update(cx, |shell, _, cx| {
                let view = shell.everywhere.as_ref().unwrap().view.clone();
                view.update(cx, |v, cx| v.select_tab(Tab::Content, cx));
                shell.set_everywhere_query("needle".into(), cx);
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(600));
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let view = shell.everywhere.as_ref().unwrap().view.clone();
                let rows = view.read(cx).row_labels();
                assert_eq!(rows.len(), 3, "{rows:?}");
                assert!(
                    rows.iter()
                        .all(|(title, detail)| title == "the needle is here"
                            && detail.ends_with(":2"))
                );
                view.update(cx, |v, cx| v.pick_row(0, cx));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert!(shell.everywhere.is_none());
                assert!(shell.workspace.recent_files[&cwd][0].starts_with(&cwd));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_name_that_several_files_share_opens_search_everywhere_on_it(cx: &mut TestAppContext) {
        let dir = project_with(&["a/utils.py", "b/utils.py"]);
        let cwd = dir.path().to_string_lossy().to_string();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_loose_reference("utils.py".into(), cwd.clone(), w, cx)
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let open = shell.everywhere.as_ref().expect("the user picks which");
                assert_eq!(open.view.read(cx).input.read(cx).content, "utils.py");
                assert_eq!(open.view.read(cx).tab(), Tab::Files);
                assert!(
                    shell
                        .workspace
                        .recent_files
                        .get(&cwd)
                        .is_none_or(Vec::is_empty)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_bare_file_name_in_a_reply_opens_the_one_file_that_has_it(cx: &mut TestAppContext) {
        let dir = project_with(&["app/utils.py", "app/models.py", "docs/notes.md"]);
        let cwd = dir.path().to_string_lossy().to_string();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_loose_reference("models.py:7".into(), cwd.clone(), w, cx)
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let opened = shell
                    .workspace
                    .recent_files
                    .get(&cwd)
                    .and_then(|f| f.first());
                assert_eq!(
                    opened.map(String::as_str),
                    Some(dir.path().join("app/models.py").to_string_lossy().as_ref())
                );
                assert!(shell.everywhere.is_none());
            })
            .unwrap();
    }

    /// What a click on a reply's file chip does: the chip's link is the
    /// reference joined to the session's folder, sent as `OpenPath`.
    #[gpui::test]
    fn a_chip_naming_a_file_without_its_directory_opens_it_at_the_line(cx: &mut TestAppContext) {
        let dir = project_with(&[
            "application/core/utils/user_login_utils.py",
            "app/models.py",
        ]);
        let cwd = dir.path().to_string_lossy().to_string();
        let (window, _, _) = super::super::tests::window(cx);
        let runs = vec![crate::markdown_model::InlineRun {
            text: "user_login_utils.py:399".into(),
            style: crate::markdown_model::InlineStyle {
                code: true,
                ..Default::default()
            },
        }];
        let mut row =
            crate::markdown::rows(&crate::markdown_model::Block::Paragraph { runs }).remove(0);
        crate::markdown::resolve_file_links(&mut row, &cwd);
        let crate::markdown::Part::Text { runs, .. } = &row.part else {
            panic!("a paragraph");
        };
        let link = runs.iter().find_map(|run| run.style.link.clone()).unwrap();
        assert_eq!(link, format!("{cwd}/user_login_utils.py:399:1"));
        window
            .update(cx, |shell, w, cx| {
                let project = cwd.clone();
                shell.git.update(cx, |git, cx| git.set_project(project, cx));
                w.focus(&shell.focus, cx);
                w.dispatch_action(Box::new(crate::file_editor::OpenPath(link.clone())), cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                let opened = (shell.workspace.recent_files.get(&cwd)).and_then(|f| f.first());
                let file = dir
                    .path()
                    .join("application/core/utils/user_login_utils.py");
                assert_eq!(
                    opened.map(String::as_str),
                    Some(file.to_string_lossy().as_ref())
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_missing_name_still_ends_in_the_usual_not_found_tab(cx: &mut TestAppContext) {
        let dir = project_with(&["a/utils.py"]);
        let cwd = dir.path().to_string_lossy().to_string();
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, w, cx| {
                shell.open_loose_reference("nothing.py".into(), cwd.clone(), w, cx)
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert_eq!(
                    shell.workspace.recent_files[&cwd][0],
                    dir.path().join("nothing.py").to_string_lossy()
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_file_picked_from_an_app_view_leaves_the_view_and_shows_the_file(cx: &mut TestAppContext) {
        use super::super::app_views::AppView;
        let dir = project_with(&["app/utils.py"]);
        let cwd = dir.path().to_string_lossy().to_string();
        let path = dir
            .path()
            .join("app/utils.py")
            .to_string_lossy()
            .to_string();
        let (window, _, _) = super::super::tests::window(cx);
        let db = tempfile::tempdir().unwrap();
        for view in [AppView::Database, AppView::Inbox, AppView::Notes] {
            window
                .update(cx, |shell, w, cx| {
                    shell.database_dir = db.path().to_owned();
                    shell.open_app(view, w, cx);
                    assert_eq!(shell.active_app_view(), Some(view));
                    shell.open_file_at(cwd.clone(), path.clone(), None, w, cx);
                })
                .unwrap();
            cx.run_until_parked();
            window
                .update(cx, |shell, _, _| {
                    assert!(
                        !shell.app_view_open(),
                        "the file is in front of the {view:?} tab"
                    );
                    assert_eq!(shell.workspace.recent_files[&cwd][0], path);
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn a_file_of_another_open_project_opens_in_that_project(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind(cx);
            super::super::bind_keys(cx);
            crate::popover::bind(cx);
        });
        let (window, _, _) = crate::shell::tests::window(cx);
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let path = |d: &tempfile::TempDir| d.path().to_string_lossy().into_owned();
        let (a, b) = (path(&one), path(&two));
        window
            .update(cx, |shell, w, cx| {
                shell.workspace.projects = vec![a.clone(), b.clone()];
                shell.select_project(a.clone(), w, cx);
                shell.persist(cx);
                // This project's file: no switch.
                let cwd = shell.switch_to_file_project(&format!("{a}/src/x.rs"), w, cx);
                assert_eq!(cwd, a);
                assert_eq!(shell.workspace.project, a);
                // Another open project's file: its project comes to the front.
                let cwd = shell.switch_to_file_project(&format!("{b}/src/y.rs"), w, cx);
                assert_eq!(cwd, b);
                assert_eq!(shell.workspace.project, b);
                // A file in no open project stays here.
                let cwd = shell.switch_to_file_project("/elsewhere/z.rs", w, cx);
                assert_eq!(cwd, shell.working_copy());
                assert_eq!(shell.workspace.project, b);
            })
            .unwrap();
    }
}
