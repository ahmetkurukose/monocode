//! Site permissions and certificate trust (docs/native/browser/context.md
//! K55, K62, K63, §3.3): what the app decides when a page asks, kept apart
//! from the engine so it can be tested.
//!
//! - Permissions: [`policy`] sorts what a page asks for into granted
//!   silently, asked, or refused, after Orca; [`decide`] answers a request
//!   from the remembered settings, refuses it while an agent drives the page
//!   out of the user's sight (K63), or asks the user. [`SitePermissions`] is
//!   a profile's remembered answers, one JSON file in the profile's folder
//!   (global per profile, K55); a throwaway profile keeps them in memory.
//! - Certificates: [`is_local_host`] says whose bad certificates are taken
//!   without asking (localhost, `*.local`, loopback and private addresses);
//!   [`CertExceptions`] are the user's "proceed anyway" for this run only.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

// ---- Permissions ----

/// How the app treats one kind of permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Granted without asking: harmless, or the user's own pick is the
    /// consent (pointer lock, storage access, a file the user chooses).
    Allow,
    /// Asked per site, and remembered.
    Ask,
    /// Never granted (screen sharing, VR, protocol handlers, …).
    Deny,
}

/// The policy for a permission by its engine name (`monocode_cef`'s
/// `PermissionRequest::permissions`).
pub fn policy(name: &str) -> Policy {
    match name {
        "pointer_lock"
        | "storage_access"
        | "top_level_storage_access"
        | "file_system_access"
        | "disk_quota" => Policy::Allow,
        "camera" | "microphone" | "geolocation" | "notifications" | "clipboard_read"
        | "midi_sysex" | "window_management" | "idle_detection" | "local_network"
        | "loopback_network" => Policy::Ask,
        _ => Policy::Deny,
    }
}

/// What the user is asked: "Use your camera".
pub fn label(name: &str) -> &'static str {
    match name {
        "camera" => "Use your camera",
        "microphone" => "Use your microphone",
        "geolocation" => "Know your location",
        "notifications" => "Show notifications",
        "clipboard_read" => "See text and images copied to the clipboard",
        "midi_sysex" => "Control and reprogram your MIDI devices",
        "window_management" => "Manage windows on all your displays",
        "idle_detection" => "Know when you are actively using this device",
        "local_network" => "Find and connect to devices on your local network",
        "loopback_network" => "Connect to apps running on this device",
        "screen_share" => "Share your screen",
        _ => "Use a feature of this device",
    }
}

/// A short name for lists: "Camera".
pub fn title(name: &str) -> &'static str {
    match name {
        "camera" => "Camera",
        "microphone" => "Microphone",
        "geolocation" => "Location",
        "notifications" => "Notifications",
        "clipboard_read" => "Clipboard",
        "midi_sysex" => "MIDI devices",
        "window_management" => "Window management",
        "idle_detection" => "Idle detection",
        "local_network" => "Local network",
        "loopback_network" => "Apps on this device",
        "screen_share" => "Screen sharing",
        _ => "Other",
    }
}

/// A remembered answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Setting {
    Allow,
    Block,
}

