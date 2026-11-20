//! Monocode's Chromium engine (docs/native/browser/context.md).
//!
//! - [`install`]: downloads and verifies the pinned CEF runtime on first use.
//! - [`runtime`]: loads and initializes CEF inside the app.
//! - [`view`]: a page shown as a native child view of the host window.
//!
//! Nothing here depends on the UI toolkit; the app hands in native view
//! pointers and receives page events.

pub mod install;
pub mod runtime;
pub mod view;

#[cfg(target_os = "macos")]
mod mac;

pub use install::{CEF_VERSION, InstalledRuntime, Progress};
pub use runtime::{RuntimeConfig, StartError};
pub use view::{BrowserEvent, BrowserView, CreateError, Frame};

/// `OnScheduleMessagePumpWork`, from any CEF thread.
fn pump_schedule(delay_ms: i64) {
    #[cfg(target_os = "macos")]
    mac::pump::schedule_work(delay_ms);
    #[cfg(not(target_os = "macos"))]
    let _ = delay_ms;
}
