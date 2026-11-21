//! What the workspace shows when nothing is open in the project in front: its
//! most used actions with their live shortcuts, and the project's recent
//! conversations to pick up. Closing the last tab, selecting a project with
//! nothing open and launching with an empty saved workspace all land here
//! instead of on a blank chat. A sleeping project keeps its sleep screen, and a
//! Mono or an app view replaces the workspace as before.
//!
//! Panes need a session that owns them. From here the project's own session
//! lends its scope, or a blank one is created in the background first, so
//! opening a file, a terminal or a page never fails for want of one.
use super::*;
use gpui::Action;

/// The rows, in order: id, name, and the action the shortcut runs. A click
/// runs the same action, so a row can never do something its key does not.
type Row = (&'static str, &'static str, fn() -> Box<dyn Action>);
const ROWS: [Row; 8] = [
    ("new-session", "New session", || Box::new(NewCodex)),
    ("go-to-file", "Go to file", || Box::new(GoToFile)),
    ("command-palette", "Command palette", || {
        Box::new(OpenCommandPalette)
    }),
    ("search", "Search", || Box::new(FocusSearch)),
    ("find-in-files", "Find in files", || Box::new(SearchFiles)),
    ("toggle-terminal", "Toggle terminal", || {
        Box::new(ToggleTerminal)
    }),
    ("open-project", "Open project", || Box::new(OpenProject)),
    ("settings", "Settings", || Box::new(OpenSettings)),
];

/// Width of the name column, which is right-aligned against the keys.
const NAME_WIDTH: f32 = 168.;
/// Width of the shortcut column, which is left-aligned.
const KEY_WIDTH: f32 = 112.;

/// The shortcut the live keymap gives `action`, written as the menus write it.
/// The last binding is the one a second alias (⌘N beside ⌘T) is outranked by.
pub(super) fn bound_shortcut(action: &dyn Action, cx: &App) -> Option<String> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let binding = keymap.bindings_for_action(action).next_back()?;
    let keys: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|stroke| crate::settings::shortcut_label(&stroke.unparse()))
        .collect();
    (!keys.is_empty()).then(|| keys.join(" "))
}

impl Shell {
    /// Nothing is open in the project in front, and nothing else covers it.
    pub(super) fn empty_workspace_showing(&self, _: &App) -> bool {
        self.workspace.active.is_none() && !self.project_asleep(&self.workspace.project)
    }

    /// The workspace area for an empty project, centred and quiet.
    pub(super) fn empty_workspace_screen(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.empty_workspace_showing(cx) {
            return None;
        }
        let project = self.workspace.project.clone();
        let now = monocode_core::time::now_ms();
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
        let rows = ROWS
            .into_iter()
            .enumerate()
            .map(|(ix, (id, name, action))| {
                let keys = bound_shortcut(action().as_ref(), cx);
                div()
                    .id(("empty-workspace-row", ix))
                    .debug_selector(move || format!("empty-workspace-row-{id}"))
                    .role(Role::Button)
                    .aria_label(name)
                    .cursor_pointer()
                    .h(px(30.))
                    .px_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .hover(|s| s.bg(content(0.05)))
                    .child(
                        div()
                            .w(px(NAME_WIDTH))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .text_size(px(13.))
                            .text_color(content(0.55))
                            .child(name),
                    )
                    .child(
                        div()
                            .w(px(KEY_WIDTH))
                            .flex_shrink_0()
                            .text_size(px(13.))
                            .text_color(accent_color())
                            .children(keys),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        let action = action();
                        // The click arrives inside the shell's own update; the
                        // action's handler updates the shell again.
                        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                    }))
            });
        let chats = (!recent.is_empty()).then(|| {
            div()
                .w(px(360.))
                .max_w_full()
                .mt(px(32.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(content(0.4))
                        .child("Recent conversations"),
                )
                .children(
                    recent
                        .into_iter()
                        .enumerate()
                        .map(|(ix, (id, title, when))| {
                            div()
                                .id(("empty-workspace-chat", ix))
                                .debug_selector(move || format!("empty-workspace-chat-{ix}"))
                                .role(Role::Button)
                                .aria_label(format!("Open {title}"))
                                .cursor_pointer()
                                .h(px(30.))
                                .px_2()
                                .rounded_md()
                                .flex()
                                .items_center()
                                .gap_2()
                                .hover(|s| s.bg(content(0.05)))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(13.))
                                        .text_color(content(0.6))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(12.))
                                        .text_color(content(0.35))
                                        .child(when),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open(id, window, cx);
                                }))
                        }),
                )
        });
        Some(
            div()
                .id("empty-workspace")
                .debug_selector(|| "empty-workspace".into())
                .role(Role::Region)
                .aria_label("Nothing is open")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_6()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .debug_selector(|| "empty-workspace-actions".into())
                        .children(rows),
                )
                .children(chats)
                .into_any_element(),
        )
    }

    /// Runs `then` once the project has a session to own a pane: right away
    /// when the project has one, else after a blank one is created, unopened,
    /// in the background. `what` names the pane for the remote project case.
    pub(super) fn with_pane_owner(
        &mut self,
        what: &str,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let cwd = self.workspace.project.clone();
        if cwd.starts_with("remote://") {
            self.error = Some(format!(
                "Open a session in this project before opening {what}"
            ));
            cx.notify();
            return;
        }
        // A second request while the first session is being made waits for
        // its own turn instead of creating another.
        let key = format!("owner\0{cwd}");
        if !self.pending_project_blanks.insert(key.clone()) {
            return;
        }
        let defaults = session_loading::NewSessionDefaults::read(self.default_choice(), &cwd, cx);
        let store = self.store.clone();
        let work = cx.background_executor().spawn({
            let cwd = cwd.clone();
            async move { defaults.create(&*locked(&store)?, &cwd, None) }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending_project_blanks.remove(&key);
                match result {
                    Ok((summary, settings)) => {
                        let id = summary.id;
                        this.add(summary, cx);
                        if let Some(view) = this.sessions.get(&id) {
                            view.update(cx, |view, cx| view.seed_new_settings(settings, cx));
                        }
                        this.order.retain(|s| *s != id);
                        this.order.insert(0, id);
                        this.invalidate_chrome(cx);
                        if this.workspace.project == cwd {
                            then(this, window, cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}
