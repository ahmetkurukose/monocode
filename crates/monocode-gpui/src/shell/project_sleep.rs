//! Sleeping a project gives back what an idle project still holds: its
//! shells and the jobs in them, the warm agent processes its chats left
//! behind and the chats' loaded transcripts. What is needed to pick it up
//! again stays: the chats themselves (composer text, queued messages, model
//! and permission choices), their tabs and the terminal folders. Selecting the project, or one of its chats,
//! shows its sleep screen; only Wake wakes it, along with what starts work
//! there: a new chat, a terminal, or a turn. Each chat shown then reloads its
//! history, and its next turn resumes the provider session cold.
//!
//! The project to sleep must be settled: a chat that is still answering or
//! waiting for an approval keeps the whole project awake.
use super::*;
use crate::modal::primary_button;

/// What one Sleep gave back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SleepTally {
    /// Chats whose loaded transcript was released.
    pub(super) chats: usize,
    /// Warm, idle agent processes stopped.
    pub(super) agents: usize,
    /// Shells closed, with whatever ran in them.
    pub(super) terminals: usize,
}
impl SleepTally {
    fn add(&mut self, other: Self) {
        self.chats += other.chats;
        self.agents += other.agents;
        self.terminals += other.terminals;
    }
    /// "2 agents stopped · 1 terminal closed", or `None` when nothing was held.
    pub(super) fn summary(&self) -> Option<String> {
        let part = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        };
        let parts: Vec<String> = [
            part(self.agents, "agent stopped", "agents stopped"),
            part(self.terminals, "terminal closed", "terminals closed"),
            part(self.chats, "chat unloaded", "chats unloaded"),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// The in-memory side of the sleeping projects, which the workspace does not
/// save: what each Sleep freed.
#[derive(Default)]
pub(super) struct ProjectSleep {
    tallies: HashMap<String, SleepTally>,
}

impl Shell {
    pub(super) fn project_asleep(&self, path: &str) -> bool {
        self.workspace.slept_projects.contains_key(path)
    }
    fn current_project_asleep(&self) -> bool {
        self.project_asleep(&self.workspace.project)
    }

    /// Why `path` cannot sleep right now, if it cannot.
    pub(super) fn sleep_blocker(&self, path: &str, cx: &App) -> Option<String> {
        if path.starts_with("remote://") {
            return Some("Remote projects run on their host".into());
        }
        let working = self
            .project_sessions(path)
            .into_iter()
            .filter(|&id| {
                let meta = self.session_metadata(id, cx);
                meta.busy || meta.needs_approval()
            })
            .count();
        match working {
            0 => None,
            1 => Some("A chat is still working".into()),
            n => Some(format!("{n} chats are still working")),
        }
    }

    /// The shells of `path` that are running something, as `(shell, job)`.
    fn project_running_jobs(&self, path: &str, cx: &App) -> Vec<(String, String)> {
        let sessions: HashSet<i64> = self.project_sessions(path).into_iter().collect();
        let dock = self.terminals.get(path).into_iter().flatten();
        let panes = self
            .workspace
            .terminal_panes
            .iter()
            .filter(|(_, pane)| sessions.contains(&pane.owner))
            .filter_map(|(id, _)| self.terminal_pane_views.get(id));
        dock.chain(panes)
            .filter_map(|terminal| {
                let terminal = terminal.read(cx);
                let job = terminal.running_job()?;
                Some((default_shell_name(&terminal.cwd), job))
            })
            .collect()
    }

    /// Sleep `paths`, asking first when that would stop a running job.
    /// Projects that cannot sleep are left awake and named in the notice.
    pub(super) fn request_sleep_projects(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_menu(cx);
        let mut ready = Vec::new();
        let mut kept = 0;
        for path in paths {
            if self.project_asleep(&path) || ready.contains(&path) {
                continue;
            }
            if self.sleep_blocker(&path, cx).is_some() {
                kept += 1;
            } else {
                ready.push(path);
            }
        }
        if ready.is_empty() {
            if kept > 0 {
                self.show_sleep_notice(
                    "Nothing went to sleep",
                    "Chats are still working there. Try again once they finish.".into(),
                    cx,
                );
            }
            return;
        }
        let jobs: Vec<(String, String, String)> = ready
            .iter()
            .flat_map(|path| {
                let label = self.project_label(path);
                self.project_running_jobs(path, cx)
                    .into_iter()
                    .map(move |(shell, job)| (label.clone(), shell, job))
            })
            .collect();
        if jobs.is_empty() {
            self.sleep_projects(ready, kept, cx);
            return;
        }
        let title = match ready.as_slice() {
            [path] => format!("Sleep “{}”?", self.project_label(path)),
            many => format!("Sleep {} projects?", many.len()),
        };
        let many = ready.len() > 1;
        let list = jobs
            .iter()
            .take(8)
            .map(|(project, shell, job)| {
                if many {
                    format!("• {job} — {project}, {shell}")
                } else {
                    format!("• {job} — {shell}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let more = jobs.len().saturating_sub(8);
        let detail = format!(
            "{} will stop:\n{list}{}\n\nChats, tabs and terminal folders are kept. \
             Waking the project opens fresh shells.",
            if jobs.len() == 1 {
                "This terminal job"
            } else {
                "These terminal jobs"
            },
            if more > 0 {
                format!("\n• and {more} more")
            } else {
                String::new()
            },
        );
        let prompt = window.prompt(
            gpui::PromptLevel::Warning,
            &title,
            Some(&detail),
            &["Sleep", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await != Ok(0) {
                return;
            }
            let _ = this.update(cx, |this, cx| this.sleep_projects(ready, kept, cx));
        })
        .detach();
    }

    /// Sleep each of `paths` now. `kept` projects were left awake.
    pub(super) fn sleep_projects(
        &mut self,
        paths: Vec<String>,
        kept: usize,
        cx: &mut Context<Self>,
    ) {
        // Asked a moment ago; a chat may have started since.
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|path| !self.project_asleep(path) && self.sleep_blocker(path, cx).is_none())
            .collect();
        if paths.is_empty() {
            return;
        }
        let now = monocode_core::time::now_ms();
        for path in &paths {
            self.workspace.slept_projects.insert(path.clone(), now);
        }
        // The current project's chats leave the screen first, so their
        // transcripts count as hidden and can be released.
        self.update_chat_visibility(cx);
        let mut total = SleepTally::default();
        for path in &paths {
            let tally = self.release_project(path, cx);
            self.project_sleep.tallies.insert(path.clone(), tally);
            total.add(tally);
        }
        crate::gpu_trim::trim(cx);
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
        let title = match paths.len() {
            1 => "Project asleep",
            _ => "Projects asleep",
        };
        let subject = match paths.as_slice() {
            [path] => self.project_label(path),
            many => format!("{} projects", many.len()),
        };
        let mut message = match total.summary() {
            Some(freed) => format!("{subject}: {freed}."),
            None => format!("{subject} was already quiet."),
        };
        if kept > 0 {
            message += &match kept {
                1 => " 1 project stayed awake for a working chat.".to_owned(),
                n => format!(" {n} projects stayed awake for working chats."),
            };
        }
        self.show_sleep_notice(title, message, cx);
    }

    /// Give back what `path` holds, keeping what restores it.
    fn release_project(&mut self, path: &str, cx: &mut Context<Self>) -> SleepTally {
        let sessions: HashSet<i64> = self.project_sessions(path).into_iter().collect();
        let mut tally = SleepTally::default();
        if self
            .session_preview
            .as_ref()
            .is_some_and(|preview| sessions.contains(&preview.id))
        {
            self.close_preview(cx);
        }
        // Dropping a terminal hangs up its shell and every job in it. The
        // dock keeps its folders, so waking opens fresh shells there.
        tally.terminals += self.terminals.remove(path).map_or(0, |live| live.len());
        self.action_terminals
            .retain(|(project, _), _| project != path);
        let panes: Vec<i64> = self
            .workspace
            .terminal_panes
            .iter()
            .filter(|(_, pane)| sessions.contains(&pane.owner))
            .map(|(&id, _)| id)
            .collect();
        for pane in panes {
            if self.terminal_pane_views.remove(&pane).is_some() {
                tally.terminals += 1;
            }
        }
        // Idle agents resume their provider session cold on the next turn.
        tally.agents = monocode_core::provider::retire_idle_sessions(&sessions);
        // A chat with unsent text, attachments or queued messages keeps its
        // transcript; everything else reloads it when shown again.
        for id in &sessions {
            self.backgrounds.remove(id);
            let Some(view) = self.sessions.get(id).cloned() else {
                continue;
            };
            if view.update(cx, |chat, cx| chat.unload_history(cx)) {
                tally.chats += 1;
            }
        }
        tally
    }

    /// Wake `path` if it sleeps. Shells come back as they are shown; the chats on screen reload their history.
    pub(super) fn wake_project(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        if self.workspace.slept_projects.remove(path).is_none() {
            return false;
        }
        self.project_sleep.tallies.remove(path);
        self.update_chat_visibility(cx);
        self.persist(cx);
        self.invalidate_chrome(cx);
        cx.notify();
        true
    }
    /// Forget a project that leaves the rail.
    pub(super) fn forget_project_sleep(&mut self, path: &str) {
        self.workspace.slept_projects.remove(path);
        self.project_sleep.tallies.remove(path);
    }
    /// Wake the current project from its sleep screen and put the user back
    /// in its active chat.
    fn wake_current_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.workspace.project.clone();
        if !self.wake_project(&project, cx) {
            return;
        }
        // Asleep since launch or after leaving an app view, the project may
        // have no chat on screen yet: its last one, else a blank one.
        if self.workspace.active.is_none() {
            let previous = self
                .workspace
                .project_focus
                .get(&project)
                .copied()
                .filter(|id| self.workspace.tab_of(*id).is_some());
            match previous {
                Some(id) => self.activate_session(id, window, cx),
                None => self.ensure_project_session(window, cx),
            }
        }
        self.focus_active(window, cx);
        self.persist(cx);
    }

    fn show_sleep_notice(&mut self, title: &'static str, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .project_slept
            .update(cx, |notice, cx| notice.show_titled(title, message, cx));
        cx.notify();
    }

    /// The other listed, awake local projects.
    fn other_awake_projects(&self, path: &str) -> Vec<String> {
        self.workspace
            .projects
            .iter()
            .filter(|p| *p != path && !p.starts_with("remote://") && !self.project_asleep(p))
            .cloned()
            .collect()
    }
    /// The projects assigned to rail group `id`.
    fn group_projects(&self, id: &str) -> Vec<String> {
        self.workspace
            .projects
            .iter()
            .filter(|p| {
                self.workspace
                    .project_rail_assignments
                    .get(*p)
                    .is_some_and(|group| group == id)
            })
            .cloned()
            .collect()
    }

    /// The project menu's Sleep or Wake, and Sleep other projects.
    pub(super) fn project_sleep_menu(&self, path: &str, cx: &App) -> Vec<MenuItem> {
        if path.starts_with("remote://") {
            return Vec::new();
        }
        let mut items: Vec<MenuItem> = Vec::new();
        if self.project_asleep(path) {
            items.push(
                MenuAction::new("project-wake", "Wake project")
                    .icon("sun")
                    .into(),
            );
        } else {
            let action = MenuAction::new("project-sleep", "Sleep project").icon("moon");
            items.push(
                match self.sleep_blocker(path, cx) {
                    Some(reason) => action.description(reason).disabled(true),
                    None => action.description("Free its memory until you wake it"),
                }
                .into(),
            );
        }
        let others = self.other_awake_projects(path).len();
        if others > 0 {
            items.push(
                MenuAction::new("project-sleep-others", "Sleep other projects")
                    .icon("moon")
                    .description(match others {
                        1 => "1 project".to_owned(),
                        n => format!("{n} projects"),
                    })
                    .into(),
            );
        }
        items
    }
    pub(super) fn run_project_sleep_menu(
        &mut self,
        path: &str,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match action {
            "project-sleep" => self.request_sleep_projects(vec![path.to_owned()], window, cx),
            "project-sleep-others" => {
                let others = self.other_awake_projects(path);
                self.request_sleep_projects(others, window, cx);
            }
            "project-wake" => {
                self.close_menu(cx);
                if path == self.workspace.project {
                    self.wake_current_project(window, cx);
                } else {
                    self.wake_project(path, cx);
                }
            }
            _ => return false,
        }
        true
    }

    /// A rail group's Sleep all and Wake all.
    pub(super) fn project_group_sleep_menu(&self, id: &str, cx: &App) -> Vec<MenuItem> {
        let projects = self.group_projects(id);
        let local: Vec<&String> = projects
            .iter()
            .filter(|p| !p.starts_with("remote://"))
            .collect();
        let awake: Vec<&&String> = local.iter().filter(|p| !self.project_asleep(p)).collect();
        let asleep = local.len() - awake.len();
        let mut items: Vec<MenuItem> = Vec::new();
        if !awake.is_empty() {
            let working = awake
                .iter()
                .filter(|p| self.sleep_blocker(p, cx).is_some())
                .count();
            let ready = awake.len() - working;
            let description = match (ready, working) {
                (_, 0) if ready == 1 => "1 project".to_owned(),
                (_, 0) => format!("{ready} projects"),
                (0, _) => "Chats are still working".to_owned(),
                (ready, working) => format!("{ready} now; {working} still working"),
            };
            items.push(
                MenuAction::new("group-sleep", "Sleep group")
                    .icon("moon")
                    .description(description)
                    .disabled(ready == 0)
                    .into(),
            );
        }
        if asleep > 0 {
            items.push(
                MenuAction::new("group-wake", "Wake group")
                    .icon("sun")
                    .description(match asleep {
                        1 => "1 sleeping project".to_owned(),
                        n => format!("{n} sleeping projects"),
                    })
                    .into(),
            );
        }
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        items
    }
    pub(super) fn run_project_group_sleep_menu(
        &mut self,
        id: &str,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match action {
            "group-sleep" => {
                let projects = self.group_projects(id);
                self.request_sleep_projects(projects, window, cx);
            }
            "group-wake" => {
                self.close_menu(cx);
                for project in self.group_projects(id) {
                    if project == self.workspace.project {
                        self.wake_current_project(window, cx);
                    } else {
                        self.wake_project(&project, cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    /// The command palette's Sleep this project / other projects.
    pub(super) fn sleep_current_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.workspace.project.clone();
        if let Some(reason) = self.sleep_blocker(&project, cx) {
            self.show_sleep_notice("Project stays awake", format!("{reason}."), cx);
            return;
        }
        self.request_sleep_projects(vec![project], window, cx);
    }
    pub(super) fn sleep_other_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let others = self.other_awake_projects(&self.workspace.project.clone());
        self.request_sleep_projects(others, window, cx);
    }

    /// A project mark with the moon of a sleeping project at its corner.
    pub(super) fn with_sleep_badge(
        &self,
        path: &str,
        mark: impl IntoElement,
        side: f32,
    ) -> AnyElement {
        if !self.project_asleep(path) {
            return mark.into_any_element();
        }
        let badge = (side * 0.62).max(8.);
        div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(div().opacity(0.55).child(mark))
            .child(
                div()
                    .debug_selector({
                        let path = path.to_owned();
                        move || format!("project-asleep-badge-{path}")
                    })
                    .absolute()
                    .right(px(-badge * 0.45))
                    .bottom(px(-badge * 0.35))
                    .size(px(badge + 2.))
                    .rounded_full()
                    .bg(pane_fill())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("moon", badge).text_color(rgb(SLEEP_TINT))),
            )
            .into_any_element()
    }

    /// The current project's main area while it sleeps: what it is, what
    /// sleeping freed, Wake, and its recent chats to pick up directly.
    pub(super) fn project_sleep_screen(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.current_project_asleep() {
            return None;
        }
        let project = self.workspace.project.clone();
        let label = self.project_label(&project);
        let now = monocode_core::time::now_ms();
        let slept = self
            .workspace
            .slept_projects
            .get(&project)
            .copied()
            .unwrap_or(now);
        let since = match monocode_core::time::relative(slept, now).as_str() {
            "" | "now" => "Asleep since just now".to_owned(),
            ago => format!("Asleep for {ago}"),
        };
        let freed = self
            .project_sleep
            .tallies
            .get(&project)
            .and_then(SleepTally::summary);
        let mut recent: Vec<&Session> = self
            .session_summaries
            .values()
            .filter(|s| s.cwd == project && s.has_messages && !s.archived)
            .collect();
        recent.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        let recent: Vec<(i64, String, String)> = recent
            .into_iter()
            .take(5)
            .map(|s| {
                (
                    s.id,
                    self.session_metadata(s.id, cx).display_title(),
                    monocode_core::time::relative(s.updated_at, now),
                )
            })
            .collect();
        let chats = (!recent.is_empty()).then(|| {
            div()
                .w(px(360.))
                .max_w_full()
                .mt(px(28.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(content(0.45))
                        .child("Pick up a chat"),
                )
                .children(
                    recent
                        .into_iter()
                        .enumerate()
                        .map(|(ix, (id, title, when))| {
                            div()
                                .id(("project-sleep-chat", ix))
                                .debug_selector(move || format!("project-sleep-chat-{ix}"))
                                .role(Role::Button)
                                .aria_label(format!("Wake and open {title}"))
                                .cursor_pointer()
                                .h(px(32.))
                                .px_2()
                                .rounded_md()
                                .flex()
                                .items_center()
                                .gap_2()
                                .hover(|s| s.bg(content(0.06)))
                                .child(icon("chat", 14.).text_color(content(0.45)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.))
                                        .text_color(content(0.85))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(12.))
                                        .text_color(content(0.4))
                                        .child(when),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open(id, window, cx);
                                    this.wake_current_project(window, cx);
                                }))
                        }),
                )
        });
        Some(
            div()
                .id("project-sleep-screen")
                .debug_selector(|| "project-sleep-screen".into())
                .role(Role::Region)
                .aria_label(format!("{label} is asleep"))
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_6()
                .child(
                    div()
                        .size(px(56.))
                        .rounded_full()
                        .bg(rgb(SLEEP_TINT).opacity(0.14))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("moon", 26.).text_color(rgb(SLEEP_TINT))),
                )
                .child(
                    div()
                        .mt(px(18.))
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(content(1.))
                        .child(format!("{label} is asleep")),
                )
                .child(
                    div()
                        .mt(px(6.))
                        .max_w(px(420.))
                        .text_center()
                        .text_size(px(13.))
                        .line_height(px(19.))
                        .text_color(content(0.6))
                        .child(format!(
                            "{since}. Its chats, tabs and terminal folders are kept; \
                             nothing runs here until you wake it."
                        )),
                )
                .children(freed.map(|freed| {
                    div()
                        .id("project-sleep-freed")
                        .debug_selector(|| "project-sleep-freed".into())
                        .mt(px(12.))
                        .px(px(10.))
                        .py(px(4.))
                        .rounded_full()
                        .bg(content(0.06))
                        .text_size(px(12.))
                        .text_color(content(0.6))
                        .child(freed)
                }))
                .child(
                    primary_button("project-wake", "Wake project")
                        .debug_selector(|| "project-wake".into())
                        .mt(px(20.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.wake_current_project(window, cx);
                        })),
                )
                .children(chats)
                .into_any_element(),
        )
    }
}

impl Shell {
    /// The right panel's Terminal tab while its project sleeps.
    pub(super) fn sleeping_terminal_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("panel-terminal-asleep")
            .debug_selector(|| "panel-terminal-asleep".into())
            .role(Role::Region)
            .aria_label("Terminal, asleep")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .child(icon("moon", 20.).text_color(rgb(SLEEP_TINT)))
            .child(
                div()
                    .text_center()
                    .text_size(px(12.))
                    .text_color(content(0.55))
                    .child("Terminals sleep with the project."),
            )
            .child(
                crate::modal::ghost_button("panel-terminal-wake", "Wake project")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.wake_current_project(window, cx);
                    })),
            )
            .into_any_element()
    }
}

