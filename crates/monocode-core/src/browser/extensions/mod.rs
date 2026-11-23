//! The browser's own extension layer (docs/native/browser/context.md K69):
//! Chrome Web Store extensions (Manifest V3) run without Chromium's
//! extension system. This half is everything that needs no browser:
//!
//! - [`crx`]: CRX3 packages, verified as Chromium does (`crx_verifier.cc`).
//! - [`zip`]: unpacking a package's archive.
//! - [`manifest`]: `manifest.json`, localized, with what the layer needs.
//! - [`pattern`]: match patterns and globs.
//! - [`registry`]: installed extensions on disk, install, update, remove.
//! - [`webstore`]: the Web Store's download and update protocol.
//! - [`permissions`]: install warnings, and whether an update adds any.
//! - [`storage`]: `chrome.storage` areas over SQLite, with Chrome's quotas.
//! - [`tabs`]: `chrome.tabs.Tab` and `tabs.query` over the app's tab model.
//! - [`scripts`]: the scripts the layer runs in pages and extension
//!   contexts: the `chrome.*` shim (`shim.js`), content script programs.
//! - [`api`]: which call needs which permission, and the error texts.
//!
//! The app (`monocode-gpui`, `browser/extensions`) runs the extension:
//! the `mcext://` scheme, the background host, content scripts over CDP,
//! the toolbar and popup.

pub mod api;
pub mod crx;
pub mod manifest;
pub mod pattern;
pub mod permissions;
pub mod registry;
pub mod scripts;
pub mod storage;
pub mod tabs;
pub mod webstore;
pub mod zip;

#[cfg(test)]
mod shim_tests;

/// The scheme extension files are served from: `mcext://<id>/<path>`.
pub const SCHEME: &str = "mcext";

/// Proton Pass, the first extension the layer supports (K60, K70).
pub const PROTON_PASS_ID: &str = "ghmbeldphafepmbegfdlkpapadhbakde";

/// Extensions that manage passwords: while one is enabled, the browser's
/// own password manager neither offers to save nor fills (K60).
pub const PASSWORD_MANAGERS: &[&str] = &[
    PROTON_PASS_ID,
    // 1Password, Bitwarden: next in line (phase 5b).
    "aeblfdkhhhdcdjpifhhbdiojplfjncoa",
    "nngceckbapebfimnlniiiahkandclblb",
];

/// An extension's origin: `mcext://<id>`.
pub fn origin(id: &str) -> String {
    format!("{SCHEME}://{id}")
}

/// An extension's base URL: `mcext://<id>/`.
pub fn base_url(id: &str) -> String {
    format!("{SCHEME}://{id}/")
}

/// Chromium's extension ids: 32 letters a to p.
pub fn is_valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Chrome versions: up to four dot-separated integers; missing parts are 0.
/// `None` when it is not one.
pub fn parse_version(version: &str) -> Option<[u32; 4]> {
    let mut out = [0u32; 4];
    let parts: Vec<&str> = version.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    for (slot, part) in out.iter_mut().zip(&parts) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(out)
}

/// Orders two Chrome versions; an unreadable one sorts first.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    parse_version(a).cmp(&parse_version(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_versions_read_as_chrome_reads_them() {
        assert!(is_valid_id(PROTON_PASS_ID));
        assert!(!is_valid_id("ghmbeldphafepmbegfdlkpapadhbakdz"));
        assert!(!is_valid_id("short"));
        assert_eq!(parse_version("1.44"), Some([1, 44, 0, 0]));
        assert_eq!(parse_version("1.2.3.4.5"), None);
        assert_eq!(parse_version("1.x"), None);
        assert!(compare_versions("1.44.0", "1.9.9").is_gt());
        assert!(compare_versions("1.44", "1.44.0.0").is_eq());
        assert_eq!(origin("abc"), "mcext://abc");
    }
}
