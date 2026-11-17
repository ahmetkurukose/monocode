//! The values a debug session hands to its readers: threads, frames, scopes,
//! variables, output and the error type. They are small typed views of the
//! protocol's JSON, holding only what the panes and the agents' tools show.
use serde_json::Value;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DapError {
    /// A tool the debugger needs is not installed. Nothing is ever installed
    /// for the user; `hint` says what to run.
    ToolMissing { tool: String, hint: String },
    /// The adapter process could not be started or did not come up.
    Start(String),
    /// The adapter did not answer `command` in time.
    Timeout { command: String },
    /// The connection to the adapter is gone.
    Closed,
    /// The adapter answered `command` with a failure.
    Adapter { command: String, message: String },
    /// The adapter does not offer this, by its capabilities.
    Unsupported(String),
    /// The call does not fit the session's state (not stopped, no thread).
    State(String),
    /// A wait ran out while the debuggee was still running.
    StillRunning,
}

impl fmt::Display for DapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolMissing { tool, hint } => write!(f, "{tool} was not found. {hint}"),
            Self::Start(message) | Self::State(message) => f.write_str(message),
            Self::Timeout { command } => {
                write!(f, "The debugger did not answer \"{command}\" in time.")
            }
            Self::Closed => f.write_str("The debugger has exited."),
            Self::StillRunning => {
                f.write_str("The program has not stopped yet; it is still running.")
            }
            Self::Adapter { command, message } => {
                if message.is_empty() {
                    write!(f, "The debugger refused \"{command}\".")
                } else {
                    f.write_str(message)
                }
            }
            Self::Unsupported(what) => write!(f, "This debugger does not support {what}."),
        }
    }
}

impl std::error::Error for DapError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    /// Absent for frames without a file (built-ins, generated code).
    pub path: Option<PathBuf>,
    /// 1-based.
    pub line: u32,
    pub column: u32,
    /// Library or otherwise uninteresting code, to be drawn dimmed.
    pub subtle: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    /// Costly to fetch (globals of a big module): do not expand unasked.
    pub expensive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    /// Non-zero when the value has children to fetch with `variables`.
    pub variables_reference: i64,
    pub named_children: Option<u64>,
    /// Set for large sequences; page through them with `Page`.
    pub indexed_children: Option<u64>,
    /// The expression that reaches this value from the frame, if known.
    pub evaluate_name: Option<String>,
}

/// A slice of a variable's indexed children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Page {
    pub start: u64,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub result: String,
    pub type_name: Option<String>,
    pub variables_reference: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvaluateContext {
    Repl,
    Hover,
    Watch,
}

impl EvaluateContext {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Repl => "repl",
            Self::Hover => "hover",
            Self::Watch => "watch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputCategory {
    /// The debugger's own messages.
    Console,
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub category: OutputCategory,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Over,
    In,
    Out,
}

impl StepKind {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Over => "next",
            Self::In => "stepIn",
            Self::Out => "stepOut",
        }
    }
}

/// Where and why the debuggee is paused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stop {
    /// The adapter's word: `breakpoint`, `step`, `exception`, `pause`, `entry`.
    pub reason: String,
    pub thread_id: Option<i64>,
    /// The top frame, when the adapter gave a stack.
    pub frame_id: Option<i64>,
    pub path: Option<PathBuf>,
    pub line: Option<u32>,
    /// The exception's type and message when stopped on one.
    pub exception: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The adapter is up; the debuggee is not running user code yet.
    Starting,
    Running,
    Stopped(Stop),
    /// Over. The exit code is absent when the adapter never reported one
    /// (an attached process, a start that failed).
    Terminated {
        exit_code: Option<i32>,
    },
}

impl SessionState {
    pub fn is_terminated(&self) -> bool {
        matches!(self, Self::Terminated { .. })
    }

    pub fn stop(&self) -> Option<&Stop> {
        match self {
            Self::Stopped(stop) => Some(stop),
            _ => None,
        }
    }
}

/// One frame of a stop with the variables of its local scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameLocals {
    pub frame: Frame,
    pub locals: Vec<Variable>,
}

/// Who started a session or is steering it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    User,
    /// An agent, by its session id.
    Agent(String),
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn line(value: &Value, key: &str) -> u32 {
    u32::try_from(number(value, key)).unwrap_or(0)
}

impl Thread {
    pub(crate) fn list(body: &Value) -> Vec<Self> {
        items(body, "threads")
            .map(|thread| Self {
                id: number(thread, "id"),
                name: text(thread, "name").unwrap_or_default(),
            })
            .collect()
    }
}

impl Frame {
    pub(crate) fn list(body: &Value) -> Vec<Self> {
        items(body, "stackFrames")
            .map(|frame| {
                let source = frame.get("source").unwrap_or(&Value::Null);
                let hint = |value: &Value| {
                    matches!(
                        value.get("presentationHint").and_then(Value::as_str),
                        Some("subtle" | "deemphasize" | "label")
                    )
                };
                Self {
                    id: number(frame, "id"),
                    name: text(frame, "name").unwrap_or_default(),
                    path: text(source, "path").map(PathBuf::from),
                    line: line(frame, "line"),
                    column: line(frame, "column"),
                    subtle: hint(frame) || hint(source),
                }
            })
            .collect()
    }
}

impl Scope {
    pub(crate) fn list(body: &Value) -> Vec<Self> {
        items(body, "scopes")
            .map(|scope| Self {
                name: text(scope, "name").unwrap_or_default(),
                variables_reference: number(scope, "variablesReference"),
                expensive: scope.get("expensive").and_then(Value::as_bool) == Some(true),
            })
            .collect()
    }
}

impl Variable {
    pub(crate) fn list(body: &Value) -> Vec<Self> {
        items(body, "variables")
            .map(|variable| Self {
                name: text(variable, "name").unwrap_or_default(),
                value: text(variable, "value").unwrap_or_default(),
                type_name: text(variable, "type"),
                variables_reference: number(variable, "variablesReference"),
                named_children: variable.get("namedVariables").and_then(Value::as_u64),
                indexed_children: variable.get("indexedVariables").and_then(Value::as_u64),
                evaluate_name: text(variable, "evaluateName"),
            })
            .collect()
    }
}

impl Evaluation {
    pub(crate) fn from_body(body: &Value) -> Self {
        Self {
            // `setVariable` answers with `value`, `evaluate` with `result`.
            result: text(body, "result")
                .or_else(|| text(body, "value"))
                .unwrap_or_default(),
            type_name: text(body, "type"),
            variables_reference: number(body, "variablesReference"),
        }
    }
}

fn items<'a>(body: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    body.get(key)
        .and_then(Value::as_array)
        .map(|list| list.iter())
        .unwrap_or_default()
}
