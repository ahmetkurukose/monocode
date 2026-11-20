//! Subagent rows, as Orca lists them under the agent that spawned them: the
//! same name, state, detail and age on the rail and in a subagent's own view.
//! Only running subagents are listed; a finished one leaves the rail.
use crate::tool::tasks::{AgentRun, AgentStep};
use crate::tool::{ToolCall, ToolState};
use serde_json::Value;

/// Without news for this long a live subagent claims nothing ("No update in").
pub const STALE_AFTER_MS: u64 = 30 * 60 * 1000;

/// A subagent's state as its dot shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Waiting,
    Failed,
    Working,
    Interrupted,
    Done,
    /// Still listed as running, but nothing has been heard for a long while.
    Unverifiable,
}

/// Most urgent first: a collapsed parent shows the first its children hold.
const SUMMARY_ORDER: [State; 6] = [
    State::Waiting,
    State::Failed,
    State::Working,
    State::Interrupted,
    State::Done,
    State::Unverifiable,
];

impl State {
    pub fn from_tool(state: &ToolState) -> Self {
        match state {
            ToolState::Running => Self::Working,
            ToolState::Completed => Self::Done,
            ToolState::Failed => Self::Failed,
            ToolState::Cancelled => Self::Interrupted,
        }
    }
    /// The dot's hover label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting for input",
            Self::Failed => "Failed",
            Self::Working => "Working",
            Self::Interrupted => "Interrupted",
            Self::Done => "Done",
            Self::Unverifiable => "No recent update",
        }
    }
    /// The state where it leads a line: "Working · 7s".
    pub fn heading(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Failed => "Failed",
            Self::Working => "Working",
            Self::Interrupted => "Stopped",
            Self::Done => "Completed",
            Self::Unverifiable => "Status unavailable",
        }
    }
    fn rank(self) -> usize {
        SUMMARY_ORDER
            .iter()
            .position(|s| *s == self)
            .unwrap_or(usize::MAX)
    }
    /// Quiet states never colour a collapsed parent's count.
    fn quiet(self) -> bool {
        matches!(self, Self::Done | Self::Unverifiable)
    }
}

/// What a row says beside its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    /// The tool it is running now, `tool: input`.
    Operation {
        tool: String,
        input: String,
    },
    Message(String),
    Role(String),
    NoUpdate,
}

/// One subagent of a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The spawning tool call's id; the row's identity.
    pub id: String,
    /// The provider's own task/thread ids, which a targeted stop names.
    pub task_ids: Vec<String>,
    /// '' when the subagent reported no label; the row then reads its state.
    pub name: String,
    /// Set when it named itself only by role or not at all.
    pub id_suffix: Option<String>,
    pub agent_type: Option<String>,
    pub model: Option<String>,
    pub state: State,
    pub detail: Option<Detail>,
    /// The task the main agent gave it.
    pub prompt: String,
    /// The subagent that spawned this one; `None` when the main agent did.
    pub parent: Option<String>,
    pub running: bool,
}

/// Labels that name no one subagent.
const PLACEHOLDERS: [&str; 4] = ["unknown", "untitled", "task", "subagent"];

fn usable(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty() && !PLACEHOLDERS.contains(&trimmed.to_lowercase().as_str()))
        .then(|| trimmed.to_owned())
}

/// Like Codex's own agent list, an agent with no task name or nickname is
/// told apart by its id's tail.
pub fn child_name(
    description: Option<&str>,
    name: Option<&str>,
    agent_type: Option<&str>,
    id: &str,
) -> (String, Option<String>) {
    // Some hosts publish the role as the name; that names no one child.
    let label = usable(description).or_else(|| {
        if name.is_some() && name == agent_type {
            None
        } else {
            usable(name)
        }
    });
    let suffix = label.is_none().then(|| {
        let tail = id.rsplit('-').next().unwrap_or(id).trim();
        let skip = tail.chars().count().saturating_sub(12);
        tail.chars().skip(skip).collect::<String>()
    });
    (
        label.or_else(|| usable(agent_type)).unwrap_or_default(),
        suffix.filter(|s| !s.is_empty()),
    )
}

/// "Explore 1" and "Explore 2" read as two agents; a repeated label takes the
/// next free ordinal.
pub fn disambiguate(labels: &[String]) -> Vec<String> {
    use std::collections::{HashMap, HashSet};
    let mut taken = HashSet::new();
    let mut repeated = HashSet::new();
    for label in labels {
        if !taken.insert(label.clone()) {
            repeated.insert(label.clone());
        }
    }
    let mut claimed: HashSet<String> = HashSet::new();
    let mut next: HashMap<String, usize> = HashMap::new();
    labels
        .iter()
        .map(|base| {
            let mut label = base.clone();
            if repeated.contains(&label) && claimed.contains(&label) {
                let mut ordinal = next.get(base).copied().unwrap_or(2);
                while taken.contains(&label) || claimed.contains(&label) {
                    label = format!("{base} {ordinal}");
                    ordinal += 1;
                }
                next.insert(base.clone(), ordinal);
            }
            claimed.insert(label.clone());
            label
        })
        .collect()
}

