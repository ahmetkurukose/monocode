//! The shell's side of `code_shell`: what the code features' extension
//! points do to the window. The features call `crate::code_shell`; that calls
//! in here, and the rest of the shell calls in from a handful of one-line
//! hooks (the frame, the dock header, the footer, the project menu).
// Half of this is reached only through `code_shell`, whose callers are the
// feature modules.
#![allow(dead_code)]
use super::*;
use crate::{
    code_shell::{
        chips::{self, Tone},
        context::{Checkout, FocusedFile, Snapshot},
        dock::{DockArea, DockPlacement, DockTab, DockTabHandle, Run, RunRequest},
        settings::{self, DialogEvent, PageContext, PageDialog, PageHost},
    },
    right_panel::RightTab,
    terminal::{hosted::Hosted, run::Program},
};
use monocode_core::files::path_key;
use std::hash::{Hash, Hasher};

/// What the shell keeps for the code features' chrome.
#[derive(Default)]
pub(super) struct CodeChrome {
    /// Where each footer chip is, for hanging its list from it.
    chip_anchors: HashMap<&'static str, AnchorSlot>,
    /// The saved settings pages' values have been published once.
    published: bool,
}

/// A footer chip's tooltip, as the rail's icons show theirs.
struct ChipTip(SharedString);
impl Render for ChipTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(pane_fill())
            .border_1()
            .border_color(content(0.15))
            .text_size(px(12.))
            .text_color(content(1.))
            .child(self.0.clone())
    }
}

fn tone_color(tone: Tone) -> gpui::Hsla {
    match tone {
        Tone::Quiet => content(0.55),
        Tone::Busy => rgb(0xe39b4a).into(),
        Tone::Good => rgb(0x10b981).into(),
        Tone::Warning => rgb(0xfbbf24).into(),
        Tone::Error => rgb(0xf87171).into(),
    }
}

impl Shell {
    /// Redraws everything a feature's contribution shows in.
    pub(crate) fn code_refresh(&mut self, cx: &mut Context<Self>) {
        self.invalidate_chrome(cx);
        cx.notify();
    }

    // Context.

    /// The project as the workspace writes it, however `path` is written.
    fn code_project(&self, path: &str) -> Result<String, String> {
        if path.starts_with("remote://") {
            return Err("Terminals are not available on remote projects.".into());
        }
        let key = path_key(path);
        (self.workspace.projects.iter())
            .find(|project| path_key(project) == key)
            .cloned()
            .ok_or_else(|| "This project is not open.".into())
    }

    fn code_focused_editor(
        &self,
        cx: &App,
    ) -> Option<(i64, Entity<crate::file_editor::FileEditor>)> {
        let pane = self.workspace.active?;
        let editor = self.reviews.get(&pane)?.read(cx).active_editor()?;
        Some((pane, editor))
    }

