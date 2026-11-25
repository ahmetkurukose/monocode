//! The browser's agent layer (docs/native/browser/context.md §6, K21–K42):
//! the `monocode_browser` MCP tools and the external CDP endpoint.
//!
//! - [`cdp`]: a page's CDP multiplexer, shared by everyone who speaks CDP to it.
//! - [`host`]: what the app gives: pages of a scope, opening, closing, timers.
//! - [`log`]: a page's console and network log (K36).
//! - [`grab`]: the element grab, a picked element for an agent (K49).
//! - [`audio`]: whether a page plays sound, for its tab.
//! - [`passkeys`]: a page's passkey requests (K58).
//! - [`media`]: a page's camera and microphone requests, held until macOS decides.
//! - [`history`]: browsing history, kept per project (K55, K65).
//! - [`import`]: cookies and passwords from Chrome, Arc and Brave (K61).
//! - [`dev_servers`]: a project's running dev servers, for the new tab page (K66).
//! - [`extensions`]: the browser's own extension layer (K69).
//! - [`passwords`]: the password vault, sign-in capture and autofill (K56, K57).
//! - [`tools`]: the tool set and its implementation over [`host`].
//! - [`bridge`]: the MCP server: loopback listener, session tokens, the
//!   `browser-mcp` stdio subcommand, provider wiring.
//! - [`proxy`]: the external, Chrome-compatible CDP endpoint (K26, K27).
//!
//! Threads: the bridge and the proxy listen on threads of their own; every
//! piece of work reaches the app's main thread as a [`Job`] through
//! [`submit`], and the app runs each with its [`Host`] (see [`jobs`]).

pub mod audio;
pub mod bridge;
pub mod cdp;
pub mod control;
pub mod dev_servers;
pub mod extensions;
pub mod favicon;
pub mod grab;
pub mod history;
pub mod host;
pub mod import;
pub mod log;
pub mod passwords;
pub mod media;
pub mod passkeys;
pub mod playback;
pub mod proxy;
pub mod sites;
pub mod suggest;
pub mod tools;

pub use cdp::{CdpClient, CdpError, CdpEvent, Subscription};
pub use control::{ControlState, UserControl, UserInput};
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

/// Work for the main thread: given the app's host, it returns a future the
/// app runs on its foreground executor.
pub type Job = Box<
    dyn FnOnce(std::rc::Rc<dyn Host>) -> futures_util::future::LocalBoxFuture<'static, ()> + Send,
>;

fn job_channel() -> &'static (async_channel::Sender<Job>, async_channel::Receiver<Job>) {
    static JOBS: std::sync::OnceLock<(async_channel::Sender<Job>, async_channel::Receiver<Job>)> =
        std::sync::OnceLock::new();
    JOBS.get_or_init(async_channel::unbounded)
}

/// Hands `job` to the main thread, from any thread. False only if the app
/// stopped taking work.
pub fn submit(job: Job) -> bool {
    job_channel().0.try_send(job).is_ok()
}

/// The jobs [`submit`] queued. The app takes it once at start and runs, on
/// its main thread, `job(host.clone())` for each, without waiting for one to
/// end before starting the next.
pub fn jobs() -> async_channel::Receiver<Job> {
    job_channel().1.clone()
}
