//! The session's subagents, read from its transcript for the rail and for a
//! subagent's own view. Tool rows are parsed once per change: a patch drops
//! what it replaced, so a streaming turn reparses only its own tail.
use super::*;
use gpui::AnyElement;
use monocode_core::subagents::{self as model, Row as SubagentRow};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};

/// When a subagent was first seen, last changed and settled (Unix ms).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Clock {
    pub first_seen: u64,
    pub last_change: u64,
    pub settled_at: Option<u64>,
    fingerprint: u64,
}

#[derive(Default)]
pub(crate) struct Cache {
    /// Per source row: unparsed, not a subagent, or the spawning call.
    calls: RefCell<Vec<Option<Option<Arc<ToolCall>>>>>,
    clocks: RefCell<HashMap<String, Clock>>,
}

/// One subagent with what the rail and its view need beside its row.
#[derive(Clone, Debug)]
pub(crate) struct Subagent {
    pub row: SubagentRow,
    pub clock: Clock,
    pub depth: usize,
}

fn fingerprint(call: &ToolCall) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    std::mem::discriminant(&call.state).hash(&mut hasher);
    if let Some(Panel::Agent(run)) = &call.panel {
        run.steps.len().hash(&mut hasher);
        if let Some(step) = run.steps.last() {
            (&step.id, &step.text, step.detail.len()).hash(&mut hasher);
            step.state
                .as_ref()
                .map(std::mem::discriminant)
                .hash(&mut hasher);
        }
    }
    call.output.len().hash(&mut hasher);
    hasher.finish()
}

impl TranscriptView {
    /// Forget parsed rows from `start` on; the next read parses them again.
    pub(super) fn invalidate_subagents(&self, start: usize) {
        self.subagent_cache.calls.borrow_mut().truncate(start);
    }

    fn agent_calls(&self) -> Vec<(usize, Arc<ToolCall>)> {
        let mut calls = self.subagent_cache.calls.borrow_mut();
        for ix in calls.len()..self.source.len() {
            let row = &self.source[ix];
            let call = (row.role == "tool")
                .then(|| serde_json::from_str::<ToolCall>(&row.text).ok())
                .flatten()
                .and_then(|mut call| {
                    let Some(Panel::Agent(run)) = call.panel() else {
                        return None;
                    };
                    call.panel = Some(Panel::Agent(run));
                    Some(Arc::new(call))
                });
            calls.push(Some(call));
        }
        calls
            .iter()
            .enumerate()
            .filter_map(|(ix, call)| Some((ix, call.clone()??)))
            .collect()
    }

    /// Every subagent the loaded transcript names, spawners before what they
    /// spawned, or with `running_only` just the live ones (the rail's view,
    /// asked on every frame: settled ones only keep their clocks). `background`
    /// holds the task ids still running after the turn.
    pub(crate) fn subagents(
        &self,
        background: &HashSet<String>,
        running_only: bool,
    ) -> Vec<Subagent> {
        let now = monocode_core::time::now_ms();
        let prompt = self.source.iter().rposition(|row| row.is_prompt());
        let mut out = Vec::new();
        let mut clocks = self.subagent_cache.clocks.borrow_mut();
        let mut seen = HashSet::new();
        let mut clock_for = |id: &str, call: &ToolCall, running: bool| {
            let print = fingerprint(call);
            let clock = clocks.entry(id.to_owned()).or_insert(Clock {
                first_seen: now,
                last_change: now,
                settled_at: None,
                fingerprint: print,
            });
            if clock.fingerprint != print {
                clock.fingerprint = print;
                clock.last_change = now;
            }
            match (running, clock.settled_at) {
                (false, None) => clock.settled_at = Some(now),
                (true, Some(_)) => clock.settled_at = None,
                _ => {}
            }
            *clock
        };
        for (start, call) in self.agent_calls() {
            let Some(Panel::Agent(run)) = &call.panel else {
                continue;
            };
            let live_call = self.streaming
                && call.state == ToolState::Running
                && prompt.is_none_or(|prompt| start > prompt);
            let running = live_call || run.ids.iter().any(|id| background.contains(id));
            seen.insert(call.id.clone());
            let clock = clock_for(&call.id, &call, running);
            // Nothing under a settled spawner still runs.
            if running_only && !running {
                continue;
            }
            let row = model::row(&call, run, running, None).decayed(clock.last_change, now);
            out.push(Subagent {
                row,
                clock,
                depth: 0,
            });
            // A subagent's own subagents, as far as its steps name them.
            for step in run.steps.iter().filter(|step| step.kind == "tool") {
                let child = step.to_tool_call();
                let Some(Panel::Agent(child_run)) = child.panel() else {
                    continue;
                };
                let id = format!("{}/{}", call.id, step.id);
                let running = running && child.state == ToolState::Running;
                if running_only && !running {
                    continue;
                }
                let mut child = child.clone();
                child.id = id.clone();
                seen.insert(id.clone());
                let clock = clock_for(&id, &child, running);
                out.push(Subagent {
                    row: model::row(&child, &child_run, running, Some(call.id.clone()))
                        .decayed(clock.last_change, now),
                    clock,
                    depth: 1,
                });
            }
        }
        if !running_only {
            clocks.retain(|id, _| seen.contains(id));
        }
        out
    }

