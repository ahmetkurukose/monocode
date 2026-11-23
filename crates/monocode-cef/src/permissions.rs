//! Site permissions (docs/native/browser/context.md K55, K63, §3.3).
//!
//! An Alloy page has no permission UI of its own: CEF asks
//! `CefPermissionHandler`, which hands every question to the host as a
//! [`PermissionRequest`] naming what is asked for ("camera",
//! "geolocation", …). The host decides (monocode-core's
//! `browser::sites` policy) and answers; dropping the last clone
//! unanswered ignores a prompt and refuses media.
//!
//! What Chromium asks through `OnShowPermissionPrompt` it also remembers:
//! an accepted or denied prompt becomes the site's content setting in the
//! profile. [`reset_site_permission`] gives a site the question back.
//! Camera and microphone come through `OnRequestMediaAccessPermission`,
//! which Chromium does not remember. Every callback runs on CEF's UI thread.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use cef::*;

use crate::{
    dialogs::AnyThread,
    view::{BrowserEvent, PageState},
};

type Kind = sys::cef_permission_request_types_t;
type Media = sys::cef_media_access_permission_types_t;

/// Chromium's permission prompt types and the names the host knows them by.
const PROMPT_NAMES: &[(Kind, &str)] = &[
    (Kind::CEF_PERMISSION_TYPE_AR_SESSION, "ar_session"),
    (
        Kind::CEF_PERMISSION_TYPE_CAMERA_PAN_TILT_ZOOM,
        "camera_pan_tilt_zoom",
    ),
    (Kind::CEF_PERMISSION_TYPE_CAMERA_STREAM, "camera"),
    (
        Kind::CEF_PERMISSION_TYPE_CAPTURED_SURFACE_CONTROL,
        "captured_surface_control",
    ),
    (Kind::CEF_PERMISSION_TYPE_CLIPBOARD, "clipboard_read"),
    (
        Kind::CEF_PERMISSION_TYPE_TOP_LEVEL_STORAGE_ACCESS,
        "top_level_storage_access",
    ),
    (Kind::CEF_PERMISSION_TYPE_DISK_QUOTA, "disk_quota"),
    (Kind::CEF_PERMISSION_TYPE_LOCAL_FONTS, "local_fonts"),
    (Kind::CEF_PERMISSION_TYPE_GEOLOCATION, "geolocation"),
    (Kind::CEF_PERMISSION_TYPE_HAND_TRACKING, "hand_tracking"),
    (
        Kind::CEF_PERMISSION_TYPE_IDENTITY_PROVIDER,
        "identity_provider",
    ),
    (Kind::CEF_PERMISSION_TYPE_IDLE_DETECTION, "idle_detection"),
    (Kind::CEF_PERMISSION_TYPE_MIC_STREAM, "microphone"),
    (Kind::CEF_PERMISSION_TYPE_MIDI_SYSEX, "midi_sysex"),
    (
        Kind::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS,
        "multiple_downloads",
    ),
    (Kind::CEF_PERMISSION_TYPE_NOTIFICATIONS, "notifications"),
    (Kind::CEF_PERMISSION_TYPE_KEYBOARD_LOCK, "keyboard_lock"),
    (Kind::CEF_PERMISSION_TYPE_POINTER_LOCK, "pointer_lock"),
    (
        Kind::CEF_PERMISSION_TYPE_PROTECTED_MEDIA_IDENTIFIER,
        "protected_media_identifier",
    ),
    (
        Kind::CEF_PERMISSION_TYPE_REGISTER_PROTOCOL_HANDLER,
        "register_protocol_handler",
    ),
    (Kind::CEF_PERMISSION_TYPE_STORAGE_ACCESS, "storage_access"),
    (Kind::CEF_PERMISSION_TYPE_VR_SESSION, "vr_session"),
    (
        Kind::CEF_PERMISSION_TYPE_WEB_APP_INSTALLATION,
        "web_app_installation",
    ),
    (
        Kind::CEF_PERMISSION_TYPE_WINDOW_MANAGEMENT,
        "window_management",
    ),
    (
        Kind::CEF_PERMISSION_TYPE_FILE_SYSTEM_ACCESS,
        "file_system_access",
    ),
    (
        Kind::CEF_PERMISSION_TYPE_LOCAL_NETWORK_ACCESS_DEPRECATED,
        "local_network",
    ),
    (Kind::CEF_PERMISSION_TYPE_LOCAL_NETWORK, "local_network"),
    (
        Kind::CEF_PERMISSION_TYPE_LOOPBACK_NETWORK,
        "loopback_network",
    ),
    (Kind::CEF_PERMISSION_TYPE_SENSORS, "sensors"),
    (Kind::CEF_PERMISSION_TYPE_AMBIENT_LOGIN, "ambient_login"),
];