/// What became of a request, as agents read it (K63, K45).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Granted by the policy, nobody asked.
    AllowedAutomatically,
    /// Never granted here.
    DeniedUnsupported,
    /// The site's remembered answer.
    AllowedBySetting,
    BlockedBySetting,
    /// An agent drives the page and the user does not see it: refused,
    /// and nothing remembered.
    DeniedBackground,
    /// The page shows a prompt and waits for the user.
    WaitingForUser,
    AllowedByUser,
    BlockedByUser,
    /// The user allowed it, the operating system did not (macOS privacy
    /// settings).
    DeniedBySystem,
    /// The prompt went unanswered: the page navigated or closed.
    Dismissed,
    /// The page's origin cannot hold permissions (`file:`, opaque origins).
    DeniedOrigin,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::AllowedAutomatically => "allowed_automatically",
            Outcome::DeniedUnsupported => "denied_unsupported",
            Outcome::AllowedBySetting => "allowed_by_setting",
            Outcome::BlockedBySetting => "blocked_by_setting",
            Outcome::DeniedBackground => "denied_background",
            Outcome::WaitingForUser => "waiting_for_user",
            Outcome::AllowedByUser => "allowed_by_user",
            Outcome::BlockedByUser => "blocked_by_user",
            Outcome::DeniedBySystem => "denied_by_system",
            Outcome::Dismissed => "dismissed",
            Outcome::DeniedOrigin => "denied_origin",
        }
    }

    /// What it means for an agent, as one sentence.
    pub fn explain(self) -> &'static str {
        match self {
            Outcome::AllowedAutomatically => "granted: it needs no permission here",
            Outcome::DeniedUnsupported => "denied: Monocode never grants it",
            Outcome::AllowedBySetting => "granted: the user allowed it for this site",
            Outcome::BlockedBySetting => "denied: the user blocked it for this site",
            Outcome::DeniedBackground => {
                "denied: the page is in the background, out of the user's sight, and agents cannot grant permissions; ask the user to show the page (browser_tab_show) and allow it there"
            }
            Outcome::WaitingForUser => {
                "a permission prompt waits for the user; agents cannot answer it, ask the user"
            }
            Outcome::AllowedByUser => "the user allowed it",
            Outcome::BlockedByUser => "the user blocked it",
            Outcome::DeniedBySystem => {
                "denied: the user allowed it, but macOS privacy settings do not let Monocode use it"
            }
            Outcome::Dismissed => "the prompt went away unanswered",
            Outcome::DeniedOrigin => "denied: this page's origin cannot hold permissions",
        }
    }
}

/// How the app answers a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Answered now.
    Answer { allow: bool, outcome: Outcome },
    /// The user is asked about these permissions (those of the request the
    /// policy asks about).
    Ask(Vec<String>),
}

/// The answer to a page asking for `names`, given each one's remembered
/// [`Setting`] (`known`), whether the user sees the page and whether an
/// agent drives it. One refused or blocked permission refuses the request:
/// Chromium answers a request as a whole.
pub fn decide(
    names: &[&str],
    known: impl Fn(&str) -> Option<Setting>,
    visible: bool,
    agent_active: bool,
) -> Decision {
    if names.is_empty() || names.iter().any(|n| policy(n) == Policy::Deny) {
        return Decision::Answer {
            allow: false,
            outcome: Outcome::DeniedUnsupported,
        };
    }
    let asked: Vec<&str> = (names.iter().copied())
        .filter(|n| policy(n) == Policy::Ask)
        .collect();
    if asked.is_empty() {
        return Decision::Answer {
            allow: true,
            outcome: Outcome::AllowedAutomatically,
        };
    }
    let settings: Vec<Option<Setting>> = asked.iter().map(|n| known(n)).collect();
    if settings.contains(&Some(Setting::Block)) {
        return Decision::Answer {
            allow: false,
            outcome: Outcome::BlockedBySetting,
        };
    }
    if settings.iter().all(|s| *s == Some(Setting::Allow)) {
        return Decision::Answer {
            allow: true,
            outcome: Outcome::AllowedBySetting,
        };
    }
    if agent_active && !visible {
        return Decision::Answer {
            allow: false,
            outcome: Outcome::DeniedBackground,
        };
    }
    Decision::Ask(asked.into_iter().map(str::to_owned).collect())
}

/// `https://example.com` for any URL of that origin; `None` for an origin
/// that cannot hold permissions (`file:`, `data:`, opaque origins).
pub fn origin_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let origin = parsed.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// A request's record, for the agent that drives the page (K45, K63).
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionNote {
    /// Increases with every note of the app's run.
    pub seq: u64,
    pub permissions: Vec<String>,
    pub origin: String,
    pub outcome: Outcome,
}

impl PermissionNote {
    /// One line: `permissionRequest {"kind":…,"origin":…,"outcome":…}: what it means.`
    pub fn describe(&self) -> String {
        let record = serde_json::json!({
            "kind": self.permissions.join(","),
            "origin": self.origin,
            "outcome": self.outcome.as_str(),
        });
        format!("permissionRequest {record}: {}.", self.outcome.explain())
    }
}

