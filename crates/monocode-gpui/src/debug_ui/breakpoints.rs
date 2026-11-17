//! Breakpoints in the file editor's gutter. The shared `BreakpointStore` is
//! the truth: a press in the breakpoint column edits it, every change of it
//! (the user's, an agent's over MCP, a debugger's answer) is drawn from it,
//! and the editor's report of marks an edit moved is written back to it.
//! Each project's breakpoints are saved with the workspace, as a per-project
//! value of the settings seam, and loaded again when the project opens.
use crate::{
    code_editor::{
        self, Breakpoint as MarkState, DocumentInfo, GutterButton, GutterClick, GutterMark,
        GutterMarkKind, GutterMenu, GutterProvider, GutterSlot,
    },
    code_shell::{
        self,
        context::{self, CodeEvent},
        settings::{self, PageHost},
    },
    input::{self, Input},
    popover::{MenuAction, MenuItem},
    theme::*,
};
use gpui::{
    App, AppContext, Context, Entity, Focusable, Global, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use monocode_core::code::{
    dap::{Breakpoint, BreakpointChange, BreakpointStatus, BreakpointStore, ExceptionFilters},
    services::services,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

/// The key a project's breakpoints are saved under in its code settings.
pub(super) const PAGE: &str = "debug-breakpoints";
/// A burst of changes (typing above a breakpoint) is saved once.
const PERSIST_DELAY: Duration = Duration::from_millis(400);

#[derive(Default)]
struct State {
    /// What was last drawn for a file: each mark's row and the line its
    /// breakpoint has in the store, in row order.
    shown: HashMap<PathBuf, Vec<(usize, u32)>>,
    /// Changes of a file this module wrote itself after an edit and has
    /// already drawn; their echo from the store is not drawn again.
    own_writes: HashMap<PathBuf, usize>,
    dirty: HashSet<String>,
    persist: Option<Task<()>>,
}
impl Global for State {}

pub(super) fn store() -> &'static BreakpointStore {
    services().breakpoints()
}

/// Whether a debug adapter could serve the file, which gives it the column.
pub(super) fn serves(doc: &DocumentInfo) -> bool {
    doc.language == Some("python")
}

pub(super) fn init(cx: &mut App) {
    let provider: Rc<dyn GutterProvider> = Rc::new(Gutter);
    code_editor::register(cx, move |doc, providers, _| {
        if serves(doc) {
            providers.breakpoints = Some(provider.clone());
        }
    });
    let changes = store().subscribe();
    cx.spawn(async move |cx| {
        while let Ok(change) = changes.recv().await {
            let mut batch = vec![change];
            while let Ok(more) = changes.try_recv() {
                batch.push(more);
            }
            cx.update(|cx| store_changed(batch, cx));
        }
    })
    .detach();
    let events = context::events(cx);
    cx.subscribe(&events, |_, event: &CodeEvent, cx| {
        if let CodeEvent::ProjectOpened(project) = event {
            restore(project, cx);
        }
    })
    .detach();
    // What was set before the interface came up, by an agent for one.
    for path in store().files() {
        draw(&path, cx);
    }
}

/// Loads what the project saved into the shared store.
pub(super) fn restore(project: &str, cx: &mut App) {
    if let Some(saved) = settings::value(project, PAGE, cx) {
        store().load_json(&saved);
    }
}

fn store_changed(changes: Vec<BreakpointChange>, cx: &mut App) {
    let mut seen = HashSet::new();
    for change in changes {
        match change {
            BreakpointChange::File(path) => {
                let writes = &mut cx.default_global::<State>().own_writes;
                let own = match writes.get_mut(&path) {
                    Some(count) => {
                        *count -= 1;
                        if *count == 0 {
                            writes.remove(&path);
                        }
                        true
                    }
                    None => false,
                };
                if !own && seen.insert(path.clone()) {
                    draw(&path, cx);
                }
                if let Some(checkout) = context::checkout_for_path(&path.to_string_lossy(), cx) {
                    mark_dirty(checkout.project, cx);
                }
            }
            BreakpointChange::Status(path) => {
                if seen.insert(path.clone()) {
                    draw(&path, cx);
                }
            }
            BreakpointChange::Exceptions => {
                for project in context::snapshot(cx).projects {
                    mark_dirty(project, cx);
                }
            }
        }
    }
}

fn describe(breakpoint: &Breakpoint) -> SharedString {
    let mut lines = vec![if breakpoint.enabled {
        "Breakpoint".to_owned()
    } else {
        "Breakpoint, disabled".to_owned()
    }];
    if let Some(condition) = &breakpoint.condition {
        lines.push(format!("Stops when {condition}"));
    }
    if let Some(hits) = &breakpoint.hit_condition {
        lines.push(format!("Hit count {hits}"));
    }
    if let Some(message) = &breakpoint.log_message {
        lines.push(format!("Logs \"{message}\" instead of stopping"));
    }
    if let BreakpointStatus::Rejected { message } = &breakpoint.status {
        lines.push(
            message
                .clone()
                .unwrap_or_else(|| "The debugger could not set this breakpoint.".to_owned()),
        );
    }
    lines.join("\n").into()
}

/// The mark of one breakpoint: where the adapter placed it when it moved
/// it, and how it is drawn.
pub(super) fn mark(breakpoint: &Breakpoint) -> GutterMark {
    let line = match breakpoint.status {
        BreakpointStatus::Verified { line } => line,
        _ => breakpoint.line,
    };
    let detailed = breakpoint.condition.is_some()
        || breakpoint.hit_condition.is_some()
        || breakpoint.log_message.is_some();
    let state = if !breakpoint.enabled {
        MarkState::Disabled
    } else if matches!(breakpoint.status, BreakpointStatus::Rejected { .. }) {
        MarkState::Unverified
    } else if detailed {
        MarkState::Conditional
    } else {
        MarkState::Enabled
    };
    GutterMark {
        row: line.saturating_sub(1) as usize,
        kind: GutterMarkKind::Breakpoint(state),
        tooltip: describe(breakpoint),
    }
}

/// Draws the file's breakpoints as the store has them.
fn draw(path: &Path, cx: &mut App) {
    let mut drawn: Vec<(GutterMark, u32)> = (store().in_file(path).iter())
        .map(|breakpoint| (mark(breakpoint), breakpoint.line))
        .collect();
    // As the hub keeps them: by row, one per row.
    drawn.sort_by_key(|(mark, _)| mark.row);
    drawn.dedup_by_key(|(mark, _)| mark.row);
    let shown = drawn.iter().map(|(mark, line)| (mark.row, *line)).collect();
    let state = cx.default_global::<State>();
    if drawn.is_empty() {
        state.shown.remove(path);
    } else {
        state.shown.insert(path.to_owned(), shown);
    }
    let marks = drawn.into_iter().map(|(mark, _)| mark).collect();
    code_editor::hub(cx).update(cx, |hub, cx| {
        hub.set_gutter_marks(path, GutterSlot::Breakpoint, marks, cx)
    });
}

/// The store's line of the breakpoint drawn on `row`.
fn line_on_row(path: &Path, row: usize, cx: &App) -> Option<u32> {
    let shown = cx.try_global::<State>()?.shown.get(path)?;
    (shown.iter()).find_map(|(drawn, line)| (*drawn == row).then_some(*line))
}

/// Adds a breakpoint on the row, or removes the one drawn there.
pub(super) fn toggle_row(path: &Path, row: usize, cx: &App) {
    match line_on_row(path, row, cx) {
        Some(line) => {
            store().remove(path, line);
        }
        None => {
            store().toggle(path, row as u32 + 1);
        }
    }
}

/// Pairs the marks as they were drawn with where an edit left them. The
/// editor reports rows only, so lines that were deleted or joined show as
/// fewer marks: what stayed in front and what moved together behind the
/// edit are matched from both ends, and the rest pair up in order.
/// Each pair is a breakpoint's line in the store and its new row and line.
fn moved_lines(shown: &[(usize, u32)], rows: &[usize]) -> Vec<(u32, (usize, u32))> {
    let line_at = |(old_row, line): (usize, u32), row: usize| {
        let moved = i64::from(line) + row as i64 - old_row as i64;
        (line, (row, u32::try_from(moved.max(1)).unwrap_or(1)))
    };
    if shown.len() == rows.len() {
        return (shown.iter().zip(rows))
            .map(|(old, row)| line_at(*old, *row))
            .collect();
    }
    let front = (shown.iter().zip(rows))
        .take_while(|((old_row, _), row)| old_row == *row)
        .count();
    let shift = |k: usize| -> Option<i64> {
        let (old, new) = (
            shown.len().checked_sub(k + 1)?,
            rows.len().checked_sub(k + 1)?,
        );
        (old >= front && new >= front).then(|| rows[new] as i64 - shown[old].0 as i64)
    };
    let mut back = 0;
    while shift(back).is_some() && shift(back) == shift(0) {
        back += 1;
    }
    let middle_old = &shown[front..shown.len() - back];
    let middle_new = &rows[front..rows.len() - back];
    (shown[..front].iter().zip(&rows[..front]))
        .chain(middle_old.iter().zip(middle_new))
        .chain(
            shown[shown.len() - back..]
                .iter()
                .zip(&rows[rows.len() - back..]),
        )
        .map(|(old, row)| line_at(*old, *row))
        .collect()
}

/// An edit moved the file's marks: the store follows the editor.
fn marks_moved(path: &Path, marks: &[GutterMark], cx: &mut App) {
    let Some(shown) = cx.default_global::<State>().shown.get(path).cloned() else {
        return;
    };
    let rows: Vec<usize> = marks.iter().map(|mark| mark.row).collect();
    let moved = moved_lines(&shown, &rows);
    let now: Vec<(usize, u32)> = moved.iter().map(|(_, now)| *now).collect();
    if now == shown {
        return;
    }
    let before = store().in_file(path);
    let after: Vec<Breakpoint> = (moved.iter())
        .filter_map(|(old_line, (_, line))| {
            let mut breakpoint = before.iter().find(|known| known.line == *old_line)?.clone();
            breakpoint.line = *line;
            Some(breakpoint)
        })
        .collect();
    let state = cx.default_global::<State>();
    state.shown.insert(path.to_owned(), now);
    *state.own_writes.entry(path.to_owned()).or_default() += 1;
    // One replacement of the file's list, so breakpoints trading lines
    // never meet halfway.
    store().load_json(&json!({ "files": { path.to_string_lossy(): after } }));
    // The editor that was typed in is already right; the hub's copy serves
    // the others and editors opened later.
    let marks = marks.to_vec();
    code_editor::hub(cx).update(cx, |hub, cx| {
        hub.set_gutter_marks(path, GutterSlot::Breakpoint, marks, cx)
    });
}

struct Gutter;
impl GutterProvider for Gutter {
    fn click(&self, doc: &DocumentInfo, click: &GutterClick, _: &mut Window, cx: &mut App) {
        if click.button == GutterButton::Left {
            toggle_row(&doc.path, click.row, cx);
        }
    }
    fn menu(
        &self,
        doc: &DocumentInfo,
        click: &GutterClick,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<GutterMenu> {
        if click.button != GutterButton::Right {
            return None;
        }
        // A right press on an empty row sets one there and opens its form.
        let line = line_on_row(&doc.path, click.row, cx).unwrap_or_else(|| {
            let line = click.row as u32 + 1;
            store().set(&doc.path, Breakpoint::at(line));
            line
        });
        Some(menu(&doc.path, line, cx))
    }
    fn moved(&self, doc: &DocumentInfo, marks: &[GutterMark], cx: &mut App) {
        marks_moved(&doc.path, marks, cx);
    }
}

fn find(path: &Path, line: u32) -> Option<Breakpoint> {
    (store().in_file(path).into_iter()).find(|breakpoint| breakpoint.line == line)
}

/// Applies `edit` to the breakpoint on the line, if it is still there.
pub(super) fn update(path: &Path, line: u32, edit: impl FnOnce(&mut Breakpoint)) {
    if let Some(mut breakpoint) = find(path, line) {
        let before = breakpoint.clone();
        edit(&mut breakpoint);
        if breakpoint != before {
            store().set(path, breakpoint);
        }
    }
}

/// The menu of one breakpoint: its three texts as a form, then the actions.
pub(super) fn menu(path: &Path, line: u32, cx: &mut App) -> GutterMenu {
    let breakpoint = find(path, line).unwrap_or_else(|| Breakpoint::at(line));
    let form = cx.new(|cx| Form::new(path.to_owned(), &breakpoint, cx));
    let focus = form.read(cx).condition.read(cx).focus_handle(cx);
    let toggle = if breakpoint.enabled {
        "Disable"
    } else {
        "Enable"
    };
    let path = path.to_owned();
    GutterMenu {
        label: "Breakpoint".into(),
        items: vec![
            MenuItem::Custom(form.into(), focus),
            MenuItem::Separator,
            MenuAction::new("toggle", toggle).into(),
            MenuAction::new("remove", "Remove").danger().into(),
        ],
        width: 300.,
        on_pick: Rc::new(move |id, _, _| match id {
            "toggle" => update(&path, line, |breakpoint| {
                breakpoint.enabled = !breakpoint.enabled
            }),
            "remove" => {
                store().remove(&path, line);
            }
            _ => {}
        }),
    }
}

/// The condition, hit count and log message of one breakpoint. What is
/// typed is stored on Enter and when the menu closes.
pub(super) struct Form {
    path: PathBuf,
    line: u32,
    condition: Entity<Input>,
    hits: Entity<Input>,
    log: Entity<Input>,
    _events: Vec<Subscription>,
}
impl Form {
    fn new(path: PathBuf, breakpoint: &Breakpoint, cx: &mut Context<Self>) -> Self {
        let field = |label: &'static str,
                     hint: &'static str,
                     text: &Option<String>,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut field = Input::field(label, hint, content(0.35), cx);
                field.set_text(text.clone().unwrap_or_default(), cx);
                field
            })
        };
        let condition = field("Condition", "x > 10", &breakpoint.condition, cx);
        let hits = field("Hit count", "5 or >= 3", &breakpoint.hit_condition, cx);
        let log = field(
            "Log message",
            "value is {value}",
            &breakpoint.log_message,
            cx,
        );
        let events = [&condition, &hits, &log]
            .map(|field| cx.subscribe(field, |this, _, _: &input::Submitted, cx| this.commit(cx)))
            .into();
        cx.on_release(|this, cx| this.commit(cx)).detach();
        Self {
            path,
            line: breakpoint.line,
            condition,
            hits,
            log,
            _events: events,
        }
    }
    fn commit(&self, cx: &App) {
        let text = |field: &Entity<Input>| {
            let text = field.read(cx).content.trim().to_owned();
            (!text.is_empty()).then_some(text)
        };
        let (condition, hits, log) = (text(&self.condition), text(&self.hits), text(&self.log));
        update(&self.path, self.line, |breakpoint| {
            breakpoint.condition = condition;
            breakpoint.hit_condition = hits;
            breakpoint.log_message = log;
        });
    }
    #[cfg(test)]
    pub(super) fn set_condition(&mut self, text: &str, cx: &mut Context<Self>) {
        self.condition
            .update(cx, |field, cx| field.set_text(text.to_owned(), cx));
        self.commit(cx);
    }
}
impl Render for Form {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let row = |label: &'static str, field: &Entity<Input>| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(78.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(content(0.55))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(26.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .bg(content(0.06))
                        .child(field.clone()),
                )
        };
        div()
            .id("breakpoint-form")
            .debug_selector(|| "breakpoint-form".into())
            .p_2()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.7))
                    .child(SharedString::from(format!(
                        "Breakpoint on line {}",
                        self.line
                    ))),
            )
            .child(row("Condition", &self.condition))
            .child(row("Hit count", &self.hits))
            .child(row("Log message", &self.log))
    }
}