    /// The spawning call behind a subagent's view, and its source row. A
    /// nested subagent (`parent/step`) is its spawner's step as a call.
    pub(crate) fn subagent_call(&self, id: &str) -> Option<(usize, Arc<ToolCall>)> {
        let (parent, step) = match id.split_once('/') {
            Some((parent, step)) => (parent, Some(step)),
            None => (id, None),
        };
        let (start, call) = self
            .agent_calls()
            .into_iter()
            .find(|(_, call)| call.id == parent)?;
        let Some(step) = step else {
            return Some((start, call));
        };
        let Some(Panel::Agent(run)) = &call.panel else {
            return None;
        };
        let mut child = run.steps.iter().find(|s| s.id == step)?.to_tool_call();
        let panel = child.panel()?;
        child.panel = Some(panel);
        child.id = id.to_owned();
        Some((start, Arc::new(child)))
    }
}

/// Long prompts fold like a long user message.
const PROMPT_FOLD_HEIGHT: f32 = 168.;

#[derive(Default)]
pub(crate) struct Focus {
    pub id: Option<String>,
    scroll: gpui::ScrollHandle,
    prompt_open: bool,
}

impl TranscriptView {
    pub(crate) fn subagent_focus(&self) -> Option<&str> {
        self.subagent_focus.id.as_deref()
    }
    /// Show one subagent's own transcript in place of the session's, or the
    /// session again with `None`.
    pub(crate) fn set_subagent_focus(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.subagent_focus.id != id {
            self.subagent_focus = Focus {
                id,
                ..Focus::default()
            };
            cx.notify();
        }
    }

    /// A subagent's view: the task the main agent gave it, then its work and
    /// its report. `None` when the focused subagent is not in the transcript.
    pub(super) fn render_subagent_focus(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.subagent_focus.id.clone()?;
        let (start, call) = self.subagent_call(&id)?;
        let Some(Panel::Agent(run)) = &call.panel else {
            return None;
        };
        let prompt_ix = self.source.iter().rposition(|row| row.is_prompt());
        let active = self.streaming
            && call.state == ToolState::Running
            && prompt_ix.is_none_or(|prompt| start > prompt);
        let row = model::row(&call, run, active, None);
        let viewport = f32::from(window.viewport_size().width);
        let width = px((viewport.min(COLUMN) - 2. * crate::theme::COLUMN_PAD).max(1.));
        let trail = tasks::agent_trail(self, &call, run, start, active, width, cx);
        let failed = matches!(call.state, ToolState::Failed | ToolState::Cancelled);
        let report = call.agent_report().map(|report| {
            if failed {
                div()
                    .font_family(crate::fonts::mono())
                    .text_size(px(12.))
                    .line_height(px(20.))
                    .text_color(rgb(0xf87171))
                    .child(report.to_owned())
                    .into_any_element()
            } else {
                tasks::markdown_text(
                    self,
                    report,
                    &format!("{}:report", call.id),
                    markdown::Tone::Normal,
                    cx,
                )
            }
        });
        let prompt = (!row.prompt.is_empty()).then(|| {
            let text = tasks::markdown_text(
                self,
                &row.prompt,
                &format!("subagent-prompt:{id}"),
                markdown::Tone::Normal,
                cx,
            );
            let long = row.prompt.lines().count() > 8 || row.prompt.len() > 600;
            let open = self.subagent_focus.prompt_open;
            div()
                .id("subagent-prompt")
                .debug_selector(|| "subagent-prompt".into())
                .w_full()
                .rounded(px(12.))
                .border_1()
                .border_color(content(0.1))
                .bg(content(0.04))
                .px(px(14.))
                .py(px(10.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .mb_1()
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .child("Prompt from main agent"),
                )
                .child(
                    div()
                        .w_full()
                        .text_size(px(14.))
                        .when(long && !open, |d| {
                            d.max_h(px(PROMPT_FOLD_HEIGHT)).overflow_hidden()
                        })
                        .child(text),
                )
                .when(long, |d| {
                    d.child(
                        div()
                            .id("subagent-prompt-fold")
                            .debug_selector(|| "subagent-prompt-fold".into())
                            .role(Role::Button)
                            .aria_expanded(open)
                            .mt_1()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .hover(|s| s.text_color(content(0.85)))
                            .child(if open { "Show less" } else { "Show more" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.subagent_focus.prompt_open = !this.subagent_focus.prompt_open;
                                cx.notify();
                            })),
                    )
                })
        });
        Some(
            div()
                .id("subagent-view")
                .debug_selector(|| "subagent-view".into())
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.subagent_focus.scroll)
                .child(
                    div().w_full().flex().justify_center().child(
                        div()
                            .w_full()
                            .max_w(px(COLUMN))
                            .px(px(crate::theme::COLUMN_PAD))
                            .pt(px(16.))
                            .pb(px(24.))
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .children(prompt)
                            .child(trail)
                            .children(report.map(|report| {
                                div()
                                    .relative()
                                    .pl(px(20.))
                                    .py_1()
                                    .child(crate::activity::rail(true))
                                    .child(report)
                            })),
                    ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
impl TranscriptView {
    pub(crate) fn source_len(&self) -> usize {
        self.source.len()
    }
    pub(crate) fn patch_view_for_tests(
        &mut self,
        start: usize,
        rows: Vec<monocode_core::transcript::Row>,
        cx: &mut Context<Self>,
    ) {
        self.patch_view(start, rows, cx);
    }
}