/// The file a profile's answers live in, inside the profile's folder.
pub const FILE_NAME: &str = "monocode-site-permissions.json";

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    sites: BTreeMap<String, BTreeMap<String, Setting>>,
    /// Granted for a page without being remembered: Chromium keeps a
    /// granted prompt for the site anyway, so it is taken back when the
    /// page goes, or at the next start after a crash.
    #[serde(default)]
    temporary: BTreeMap<String, BTreeSet<String>>,
}

/// A profile's remembered permission answers, by origin.
#[derive(Default)]
pub struct SitePermissions {
    stored: Stored,
    /// `None`: kept in memory only (a throwaway profile).
    path: Option<PathBuf>,
}

impl SitePermissions {
    /// The answers of the profile whose folder is `dir`; none yet when
    /// there is no file or it does not read.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        let stored = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        SitePermissions {
            stored,
            path: Some(path),
        }
    }

    pub fn in_memory() -> Self {
        Self::default()
    }

    pub fn get(&self, origin: &str, name: &str) -> Option<Setting> {
        self.stored.sites.get(origin)?.get(name).copied()
    }

    pub fn set(&mut self, origin: &str, name: &str, setting: Setting) {
        (self.stored.sites.entry(origin.to_owned()).or_default()).insert(name.to_owned(), setting);
        self.save();
    }

    /// Forgets one answer; true when there was one.
    pub fn remove(&mut self, origin: &str, name: &str) -> bool {
        let Some(site) = self.stored.sites.get_mut(origin) else {
            return false;
        };
        let had = site.remove(name).is_some();
        if site.is_empty() {
            self.stored.sites.remove(origin);
        }
        if had {
            self.save();
        }
        had
    }

    /// The answers of every site, by origin, sorted.
    pub fn sites(&self) -> Vec<(String, Vec<(String, Setting)>)> {
        (self.stored.sites.iter())
            .map(|(origin, site)| {
                let names = site.iter().map(|(n, s)| (n.clone(), *s)).collect();
                (origin.clone(), names)
            })
            .collect()
    }

    /// A site's answers.
    pub fn site(&self, origin: &str) -> Vec<(String, Setting)> {
        (self.stored.sites.get(origin))
            .map(|site| site.iter().map(|(n, s)| (n.clone(), *s)).collect())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.stored.sites.is_empty()
    }

    /// Notes a grant that is not remembered, to take back later.
    pub fn note_temporary(&mut self, origin: &str, name: &str) {
        let added =
            (self.stored.temporary.entry(origin.to_owned()).or_default()).insert(name.to_owned());
        if added {
            self.save();
        }
    }

    /// Takes the grant back from the list; true when it was there.
    pub fn end_temporary(&mut self, origin: &str, name: &str) -> bool {
        let Some(names) = self.stored.temporary.get_mut(origin) else {
            return false;
        };
        let had = names.remove(name);
        if names.is_empty() {
            self.stored.temporary.remove(origin);
        }
        if had {
            self.save();
        }
        had
    }

    /// Every grant not taken back yet (an earlier run that ended early).
    pub fn take_temporary(&mut self) -> Vec<(String, String)> {
        let all: Vec<(String, String)> = (std::mem::take(&mut self.stored.temporary).into_iter())
            .flat_map(|(origin, names)| names.into_iter().map(move |n| (origin.clone(), n)))
            .collect();
        if !all.is_empty() {
            self.save();
        }
        all
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(error) = write_atomic(path, &self.stored) {
            eprintln!(
                "browser: could not save site permissions to {}: {error}",
                path.display()
            );
        }
    }
}

fn write_atomic(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp, path)
}

// ---- Certificates ----

/// Whether a page at `url` is on this machine or a private network, whose
/// self-signed and otherwise invalid certificates are taken without asking
/// (K62): `localhost` and `*.localhost`, `*.local`, loopback (127.0.0.0/8,
/// `::1`), private (10/8, 172.16/12, 192.168/16, fc00::/7) and link-local
/// addresses, and `0.0.0.0`. A name that only looks like one
/// (`localhost.evil.com`, `127.0.0.1.nip.io`) is not.
pub fn is_local_host(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.strip_suffix('.').unwrap_or(domain);
            let domain = domain.to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost") || domain.ends_with(".local")
        }
        Some(url::Host::Ipv4(ip)) => local_v4(ip),
        Some(url::Host::Ipv6(ip)) => local_v6(ip),
        None => false,
    }
}

