//! Debugging over the Debug Adapter Protocol: the client that talks to an
//! adapter process, the session that models one debuggee, the breakpoint
//! store and the manager that owns the app's sessions. Headless; the debug
//! pane and the agents' tools are both callers of `Manager` and `Session`.
pub mod adapter;
pub mod breakpoints;
pub mod client;
mod manager;
mod session;
mod types;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod real_tests;
#[cfg(test)]
mod session_tests;

pub use adapter::{
    attach_to_process, derive, Adapter, Console, DebugRequest, LaunchOptions, RequestKind,
};
pub use breakpoints::{
    Breakpoint, BreakpointChange, BreakpointStatus, BreakpointStore, ExceptionFilters,
};
pub use manager::Manager;
pub use session::{Refresh, Session, SessionEvent, SessionId, TerminalRequest};
pub use types::{
    Actor, DapError, EvaluateContext, Evaluation, Frame, FrameLocals, Output, OutputCategory, Page,
    Scope, SessionState, StepKind, Stop, Thread, Variable,
};
