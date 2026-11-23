//! General → "Site permissions" (docs/native/browser/context.md K55): the
//! browser's remembered answers of every profile, with a reset per site
//! and one for all. Sites ask again once reset.
use crate::settings::{SettingsView, widgets::secondary_button};
use gpui::{AnyElement, Context, prelude::*};
use monocode_core::browser::sites::{self, Setting};

impl SettingsView {
    /// Where the browser keeps its profiles in this data mode.
    fn browser_root(&self) -> Option<std::path::PathBuf> {
        let store = self.ctx.store.as_ref()?;
        let store = crate::runtime::locked(&store.0).ok()?;
        Some(store.data_dir()?.join("Browser"))
    }

    pub(in crate::settings) fn site_permission_rows(
        &self,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(root) = self.browser_root() else {
            return Vec::new();
        };
        let profiles = crate::browser::all_sites(&root);
        let count: usize = profiles.iter().map(|(_, sites)| sites.len()).sum();
        let description = if count == 0 {
            "Sites ask in the browser before they use your camera, microphone, location, notifications and the like. Nothing is remembered yet.".to_owned()
        } else {
            format!(
                "What {count} site{} may use. Reset a site to be asked again.",
                if count == 1 { "" } else { "s" }
            )
        };
        let everything: Vec<(std::path::PathBuf, String, Vec<String>)> = (profiles.iter())
            .flat_map(|(dir, sites)| {
                sites.iter().map(move |(origin, names)| {
                    let names = names.iter().map(|(n, _)| n.clone()).collect();
                    (dir.clone(), origin.clone(), names)
                })
            })
            .collect();
        let clear_root = root.clone();
        let mut rows = vec![self.row(
            Some("browser-site-permissions"),
            false,
            "Site permissions",
            Some(description.into()),
            vec![
                secondary_button(
                    "browser-site-permissions-clear",
                    "Reset all",
                    false,
                    count == 0,
                )
                .child("Reset all")
                .when(count > 0, |d| {
                    d.on_click(cx.listener(move |_, _, _, cx| {
                        for (dir, origin, names) in &everything {
                            for name in names {
                                crate::browser::forget_site_permission(
                                    &clear_root,
                                    dir,
                                    origin,
                                    name,
                                );
                            }
                        }
                        cx.notify();
                    }))
                })
                .into_any_element(),
            ],
        )];
        let isolated = profiles.len() > 1
            || profiles
                .first()
                .is_some_and(|(dir, _)| !dir.ends_with("default"));
        for (dir, sites) in &profiles {
            for (ix, (origin, names)) in sites.iter().enumerate() {
                let summary = names
                    .iter()
                    .map(|(name, setting)| {
                        format!(
                            "{}: {}",
                            sites::title(name),
                            match setting {
                                Setting::Allow => "allowed",
                                Setting::Block => "blocked",
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                let profile = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let summary = if isolated {
                    format!("{summary} ({profile} profile)")
                } else {
                    summary
                };
                let (root, dir, origin, names) = (
                    root.clone(),
                    dir.clone(),
                    origin.clone(),
                    names.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                );
                let id = gpui::ElementId::NamedInteger(
                    "browser-site-reset".into(),
                    (rows.len() * 1000 + ix) as u64,
                );
                rows.push(self.row(
                    None,
                    false,
                    sites::host_of(&origin),
                    Some(summary.into()),
                    vec![
                        crate::modal::ghost_button(id, "Reset")
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                for name in &names {
                                    crate::browser::forget_site_permission(
                                        &root, &dir, &origin, name,
                                    );
                                }
                                cx.notify();
                            }))
                            .into_any_element(),
                    ],
                ));
            }
        }
        rows
    }
}
