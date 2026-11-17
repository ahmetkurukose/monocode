//! Code tooling for a project's checkouts: run configurations, language
//! servers and debug adapters (docs/native/code.md). Everything here is
//! headless: the panes and the agents' MCP tools sit on the same calls.
pub mod spawn;
pub mod toolchain;
pub mod wire;
