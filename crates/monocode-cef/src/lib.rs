//! Monocode's Chromium engine (docs/native/browser/context.md).
//!
//! - [`runtime`]: loads and initializes CEF inside the app.
//! - [`view`]: a page shown as a native child view of the host window.
//! - [`DevToolsView`]: a page's DevTools, docked as a second child view.
//!
//! Nothing here depends on the UI toolkit; the app hands in native view
//! pointers and receives page events.

pub mod runtime;
pub mod view;

mod capture;
mod devtools;

mod dialogs;
mod download;
#[cfg(target_os = "macos")]
mod mac;
mod popup;
mod profile;

pub use devtools::{DevToolsEvent, DevToolsView};
pub use runtime::{RuntimeConfig, StartError};
pub use view::{
    AuthChallenge, AuthRequest, BrowserEvent, BrowserView, CreateError, CreateOptions, DialogKind,
    Download, DownloadState, Frame, JsDialog, Profile, cancel_download, profile_id, take_popup,
};

/// `OnScheduleMessagePumpWork`, from any CEF thread.
fn pump_schedule(delay_ms: i64) {
    #[cfg(target_os = "macos")]
    mac::pump::schedule_work(delay_ms);
    #[cfg(not(target_os = "macos"))]
    let _ = delay_ms;
}