/// The moon's soft indigo, the same in light and dark themes.
const SLEEP_TINT: u32 = 0x818cf8;

/// A shell's folder name, to tell a project's terminals apart in a list.
fn default_shell_name(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::tests::window;
    use gpui::{TestAppContext, VisualTestContext, WindowHandle};

    const ANOTHER: &str = "/work/another-project";
    const MONOCODE: &str = "/work/monocode";

    /// Opens chat `a` in its project with the dock's one shell, back on the
    /// chat.
    fn busy_project(
        window: WindowHandle<Shell>,
        a: i64,
        cx: &mut TestAppContext,
    ) -> VisualTestContext {
        cx.update(|cx| {
            bind_keys(cx);
            crate::terminal::bind(cx);
            cx.set_reduce_motion(true);
        });
        window
            .update(cx, |shell, w, cx| {
                shell.open(a, w, cx);
                shell.focus_active(w, cx);
            })
            .unwrap();
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        visual.simulate_keystrokes("cmd-j");
        visual.run_until_parked();
        visual
    }
    fn sleep_menu(window: WindowHandle<Shell>, visual: &mut VisualTestContext, action: &str) {
        window
            .update(visual, |shell, w, cx| {
                let target = MenuTarget::Project {
                    path: ANOTHER.into(),
                    rail: true,
                };
                shell.run_menu(target, action, None, w, cx);
            })
            .unwrap();
        visual.run_until_parked();
    }
    fn notice(shell: &Shell, cx: &App) -> Option<String> {
        let notice = shell.notification_state.project_slept.read(cx);
        notice.message().map(str::to_owned)
    }
    fn action<'a>(items: &'a [MenuItem], id: &str) -> Option<&'a MenuAction> {
        items.iter().find_map(|item| match item {
            MenuItem::Action(action) if action.id == id => Some(action.as_ref()),
            _ => None,
        })
    }

    #[gpui::test]
    fn sleeping_frees_shells_and_history_and_waking_restores_them(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        window
            .update(&mut visual, |shell, _, cx| {
                assert_eq!(shell.workspace.project, ANOTHER);
                assert!(shell.sessions[&a].read(cx).history_loaded());
                assert_eq!(shell.terminals[ANOTHER].len(), 1);
            })
            .unwrap();
        // An idle shell sleeps without a question.
        sleep_menu(window, &mut visual, "project-sleep");
        assert!(!visual.has_pending_prompt());
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(shell.project_asleep(ANOTHER));
                assert!(!shell.project_asleep(MONOCODE));
                // The shell is gone; the dock remembers where it was.
                assert!(!shell.terminals.contains_key(ANOTHER));
                assert_eq!(shell.workspace.docks[ANOTHER].terminals.len(), 1);
                // The chat, its tab and its settings stay; its transcript goes.
                let chat = shell.sessions[&a].read(cx);
                assert!(!chat.history_loaded());
                assert!(chat.needs_history_restore());
                assert_eq!(shell.workspace.active, Some(a));
                assert_eq!(
                    shell.project_sleep.tallies[ANOTHER],
                    SleepTally {
                        chats: 1,
                        agents: 0,
                        terminals: 1,
                    }
                );
                let message = notice(shell, cx).unwrap();
                assert!(message.starts_with("another-project:"), "{message}");
                assert!(message.contains("1 terminal closed"), "{message}");
            })
            .unwrap();
        // It stays asleep across a restart.
        window
            .update(&mut visual, |shell, _, _| {
                assert!(shell.workspace.saved_json().contains("slept_projects"));
            })
            .unwrap();
        // The sleep screen stands in for the chat and the dock.
        assert!(visual.debug_bounds("project-sleep-screen").is_some());
        assert!(visual.debug_bounds("project-sleep-freed").is_some());
        assert!(visual.debug_bounds("project-sleep-chat-0").is_some());
        assert!(visual.debug_bounds("project-slept").is_some());
        window
            .update(&mut visual, |shell, _, cx| {
                // Drawing the sleeping project spawns nothing and loads nothing.
                assert!(!shell.terminals.contains_key(ANOTHER));
                assert!(!shell.sessions[&a].read(cx).history_loaded());
            })
            .unwrap();
        let wake = visual.debug_bounds("project-wake").unwrap();
        visual.simulate_click(wake.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_none());
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(!shell.project_asleep(ANOTHER));
                assert!(!shell.workspace.saved_json().contains("slept_projects"));
                // A fresh shell where the old one was, and the chat's
                // history reloaded from the store.
                assert_eq!(shell.terminals[ANOTHER].len(), 1);
                let chat = shell.sessions[&a].read(cx);
                assert!(chat.history_loaded());
                assert!(!chat.needs_history_restore());
                assert!(chat.history_weight(cx) > 0);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_chat_on_the_sleep_screen_wakes_the_project_and_opens(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        sleep_menu(window, &mut visual, "project-sleep");
        let chat = visual.debug_bounds("project-sleep-chat-0").unwrap();
        visual.simulate_click(chat.center(), Default::default());
        visual.run_until_parked();
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(!shell.project_asleep(ANOTHER));
                assert_eq!(shell.workspace.active, Some(a));
                assert!(shell.sessions[&a].read(cx).history_loaded());
            })
            .unwrap();
        assert!(visual.debug_bounds("project-sleep-screen").is_none());
    }

    #[gpui::test]
    fn sleeping_asks_before_stopping_a_running_job(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        let terminal = window
            .update(&mut visual, |shell, _, cx| {
                let terminal = shell.terminals[ANOTHER][0].clone();
                terminal.update(cx, |t, _| t.foreground = Some("vite".into()));
                terminal.downgrade()
            })
            .unwrap();
        for answer in ["Cancel", "Sleep"] {
            sleep_menu(window, &mut visual, "project-sleep");
            assert!(visual.has_pending_prompt());
            let (title, detail) = visual.pending_prompt().unwrap();
            assert_eq!(title, "Sleep “another-project”?");
            assert!(detail.contains("• vite"), "{detail}");
            visual.simulate_prompt_answer(answer);
            visual.run_until_parked();
            let slept = answer == "Sleep";
            window
                .update(&mut visual, |shell, _, _| {
                    assert_eq!(shell.project_asleep(ANOTHER), slept);
                })
                .unwrap();
            assert_eq!(terminal.upgrade().is_none(), slept);
        }
    }

    #[gpui::test]
    fn a_working_chat_keeps_its_project_awake_and_a_new_turn_wakes_it(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        window
            .update(&mut visual, |shell, w, cx| {
                shell.sessions[&a].update(cx, |chat, _| chat.busy = true);
                let items = shell.project_sleep_menu(ANOTHER, cx);
                let sleep = action(&items, "project-sleep").unwrap();
                assert!(sleep.disabled);
                assert_eq!(
                    sleep.description.as_deref(),
                    Some("A chat is still working")
                );
                shell.request_sleep_projects(vec![ANOTHER.into()], w, cx);
                assert!(!shell.project_asleep(ANOTHER));
                assert!(notice(shell, cx).unwrap().contains("still working"));
                // Settled again, it sleeps.
                shell.sessions[&a].update(cx, |chat, _| chat.busy = false);
                shell.request_sleep_projects(vec![ANOTHER.into()], w, cx);
                assert!(shell.project_asleep(ANOTHER));
                let items = shell.project_sleep_menu(ANOTHER, cx);
                assert!(action(&items, "project-wake").is_some());
                // A turn starting there, as an automation's would, wakes it.
                shell.sessions[&a].update(cx, |chat, _| chat.busy = true);
                shell.refresh_session_mark(a, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(!shell.project_asleep(ANOTHER));
                shell.sessions[&a].update(cx, |chat, _| chat.busy = false);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_group_sleeps_and_wakes_together_and_each_wakes_on_its_own(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        let group = window
            .update(&mut visual, |shell, w, cx| {
                shell.workspace.classic_layout = true;
                shell.workspace.project_rail_open = true;
                let group = shell.create_project_rail_group(Some(ANOTHER), cx);
                shell.assign_project_rail_group(MONOCODE, &group, cx);
                let items = shell.project_group_sleep_menu(&group, cx);
                let sleep = action(&items, "group-sleep").unwrap();
                assert_eq!(sleep.description.as_deref(), Some("2 projects"));
                assert!(action(&items, "group-wake").is_none());
                let target = MenuTarget::ProjectRailGroup(group.clone());
                shell.run_menu(target, "group-sleep", None, w, cx);
                group
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(shell.project_asleep(ANOTHER) && shell.project_asleep(MONOCODE));
                assert!(notice(shell, cx).unwrap().starts_with("2 projects:"));
                let items = shell.project_group_sleep_menu(&group, cx);
                assert!(action(&items, "group-sleep").is_none());
                assert!(action(&items, "group-wake").is_some());
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        visual.run_until_parked();
        // The rail marks both as asleep.
        for path in [ANOTHER, MONOCODE] {
            let badge: &'static str = format!("project-asleep-badge-{path}").leak();
            assert!(visual.debug_bounds(badge).is_some(), "{badge}");
        }
        // Selecting one only shows it; its Wake wakes only that one.
        window
            .update(&mut visual, |shell, w, cx| {
                shell.select_project(MONOCODE.into(), w, cx);
                assert!(shell.project_asleep(MONOCODE));
                let target = MenuTarget::Project {
                    path: MONOCODE.into(),
                    rail: true,
                };
                shell.run_menu(target, "project-wake", None, w, cx);
                assert!(!shell.project_asleep(MONOCODE));
                assert!(shell.project_asleep(ANOTHER));
                let target = MenuTarget::ProjectRailGroup(group.clone());
                shell.run_menu(target, "group-wake", None, w, cx);
                assert!(!shell.project_asleep(ANOTHER));
            })
            .unwrap();
    }

    #[gpui::test]
    fn the_tree_shows_a_sleeping_project_and_its_chat_until_wake_is_pressed(
        cx: &mut TestAppContext,
    ) {
        let (window, a, b) = window(cx);
        let mut visual = busy_project(window, a, cx);
        window
            .update(&mut visual, |shell, w, cx| {
                shell.workspace.classic_layout = true;
                shell.workspace.project_rail_open = true;
                shell.workspace.preferences.project_rail_view = "tree".into();
                shell.workspace.project_rail_expanded.insert(ANOTHER.into());
                // Sleep this project from the other one.
                shell.select_project(MONOCODE.into(), w, cx);
                shell.open(b, w, cx);
                shell.request_sleep_projects(vec![ANOTHER.into()], w, cx);
                assert!(shell.project_asleep(ANOTHER));
                shell.invalidate_chrome(cx);
            })
            .unwrap();
        visual.run_until_parked();
        let ix = window
            .update(&mut visual, |shell, _, _| {
                shell.workspace.projects.iter().position(|p| p == ANOTHER)
            })
            .unwrap()
            .unwrap();
        let selector: &'static str = format!("rail-project-button-{ix}").leak();
        // Clicking its row selects it behind the sleep screen.
        let row = visual.debug_bounds(selector).unwrap();
        // On its name, clear of the app icons at the row's end.
        let at = gpui::point(row.left() + gpui::px(40.), row.center().y);
        visual.simulate_click(at, Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_some());
        window
            .update(&mut visual, |shell, _, _| {
                assert_eq!(shell.workspace.project, ANOTHER);
                assert!(shell.project_asleep(ANOTHER));
                assert!(!shell.terminals.contains_key(ANOTHER));
            })
            .unwrap();
        // So does its chat in the tree, without loading the chat's history.
        let selector: &'static str = format!("rail-session-{a}").leak();
        let chat = visual.debug_bounds(selector).unwrap();
        visual.simulate_click(chat.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_some());
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(shell.project_asleep(ANOTHER));
                assert_eq!(shell.workspace.active, Some(a));
                assert!(!shell.sessions[&a].read(cx).history_loaded());
            })
            .unwrap();
        // Wake shows that chat with its history, and the dock's shell again.
        let wake = visual.debug_bounds("project-wake").unwrap();
        visual.simulate_click(wake.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_none());
        window
            .update(&mut visual, |shell, _, cx| {
                assert!(!shell.project_asleep(ANOTHER));
                assert_eq!(shell.workspace.active, Some(a));
                assert!(shell.sessions[&a].read(cx).history_loaded());
                assert_eq!(shell.terminals[ANOTHER].len(), 1);
            })
            .unwrap();
    }

    #[gpui::test]
    fn other_projects_sleep_from_the_current_one_and_leave_it_awake(cx: &mut TestAppContext) {
        let (window, a, _) = window(cx);
        let mut visual = busy_project(window, a, cx);
        window
            .update(&mut visual, |shell, w, cx| {
                shell.select_project(MONOCODE.into(), w, cx);
                let items = shell.project_sleep_menu(MONOCODE, cx);
                let others = action(&items, "project-sleep-others").unwrap();
                assert_eq!(others.description.as_deref(), Some("1 project"));
                shell.sleep_other_projects(w, cx);
                assert!(shell.project_asleep(ANOTHER));
                assert!(!shell.project_asleep(MONOCODE));
                // The other project's shell went with it.
                assert!(!shell.terminals.contains_key(ANOTHER));
                let items = shell.project_sleep_menu(MONOCODE, cx);
                assert!(action(&items, "project-sleep-others").is_none());
            })
            .unwrap();
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_none());
        // Archiving forgets that it slept.
        window
            .update(&mut visual, |shell, w, cx| {
                shell.archive_project(ANOTHER, w, cx);
                assert!(!shell.workspace.slept_projects.contains_key(ANOTHER));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_project_asleep_at_launch_stays_asleep_until_woken(cx: &mut TestAppContext) {
        use monocode_core::store::Store;
        use std::sync::{Arc, Mutex};
        cx.update(|cx| cx.set_reduce_motion(true));
        let mut store = Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut chat = store.create("claude", MONOCODE).unwrap();
        store
            .write_rows(
                chat.id,
                0,
                &[monocode_core::transcript::Row {
                    role: "assistant".into(),
                    text: "Picked up after sleeping".into(),
                    first: true,
                    sent_at: None,
                }],
            )
            .unwrap();
        chat.has_messages = true;
        // No tab is open: the project is asleep, then empty once woken.
        let mut state = crate::workspace::Workspace::default();
        state.add_project(MONOCODE);
        state.add_project(ANOTHER);
        state.project = MONOCODE.into();
        state.slept_projects.insert(MONOCODE.into(), 1);
        let saved = serde_json::to_string(&state).unwrap();
        let store = Arc::new(Mutex::new(store));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Shell::new(store, vec![chat.clone()], MONOCODE.into(), Some(saved), cx))
            })
            .unwrap()
        });
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_some());
        window
            .update(&mut visual, |shell, w, cx| {
                assert!(shell.project_asleep(MONOCODE));
                assert_eq!(shell.workspace.active, None);
                assert!(shell.sessions.is_empty(), "sleeping loads no chat");
                // Leaving an app view returns to the sleep screen, not a chat.
                shell.open_inbox(w, cx);
                assert!(shell.app_view_open());
                shell.close_app_view(w, cx);
                assert!(shell.project_asleep(MONOCODE));
                assert_eq!(shell.workspace.active, None);
            })
            .unwrap();
        visual.run_until_parked();
        // Waking a project with nothing open shows the empty workspace.
        let wake = visual.debug_bounds("project-wake").unwrap();
        visual.simulate_click(wake.center(), Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("project-sleep-screen").is_none());
        assert!(visual.debug_bounds("empty-workspace").is_some());
        window
            .update(&mut visual, |shell, _, _| {
                assert!(!shell.project_asleep(MONOCODE));
                assert!(shell.workspace.active.is_none());
            })
            .unwrap();
    }

    #[test]
    fn restoring_keeps_only_listed_local_projects_asleep() {
        let saved = serde_json::json!({
            "projects": ["/work/a", "remote://host/b"],
            "project": "/work/a",
            "slept_projects": {"/work/a": 1, "remote://host/b": 2, "/work/gone": 3},
        })
        .to_string();
        let state = crate::workspace::Workspace::restore(Some(&saved), &[], "/work/a");
        assert_eq!(
            state.slept_projects,
            HashMap::from([("/work/a".to_owned(), 1)])
        );
    }

    #[test]
    fn the_tally_reads_as_what_was_freed() {
        assert_eq!(SleepTally::default().summary(), None);
        let tally = SleepTally {
            chats: 3,
            agents: 1,
            terminals: 2,
        };
        assert_eq!(
            tally.summary().as_deref(),
            Some("1 agent stopped · 2 terminals closed · 3 chats unloaded")
        );
    }
}
