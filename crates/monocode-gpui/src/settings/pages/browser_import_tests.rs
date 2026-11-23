use super::*;
use crate::browser::import::{Importer, ImporterService, VaultMerge};
use crate::settings::{Preferences, Section, SettingsContext};
use gpui::{
    App, Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowHandle, WindowOptions,
    point, size,
};
use monocode_core::browser::import::{
    CookieRead, ImportData, ImportError, ImportedCookie, ImportedLogin, LoginRead, SameSite,
    SourceProfile,
};
use std::sync::{Arc, Mutex};

/// Two browsers, canned data, and a record of what was asked.
#[derive(Default)]
struct FakeImporter {
    fail: Option<ImportError>,
    requests: Mutex<Vec<ImportRequest>>,
    written: Mutex<usize>,
    saved: Mutex<usize>,
}

fn profile(browser: SourceBrowser, folder: &str, name: &str) -> SourceProfile {
    SourceProfile {
        browser,
        name: name.into(),
        folder: folder.into(),
        dir: format!("/fake/{}/{folder}", browser.id()).into(),
        has_cookies: true,
        has_logins: true,
    }
}

impl Importer for FakeImporter {
    fn discover(&self) -> Vec<InstalledBrowser> {
        [SourceBrowser::Chrome, SourceBrowser::Arc]
            .into_iter()
            .map(|browser| InstalledBrowser {
                browser,
                root: "/fake".into(),
                profiles: vec![
                    profile(browser, "Default", "Me"),
                    profile(browser, "Profile 1", "Work"),
                ],
            })
            .collect()
    }

    fn read(&self, request: &ImportRequest) -> Result<ImportData, ImportError> {
        self.requests.lock().unwrap().push(request.clone());
        if let Some(e) = &self.fail {
            return Err(e.clone());
        }
        let cookie = |name: &str| ImportedCookie {
            name: name.into(),
            value: "v".into(),
            host_key: ".example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
            expires: None,
            creation: 0,
            last_access: 0,
        };
        Ok(ImportData {
            cookies: request.cookies.then(|| CookieRead {
                cookies: vec![cookie("a"), cookie("b")],
                skipped_google: 1,
                ..Default::default()
            }),
            logins: request.passwords.then(|| LoginRead {
                logins: vec![ImportedLogin {
                    origin: "https://example.com".into(),
                    signon_realm: "https://example.com/".into(),
                    username: "me".into(),
                    password: "pw".into(),
                    created_ms: 1,
                    last_used_ms: 1,
                    password_modified_ms: 1,
                    times_used: 1,
                }],
                never_save: vec!["https://never.example".into()],
                ..Default::default()
            }),
        })
    }

    fn write_cookies(
        &self,
        _: &std::path::Path,
        target: &crate::browser::Profile,
        cookies: Vec<ImportedCookie>,
        _: &mut App,
    ) -> Task<Result<CookieWrite, String>> {
        assert_eq!(*target, crate::browser::Profile::Default);
        *self.written.lock().unwrap() += cookies.len();
        Task::ready(Ok(CookieWrite {
            written: cookies.len(),
            failed: 0,
        }))
    }

    fn save_logins(
        &self,
        _: &crate::browser::Profile,
        logins: Vec<ImportedLogin>,
        _: Vec<String>,
        _: &mut App,
    ) -> Option<VaultMerge> {
        *self.saved.lock().unwrap() += logins.len();
        Some(VaultMerge {
            added: logins.len(),
            updated: 0,
        })
    }
}

fn window(importer: Arc<FakeImporter>, cx: &mut TestAppContext) -> WindowHandle<SettingsView> {
    cx.update(|cx| cx.set_global(ImporterService(importer)));
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                // Tall enough that every General row is on screen.
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1200.), px(6000.)),
                ))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let mut view = SettingsView::new(
                        Section::General,
                        Preferences::default(),
                        SettingsContext::default(),
                        cx,
                    );
                    view.discovery_task = None;
                    view.discovery_loading = false;
                    view
                })
            },
        )
        .unwrap()
    });
    cx.run_until_parked();
    window
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let at = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not drawn"))
        .center();
    visual.simulate_click(at, Default::default());
    visual.run_until_parked();
}

#[gpui::test]
fn importing_picks_a_browser_and_reports_what_came_over(cx: &mut TestAppContext) {
    let importer = Arc::new(FakeImporter::default());
    let window = window(importer.clone(), cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("settings-browser-import-run").is_none(),
        "closed at first"
    );
    window
        .update(visual, |view, _, cx| view.open_browser_import(cx))
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            let panel = &view.browser_import;
            assert_eq!(panel.browser, Some(SourceBrowser::Chrome));
            assert_eq!(panel.profile.as_deref(), Some("Default"));
            assert!(panel.cookies && panel.passwords);
        })
        .unwrap();

    click(visual, "settings-browser-import-browser-arc");
    // Passwords off and on again: the toggles answer.
    click(visual, "settings-toggle-browser-import-passwords");
    window
        .update(visual, |view, _, _| assert!(!view.browser_import.passwords))
        .unwrap();
    click(visual, "settings-toggle-browser-import-passwords");
    click(visual, "settings-browser-import-run");

    let requests = importer.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].profile.browser, SourceBrowser::Arc);
    assert_eq!(requests[0].profile.folder, "Default");
    assert!(requests[0].cookies && requests[0].passwords);
    assert_eq!(*importer.written.lock().unwrap(), 2);
    assert_eq!(*importer.saved.lock().unwrap(), 1);
    window
        .update(visual, |view, _, _| {
            let ImportStatus::Done { lines, notes } = &view.browser_import.status else {
                panic!("{:?}", view.browser_import.status);
            };
            assert_eq!(
                lines,
                &[
                    "Cookies: 2 imported, 1 skipped (1 Google)".to_owned(),
                    "Passwords: 1 read (1 new, 0 updated), 1 never-save sites".to_owned(),
                ]
            );
            assert!(
                notes
                    .iter()
                    .any(|n| n.contains("sign in to Google in Monocode"))
            );
        })
        .unwrap();
    assert!(
        visual
            .debug_bounds("settings-browser-import-status")
            .is_some()
    );
}

#[gpui::test]
fn a_denied_keychain_is_said_plainly(cx: &mut TestAppContext) {
    let importer = Arc::new(FakeImporter {
        fail: Some(ImportError::KeychainDenied(SourceBrowser::Chrome)),
        ..Default::default()
    });
    let window = window(importer.clone(), cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |view, _, cx| view.open_browser_import(cx))
        .unwrap();
    visual.run_until_parked();
    click(visual, "settings-browser-import-run");
    window
        .update(visual, |view, _, _| {
            assert_eq!(
                view.browser_import.status,
                ImportStatus::Failed(
                    "Keychain access to \"Chrome Safe Storage\" was denied. Import again and choose Allow."
                        .into()
                )
            );
        })
        .unwrap();
    assert_eq!(*importer.written.lock().unwrap(), 0);
}
