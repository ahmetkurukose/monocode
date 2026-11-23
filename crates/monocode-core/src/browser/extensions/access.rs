//! Who may load what from `mcext://<id>/`: the extension's own contexts
//! everything; web pages only its web-accessible resources, for the pages
//! the manifest lists (Chrome's `web_accessible_resources`); the layer's
//! own files (`__mcext/…`: the shim with its token, the bridge) only the
//! extension. The app's scheme handler asks [`decide`] for every request.

use super::{manifest::Manifest, pattern::origin_of, scripts::RESERVED};

/// What a request is, from the engine (`monocode_cef::extensions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    MainFrame,
    SubFrame,
    Worker,
    Resource,
}

#[derive(Clone, Debug, Default)]
pub struct Requester<'a> {
    pub load: Option<Load>,
    /// The frame's URL (for a document: the frame's old document).
    pub frame_url: Option<&'a str>,
    pub parent_url: Option<&'a str>,
    pub referrer: &'a str,
    /// The request comes from a page the layer hosts for this extension
    /// (its background, popup, offscreen document).
    pub from_host: bool,
    /// The app opened this top-level address itself (`tabs.create`, the
    /// options page, the popup) and expects it.
    pub expected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// The extension itself.
    Extension,
    /// A web page (its URL), or nobody known (`None`).
    Web(Option<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    Allow,
    /// Not web-accessible for this page, or the layer's own file.
    Deny,
}

/// Who is asking, for extension `id`.
pub fn who(id: &str, r: &Requester) -> Who {
    let own = super::origin(id);
    let is_own = |url: &str| origin_of(url).is_some_and(|o| o == own);
    let initiator: Option<&str> = match r.load.unwrap_or(Load::Resource) {
        Load::MainFrame => {
            if r.expected || r.from_host {
                return Who::Extension;
            }
            (!r.referrer.is_empty()).then_some(r.referrer)
        }
        Load::SubFrame => r.parent_url,
        Load::Worker | Load::Resource => match r.frame_url {
            Some(url) if !url.is_empty() && url != "about:blank" => Some(url),
            // A worker's requests: no frame; its creator is the referrer.
            _ if r.from_host => return Who::Extension,
            _ => (!r.referrer.is_empty()).then_some(r.referrer),
        },
    };
    match initiator {
        Some(url) if is_own(url) => Who::Extension,
        other => Who::Web(other.map(str::to_owned)),
    }
}

/// Whether `path` of extension `manifest` may load for `who`.
pub fn decide(manifest: &Manifest, path: &str, who: &Who) -> Access {
    let path = path.trim_start_matches('/');
    match who {
        Who::Extension => Access::Allow,
        Who::Web(_) if path.starts_with(RESERVED) => Access::Deny,
        Who::Web(url) => {
            if manifest.web_accessible(path, url.as_deref(), None) {
                Access::Allow
            } else {
                Access::Deny
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::extensions::PROTON_PASS_ID;

    fn proton() -> Manifest {
        Manifest::from_value(
            serde_json::from_str(super::super::manifest::PROTON_MANIFEST).unwrap(),
            serde_json::Map::new(),
        )
        .unwrap()
    }

    fn base(path: &str) -> String {
        format!("mcext://{PROTON_PASS_ID}/{path}")
    }

    #[test]
    fn the_extension_loads_everything_web_pages_only_what_is_listed() {
        let m = proton();
        let page = base("popup.html");
        let own = Requester {
            load: Some(Load::Resource),
            frame_url: Some(&page),
            ..Default::default()
        };
        assert_eq!(who(PROTON_PASS_ID, &own), Who::Extension);
        assert_eq!(decide(&m, "background.js", &Who::Extension), Access::Allow);
        assert_eq!(
            decide(&m, "__mcext/shim.js", &Who::Extension),
            Access::Allow
        );

        // A web page embedding the dropdown: allowed; the popup: not.
        let iframe = Requester {
            load: Some(Load::SubFrame),
            frame_url: Some("about:blank"),
            parent_url: Some("https://example.com/login"),
            ..Default::default()
        };
        let web = who(PROTON_PASS_ID, &iframe);
        assert_eq!(web, Who::Web(Some("https://example.com/login".into())));
        assert_eq!(decide(&m, "dropdown.html", &web), Access::Allow);
        assert_eq!(decide(&m, "popup.html", &web), Access::Deny);
        // Never the layer's own files: the shim carries the bridge token.
        assert_eq!(decide(&m, "__mcext/shim.js", &web), Access::Deny);
        assert_eq!(decide(&m, "__mcext/api", &web), Access::Deny);

        // A page's script or worker reaching in.
        let script = Requester {
            load: Some(Load::Resource),
            frame_url: Some("https://evil.example/"),
            ..Default::default()
        };
        let web = who(PROTON_PASS_ID, &script);
        assert_eq!(
            decide(&m, "assets/protonpass-icon-16.png", &web),
            Access::Allow
        );
        assert_eq!(decide(&m, "background.js", &web), Access::Deny);
        let worker = Requester {
            load: Some(Load::Worker),
            referrer: "https://evil.example/",
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "__mcext/shim.js", &who(PROTON_PASS_ID, &worker)),
            Access::Deny
        );
    }

    #[test]
    fn subresources_of_extension_frames_inside_web_pages_are_the_extensions() {
        let m = proton();
        let dropdown = base("dropdown.html");
        let r = Requester {
            load: Some(Load::Resource),
            frame_url: Some(&dropdown),
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "dropdown.js", &who(PROTON_PASS_ID, &r)),
            Access::Allow
        );
        // Its worker (no frame), referred by the frame.
        let w = Requester {
            load: Some(Load::Worker),
            referrer: &dropdown,
            ..Default::default()
        };
        assert_eq!(who(PROTON_PASS_ID, &w), Who::Extension);
        // Another extension's frame is a stranger.
        let other = "mcext://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/x.html";
        let r = Requester {
            load: Some(Load::Resource),
            frame_url: Some(other),
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "popup.html", &who(PROTON_PASS_ID, &r)),
            Access::Deny
        );
    }

    #[test]
    fn top_level_loads_need_the_app_or_a_listed_resource() {
        let m = proton();
        // A web page navigating the tab to the popup.
        let nav = Requester {
            load: Some(Load::MainFrame),
            frame_url: Some("https://evil.example/"),
            referrer: "https://evil.example/",
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "popup.html", &who(PROTON_PASS_ID, &nav)),
            Access::Deny
        );
        // No referrer at all (typed, or a no-referrer link): not trusted.
        let typed = Requester {
            load: Some(Load::MainFrame),
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "settings.html", &who(PROTON_PASS_ID, &typed)),
            Access::Deny
        );
        // The app's own opening (tabs.create, the popup, the background).
        let expected = Requester {
            load: Some(Load::MainFrame),
            expected: true,
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "settings.html", &who(PROTON_PASS_ID, &expected)),
            Access::Allow
        );
        // Within the extension: its settings page links on.
        let settings = base("settings.html");
        let inside = Requester {
            load: Some(Load::MainFrame),
            referrer: &settings,
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "onboarding.html", &who(PROTON_PASS_ID, &inside)),
            Access::Allow
        );
        // Onboarding is web-accessible for account.proton.me.
        let account = Requester {
            load: Some(Load::MainFrame),
            referrer: "https://account.proton.me/pass/signup",
            ..Default::default()
        };
        assert_eq!(
            decide(&m, "onboarding.html", &who(PROTON_PASS_ID, &account)),
            Access::Allow
        );
    }
}
