//! What the layer's API calls need, and the error texts Chrome uses, so
//! extensions that match on them keep working.
//!
//! The shim (`shim.js`) exposes the namespaces; the app answers each call
//! (`browser/extensions/dispatch.rs` in `monocode-gpui`) only when the
//! extension holds the permission [`required_permission`] names.

/// The manifest permission a call needs (`"tabs.query"` → none: Chrome
/// gives `tabs` to every extension, and only URLs need the permission).
pub fn required_permission(api: &str) -> Option<&'static str> {
    let namespace = api.split('.').next().unwrap_or("");
    Some(match namespace {
        "storage" => "storage",
        "alarms" => "alarms",
        "scripting" => "scripting",
        "webNavigation" => "webNavigation",
        "webRequest" => "webRequest",
        "offscreen" => "offscreen",
        "privacy" => "privacy",
        "cookies" => "cookies",
        "notifications" => "notifications",
        "contextMenus" => "contextMenus",
        _ => return None,
    })
}

/// Permissions the layer cannot honor: install shows them, the API
/// answers [`unsupported`].
pub const UNSUPPORTED_PERMISSIONS: &[&str] = &[
    "identity",
    "debugger",
    "proxy",
    "tts",
    "ttsEngine",
    "userScripts",
    "history",
    "bookmarks",
    "tabGroups",
    "declarativeNetRequest",
    "declarativeNetRequestWithHostAccess",
    "sidePanel",
    "pageCapture",
    "tabCapture",
    "desktopCapture",
    "fileSystemProvider",
    "documentScan",
    "printerProvider",
    "vpnProvider",
    "enterprise.platformKeys",
];

/// Chrome's text when nothing listens for a message.
pub const NO_RECEIVER: &str = "Could not establish connection. Receiving end does not exist.";
/// Chrome's text when a listener did not answer.
pub const PORT_CLOSED: &str = "The message port closed before a response was received.";

pub fn no_tab(id: i64) -> String {
    format!("No tab with id: {id}.")
}

pub fn no_window(id: i64) -> String {
    format!("No window with id: {id}.")
}

pub fn missing_permission(api: &str, permission: &str) -> String {
    format!("'{api}' requires the \"{permission}\" permission.")
}

/// A method the layer knows of but does not run.
pub fn unsupported(api: &str) -> String {
    format!("{api} is not supported in Monocode's browser.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_name_their_permission() {
        assert_eq!(required_permission("storage.local.get"), Some("storage"));
        assert_eq!(required_permission("tabs.query"), None);
        assert_eq!(required_permission("runtime.sendMessage"), None);
        assert_eq!(
            required_permission("scripting.executeScript"),
            Some("scripting")
        );
        assert!(missing_permission("alarms.create", "alarms").contains("\"alarms\""));
    }
}

/// `action.setIcon({imageData})`: RGBA pixels as a PNG for the toolbar.
pub fn icon_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > 512 || height > 512 {
        return None;
    }
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn image_data_becomes_a_png() {
        let png = super::icon_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]).unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(super::icon_png(2, 2, &[0; 4]).is_none());
    }
}