fn local_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

fn local_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return local_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        // fc00::/7, unique local.
        || (first & 0xfe00) == 0xfc00
        // fe80::/10, link-local.
        || (first & 0xffc0) == 0xfe80
}

/// The host of `url` as the address bar names it, without brackets.
pub fn host_of(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_owned()))
        .unwrap_or_default()
}

/// Chromium's name for a certificate error, e.g. `ERR_CERT_AUTHORITY_INVALID`.
pub fn cert_error_name(code: i32) -> String {
    let name = match code {
        -200 => "ERR_CERT_COMMON_NAME_INVALID",
        -201 => "ERR_CERT_DATE_INVALID",
        -202 => "ERR_CERT_AUTHORITY_INVALID",
        -203 => "ERR_CERT_CONTAINS_ERRORS",
        -204 => "ERR_CERT_NO_REVOCATION_MECHANISM",
        -205 => "ERR_CERT_UNABLE_TO_CHECK_REVOCATION",
        -206 => "ERR_CERT_REVOKED",
        -207 => "ERR_CERT_INVALID",
        -208 => "ERR_CERT_WEAK_SIGNATURE_ALGORITHM",
        -210 => "ERR_CERT_NON_UNIQUE_NAME",
        -211 => "ERR_CERT_WEAK_KEY",
        -212 => "ERR_CERT_NAME_CONSTRAINT_VIOLATION",
        -213 => "ERR_CERT_VALIDITY_TOO_LONG",
        -214 => "ERR_CERTIFICATE_TRANSPARENCY_REQUIRED",
        -215 => "ERR_CERT_SYMANTEC_LEGACY",
        -217 => "ERR_CERT_KNOWN_INTERCEPTION_BLOCKED",
        -219 => "ERR_CERT_SELF_SIGNED_LOCAL_NETWORK",
        _ => return format!("ERR_CERT ({code})"),
    };
    name.to_owned()
}

/// What a certificate error means, for people.
pub fn cert_error_text(code: i32) -> &'static str {
    match code {
        -200 => "The certificate is for another site.",
        -201 => "The certificate has expired or is not valid yet.",
        -202 | -219 => "The certificate is not issued by an authority this computer trusts.",
        -206 => "The certificate was revoked.",
        -208 | -211 => "The certificate uses weak security.",
        _ => "The certificate is not valid.",
    }
}

/// The user's "proceed anyway" for this run: a host with one certificate.
/// Never saved; gone when the app quits.
#[derive(Default)]
pub struct CertExceptions(HashSet<(String, String)>);

impl CertExceptions {
    pub fn allow(&mut self, host: &str, fingerprint: &str) {
        self.0
            .insert((host.to_ascii_lowercase(), fingerprint.to_owned()));
    }

    /// Whether the user proceeded past this host's certificate.
    pub fn allows(&self, host: &str, fingerprint: &str) -> bool {
        self.0
            .contains(&(host.to_ascii_lowercase(), fingerprint.to_owned()))
    }

    /// Whether the user proceeded past any certificate of this host.
    pub fn has_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.0.iter().any(|(h, _)| *h == host)
    }
}

/// How the app answers a certificate Chromium does not trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertDecision {
    /// Loaded without asking: a local or private host (K62).
    ProceedLocal,
    /// Loaded: the user proceeded past this certificate earlier this run.
    ProceedException,
    /// Refused: the page shows the warning.
    Block,
}

pub fn decide_cert(url: &str, fingerprint: &str, exceptions: &CertExceptions) -> CertDecision {
    if is_local_host(url) {
        CertDecision::ProceedLocal
    } else if !fingerprint.is_empty() && exceptions.allows(&host_of(url), fingerprint) {
        CertDecision::ProceedException
    } else {
        CertDecision::Block
    }
}