/// The names of the prompt types in `bits`, once each, in Chromium's order;
/// a bit CEF added later is "unknown".
pub(crate) fn prompt_names(bits: u32) -> Vec<&'static str> {
    let mut names = Vec::new();
    let mut known = 0;
    for (kind, name) in PROMPT_NAMES {
        let bit = *kind as u32;
        known |= bit;
        if bits & bit != 0 && !names.contains(name) {
            names.push(*name);
        }
    }
    if bits & !known != 0 {
        names.push("unknown");
    }
    names
}

const DEVICE_AUDIO: u32 = Media::CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE as u32;
const DEVICE_VIDEO: u32 = Media::CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE as u32;
const DESKTOP: u32 = Media::CEF_MEDIA_PERMISSION_DESKTOP_AUDIO_CAPTURE as u32
    | Media::CEF_MEDIA_PERMISSION_DESKTOP_VIDEO_CAPTURE as u32;

/// The names of the media types in `bits`: a screen capture is
/// "screen_share", whatever else it asks for with it.
pub(crate) fn media_names(bits: u32) -> Vec<&'static str> {
    let mut names = Vec::new();
    if bits & DEVICE_VIDEO != 0 {
        names.push("camera");
    }
    if bits & DEVICE_AUDIO != 0 {
        names.push("microphone");
    }
    if bits & DESKTOP != 0 {
        names.push("screen_share");
    }
    names
}

/// How the host answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionAnswer {
    /// Granted. Chromium remembers a granted prompt for the site.
    Allow,
    /// Refused, and Chromium remembers it for the site.
    Block,
    /// Refused this time: Chromium remembers nothing (media: refused).
    Dismiss,
}

enum Reply {
    Prompt(PermissionPromptCallback),
    Media {
        callback: MediaAccessCallback,
        requested: u32,
    },
}

struct PermissionReply(Mutex<Option<AnyThread<Reply>>>);

impl PermissionReply {
    fn take(&self) -> Option<Reply> {
        self.0.lock().ok()?.take().map(|c| c.0)
    }

    fn answer(&self, answer: PermissionAnswer) {
        match self.take() {
            Some(Reply::Prompt(callback)) => callback.cont(match answer {
                PermissionAnswer::Allow => PermissionRequestResult::ACCEPT,
                PermissionAnswer::Block => PermissionRequestResult::DENY,
                PermissionAnswer::Dismiss => PermissionRequestResult::DISMISS,
            }),
            Some(Reply::Media {
                callback,
                requested,
            }) => callback.cont(match answer {
                // getUserMedia takes exactly what it asked for, or nothing.
                PermissionAnswer::Allow => requested & (DEVICE_AUDIO | DEVICE_VIDEO),
                _ => Media::CEF_MEDIA_PERMISSION_NONE as u32,
            }),
            None => {}
        }
    }
}

impl Drop for PermissionReply {
    fn drop(&mut self) {
        let Some(reply) = self.take() else { return };
        if !crate::runtime::is_started() {
            std::mem::forget(reply);
            return;
        }
        match reply {
            Reply::Prompt(callback) => callback.cont(PermissionRequestResult::IGNORE),
            Reply::Media { callback, .. } => callback.cancel(),
        }
    }
}

/// A page waits for permissions. Answer once.
#[derive(Clone)]
pub struct PermissionRequest {
    /// Unique for the app's run; [`BrowserEvent::PermissionGone`] names it.
    pub id: u64,
    /// The origin asking, e.g. `https://example.com` (may end in `/`).
    pub origin: String,
    /// What it asks for, by name ([`prompt_names`], [`media_names`]).
    pub permissions: Vec<&'static str>,
    reply: Arc<PermissionReply>,
}

impl PermissionRequest {
    pub fn answer(&self, answer: PermissionAnswer) {
        self.reply.answer(answer);
    }

    /// A request no page waits on, for the host's tests.
    pub fn detached(id: u64, origin: &str, permissions: Vec<&'static str>) -> Self {
        PermissionRequest {
            id,
            origin: origin.to_owned(),
            permissions,
            reply: Arc::new(PermissionReply(Mutex::new(None))),
        }
    }
}

impl std::fmt::Debug for PermissionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionRequest")
            .field("id", &self.id)
            .field("origin", &self.origin)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

impl PartialEq for PermissionRequest {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

/// Media requests get ids of their own, apart from CEF's prompt ids.
fn media_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    (1 << 62) | NEXT.fetch_add(1, Ordering::Relaxed)
}

