//! The browser's agent layer (docs/native/browser/context.md §6, K21–K42):
//! the `monocode_browser` MCP tools and the external CDP endpoint.
//!
//! - [`cdp`]: a page's CDP multiplexer, shared by everyone who speaks CDP to it.
//! - [`host`]: what the app gives: pages of a scope, opening, closing, timers.
//! - [`log`]: a page's console and network log (K36).
//! - [`tools`]: the tool set and its implementation over [`host`].
//! - [`bridge`]: the MCP server: loopback listener, session tokens, the
//!   `browser-mcp` stdio subcommand, provider wiring.
//! - [`proxy`]: the external, Chrome-compatible CDP endpoint (K26, K27).
//!
//! Threads: the bridge and the proxy listen on threads of their own; every
//! call reaches the app's main thread as a [`Call`] and is run there.

pub mod cdp;
pub mod host;
pub mod log;
pub mod tools;

pub use cdp::{CdpClient, CdpError, CdpEvent, Subscription};
pub use host::{DialogInfo, DialogKind, Host, Page, PageId, PageInfo, Scope};

/// What a tool answers: MCP `content` items, and whether it failed.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub content: Vec<Content>,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    /// Base64 image data with its MIME type (`image/jpeg`, `image/png`).
    Image {
        mime: String,
        data: String,
    },
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        ToolOutput {
            content: vec![Content::Text(text.into())],
            is_error: false,
        }
    }

    /// A failure with a stable code (K41), e.g. `browser_stale_ref`.
    pub fn error(code: &str, message: impl std::fmt::Display) -> Self {
        ToolOutput {
            content: vec![Content::Text(format!("{code}: {message}"))],
            is_error: true,
        }
    }
}

/// One tool call, from the bridge's thread to the main thread.
pub struct Call {
    pub scope: Scope,
    pub tool: String,
    pub arguments: serde_json::Value,
    /// Answers the call; may be called from the main thread only once.
    pub reply: Box<dyn FnOnce(ToolOutput) + Send>,
}
