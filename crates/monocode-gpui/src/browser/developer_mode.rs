//! Developer Mode for a site (docs/native/modes.md §Site Control Center), as
//! SOCode's: a choice per host and profile, on by default for `localhost`
//! and its subdomains, turned on and off in the Site Control Center. While
//! it is on, a Browser mode page shows the developer bar over it: its full
//! address in a fixed-width face, then Copy URL, Capture, Grab, DevTools and
//! the Site Control Center.
//!
//! The choices live in the profile's folder (`monocode-developer-sites.json`);
//! a throwaway profile keeps them in memory.
use super::*;
use std::collections::BTreeSet;

const FILE_NAME: &str = "monocode-developer-sites.json";

/// A profile's explicit choices; the default covers what is not in them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DeveloperSites {
    #[serde(default)]
    on: BTreeSet<String>,
    #[serde(default)]
    off: BTreeSet<String>,
}

impl DeveloperSites {
    /// On for `host`: chosen so, or `localhost` (and its subdomains) not
    /// turned off.
    pub(crate) fn enabled(&self, host: &str) -> bool {
        !self.off.contains(host)
            && (self.on.contains(host) || host == "localhost" || host.ends_with(".localhost"))
    }

    pub(crate) fn set(&mut self, host: &str, enabled: bool) {
        if enabled {
            self.off.remove(host);
            self.on.insert(host.to_owned());
        } else {
            self.on.remove(host);
            self.off.insert(host.to_owned());
        }
    }

    fn load(path: &Path) -> Self {
        (std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, bytes) {
                    log::warn!("browser: developer sites not saved: {e}");
                }
            }
            Err(e) => log::warn!("browser: developer sites not saved: {e}"),
        }
    }
}

thread_local! {
    /// Every profile's choices, by its file (or a throwaway profile's
    /// name), loaded once.
    static STORES: RefCell<HashMap<PathBuf, DeveloperSites>> = RefCell::new(HashMap::new());
}

/// Where `profile`'s choices are kept, and whether that is a file.
fn key(root: &Path, profile: &monocode_cef::Profile) -> (PathBuf, bool) {
    match sites::profile_dir(root, profile) {
        Some(dir) => (dir.join(FILE_NAME), true),
        None => (PathBuf::from(format!("ephemeral:{profile:?}")), false),
    }
}

fn with_store<T>(
    root: &Path,
    profile: &monocode_cef::Profile,
    f: impl FnOnce(&mut DeveloperSites) -> T,
) -> T {
    let (path, file) = key(root, profile);
    STORES.with(|stores| {
        let mut stores = stores.borrow_mut();
        let store = (stores.entry(path.clone())).or_insert_with(|| {
            if file {
                DeveloperSites::load(&path)
            } else {
                DeveloperSites::default()
            }
        });
        f(store)
    })
}

impl BrowserPage {
    /// The host Developer Mode would be for: a web page's.
    pub(crate) fn developer_host(&self) -> Option<String> {
        use monocode_core::browser::sites::{host_of, origin_of};
        origin_of(&self.url).map(|_| host_of(&self.url))
    }

    pub(crate) fn developer_mode(&self) -> bool {
        let Some(host) = self.developer_host() else {
            return false;
        };
        let profile = self.host.options.borrow().profile.clone();
        with_store(&self.profiles, &profile, |store| store.enabled(&host))
    }

    /// The Site Control Center's Developer Mode row.
    pub(crate) fn toggle_developer_mode(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.developer_host() else {
            return;
        };
        let profile = self.host.options.borrow().profile.clone();
        let (path, file) = key(&self.profiles, &profile);
        with_store(&self.profiles, &profile, |store| {
            let enabled = !store.enabled(&host);
            store.set(&host, enabled);
            if file {
                store.save(&path);
            }
        });
        cx.notify();
    }

    /// The developer bar over a Browser mode page in Developer Mode, as
    /// SOCode's: `None` elsewhere (a Dev pane's toolbar has the tools).
    pub(super) fn developer_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.nav_in_sidebar || !self.developer_mode() {
            return None;
        }
        let ready = self.host.view.borrow().is_some();
        let ink = gpui::rgb(0xe8e7ff);
        let button = |id: &'static str, label: &'static str, path: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::Button)
                .aria_label(label)
                .flex_shrink_0()
                .size(px(28.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(gpui::hsla(0., 0., 1., 0.08)))
                .child(icon(path, 15.).text_color(ink))
        };
        let divider = |margin: f32| {
            div()
                .mx(px(margin))
                .w(px(1.))
                .h(px(18.))
                .bg(gpui::hsla(0., 0., 1., 0.1))
        };
        let url = self.url.trim_end_matches('/').to_owned();
        Some(
            div()
                .id("browser-developer-bar")
                .debug_selector(|| "browser-developer-bar".into())
                .flex_shrink_0()
                .h(px(40.))
                .px(px(4.))
                .flex()
                .items_center()
                .bg(gpui::rgb(0x3e43cf))
                .text_color(ink)
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(self.site_controls.bar_anchor.probe())
                        .child(
                            button(
                                "browser-developer-site-controls",
                                "Site Controls",
                                "sliders",
                            )
                            .mr(px(2.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let at = this.site_controls.bar_anchor.get();
                                this.toggle_site_controls_at(at, cx);
                            })),
                        ),
                )
                .child(
                    div()
                        .id("browser-developer-address")
                        .flex_1()
                        .min_w_0()
                        .px(px(4.))
                        .truncate()
                        .font_family("Menlo")
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(url.clone()),
                )
                .child(
                    button("browser-developer-copy", "Copy URL", "copy").on_click(
                        move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()))
                        },
                    ),
                )
                .child(divider(10.))
                .child(
                    button("browser-developer-capture", "Capture", "scan").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.start_capture(cx);
                        },
                    )),
                )
                .child(
                    button("browser-developer-grab", "Send an Element to Agent", "grab")
                        .when(self.picking(), |b| b.bg(gpui::hsla(0., 0., 1., 0.16)))
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_grab(window, cx))),
                )
                .child(divider(6.))
                .child(
                    button("browser-developer-devtools", "DevTools", "code")
                        .when(!ready, |b| b.opacity(0.45))
                        .when(self.devtools.is_some(), |b| {
                            b.bg(gpui::hsla(0., 0., 1., 0.16))
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_dev_tools(cx))),
                )
                .child(
                    button(
                        "browser-developer-reload",
                        "Reload Without Cache",
                        "refresh-cw",
                    )
                    .on_click(cx.listener(|this, _, _, _| this.reload(true))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_is_on_until_turned_off_and_others_until_turned_on() {
        let mut sites = DeveloperSites::default();
        assert!(sites.enabled("localhost"));
        assert!(sites.enabled("app.localhost"));
        assert!(!sites.enabled("example.com"));
        assert!(!sites.enabled("localhost.example.com"));
        sites.set("localhost", false);
        sites.set("example.com", true);
        assert!(!sites.enabled("localhost"));
        assert!(sites.enabled("app.localhost"));
        assert!(sites.enabled("example.com"));
        sites.set("example.com", false);
        assert!(!sites.enabled("example.com"));
    }

    #[test]
    fn choices_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p").join(FILE_NAME);
        let mut sites = DeveloperSites::default();
        sites.set("localhost", false);
        sites.set("example.com", true);
        sites.save(&path);
        assert_eq!(DeveloperSites::load(&path), sites);
        assert_eq!(
            DeveloperSites::load(&dir.path().join("none")),
            DeveloperSites::default()
        );
    }
}