wrap_permission_handler! {
    pub(crate) struct PagePermissionHandler {
        state: PageState,
    }

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut cef::Frame>,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let reply = Reply::Media {
                callback: callback.clone(),
                requested: requested_permissions,
            };
            self.state.send(BrowserEvent::PermissionRequested(PermissionRequest {
                id: media_id(),
                origin: requesting_origin.map(|s| s.to_string()).unwrap_or_default(),
                permissions: media_names(requested_permissions),
                reply: Arc::new(PermissionReply(Mutex::new(Some(AnyThread(reply))))),
            }));
            1
        }

        fn on_show_permission_prompt(
            &self,
            browser: Option<&mut Browser>,
            prompt_id: u64,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let multiple = Kind::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS as u32;
            if requested_permissions == multiple {
                // An agent's downloads in a row (crate::download); the
                // user's, as CEF decides.
                let expecting =
                    browser.is_some_and(|b| crate::download::expects_download(b.identifier()));
                if crate::download::accept_permission(requested_permissions, expecting) {
                    callback.cont(PermissionRequestResult::ACCEPT);
                    return 1;
                }
                return 0;
            }
            let reply = Reply::Prompt(callback.clone());
            self.state.send(BrowserEvent::PermissionRequested(PermissionRequest {
                id: prompt_id,
                origin: requesting_origin.map(|s| s.to_string()).unwrap_or_default(),
                permissions: prompt_names(requested_permissions),
                reply: Arc::new(PermissionReply(Mutex::new(Some(AnyThread(reply))))),
            }));
            1
        }

        fn on_dismiss_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            prompt_id: u64,
            _result: PermissionRequestResult,
        ) {
            // Answered, or void: the page navigated or closed.
            self.state.send(BrowserEvent::PermissionGone(prompt_id));
        }
    }
}

/// Chromium's content settings that keep a permission's answer.
fn content_types(name: &str) -> &'static [ContentSettingTypes] {
    match name {
        "camera" => &[ContentSettingTypes::MEDIASTREAM_CAMERA],
        "microphone" => &[ContentSettingTypes::MEDIASTREAM_MIC],
        "geolocation" => &[
            ContentSettingTypes::GEOLOCATION,
            ContentSettingTypes::GEOLOCATION_WITH_OPTIONS,
        ],
        "notifications" => &[ContentSettingTypes::NOTIFICATIONS],
        "clipboard_read" => &[ContentSettingTypes::CLIPBOARD_READ_WRITE],
        "midi_sysex" => &[ContentSettingTypes::MIDI_SYSEX, ContentSettingTypes::MIDI],
        "idle_detection" => &[ContentSettingTypes::IDLE_DETECTION],
        "window_management" => &[ContentSettingTypes::WINDOW_MANAGEMENT],
        "local_fonts" => &[ContentSettingTypes::LOCAL_FONTS],
        "local_network" => &[
            ContentSettingTypes::LOCAL_NETWORK,
            ContentSettingTypes::LOCAL_NETWORK_ACCESS,
        ],
        "loopback_network" => &[ContentSettingTypes::LOOPBACK_NETWORK],
        _ => &[],
    }
}

/// Forgets what Chromium remembers of `name` for `origin` in `profile`:
/// the site is asked again. Main thread, while CEF runs.
pub fn reset_site_permission(profile: &crate::Profile, origin: &str, name: &str) {
    if !crate::runtime::is_started() {
        return;
    }
    let context =
        crate::profile::request_context(profile).or_else(cef::request_context_get_global_context);
    let Some(context) = context else { return };
    let url = CefString::from(origin);
    for kind in content_types(name) {
        context.set_content_setting(Some(&url), Some(&url), *kind, ContentSettingValues::DEFAULT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_types_have_names_once_each() {
        let bits = Kind::CEF_PERMISSION_TYPE_GEOLOCATION as u32
            | Kind::CEF_PERMISSION_TYPE_NOTIFICATIONS as u32
            | Kind::CEF_PERMISSION_TYPE_LOCAL_NETWORK as u32
            | Kind::CEF_PERMISSION_TYPE_LOCAL_NETWORK_ACCESS_DEPRECATED as u32;
        assert_eq!(
            prompt_names(bits),
            ["geolocation", "notifications", "local_network"]
        );
        assert_eq!(prompt_names(1 << 31), ["unknown"]);
        assert!(prompt_names(0).is_empty());
    }

    #[test]
    fn media_names_camera_microphone_and_screens() {
        assert_eq!(
            media_names(DEVICE_AUDIO | DEVICE_VIDEO),
            ["camera", "microphone"]
        );
        assert_eq!(
            media_names(Media::CEF_MEDIA_PERMISSION_DESKTOP_VIDEO_CAPTURE as u32),
            ["screen_share"]
        );
    }

    #[test]
    fn every_asked_permission_has_a_content_setting_to_reset() {
        for name in [
            "camera",
            "microphone",
            "geolocation",
            "notifications",
            "clipboard_read",
            "midi_sysex",
            "idle_detection",
            "window_management",
            "local_fonts",
            "local_network",
            "loopback_network",
        ] {
            assert!(!content_types(name).is_empty(), "{name}");
        }
    }
}