// Persistence.

fn mark_dirty(project: String, cx: &mut App) {
    cx.default_global::<State>().dirty.insert(project);
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(PERSIST_DELAY).await;
        cx.update(persist);
    });
    cx.default_global::<State>().persist = Some(task);
}

/// A project's breakpoints: those in its folder and in its worktrees.
fn project_json(project: &str, cx: &App) -> Option<Value> {
    let mut files = serde_json::Map::new();
    let folders = (context::snapshot(cx).checkouts.into_iter())
        .filter(|checkout| checkout.project == project)
        .map(|checkout| checkout.folder)
        .chain([project.to_owned()]);
    for folder in folders {
        let saved = store().to_json(Some(Path::new(&folder)));
        if let Some(found) = saved.get("files").and_then(Value::as_object) {
            files.extend(found.clone());
        }
    }
    let exceptions = store().exceptions();
    (!files.is_empty() || exceptions != ExceptionFilters::default())
        .then(|| json!({ "version": 1, "exceptions": exceptions, "files": files }))
}

fn persist(cx: &mut App) {
    let dirty = std::mem::take(&mut cx.default_global::<State>().dirty);
    for project in dirty {
        let value = project_json(&project, cx);
        if settings::value(&project, PAGE, cx) == value {
            continue;
        }
        let host = PageHost {
            project,
            page: PAGE,
        };
        code_shell::in_window(cx, |window, cx| host.save(value, window, cx));
    }
}

