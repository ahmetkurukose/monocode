//! The `monocode_browser` tools (context.md K24, K31–K42).
//!
//! STUB: the agent-tools work fills this in. The signatures are the contract
//! the bridge and the app build on.

use std::rc::Rc;

use serde_json::Value;

use super::{Call, Host, ToolOutput};

/// MCP tool definitions (`{"name","description","inputSchema"}`), in order.
pub fn definitions() -> Vec<Value> {
    Vec::new()
}

/// Tools that need the user's approval each time (K25): left out of the
/// provider's allow list.
pub const NEEDS_APPROVAL: &[&str] = &["browser_eval"];

/// Runs one call on the main thread and answers it.
pub async fn run(host: Rc<dyn Host>, call: Call) {
    let _ = host;
    (call.reply)(ToolOutput::error(
        "browser_not_started",
        format!("{} is not implemented yet", call.tool),
    ));
}
