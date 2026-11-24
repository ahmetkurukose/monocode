//! Monocode's Chromium engine (docs/native/browser/context.md).
//!
//! - [`runtime`]: loads and initializes CEF inside the app.
//! - [`view`]: a page shown as a native child view of the host window.
//! - [`extensions`]: the `mcext` scheme and fetch proxy of the app's own
//!   extension layer (K69).
//!
//! Nothing here depends on the UI toolkit; the app hands in native view
//! pointers and receives page events.

pub mod extensions;
pub mod runtime;
pub mod view;

mod capture;
mod certs;
mod context_menu;
mod cookies;
mod devtools;

mod dialogs;
mod download;
#[cfg(target_os = "macos")]
mod mac;
/// An extension popup's window (K69).
#[cfg(target_os = "macos")]
pub use mac::ext_window::{Anchor as PopupAnchor, ExtWindow};
mod permissions;
mod popup;
mod profile;

pub use cookies::{CookieWrite, NewCookie, SameSite as CookieSameSite, set_cookies};
pub use devtools::{DevToolsAttachment, DevToolsSink};
pub use runtime::{RuntimeConfig, StartError};
pub use view::{
    AuthChallenge, AuthRequest, BrowserEvent, BrowserView, CertificateRequest, CreateError,
    CreateOptions, DialogKind, Download, DownloadInto, DownloadState, EditCommand, Frame, JsDialog,
    PermissionAnswer, PermissionRequest, Profile, cancel_download, forget_profile, profile_id,
    reset_site_permission, take_popup,
};

/// `OnScheduleMessagePumpWork`, from any CEF thread.
fn pump_schedule(delay_ms: i64) {
    #[cfg(target_os = "macos")]
    mac::pump::schedule_work(delay_ms);
    #[cfg(not(target_os = "macos"))]
    let _ = delay_ms;
}