/// Saves what is waiting now, for a test that will not wait for the timer.
#[cfg(test)]
pub(super) fn persist_now(cx: &mut App) {
    persist(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_marks_are_matched_from_both_ends() {
        let shown = [(1, 2), (4, 5), (5, 6), (8, 9)];
        // A line typed above the second one.
        assert_eq!(
            moved_lines(&shown, &[1, 5, 6, 9]),
            [(2, (1, 2)), (5, (5, 6)), (6, (6, 7)), (9, (9, 10))]
        );
        // The lines of the second and third joined: one mark is left there,
        // and the breakpoint that was on line 6 is gone.
        assert_eq!(
            moved_lines(&shown, &[1, 4, 7]),
            [(2, (1, 2)), (5, (4, 5)), (9, (7, 8))]
        );
        // A breakpoint the adapter drew one line down keeps that distance.
        assert_eq!(moved_lines(&[(3, 3)], &[5]), [(3, (5, 5))]);
        assert_eq!(
            moved_lines(&[(4, 4), (9, 10)], &[4, 7]),
            [(4, (4, 4)), (10, (7, 8))]
        );
        // Everything deleted.
        assert!(moved_lines(&shown, &[]).is_empty());
    }

    #[test]
    fn a_mark_says_what_the_debugger_made_of_its_breakpoint() {
        let state = |breakpoint: &Breakpoint| match mark(breakpoint).kind {
            GutterMarkKind::Breakpoint(state) => state,
            GutterMarkKind::Run => unreachable!(),
        };
        let mut breakpoint = Breakpoint::at(13);
        assert_eq!(state(&breakpoint), MarkState::Enabled);
        breakpoint.status = BreakpointStatus::Verified { line: 14 };
        assert_eq!(mark(&breakpoint).row, 13, "drawn where the adapter put it");
        breakpoint.status = BreakpointStatus::Rejected {
            message: Some("No code here.".into()),
        };
        assert_eq!(state(&breakpoint), MarkState::Unverified);
        assert!(mark(&breakpoint).tooltip.contains("No code here."));
        breakpoint.status = BreakpointStatus::Idle;
        breakpoint.log_message = Some("n is {n}".into());
        assert_eq!(state(&breakpoint), MarkState::Conditional);
        breakpoint.enabled = false;
        assert_eq!(state(&breakpoint), MarkState::Disabled);
    }
}
