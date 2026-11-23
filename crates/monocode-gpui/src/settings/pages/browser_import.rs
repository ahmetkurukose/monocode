//! General → "Import from another browser" (docs/native/browser/context.md
//! K61): pick Chrome, Arc or Brave and one of its profiles, choose cookies
//! and passwords, and import once. Importing again merges.
use crate::browser::import::{CookieWrite, importer};
use crate::settings::{
    SettingsView, model,
    widgets::{primary_button, secondary_button},
};
use crate::theme::content;
use gpui::{AnyElement, Context, SharedString, Task, div, prelude::*, px};
use monocode_core::browser::import::{
    ImportRequest, ImportSummary, InstalledBrowser, SourceBrowser,
};

#[cfg(test)]
#[path = "browser_import_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ImportStatus {
    Idle,
    /// What the import is doing.
    Running(String),
    Done {
        lines: Vec<String>,
        notes: Vec<String>,
    },
    Failed(String),
}

pub(crate) struct ImportPanel {
    pub(in crate::settings) open: bool,
    /// The browsers found, once looked for.
    pub(in crate::settings) browsers: Option<Vec<InstalledBrowser>>,
    pub(in crate::settings) browser: Option<SourceBrowser>,
    /// The chosen profile's folder.
    pub(in crate::settings) profile: Option<String>,
    pub(in crate::settings) cookies: bool,
    pub(in crate::settings) passwords: bool,
    /// Into the project's isolated profile rather than the shared one.
    pub(in crate::settings) into_project: bool,
    pub(in crate::settings) status: ImportStatus,
    task: Option<Task<()>>,
}

impl Default for ImportPanel {
    fn default() -> Self {
        ImportPanel {
            open: false,
            browsers: None,
            browser: None,
            profile: None,
            cookies: true,
            passwords: true,
            into_project: false,
            status: ImportStatus::Idle,
            task: None,
        }
    }
}

impl ImportPanel {
    fn running(&self) -> bool {
        matches!(self.status, ImportStatus::Running(_))
    }

    fn chosen(&self) -> Option<&InstalledBrowser> {
        let browsers = self.browsers.as_ref()?;
        browsers.iter().find(|b| Some(b.browser) == self.browser)
    }

    /// Picks the first browser and its first profile when the choice is
    /// gone or was never made.
    fn settle_choice(&mut self) {
        let Some(browsers) = &self.browsers else {
            return;
        };
        if !browsers.iter().any(|b| Some(b.browser) == self.browser) {
            self.browser = browsers.first().map(|b| b.browser);
            self.profile = None;
        }
        let profiles = self
            .chosen()
            .map(|b| b.profiles.clone())
            .unwrap_or_default();
        if !profiles
            .iter()
            .any(|p| Some(&p.folder) == self.profile.as_ref())
        {
            self.profile = profiles.first().map(|p| p.folder.clone());
        }
    }
}

impl SettingsView {
    /// Opens the import rows and brings them into view (the command
    /// palette's "Import from Another Browser…").
    pub(crate) fn open_browser_import(&mut self, cx: &mut Context<Self>) {
        self.show_browser_import(cx);
        self.go(
            model::SearchResult {
                section: crate::settings::Section::General,
                setting: Some("browser-import"),
                label: "Import from another browser",
            },
            cx,
        );
    }