    /// A number that changes whenever `code_snapshot` would.
    pub(crate) fn code_stamp(&self, cx: &App) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.workspace.project.hash(&mut hasher);
        self.workspace.projects.hash(&mut hasher);
        self.working_copy().hash(&mut hasher);
        for (project, focus) in &self.worktree_focus {
            (project, &focus.path).hash(&mut hasher);
        }
        for (session, folder) in &self.session_worktrees {
            (session, folder).hash(&mut hasher);
        }
        if let Some((pane, editor)) = self.code_focused_editor(cx) {
            (pane, editor.entity_id(), &editor.read(cx).path).hash(&mut hasher);
        }
        for project in &self.workspace.projects {
            self.project_asleep(project).hash(&mut hasher);
        }
        hasher.finish()
    }

    pub(crate) fn code_snapshot(&self, cx: &App) -> Snapshot {
        let projects: Vec<String> = (self.workspace.projects.iter())
            .filter(|project| !project.starts_with("remote://"))
            .cloned()
            .collect();
        let mut checkouts: Vec<Checkout> = Vec::new();
        let mut add = |project: &str, folder: &str| {
            let known = (checkouts.iter()).any(|known| path_key(&known.folder) == path_key(folder));
            if !known && projects.iter().any(|open| open == project) {
                checkouts.push(Checkout {
                    project: project.to_owned(),
                    folder: folder.to_owned(),
                });
            }
        };
        for project in &projects {
            add(project, project);
        }
        for (project, focus) in &self.worktree_focus {
            add(project, &focus.path);
        }
        for (session, folder) in &self.session_worktrees {
            if let (Some(folder), Some(summary)) = (folder, self.session_summaries.get(session)) {
                add(&summary.cwd, folder);
            }
        }
        let focus = Checkout {
            project: self.workspace.project.clone(),
            folder: self.working_copy().to_owned(),
        };
        let focused = self.code_focused_editor(cx).map(|(pane, editor)| {
            let path = editor.read(cx).path.clone();
            let checkout = (checkouts.iter())
                .filter(|checkout| {
                    std::path::Path::new(&path_key(&path)).starts_with(path_key(&checkout.folder))
                })
                .max_by_key(|checkout| path_key(&checkout.folder).len())
                .cloned()
                .or_else(|| self.code_pane_checkout(pane, cx))
                .unwrap_or_else(|| focus.clone());
            FocusedFile {
                path,
                checkout,
                pane,
                editor: editor.downgrade(),
            }
        });
        let asleep = (projects.iter())
            .filter(|project| self.project_asleep(project))
            .cloned()
            .collect();
        Snapshot {
            focus,
            projects,
            checkouts,
            focused,
            asleep,
        }
    }

    /// The checkout a pane works in: its session's project and working copy.
    pub(crate) fn code_pane_checkout(&self, pane: i64, cx: &App) -> Option<Checkout> {
        let session = self.workspace.session_for_pane(pane);
        let summary = self.session_summaries.get(&session)?;
        Some(Checkout {
            project: summary.cwd.clone(),
            folder: self.session_working_copy(session, cx).to_owned(),
        })
    }

    /// A session's title as the rail shows it.
    pub(crate) fn code_session_label(&self, session: i64, cx: &App) -> Option<SharedString> {
        let known =
            self.sessions.contains_key(&session) || self.session_summaries.contains_key(&session);
        known.then(|| self.session_metadata(session, cx).display_title().into())
    }

    pub(crate) fn code_open_file(
        &mut self,
        folder: Option<String>,
        path: String,
        at: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = folder.unwrap_or_else(|| self.working_copy().to_owned());
        self.open_file_at(cwd, path, at, window, cx);
    }

    pub(crate) fn code_open_url(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panes = self.workspace.browser_panes.len();
        self.new_browser_pane(Some(url), window, cx);
        self.workspace.browser_panes.len() > panes
    }

    /// The running work dialog, where the features' processes are listed.
    pub(crate) fn open_running_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resources
            .update(cx, |monitor, cx| monitor.open(window, cx));
        cx.notify();
    }

    // Dock tabs.

    /// Adds a run's or a view's tab to `project`'s dock, beside its
    /// terminals, and shows it when asked to and the project is in front.
    #[allow(clippy::too_many_arguments)]
    fn add_dock_tab(
        &mut self,
        project: &str,
        cwd: String,
        reveal: bool,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(&mut Context<Self>) -> Entity<Terminal>,
    ) -> Entity<Terminal> {
        // A restored dock's shells come first, so the lists stay in step.
        self.ensure_terminals(project, cx);
        let terminal = build(cx);
        self.observations
            .push(cx.subscribe(&terminal, |_, _, _: &TerminalEvent, cx| cx.notify()));
        let viewport = self.viewport();
        let dock = self.workspace.docks.entry(project.to_owned()).or_default();
        let had_tabs = !dock.terminals.is_empty();
        let (was_open, was_active) = (dock.open && had_tabs, dock.active);
        dock.size = dock.side.clamp(dock.size, viewport);
        let added = dock.add_transient(cwd);
        if !reveal {
            dock.open = was_open;
            if had_tabs {
                dock.active = was_active;
            }
        }
        self.terminals
            .entry(project.to_owned())
            .or_default()
            .push(terminal.clone());
        if reveal && project == self.workspace.project {
            self.dock_tab_scroll.scroll_to_item(added);
            if self.terminal_in_panel(cx) {
                if let Some(dock) = self.workspace.docks.get_mut(project) {
                    dock.open = false;
                }
                self.show_panel_terminal(window, cx);
            } else {
                if !was_open {
                    self.slide_dock(true, cx);
                }
                if focus {
                    self.focus_terminal(window, cx);
                }
                self.fit_sidebar(cx);
            }
        }
        self.persist(cx);
        cx.notify();
        terminal
    }

    pub(crate) fn run_in_dock(
        &mut self,
        request: RunRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Run>, String> {
        let project = self.code_project(&request.checkout.project)?;
        let RunRequest {
            spawn,
            checkout,
            agent,
            reveal,
            focus,
        } = request;
        let cwd = match spawn.cwd.as_os_str().is_empty() {
            true => checkout.folder.clone(),
            false => spawn.cwd.to_string_lossy().into_owned(),
        };
        // What a terminal of this checkout starts with, the way a process
        // that is not a shell gets it; then the run's own variables.
        let mut env = self
            .terminal_launch(&project, &checkout.folder)
            .process_environment(&monocode_core::scm::terminal_search_path());
        env.extend(spawn.env);
        let program = Program {
            label: spawn.label,
            program: spawn.command,
            args: spawn.args,
            env,
        };
        let folder = cwd.clone();
        let terminal = self.add_dock_tab(&project, cwd, reveal, focus, window, cx, |cx| {
            cx.new(|cx| Terminal::for_run(folder, program, agent, cx))
        });
        let run = terminal.read(cx).run_handle();
        run.ok_or_else(|| "The run could not be started.".into())
    }

    pub(crate) fn add_dock_view(
        &mut self,
        tab: DockTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<DockTabHandle, String> {
        let project = self.code_project(&tab.checkout.project)?;
        if self.find_dock_view(&project, &tab.id, cx).is_some() {
            return Err(format!("The dock already has a tab named {}.", tab.id));
        }
        let (cwd, reveal, title) = (tab.checkout.folder, tab.reveal, tab.title.to_string());
        let hosted = Hosted {
            id: tab.id,
            view: tab.view,
            focus: tab.focus,
            icon: tab.icon,
            controls: tab.controls,
            placement: None,
            on_placement: tab.on_placement,
            on_close: tab.on_close,
        };
        let folder = cwd.clone();
        let terminal = self.add_dock_tab(&project, cwd, reveal, reveal, window, cx, |cx| {
            cx.new(|cx| Terminal::hosted(folder, title, hosted, cx))
        });
        Ok(DockTabHandle(terminal.downgrade()))
    }

    fn dock_tab_position(&self, tab: &Entity<Terminal>) -> Option<(String, usize)> {
        self.terminals.iter().find_map(|(project, tabs)| {
            let ix = tabs.iter().position(|known| known == tab)?;
            Some((project.clone(), ix))
        })
    }

    pub(crate) fn reveal_dock_tab(
        &mut self,
        tab: &Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((project, ix)) = self.dock_tab_position(tab) else {
            return false;
        };
        if project != self.workspace.project {
            self.select_project(project.clone(), window, cx);
        }
        let Some(dock) = self.workspace.docks.get_mut(&project) else {
            return false;
        };
        let was_open = dock.open;
        dock.active = ix.min(dock.terminals.len().saturating_sub(1));
        dock.open = true;
        self.dock_tab_scroll.scroll_to_item(ix);
        if self.terminal_in_panel(cx) {
            if let Some(dock) = self.workspace.docks.get_mut(&project) {
                dock.open = false;
            }
            self.show_panel_terminal(window, cx);
        } else {
            if !was_open {
                self.slide_dock(true, cx);
            }
            self.focus_terminal(window, cx);
            self.fit_sidebar(cx);
        }
        self.persist(cx);
        cx.notify();
        true
    }

    pub(crate) fn close_dock_tab(
        &mut self,
        tab: &Entity<Terminal>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((project, ix)) = self.dock_tab_position(tab) {
            self.remove_terminal(&project, ix, window, cx);
        }
    }

    pub(crate) fn find_dock_view(
        &self,
        project: &str,
        id: &str,
        cx: &App,
    ) -> Option<DockTabHandle> {
        let key = path_key(project);
        let (_, tabs) = (self.terminals.iter()).find(|(known, _)| path_key(known) == key)?;
        (tabs.iter())
            .find(|tab| (tab.read(cx).hosted_view()).is_some_and(|hosted| hosted.id == id))
            .map(|tab| DockTabHandle(tab.downgrade()))
    }

    /// A run or a view stays in the dock: pulled off the strip it would
    /// become a terminal pane, which a restart reopens as a shell.
    pub(super) fn dock_tab_stays(&self, ix: usize, cx: &App) -> bool {
        (self.terminals.get(&self.workspace.project))
            .and_then(|tabs| tabs.get(ix))
            .is_some_and(|tab| !tab.read(cx).is_shell())
    }

    /// Tells the views in the dock in front where it is and how wide.
    pub(crate) fn place_dock_tabs(&mut self, cx: &mut Context<Self>) {
        let project = &self.workspace.project;
        let Some(tabs) = self.terminals.get(project) else {
            return;
        };
        if !tabs.iter().any(|tab| tab.read(cx).hosted_view().is_some()) {
            return;
        }
        let placement = if self.terminal_in_panel(cx) {
            DockPlacement {
                area: DockArea::Panel,
                extent: self.right_panel_width(),
            }
        } else {
            let Some(dock) = self.workspace.docks.get(project) else {
                return;
            };
            DockPlacement {
                area: match dock.side {
                    DockSide::Bottom => DockArea::Bottom,
                    DockSide::Top => DockArea::Top,
                    DockSide::Left => DockArea::Left,
                    DockSide::Right => DockArea::Right,
                },
                extent: dock.size,
            }
        };
        for tab in tabs.clone() {
            if tab.read(cx).hosted_view().is_some() {
                tab.update(cx, |tab, cx| tab.place(placement, cx));
            }
        }
    }

    /// The dock header's controls for the selected tab: a run's agent
    /// marker, rerun and stop, or the controls a view brought.
    pub(super) fn dock_tab_controls(
        &self,
        dock: &crate::workspace::TerminalDock,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let tab = (self.terminals.get(&self.workspace.project))?
            .get(dock.active)?
            .clone();
        if let Some(hosted) = tab.read(cx).hosted_view() {
            let controls = hosted.controls.clone()?;
            return Some(
                div()
                    .id("dock-tab-controls")
                    .debug_selector(|| "dock-tab-controls".into())
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .pr(px(6.))
                    .child(controls)
                    .into_any_element(),
            );
        }
        let run = tab.read(cx).run_handle()?;
        let (running, agent) = {
            let run = run.read(cx);
            (run.state().running(), run.agent())
        };
        let (rerun, stop) = (run.clone(), run);
        Some(
            div()
                .id("dock-run-controls")
                .debug_selector(|| "dock-run-controls".into())
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(2.))
                .pr(px(6.))
                .children(agent.map(|agent| {
                    div()
                        .id("dock-run-agent")
                        .debug_selector(|| "dock-run-agent".into())
                        .role(Role::Note)
                        .aria_label(SharedString::from(format!("Started by {agent}")))
                        .h(px(20.))
                        .max_w(px(180.))
                        .mr(px(4.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .rounded_full()
                        .bg(content(0.06))
                        .text_size(px(11.))
                        .text_color(content(0.6))
                        .child(icon("bot", 12.))
                        .child(div().min_w_0().truncate().child(agent))
                }))
                .child(
                    icon_button("rerun-dock-run", "Rerun", "rotate-ccw").on_click(
                        cx.listener(move |_, _, _, cx| rerun.update(cx, |run, cx| run.rerun(cx))),
                    ),
                )
                .child(
                    icon_button_enabled("stop-dock-run", "Stop", "square", running).when(
                        running,
                        |button| {
                            button.on_click(cx.listener(move |_, _, _, cx| {
                                stop.update(cx, |run, cx| run.stop(cx))
                            }))
                        },
                    ),
                )
                .into_any_element(),
        )
    }

    // Footer chips.

    /// The features' chips, drawn before the terminal controls.
    pub(super) fn code_chips(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let classic = self.workspace.boxy_layout();
        // With no terminal group after them the last chip takes the footer's inset.
        let alone = self.workspace.project.is_empty();
        let shown: Vec<_> = (chips::all(cx).into_iter())
            .filter_map(|chip| Some(((chip.state)(cx)?, chip.id)))
            .collect();
        let count = shown.len();
        let mut drawn = Vec::new();
        for (ix, (state, id)) in shown.into_iter().enumerate() {
            let anchor = self.code.chip_anchors.entry(id).or_default().clone();
            let open =
                matches!(&self.menu, Some(menu) if menu.target == MenuTarget::FooterChip(id));
            let last = ix + 1 == count;
            let chip = div()
                .id(id)
                .debug_selector(move || id.into())
                .role(Role::Button)
                .aria_label(state.text.clone())
                .aria_description(state.tooltip.clone())
                .aria_expanded(open)
                .tooltip({
                    let tip = state.tooltip.clone();
                    move |_, cx| cx.new(|_| ChipTip(tip.clone())).into()
                })
                .relative()
                .h(px(20.))
                .map(|d| {
                    if classic {
                        d.px(px(6.)).rounded_md()
                    } else {
                        d.px_2().rounded_full()
                    }
                })
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(6.))
                .whitespace_nowrap()
                .text_size(px(11.))
                .cursor_pointer()
                .map(|d| {
                    if open {
                        d.bg(content(0.08)).text_color(content(1.))
                    } else {
                        d.text_color(content(0.55))
                    }
                })
                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                .child(anchor.probe())
                .child(icon(state.icon, 14.).text_color(tone_color(state.tone)))
                .child(div().max_w(px(200.)).truncate().child(state.text))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if matches!(&this.menu, Some(menu) if menu.target == MenuTarget::FooterChip(id))
                    {
                        this.close_menu(cx);
                    } else if let Some(anchor) = this.code.chip_anchors.get(&id).cloned() {
                        this.open_menu(
                            MenuTarget::FooterChip(id),
                            Anchor::Slot(anchor),
                            window,
                            cx,
                        );
                    }
                }));
            drawn.push(
                div()
                    .flex_shrink_0()
                    .map(|d| match (classic, alone && last) {
                        (true, true) => d,
                        (true, false) => d.mr(px(terminal_dock::FOOTER_GAP)),
                        (false, true) => d.mr(px(6.)).pb(px(2.)),
                        (false, false) => d.mr(px(terminal_dock::FOOTER_GAP)).pb(px(2.)),
                    })
                    .child(chip)
                    .into_any_element(),
            );
        }
        drawn
    }

    /// A chip's list: its rows, each with its actions as a submenu.
    pub(super) fn code_chip_menu(
        &self,
        id: &str,
        cx: &App,
    ) -> (SharedString, Vec<MenuItem>, f32, MenuStyle) {
        let Some(chip) = chips::find(id, cx) else {
            return ("".into(), Vec::new(), 240., MenuStyle::Explorer);
        };
        let mut items: Vec<MenuItem> = ((chip.items)(cx).into_iter())
            .map(|item| {
                let mut row = MenuAction::new(item.id, item.title);
                if let Some(detail) = item.detail {
                    row = row.value(detail);
                }
                if let Some(path) = item.icon {
                    row = row.icon(path);
                    if item.tone != Tone::Quiet {
                        row = row.icon_color(tone_color(item.tone));
                    }
                }
                if !item.actions.is_empty() {
                    row = row.submenu(
                        (item.actions.into_iter())
                            .map(|(id, label)| MenuAction::new(id, label))
                            .collect(),
                    );
                }
                row.into()
            })
            .collect();
        if items.is_empty() {
            items.push(MenuItem::Note("Nothing to show right now.".into()));
        }
        (chip.label.into(), items, 280., MenuStyle::Explorer)
    }

    pub(super) fn run_code_chip_menu(
        &mut self,
        id: &str,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chip) = chips::find(id, cx) else {
            return;
        };
        let action = action.to_owned();
        // The feature may reach back into the shell; let this update end.
        window.defer(cx, move |window, cx| (chip.pick)(&action, window, cx));
    }

    // Right panel tabs.

    pub(crate) fn reveal_panel_tab(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ensure_right_panel(window, cx);
        let Some(panel) = self.right_panel.clone() else {
            return false;
        };
        let closing = panel.read(cx).tab();
        let tab = RightTab::Extra(crate::right_panel::ExtraTab(id));
        if closing != tab {
            self.corner_reveal = Some(crate::project_tabs::TabReveal::new(tab, closing));
            panel.update(cx, |panel, cx| panel.select(tab, cx));
        }
        self.set_right_panel_open(true, window, cx);
        self.invalidate_chrome(cx);
        cx.notify();
        self.workspace.right_panel_open && panel.read(cx).tab() == tab
    }

    // Project settings pages.

    /// The pages' rows for the project menu, after "Terminal setup…".
    pub(super) fn code_settings_menu_items(&self, cx: &App) -> Vec<MenuItem> {
        (settings::pages(cx).into_iter())
            .map(|page| {
                MenuAction::new(
                    SharedString::from(format!("code-settings:{}", page.id)),
                    SharedString::from(format!("{}…", page.title)),
                )
                .icon(page.icon)
                .into()
            })
            .collect()
    }

    fn code_setting(&self, project: &str, page: &str) -> Option<serde_json::Value> {
        let key = path_key(project);
        (self.workspace.project_code_settings.iter())
            .find(|(path, _)| path_key(path) == key)
            .and_then(|(_, pages)| pages.get(page).cloned())
    }

    pub(crate) fn save_code_setting(
        &mut self,
        project: &str,
        page: &str,
        value: Option<serde_json::Value>,
        cx: &mut Context<Self>,
    ) {
        let key = path_key(project);
        let saved = &mut self.workspace.project_code_settings;
        let path = (saved.keys().find(|path| path_key(path) == key).cloned())
            .unwrap_or_else(|| project.to_owned());
        let pages = saved.entry(path.clone()).or_default();
        match value {
            Some(value) => {
                pages.insert(page.to_owned(), value);
            }
            None => {
                pages.remove(page);
            }
        }
        if pages.is_empty() {
            saved.remove(&path);
        }
        self.publish_code_settings(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Replaces the project's run configurations; an empty set is dropped.
    pub(crate) fn save_project_runs(
        &mut self,
        project: &str,
        runs: monocode_core::code::run::ProjectRuns,
        cx: &mut Context<Self>,
    ) {
        let key = path_key(project);
        let saved = &mut self.workspace.project_runs;
        saved.retain(|path, _| path_key(path) != key);
        let runs = runs.normalized();
        if !runs.is_empty() {
            saved.insert(project.to_owned(), runs);
        }
        self.publish_code_settings(cx);
        self.persist(cx);
        cx.notify();
    }

    /// Notes that the project's latest start was a run, so the footer's
    /// Actions button repeats it and not the saved action it showed.
    pub(crate) fn code_remember_run(&mut self, project: &str, cx: &mut Context<Self>) {
        let last = &mut self.workspace.last_actions;
        if last.get(project).map(String::as_str) != Some(crate::run_ui::footer::LAST_RUN) {
            last.insert(project.to_owned(), crate::run_ui::footer::LAST_RUN.into());
            self.persist(cx);
        }
        self.code_refresh(cx);
    }

    /// A sentence from a feature, shown where the shell shows its own.
    pub(crate) fn code_notice(&mut self, text: String, cx: &mut Context<Self>) {
        self.error = Some(text);
        cx.notify();
    }

    pub(crate) fn code_session_title(&self, session: i64) -> Option<String> {
        (self.session_summaries.get(&session)).map(|summary| summary.title.clone())
    }

    /// Features read their saved values without the workspace.
    pub(super) fn publish_code_settings(&mut self, cx: &mut Context<Self>) {
        self.code.published = true;
        cx.set_global(settings::SavedRuns(
            (self.workspace.project_runs.iter())
                .map(|(project, runs)| (path_key(project), runs.clone()))
                .collect(),
        ));
        cx.set_global(settings::Saved(
            (self.workspace.project_code_settings.iter())
                .map(|(project, pages)| (path_key(project), pages.clone()))
                .collect(),
        ));
    }

    /// The frame hook publishes what was restored, once.
    pub(crate) fn publish_code_settings_once(&mut self, cx: &mut Context<Self>) {
        if !self.code.published {
            self.publish_code_settings(cx);
        }
    }

    pub(crate) fn open_code_settings_page(
        &mut self,
        project: String,
        page: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = settings::pages(cx)
            .into_iter()
            .find(|known| known.id == page)
        else {
            return;
        };
        self.close_menu(cx);
        self.close_preview(cx);
        let context = PageContext {
            value: self.code_setting(&project, page.id),
            host: PageHost {
                project: project.clone(),
                page: page.id,
            },
            project: project.clone(),
        };
        let label: SharedString = self.project_label(&project).into();
        let shell = cx.weak_entity();
        // The feature builds its form outside the shell's update, so the
        // form may ask the shell for whatever it needs.
        window.defer(cx, move |window, cx| {
            let view = (page.build)(context, window, cx);
            let _ = shell.update(cx, |shell, cx| {
                let dialog = cx.new(|cx| PageDialog::new(page.title, label, view, cx));
                let events =
                    cx.subscribe_in(&dialog, window, |shell, _, _: &DialogEvent, window, cx| {
                        shell.close_project_dialog(window, cx)
                    });
                window.focus(&dialog.focus_handle(cx), cx);
                shell.show_code_page_dialog(dialog, events, cx);
            });
        });
    }
}

#[cfg(test)]
impl Shell {
    /// A shell with two projects and a session in each, for the code
    /// features' own tests: the window, and the sessions of
    /// `/work/another-project` and `/work/monocode`.
    pub(crate) fn test_window(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::WindowHandle<Shell>, i64, i64) {
        super::tests::window(cx)
    }
}