/// Agent type labels as Orca writes them.
pub fn role_label(agent_type: &str) -> String {
    match agent_type.trim() {
        "" | "unknown" => "Agent".into(),
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "opencode" => "OpenCode".into(),
        "gemini" => "Gemini".into(),
        "cursor" => "Cursor".into(),
        "copilot" => "GitHub Copilot".into(),
        "pi" => "Pi".into(),
        other => other.into(),
    }
}

// A default role or one that restates the name adds nothing.
fn role_detail(name: &str, agent_type: Option<&str>) -> Option<Detail> {
    let agent_type = agent_type.unwrap_or("unknown").trim();
    (agent_type != "default" && name.trim() != agent_type)
        .then(|| Detail::Role(agent_type.to_owned()))
}

fn last_message(run: &AgentRun) -> Option<String> {
    run.steps
        .iter()
        .rev()
        .filter(|step| step.kind != "tool" && step.kind != "reasoning")
        .find_map(|step| {
            step.text
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
}

fn operation(step: &AgentStep) -> Detail {
    let call = step.to_tool_call();
    let (tool, input) = call.summary();
    Detail::Operation {
        tool: if tool.is_empty() { call.name } else { tool },
        input: input.lines().next().unwrap_or("").trim().to_owned(),
    }
}

/// The one rule for what a row says, in the order a CLI agent row decides it.
fn detail(state: State, run: &AgentRun, name: &str) -> Option<Detail> {
    let message_or_role = || {
        last_message(run)
            .map(Detail::Message)
            .or_else(|| role_detail(name, run.agent_type.as_deref()))
    };
    match state {
        State::Unverifiable => Some(Detail::NoUpdate),
        State::Working | State::Waiting => run
            .steps
            .iter()
            .rev()
            .find(|step| step.kind == "tool" && step.state == Some(ToolState::Running))
            .map(operation)
            .or_else(message_or_role),
        State::Failed | State::Done => message_or_role(),
        State::Interrupted => role_detail(name, run.agent_type.as_deref()),
    }
}

fn input_text(input: &Value, key: &str) -> Option<String> {
    input[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A subagent's row, from the tool call that spawned it. `running` is the
/// caller's verdict: the call is live in a streaming turn, or its task still
/// runs in the background.
pub fn row(call: &ToolCall, run: &AgentRun, running: bool, parent: Option<String>) -> Row {
    let input: Value = serde_json::from_str(&call.input).unwrap_or_default();
    let description = input_text(&input, "description");
    let (name, id_suffix) = child_name(
        description.as_deref(),
        run.name.as_deref(),
        run.agent_type.as_deref(),
        run.ids.first().map_or(call.id.as_str(), String::as_str),
    );
    let state = if running {
        State::Working
    } else {
        match State::from_tool(&call.state) {
            // A call that ended without settling its task was cut short.
            State::Working => State::Interrupted,
            state => state,
        }
    };
    let prompt = input_text(&input, "prompt")
        .or_else(|| input_text(&input, "message"))
        .unwrap_or_else(|| run.brief.trim().to_owned());
    Row {
        id: call.id.clone(),
        task_ids: run.ids.clone(),
        detail: detail(state, run, &name),
        name,
        id_suffix,
        agent_type: run.agent_type.clone(),
        model: run
            .model
            .clone()
            .filter(|m| !["", "auto", "default", "inherit"].contains(&m.trim())),
        state,
        prompt,
        parent,
        running,
    }
}

impl Row {
    /// The name with an anonymous agent's id tail; '' when it reported none.
    fn label(&self) -> String {
        let name = self.name.trim();
        match &self.id_suffix {
            Some(suffix) if !name.is_empty() => format!("{name} ({suffix})"),
            _ => name.to_owned(),
        }
    }
    /// The row's label, or its state's words when it reported no name.
    pub fn display_name(&self) -> String {
        let label = self.label();
        if !label.is_empty() {
            return label;
        }
        match &self.id_suffix {
            Some(suffix) => format!("{} ({suffix})", self.state.label()),
            None => self.state.label().to_owned(),
        }
    }
    /// The detail's words; `recency` is when it was last heard from.
    pub fn detail_text(&self, recency: u64, now: u64) -> String {
        match &self.detail {
            None => String::new(),
            Some(Detail::Operation { tool, input }) if input.is_empty() => tool.clone(),
            Some(Detail::Operation { tool, input }) => format!("{tool}: {input}"),
            Some(Detail::Message(text)) => text.clone(),
            Some(Detail::Role(agent_type)) => role_label(agent_type),
            Some(Detail::NoUpdate) => {
                format!(
                    "No update in {}",
                    compact_duration(now.saturating_sub(recency))
                )
            }
        }
    }
    /// Name, then detail unless it only repeats the name.
    pub fn lead_trail(&self, recency: u64, now: u64) -> (String, String) {
        let lead = self.display_name();
        let trail = self.detail_text(recency, now);
        let trail = if trail == lead { String::new() } else { trail };
        (lead, trail)
    }
    /// A live row nothing has been heard from for long claims nothing.
    pub fn decayed(mut self, last_change: u64, now: u64) -> Self {
        if self.running && now.saturating_sub(last_change) >= STALE_AFTER_MS {
            self.state = State::Unverifiable;
            self.detail = Some(Detail::NoUpdate);
        }
        self
    }
}

/// The most urgent non-quiet state among a collapsed parent's subagents.
pub fn collapsed_signal(states: impl IntoIterator<Item = State>) -> Option<State> {
    states
        .into_iter()
        .min_by_key(|state| state.rank())
        .filter(|state| !state.quiet())
}

/// Compact "now / 5m / 3h / 2d" age.
pub fn short_time_ago(at: u64, now: u64) -> String {
    let delta = now.saturating_sub(at);
    if delta < 60_000 {
        return "now".into();
    }
    compact_duration(delta)
}

/// Coarse `34m` / `2h` / `3d`, floored so it never overstates the gap.
pub fn compact_duration(delta_ms: u64) -> String {
    let minutes = delta_ms / 60_000;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

/// A running clock: `7s`, `1m 5s`, `1h 2m 3s`.
pub fn elapsed(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m {}s", minutes / 60, minutes % 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::tasks::Panel;

    fn spawn(input: Value, run: AgentRun) -> ToolCall {
        let mut call = ToolCall::new("call-1", "Agent");
        call.input = input.to_string();
        call.panel = Some(Panel::Agent(run));
        call
    }

    #[test]
    fn a_description_names_the_row_and_a_role_alone_gets_an_id_tail() {
        assert_eq!(
            child_name(Some("Find flaky tests"), None, Some("Explore"), "a-b"),
            ("Find flaky tests".into(), None)
        );
        assert_eq!(
            child_name(
                None,
                Some("Explore"),
                Some("Explore"),
                "agent-abcdef0123456789"
            ),
            ("Explore".into(), Some("ef0123456789".into()))
        );
        assert_eq!(
            child_name(Some("task"), None, None, "x-1f2e"),
            (String::new(), Some("1f2e".into()))
        );
    }

    #[test]
    fn repeated_labels_take_ordinals() {
        let labels: Vec<String> = ["Explore", "Explore", "Plan", "Explore"]
            .map(String::from)
            .into();
        assert_eq!(
            disambiguate(&labels),
            ["Explore", "Explore 2", "Plan", "Explore 3"].map(String::from)
        );
    }

    #[test]
    fn a_working_row_says_its_running_tool_then_its_last_message_then_its_role() {
        let mut run = AgentRun {
            agent_type: Some("Explore".into()),
            ..AgentRun::default()
        };
        let call = spawn(
            serde_json::json!({"description": "Map the rail"}),
            run.clone(),
        );
        let row = row(&call, &run, true, None);
        assert_eq!(row.state, State::Working);
        assert_eq!(row.detail, Some(Detail::Role("Explore".into())));

        run.steps.push(AgentStep {
            id: "m".into(),
            kind: "message".into(),
            text: "\nReading the tree builder\nmore".into(),
            ..AgentStep::default()
        });
        let call = spawn(
            serde_json::json!({"description": "Map the rail"}),
            run.clone(),
        );
        assert_eq!(
            super::row(&call, &run, true, None).lead_trail(0, 0),
            ("Map the rail".into(), "Reading the tree builder".into())
        );
    }

    #[test]
    fn the_prompt_is_the_full_task_and_a_stale_row_reports_its_silence() {
        let run = AgentRun::default();
        let call = spawn(
            serde_json::json!({"description": "d", "prompt": "Do the whole thing"}),
            run.clone(),
        );
        let row = row(&call, &run, true, None);
        assert_eq!(row.prompt, "Do the whole thing");
        let stale = row.decayed(0, STALE_AFTER_MS + 120_000);
        assert_eq!(stale.state, State::Unverifiable);
        assert_eq!(stale.detail_text(0, 45 * 60_000), "No update in 45m");
    }

    #[test]
    fn a_collapsed_parent_signals_its_most_urgent_live_child() {
        assert_eq!(
            collapsed_signal([State::Working, State::Failed]),
            Some(State::Failed)
        );
        assert_eq!(collapsed_signal([State::Unverifiable]), None);
        assert_eq!(collapsed_signal([]), None);
    }

    #[test]
    fn ages_and_clocks_read_compactly() {
        assert_eq!(short_time_ago(0, 59_000), "now");
        assert_eq!(short_time_ago(0, 5 * 60_000), "5m");
        assert_eq!(short_time_ago(0, 3 * 3_600_000), "3h");
        assert_eq!(elapsed(7), "7s");
        assert_eq!(elapsed(65), "1m 5s");
        assert_eq!(elapsed(3723), "1h 2m 3s");
    }
}
