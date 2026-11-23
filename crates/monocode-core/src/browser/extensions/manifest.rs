//! An extension's `manifest.json`, localized (`__MSG_name__` from
//! `_locales`), with what the layer runs it by. Manifest V3 only, as Orca.

use std::{collections::BTreeMap, collections::HashMap, path::Path};

use serde_json::{Map, Value};

use super::pattern::{PatternSet, glob};

/// Chrome's default for extension pages.
pub const DEFAULT_PAGE_CSP: &str = "script-src 'self' 'wasm-unsafe-eval'; object-src 'self';";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunAt {
    DocumentStart,
    DocumentEnd,
    DocumentIdle,
}

impl RunAt {
    pub fn as_str(self) -> &'static str {
        match self {
            RunAt::DocumentStart => "document_start",
            RunAt::DocumentEnd => "document_end",
            RunAt::DocumentIdle => "document_idle",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum World {
    Isolated,
    Main,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContentScript {
    pub matches: PatternSet,
    pub exclude_matches: PatternSet,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub js: Vec<String>,
    pub css: Vec<String>,
    pub run_at: RunAt,
    pub all_frames: bool,
    /// `match_about_blank` / `match_origin_as_fallback`.
    pub about_blank: bool,
    pub world: World,
}

impl ContentScript {
    /// Whether the script runs in a document at `url` (frame tests aside).
    pub fn wants(&self, url: &str) -> bool {
        self.matches.matches(url)
            && !self.exclude_matches.matches(url)
            && (self.include_globs.is_empty() || self.include_globs.iter().any(|g| glob(g, url)))
            && !self.exclude_globs.iter().any(|g| glob(g, url))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebAccessible {
    pub resources: Vec<String>,
    pub matches: PatternSet,
    pub extension_ids: Vec<String>,
    pub use_dynamic_url: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExternallyConnectable {
    pub matches: PatternSet,
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Action {
    pub popup: Option<String>,
    pub title: Option<String>,
    pub icons: BTreeMap<u32, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// The default key, as written (`Ctrl+Shift+X`); macOS's when given.
    pub key: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// The localized manifest, as `runtime.getManifest()` returns it.
    pub raw: Value,
    pub name: String,
    pub description: String,
    pub version: String,
    pub background: Option<String>,
    pub background_module: bool,
    pub content_scripts: Vec<ContentScript>,
    pub web_accessible: Vec<WebAccessible>,
    pub externally_connectable: Option<ExternallyConnectable>,
    pub permissions: Vec<String>,
    pub optional_permissions: Vec<String>,
    pub host_permissions: Vec<String>,
    pub action: Option<Action>,
    pub icons: BTreeMap<u32, String>,
    pub options_page: Option<String>,
    pub page_csp: String,
    pub commands: Vec<Command>,
    pub default_locale: Option<String>,
    /// The developer's public key (base64 DER), when the manifest has one.
    pub key: Option<String>,
    /// The localized messages (`chrome.i18n.getMessage`), by lower-case name.
    pub messages: Map<String, Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    Unreadable(String),
    NotV3(i64),
    Missing(&'static str),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Unreadable(e) => write!(f, "manifest.json is unreadable: {e}"),
            ManifestError::NotV3(2) => {
                f.write_str("Manifest V2 extensions are not supported (Chrome dropped them too)")
            }
            ManifestError::NotV3(v) => write!(f, "manifest version {v} is not supported"),
            ManifestError::Missing(what) => write!(f, "manifest.json has no {what}"),
        }
    }
}

impl std::error::Error for ManifestError {}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn patterns(v: &Value) -> PatternSet {
    PatternSet::parse(strings(v).iter().map(String::as_str))
}

fn icons(v: &Value) -> BTreeMap<u32, String> {
    match v {
        Value::Object(map) => (map.iter())
            .filter_map(|(k, v)| {
                Some((
                    k.parse().ok()?,
                    v.as_str()?.trim_start_matches('/').to_owned(),
                ))
            })
            .collect(),
        Value::String(s) => BTreeMap::from([(16, s.trim_start_matches('/').to_owned())]),
        _ => BTreeMap::new(),
    }
}

/// The locales to read messages from, most wanted first: the UI's
/// (`en-US` → `en_US`, `en`), then the default.
fn locales(ui: &str, default: Option<&str>) -> Vec<String> {
    let ui = ui.replace('-', "_");
    let mut out = vec![ui.clone()];
    if let Some((lang, _)) = ui.split_once('_') {
        out.push(lang.to_owned());
    }
    if let Some(d) = default {
        out.push(d.to_owned());
    }
    out.dedup();
    out
}

/// Messages of the extension in `dir`, by lower-case name: the most
/// wanted locale's, filled in from the others. Placeholders are resolved
/// (`$name$` → its `content`, e.g. `$1`).
pub fn messages(dir: &Path, ui_locale: &str, default: Option<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    for locale in locales(ui_locale, default).iter().rev() {
        let path = dir.join("_locales").join(locale).join("messages.json");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        for (name, entry) in map {
            let Some(mut message) = entry["message"].as_str().map(str::to_owned) else {
                continue;
            };
            if let Some(placeholders) = entry["placeholders"].as_object() {
                for (ph, def) in placeholders {
                    let content = def["content"].as_str().unwrap_or("");
                    message = replace_ci(&message, &format!("${ph}$"), content);
                }
            }
            out.insert(name.to_lowercase(), Value::String(message));
        }
    }
    out
}

fn replace_ci(text: &str, needle: &str, with: &str) -> String {
    let lower = text.to_lowercase();
    let needle = needle.to_lowercase();
    if needle.is_empty() || lower.len() != text.len() {
        return text.replace(&needle, with);
    }
    let mut out = String::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + found]);
        out.push_str(with);
        at += found + needle.len();
    }
    out.push_str(&text[at..]);
    out
}

/// `__MSG_name__` in every string of `value`.
fn localize(value: &mut Value, messages: &Map<String, Value>) {
    match value {
        Value::String(s) if s.contains("__MSG_") => {
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(start) = rest.find("__MSG_") {
                out.push_str(&rest[..start]);
                let after = &rest[start + 6..];
                match after.find("__") {
                    Some(end) => {
                        let name = after[..end].to_lowercase();
                        let text = match name.as_str() {
                            "@@extension_id" => "",
                            _ => messages.get(&name).and_then(Value::as_str).unwrap_or(""),
                        };
                        out.push_str(text);
                        rest = &after[end + 2..];
                    }
                    None => {
                        out.push_str(&rest[start..]);
                        rest = "";
                    }
                }
            }
            out.push_str(rest);
            *s = out;
        }
        Value::Array(a) => a.iter_mut().for_each(|v| localize(v, messages)),
        Value::Object(o) => o.values_mut().for_each(|v| localize(v, messages)),
        _ => {}
    }
}

impl Manifest {
    /// Reads `dir/manifest.json`.
    pub fn load(dir: &Path, ui_locale: &str) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(dir.join("manifest.json"))
            .map_err(|e| ManifestError::Unreadable(e.to_string()))?;
        let raw: Value = serde_json::from_slice(strip_bom(&bytes))
            .map_err(|e| ManifestError::Unreadable(e.to_string()))?;
        let default = raw["default_locale"].as_str().map(str::to_owned);
        let messages = messages(dir, ui_locale, default.as_deref());
        Self::from_value(raw, messages)
    }

    pub fn from_value(mut raw: Value, messages: Map<String, Value>) -> Result<Self, ManifestError> {
        if !raw.is_object() {
            return Err(ManifestError::Unreadable("not an object".into()));
        }
        localize(&mut raw, &messages);
        let mv = raw["manifest_version"].as_i64().unwrap_or(0);
        if mv != 3 {
            return Err(ManifestError::NotV3(mv));
        }
        let name = raw["name"]
            .as_str()
            .ok_or(ManifestError::Missing("name"))?
            .to_owned();
        let version = (raw["version"].as_str())
            .filter(|v| super::parse_version(v).is_some())
            .ok_or(ManifestError::Missing("valid version"))?
            .to_owned();
        let content_scripts = (raw["content_scripts"].as_array().into_iter().flatten())
            .map(|c| ContentScript {
                matches: patterns(&c["matches"]),
                exclude_matches: patterns(&c["exclude_matches"]),
                include_globs: strings(&c["include_globs"]),
                exclude_globs: strings(&c["exclude_globs"]),
                js: strings(&c["js"])
                    .into_iter()
                    .map(|f| f.trim_start_matches('/').to_owned())
                    .collect(),
                css: strings(&c["css"])
                    .into_iter()
                    .map(|f| f.trim_start_matches('/').to_owned())
                    .collect(),
                run_at: match c["run_at"].as_str() {
                    Some("document_start") => RunAt::DocumentStart,
                    Some("document_end") => RunAt::DocumentEnd,
                    _ => RunAt::DocumentIdle,
                },
                all_frames: c["all_frames"].as_bool().unwrap_or(false),
                about_blank: c["match_about_blank"].as_bool().unwrap_or(false)
                    || c["match_origin_as_fallback"].as_bool().unwrap_or(false),
                world: match c["world"].as_str() {
                    Some("MAIN") => World::Main,
                    _ => World::Isolated,
                },
            })
            .collect();
        let web_accessible = (raw["web_accessible_resources"]
            .as_array()
            .into_iter()
            .flatten())
        .filter(|w| w.is_object())
        .map(|w| WebAccessible {
            resources: strings(&w["resources"])
                .into_iter()
                .map(|r| r.trim_start_matches('/').to_owned())
                .collect(),
            matches: patterns(&w["matches"]),
            extension_ids: strings(&w["extension_ids"]),
            use_dynamic_url: w["use_dynamic_url"].as_bool().unwrap_or(false),
        })
        .collect();
        let externally_connectable =
            raw["externally_connectable"]
                .as_object()
                .map(|e| ExternallyConnectable {
                    matches: patterns(e.get("matches").unwrap_or(&Value::Null)),
                    ids: strings(e.get("ids").unwrap_or(&Value::Null)),
                });
        let action = raw["action"].as_object().map(|a| Action {
            popup: (a.get("default_popup").and_then(Value::as_str))
                .filter(|p| !p.is_empty())
                .map(|p| p.trim_start_matches('/').to_owned()),
            title: a
                .get("default_title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            icons: icons(a.get("default_icon").unwrap_or(&Value::Null)),
        });
        let commands = (raw["commands"].as_object().into_iter().flatten())
            .map(|(name, c)| Command {
                name: name.clone(),
                description: c["description"].as_str().unwrap_or("").to_owned(),
                key: (c["suggested_key"]["mac"].as_str())
                    .or(c["suggested_key"]["default"].as_str())
                    .map(str::to_owned),
            })
            .collect();
        let page_csp = match &raw["content_security_policy"] {
            Value::Object(o) => o.get("extension_pages").and_then(Value::as_str),
            _ => None,
        }
        .unwrap_or(DEFAULT_PAGE_CSP)
        .to_owned();
        Ok(Manifest {
            description: raw["description"].as_str().unwrap_or("").to_owned(),
            background: (raw["background"]["service_worker"].as_str())
                .map(|w| w.trim_start_matches('/').to_owned()),
            background_module: raw["background"]["type"].as_str() == Some("module"),
            permissions: strings(&raw["permissions"]),
            optional_permissions: strings(&raw["optional_permissions"]),
            host_permissions: strings(&raw["host_permissions"]),
            icons: icons(&raw["icons"]),
            options_page: (raw["options_ui"]["page"].as_str())
                .or(raw["options_page"].as_str())
                .map(|p| p.trim_start_matches('/').to_owned()),
            default_locale: raw["default_locale"].as_str().map(str::to_owned),
            key: raw["key"].as_str().map(str::to_owned),
            name,
            version,
            content_scripts,
            web_accessible,
            externally_connectable,
            action,
            commands,
            page_csp,
            messages,
            raw,
        })
    }

    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    /// The host permissions as patterns.
    pub fn hosts(&self) -> PatternSet {
        PatternSet::parse(self.host_permissions.iter().map(String::as_str))
    }

    /// The icon closest to `size` points at least, else the largest.
    pub fn icon(&self, size: u32) -> Option<&str> {
        let set = match &self.action {
            Some(a) if !a.icons.is_empty() => &a.icons,
            _ => &self.icons,
        };
        (set.range(size..).next().or(set.iter().next_back())).map(|(_, p)| p.as_str())
    }

    /// Whether a page at `initiator` (a web page's URL; `None`: no page,
    /// as a typed address) may load the extension's `path`
    /// (web_accessible_resources). Another extension names itself by
    /// `initiator_extension`.
    pub fn web_accessible(
        &self,
        path: &str,
        initiator: Option<&str>,
        initiator_extension: Option<&str>,
    ) -> bool {
        let path = path.trim_start_matches('/');
        self.web_accessible.iter().any(|w| {
            let listed = w.resources.iter().any(|r| glob(r, path));
            let who = match (initiator_extension, initiator) {
                (Some(ext), _) => w.extension_ids.iter().any(|i| i == "*" || i == ext),
                (None, Some(url)) => w.matches.matches(url),
                (None, None) => false,
            };
            listed && who
        })
    }

    /// Permissions or features this layer does not run, by name.
    pub fn unsupported(&self) -> Vec<String> {
        let known_unsupported = super::api::UNSUPPORTED_PERMISSIONS;
        let mut out: Vec<String> = (self.permissions.iter())
            .filter(|p| known_unsupported.contains(&p.as_str()))
            .cloned()
            .collect();
        if self.raw.get("side_panel").is_some() {
            out.push("sidePanel".into());
        }
        if self.raw.get("declarative_net_request").is_some() {
            out.push("declarativeNetRequest".into());
        }
        out.dedup();
        out
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
}

/// The extension's messages by name, for scripts: lower-case names.
pub fn messages_map(manifest: &Manifest) -> HashMap<String, String> {
    (manifest.messages.iter())
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect()
}

#[cfg(test)]
pub(crate) const PROTON_MANIFEST: &str = include_str!("fixtures/proton-pass-manifest.json");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proton() -> Manifest {
        Manifest::from_value(serde_json::from_str(PROTON_MANIFEST).unwrap(), Map::new()).unwrap()
    }

    #[test]
    fn proton_pass_reads() {
        let m = proton();
        assert_eq!(m.version, "1.44.0");
        assert_eq!(m.background.as_deref(), Some("background.js"));
        assert_eq!(m.content_scripts.len(), 2);
        let main = &m.content_scripts[1];
        assert_eq!(main.world, World::Main);
        assert_eq!(main.run_at, RunAt::DocumentStart);
        assert!(!main.all_frames);
        assert!(main.wants("https://example.com/login"));
        assert!(!main.wants("https://pass.proton.me/u/0"));
        let iso = &m.content_scripts[0];
        assert!(iso.all_frames);
        assert_eq!(iso.run_at, RunAt::DocumentEnd);
        assert_eq!(
            m.action.as_ref().unwrap().popup.as_deref(),
            Some("popup.html")
        );
        assert_eq!(m.icon(16), Some("assets/protonpass-icon-16.png"));
        assert_eq!(m.icon(20), Some("assets/protonpass-icon-32.png"));
        assert_eq!(m.icon(512), Some("assets/protonpass-icon-128.png"));
        assert_eq!(m.options_page.as_deref(), Some("settings.html"));
        assert!(
            m.externally_connectable
                .as_ref()
                .unwrap()
                .matches
                .matches("https://account.proton.me/x")
        );
        assert!(m.has_permission("webRequest"));
        assert!(m.hosts().matches("http://127.0.0.1:18765/"));
        assert_eq!(
            m.page_csp,
            "script-src 'self' 'wasm-unsafe-eval'; object-src 'self';"
        );
        assert_eq!(m.commands.len(), 2);
        assert!(m.unsupported().is_empty(), "{:?}", m.unsupported());
    }

    #[test]
    fn web_accessible_resources_are_enforced() {
        let m = proton();
        let page = Some("https://example.com/login");
        assert!(m.web_accessible("dropdown.html", page, None));
        assert!(m.web_accessible("/assets/fonts.woff2", page, None));
        assert!(m.web_accessible("chunk.crypto.wasm", Some("file:///x.html"), None));
        assert!(!m.web_accessible("popup.html", page, None));
        assert!(!m.web_accessible("background.js", page, None));
        // onboarding.html only for account.proton.me.
        assert!(!m.web_accessible("onboarding.html", page, None));
        assert!(m.web_accessible(
            "onboarding.html",
            Some("https://account.proton.me/pass"),
            None
        ));
        // No page at all (a typed address) is not a web page.
        assert!(!m.web_accessible("dropdown.html", None, None));
        // Other extensions only when listed.
        assert!(!m.web_accessible("dropdown.html", None, Some("abc")));
    }

    #[test]
    fn manifest_v2_and_broken_manifests_are_refused() {
        let v2 = json!({"name": "x", "version": "1", "manifest_version": 2});
        assert_eq!(
            Manifest::from_value(v2, Map::new()),
            Err(ManifestError::NotV3(2))
        );
        let bad = json!({"name": "x", "version": "1.x", "manifest_version": 3});
        assert_eq!(
            Manifest::from_value(bad, Map::new()),
            Err(ManifestError::Missing("valid version"))
        );
        let unsupported = json!({"name": "x", "version": "1", "manifest_version": 3,
            "permissions": ["identity", "storage", "debugger"], "side_panel": {}});
        let m = Manifest::from_value(unsupported, Map::new()).unwrap();
        assert_eq!(m.unsupported(), vec!["identity", "debugger", "sidePanel"]);
    }

    #[test]
    fn messages_localize_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let write = |loc: &str, body: Value| {
            let d = dir.path().join("_locales").join(loc);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("messages.json"), body.to_string()).unwrap();
        };
        write(
            "en",
            json!({"appName": {"message": "App"}, "greet": {"message": "Hi $who$", "placeholders": {"who": {"content": "$1"}}}}),
        );
        write("de", json!({"appName": {"message": "Anwendung"}}));
        std::fs::write(
            dir.path().join("manifest.json"),
            json!({"name": "__MSG_appName__", "description": "x __MSG_missing__ y", "version": "2.0", "manifest_version": 3, "default_locale": "en"}).to_string(),
        )
        .unwrap();
        let m = Manifest::load(dir.path(), "de-DE").unwrap();
        assert_eq!(m.name, "Anwendung");
        assert_eq!(m.description, "x  y");
        assert_eq!(m.messages["greet"], "Hi $1");
        let m = Manifest::load(dir.path(), "fr").unwrap();
        assert_eq!(m.name, "App");
        assert_eq!(m.raw["name"], "App");
    }
}