/// A page's navigation that a certificate stopped, for agents (K62).
#[derive(Clone, Debug, PartialEq)]
pub struct CertBlock {
    pub url: String,
    pub host: String,
    /// [`cert_error_name`].
    pub error: String,
    /// Increases with every stop of the app's run: a new one is told from
    /// an earlier one of the same address.
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts_are_this_machine_and_private_networks() {
        for url in [
            "https://localhost/",
            "https://localhost:8443/app",
            "https://LOCALHOST./",
            "https://api.localhost:3000/",
            "https://printer.local/",
            "https://127.0.0.1/",
            "https://127.4.5.6:9000/",
            "https://[::1]:8443/",
            "https://10.1.2.3/",
            "https://172.16.0.1/",
            "https://172.31.255.254/",
            "https://192.168.1.20:5173/",
            "https://169.254.10.10/",
            "https://[fd12:3456::1]/",
            "https://[fe80::1]/",
            "https://[::ffff:192.168.0.1]/",
            "https://0.0.0.0:3000/",
        ] {
            assert!(is_local_host(url), "{url}");
        }
    }

    #[test]
    fn names_that_only_look_local_are_not() {
        for url in [
            "https://localhost.evil.com/",
            "https://evil-localhost/",
            "https://mylocalhost/",
            "https://127.0.0.1.nip.io/",
            "https://10.0.0.1.example.com/",
            "https://192.168.1.1.evil/",
            "https://local/",
            "https://evil.com/?localhost",
            "https://localhost@evil.com/",
            "https://evil.com#.local",
            "https://172.32.0.1/",
            "https://172.15.255.255/",
            "https://11.0.0.1/",
            "https://8.8.8.8/",
            "https://[2001:db8::1]/",
            "https://[::ffff:8.8.8.8]/",
            // IDN: a Cyrillic "о" in localhost.
            "https://lоcalhost/",
            "https://xn--lcalhost-9wg/",
            "https://example.lоcal/",
            "not a url",
            "",
        ] {
            assert!(!is_local_host(url), "{url}");
        }
    }

    #[test]
    fn a_certificate_is_taken_for_local_hosts_or_a_proceeded_one() {
        let mut exceptions = CertExceptions::default();
        assert_eq!(
            decide_cert("https://localhost:8443/", "ab", &exceptions),
            CertDecision::ProceedLocal
        );
        assert_eq!(
            decide_cert("https://self-signed.badssl.com/", "ab", &exceptions),
            CertDecision::Block
        );
        exceptions.allow("Self-Signed.badssl.com", "ab");
        assert_eq!(
            decide_cert("https://self-signed.badssl.com/x", "ab", &exceptions),
            CertDecision::ProceedException
        );
        // Another certificate of the same host asks again.
        assert_eq!(
            decide_cert("https://self-signed.badssl.com/", "cd", &exceptions),
            CertDecision::Block
        );
        assert_eq!(
            decide_cert("https://self-signed.badssl.com/", "", &exceptions),
            CertDecision::Block
        );
        assert!(exceptions.has_host("self-signed.badssl.com"));
        assert_eq!(host_of("https://[::1]:8443/"), "::1");
        assert_eq!(cert_error_name(-202), "ERR_CERT_AUTHORITY_INVALID");
        assert_eq!(cert_error_name(-299), "ERR_CERT (-299)");
    }

    #[test]
    fn requests_sort_into_granted_asked_and_refused() {
        let none = |_: &str| None;
        assert_eq!(
            decide(&["pointer_lock"], none, true, false),
            Decision::Answer {
                allow: true,
                outcome: Outcome::AllowedAutomatically
            }
        );
        assert_eq!(
            decide(&["screen_share"], none, true, false),
            Decision::Answer {
                allow: false,
                outcome: Outcome::DeniedUnsupported
            }
        );
        assert_eq!(
            decide(&["camera", "microphone"], none, true, false),
            Decision::Ask(vec!["camera".into(), "microphone".into()])
        );
        // One unsupported part refuses the whole request.
        assert_eq!(
            decide(&["camera", "ar_session"], none, true, false),
            Decision::Answer {
                allow: false,
                outcome: Outcome::DeniedUnsupported
            }
        );
        assert!(matches!(
            decide(&[], none, true, false),
            Decision::Answer { allow: false, .. }
        ));
    }

