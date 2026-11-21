//! The external, Chrome-compatible CDP endpoint (context.md §6.2, K26, K27,
//! K38): one browser-level endpoint per project scope, on the loopback,
//! behind a random token.
//!
//! STUB: the CDP-endpoint work fills this in. The signatures are the
//! contract the app and the bridge build on.

use std::sync::atomic::{AtomicBool, Ordering};

use super::Scope;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether the endpoint may be handed out (Settings; off by default, K27).
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turns the endpoint on or off. Off closes every client connection.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The endpoint's base URL for `scope` (`http://127.0.0.1:<port>/<token>`),
/// which Playwright `connectOverCDP` and dev-browser `--connect` take as is.
/// Starts the listener on first use. Errors when disabled.
pub fn endpoint(scope: &Scope) -> Result<String, String> {
    let _ = scope;
    if !enabled() {
        return Err("External CDP connections are turned off in Settings".into());
    }
    Err("not implemented yet".into())
}