    fn show_browser_import(&mut self, cx: &mut Context<Self>) {
        self.browser_import.open = true;
        let importer = importer(cx);
        let found = cx
            .background_executor()
            .spawn(async move { importer.discover() });
        self.browser_import.task = Some(cx.spawn(async move |this, cx| {
            let browsers = found.await;
            let _ = this.update(cx, |this, cx| {
                this.browser_import.browsers = Some(browsers);
                this.browser_import.settle_choice();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The profile the cookies go to.
    fn import_target(&self) -> crate::browser::Profile {
        if self.browser_import.into_project && self.ctx.browser_project_isolated {
            crate::browser::Profile::for_project(&monocode_core::files::path_key(&self.ctx.cwd))
        } else {
            crate::browser::Profile::Default
        }
    }

    pub(in crate::settings) fn run_browser_import(&mut self, cx: &mut Context<Self>) {
        let panel = &self.browser_import;
        if panel.running() || !(panel.cookies || panel.passwords) {
            return;
        }
        let Some(profile) = panel.chosen().and_then(|b| {
            b.profiles
                .iter()
                .find(|p| Some(&p.folder) == panel.profile.as_ref())
                .cloned()
        }) else {
            return;
        };
        let request = ImportRequest {
            profile,
            cookies: panel.cookies,
            passwords: panel.passwords,
        };
        let label = request.profile.browser.label();
        let target = self.import_target();
        let root = self
            .browser_root()
            .unwrap_or_else(|| std::env::temp_dir().join("monocode-browser"));
        let importer = importer(cx);
        self.browser_import.status = ImportStatus::Running(format!(
            "Reading {label}… macOS may ask to allow access to \"{}\" in the Keychain.",
            request.profile.browser.keychain_item().0
        ));
        self.browser_import.task = Some(cx.spawn(async move |this, cx| {
            let reader = importer.clone();
            let read = cx
                .background_executor()
                .spawn(async move { reader.read(&request) })
                .await;
            let set = |status: ImportStatus, cx: &mut gpui::AsyncApp| {
                let _ = this.update(cx, |this, cx| {
                    this.browser_import.status = status;
                    cx.notify();
                });
            };
            let data = match read {
                Ok(data) => data,
                Err(e) => return set(ImportStatus::Failed(e.to_string()), cx),
            };
            let mut summary = ImportSummary::of_read(&data);
            let mut notes = Vec::new();
            if let Some(read) = data.cookies {
                let count = read.cookies.len();
                set(ImportStatus::Running(format!("Writing {count} cookies…")), cx);
                let write = cx.update(|cx| importer.write_cookies(&root, &target, read.cookies, cx));
                match write.await {
                    Ok(CookieWrite { written, failed }) => {
                        summary.cookies_imported = written;
                        summary.cookies_failed += failed;
                    }
                    Err(why) => {
                        summary.cookies_failed += count;
                        notes.push(format!("Cookies could not be written: {why}"));
                    }
                }
                if summary.cookies_google > 0 {
                    notes.push(
                        "Google and YouTube cookies were left out: sign in to Google in Monocode directly."
                            .into(),
                    );
                }
                if summary.cookies_app_bound > 0 {
                    notes.push("App-bound cookies (v20) can only be read by the browser that made them.".into());
                }
            }
            if let Some(read) = data.logins {
                let saved = cx.update(|cx| {
                    importer.save_logins(&target, read.logins, read.never_save, cx)
                });
                match saved {
                    Some(merge) => {
                        summary.logins_added = merge.added;
                        summary.logins_updated = merge.updated;
                    }
                    None if summary.logins_read > 0 => notes.push(
                        "Passwords were read but not saved: the password vault is not available yet."
                            .into(),
                    ),
                    None => {}
                }
            }
            set(
                ImportStatus::Done {
                    lines: summary.lines(),
                    notes,
                },
                cx,
            );
        }));
        cx.notify();
    }

    pub(in crate::settings) fn browser_import_rows(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let open = self.browser_import.open;
        let mut rows = vec![self.row(
            Some("browser-import"),
            false,
            "Import from another browser",
            Some("Cookies and saved passwords from Chrome, Arc or Brave, once; importing again merges. Google sign-ins stay behind: sign in to Google in Monocode directly.".into()),
            vec![
                secondary_button("browser-import-open", "Import…", false, false)
                    .child(if open { "Close" } else { "Import…" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.browser_import.open {
                            this.browser_import.open = false;
                            cx.notify();
                        } else {
                            this.show_browser_import(cx);
                        }
                    }))
                    .into_any_element(),
            ],
        )];
        if !open {
            return rows;
        }
        let panel = &self.browser_import;
        let Some(browsers) = panel.browsers.clone() else {
            rows.push(self.row(None, false, "Looking for browsers…", None, vec![]));
            return rows;
        };
        if browsers.is_empty() {
            rows.push(self.row(
                None,
                false,
                "No browser to import from",
                Some("Chrome, Arc and Brave are not installed for this user.".into()),
                vec![],
            ));
            return rows;
        }
        let running = panel.running();
        let options: Vec<(&'static str, &str)> = browsers
            .iter()
            .map(|b| (b.browser.id(), b.browser.label()))
            .collect();
        let current = panel.browser.map(|b| b.id()).unwrap_or_default();
        rows.push(self.row(
            None,
            false,
            "Browser",
            None,
            vec![self.segmented(
                "browser-import-browser",
                "Browser",
                current,
                &options,
                cx,
                |this, id, cx| {
                    this.browser_import.browser =
                        SourceBrowser::ALL.into_iter().find(|b| b.id() == id);
                    this.browser_import.settle_choice();
                    cx.notify();
                },
            )],
        ));
        let profiles = panel
            .chosen()
            .map(|b| b.profiles.clone())
            .unwrap_or_default();
        let profile = panel.profile.clone().unwrap_or_default();
        let chosen = profiles.iter().find(|p| p.folder == profile);
        let missing = match chosen {
            Some(p) if !p.has_cookies && !p.has_logins => {
                Some("This profile has no cookies or passwords.")
            }
            Some(p) if !p.has_logins => Some("This profile has no saved passwords."),
            Some(p) if !p.has_cookies => Some("This profile has no cookies."),
            _ => None,
        };
        rows.push(self.row(
            None,
            false,
            "Profile",
            missing.map(SharedString::from),
            vec![self.select(
                "browser-import-profile",
                "Profile",
                &profile,
                profiles
                    .iter()
                    .map(|p| (p.folder.clone(), SharedString::from(p.name.clone()), None))
                    .collect(),
                Some(208.),
                cx,
                |this, folder, cx| {
                    this.browser_import.profile = Some(folder);
                    cx.notify();
                },
            )],
        ));
        let (cookies, passwords) = (panel.cookies, panel.passwords);
        rows.push(
            self.row(
                None,
                false,
                "What to import",
                None,
                vec![
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .text_size(px(12.))
                        .text_color(content(0.7))
                        .child(div().flex().items_center().gap_2().child("Cookies").child(
                            self.toggle(
                                "browser-import-cookies",
                                "Cookies",
                                cookies,
                                running,
                                cx,
                                |this, on, cx| {
                                    this.browser_import.cookies = on;
                                    cx.notify();
                                },
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child("Passwords")
                                .child(self.toggle(
                                    "browser-import-passwords",
                                    "Passwords",
                                    passwords,
                                    running,
                                    cx,
                                    |this, on, cx| {
                                        this.browser_import.passwords = on;
                                        cx.notify();
                                    },
                                )),
                        )
                        .into_any_element(),
                ],
            ),
        );
        if self.ctx.browser_project_isolated {
            let into = if self.browser_import.into_project {
                "project"
            } else {
                "shared"
            };
            rows.push(self.row(
                None,
                false,
                "Import into",
                Some(format!("{} has its own browser profile.", self.ctx.project_label).into()),
                vec![self.segmented(
                    "browser-import-into",
                    "Import into",
                    into,
                    &[
                        ("shared", "Shared profile"),
                        ("project", "This project's profile"),
                    ],
                    cx,
                    |this, v, cx| {
                        this.browser_import.into_project = v == "project";
                        cx.notify();
                    },
                )],
            ));
        }
        let status = self.browser_import.status.clone();
        let disabled = running || !(cookies || passwords) || chosen.is_none();
        let (label, text): (&str, Option<SharedString>) = match &status {
            ImportStatus::Idle => ("Import", None),
            ImportStatus::Running(what) => ("Importing…", Some(what.clone().into())),
            ImportStatus::Done { lines, notes } => (
                "Import again",
                Some(
                    lines
                        .iter()
                        .chain(notes)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                        .into(),
                ),
            ),
            ImportStatus::Failed(why) => ("Try again", Some(why.clone().into())),
        };
        rows.push(
            div()
                .debug_selector(|| "settings-browser-import-status".into())
                .child(
                    self.row(
                        None,
                        false,
                        match status {
                            ImportStatus::Failed(_) => "Import failed",
                            ImportStatus::Done { .. } => "Imported",
                            _ => "",
                        },
                        text,
                        vec![
                            primary_button("browser-import-run", label, disabled)
                                .debug_selector(|| "settings-browser-import-run".into())
                                .child(label.to_owned())
                                .when(!disabled, |d| {
                                    d.on_click(
                                        cx.listener(|this, _, _, cx| this.run_browser_import(cx)),
                                    )
                                })
                                .into_any_element(),
                        ],
                    ),
                )
                .into_any_element(),
        );
        rows
    }
}