    #[test]
    fn remembered_answers_come_first_then_the_agents_background_refusal() {
        let camera_allowed = |n: &str| (n == "camera").then_some(Setting::Allow);
        let mic_blocked = |n: &str| (n == "microphone").then_some(Setting::Block);
        assert_eq!(
            decide(&["camera"], camera_allowed, false, true),
            Decision::Answer {
                allow: true,
                outcome: Outcome::AllowedBySetting
            }
        );
        assert_eq!(
            decide(&["camera", "microphone"], mic_blocked, true, false),
            Decision::Answer {
                allow: false,
                outcome: Outcome::BlockedBySetting
            }
        );
        // Half known: still asked, or refused out of sight.
        assert_eq!(
            decide(&["camera", "microphone"], camera_allowed, true, true),
            Decision::Ask(vec!["camera".into(), "microphone".into()])
        );
        assert_eq!(
            decide(&["geolocation"], |_| None, false, true),
            Decision::Answer {
                allow: false,
                outcome: Outcome::DeniedBackground
            }
        );
        // The user's own background tab waits for them.
        assert_eq!(
            decide(&["geolocation"], |_| None, false, false),
            Decision::Ask(vec!["geolocation".into()])
        );
        // The page the user sees asks them, even while an agent drives it.
        assert_eq!(
            decide(&["notifications"], |_| None, true, true),
            Decision::Ask(vec!["notifications".into()])
        );
    }

    #[test]
    fn an_origin_is_a_http_scheme_host_and_port() {
        assert_eq!(
            origin_of("https://Example.com:443/a?b#c").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_of("http://localhost:3000/").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(origin_of("file:///etc/passwd"), None);
        assert_eq!(origin_of("data:text/html,x"), None);
        assert_eq!(origin_of("null"), None);
    }

    #[test]
    fn a_note_reads_as_a_record_and_a_sentence() {
        let note = PermissionNote {
            seq: 1,
            permissions: vec!["camera".into()],
            origin: "https://a.test".into(),
            outcome: Outcome::DeniedBackground,
        };
        let text = note.describe();
        assert!(
            text.starts_with(
                r#"permissionRequest {"kind":"camera","origin":"https://a.test","outcome":"denied_background"}: denied"#
            ),
            "{text}"
        );
    }

    #[test]
    fn answers_are_saved_with_the_profile_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SitePermissions::load(dir.path());
        assert!(store.is_empty());
        store.set("https://a.test", "camera", Setting::Allow);
        store.set("https://a.test", "geolocation", Setting::Block);
        store.set("https://b.test", "notifications", Setting::Allow);
        store.note_temporary("https://c.test", "notifications");

        let mut again = SitePermissions::load(dir.path());
        assert_eq!(again.get("https://a.test", "camera"), Some(Setting::Allow));
        assert_eq!(
            again.get("https://a.test", "geolocation"),
            Some(Setting::Block)
        );
        assert_eq!(again.get("https://a.test", "microphone"), None);
        assert_eq!(again.sites().len(), 2);
        assert!(again.remove("https://b.test", "notifications"));
        assert!(!again.remove("https://b.test", "notifications"));
        assert_eq!(again.sites().len(), 1);
        assert_eq!(
            again.take_temporary(),
            [("https://c.test".to_owned(), "notifications".to_owned())]
        );
        let last = SitePermissions::load(dir.path());
        assert_eq!(last.sites().len(), 1);
        assert_eq!(last.site("https://a.test").len(), 2);

        // A throwaway profile writes nothing.
        let mut memory = SitePermissions::in_memory();
        memory.set("https://a.test", "camera", Setting::Allow);
        assert_eq!(memory.get("https://a.test", "camera"), Some(Setting::Allow));
    }

    #[test]
    fn a_temporary_grant_ends_once() {
        let mut store = SitePermissions::in_memory();
        store.note_temporary("https://a.test", "notifications");
        assert!(store.end_temporary("https://a.test", "notifications"));
        assert!(!store.end_temporary("https://a.test", "notifications"));
        assert!(store.take_temporary().is_empty());
    }

    #[test]
    fn every_asked_permission_has_words() {
        for name in [
            "camera",
            "microphone",
            "geolocation",
            "notifications",
            "clipboard_read",
            "midi_sysex",
            "window_management",
            "idle_detection",
            "local_network",
            "loopback_network",
        ] {
            assert_eq!(policy(name), Policy::Ask);
            assert_ne!(label(name), label("?"), "{name}");
            assert_ne!(title(name), "Other", "{name}");
        }
    }
}
