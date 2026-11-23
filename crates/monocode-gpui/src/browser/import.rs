//! Importing from Chrome, Arc and Brave (docs/native/browser/context.md
//! K61): what Settings' "Import from another browser" runs.
//!
//! Reading the source profile (`monocode_core::browser::import`) happens on
//! a background thread, since the Keychain prompt holds it; the cookies are
//! written into the chosen Chromium profile on the main thread, and the
//! logins are merged into that profile's password vault ([`save_into_vault`]).
//! An [`Importer`] does both, so the Settings flow can be tested with a
//! fake one.

use super::{Engine, EngineState, Profile, passwords};
use gpui::{App, Global, Task};
use monocode_core::browser::import::{
    self as core_import, ImportData, ImportError, ImportRequest, ImportedCookie, ImportedLogin,
    InstalledBrowser,
};
use std::{path::Path, sync::Arc, time::Duration};

/// How a cookie write went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CookieWrite {
    pub written: usize,
    pub failed: usize,
}

/// What the password vault did with the imported logins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VaultMerge {
    pub added: usize,
    pub updated: usize,
}

pub(crate) trait Importer: Send + Sync {
    /// The browsers installed and their profiles.
    fn discover(&self) -> Vec<InstalledBrowser>;
    /// Reads a profile; runs on a background thread and may wait for the
    /// Keychain prompt.
    fn read(&self, request: &ImportRequest) -> Result<ImportData, ImportError>;
    /// Writes `cookies` into `target`, `browser_root` being where this data
    /// mode keeps its browser profiles. On the main thread.
    fn write_cookies(
        &self,
        browser_root: &Path,
        target: &Profile,
        cookies: Vec<ImportedCookie>,
        cx: &mut App,
    ) -> Task<Result<CookieWrite, String>>;
    /// Merges the logins and "never save" sites into the password vault of
    /// `target` (unlocking it first: the Keychain may ask); `Err` says why
    /// they were not kept.
    fn save_logins(
        &self,
        target: &Profile,
        logins: Vec<ImportedLogin>,
        never_save: Vec<String>,
        cx: &mut App,
    ) -> Task<Result<VaultMerge, String>>;
}

/// The importer Settings uses; tests put a fake one here.
pub(crate) struct ImporterService(pub Arc<dyn Importer>);
impl Global for ImporterService {}

pub(crate) fn importer(cx: &App) -> Arc<dyn Importer> {
    match cx.try_global::<ImporterService>() {
        Some(service) => service.0.clone(),
        None => Arc::new(SystemImporter),
    }
}

/// The real browsers, the login Keychain and Chromium.
pub(crate) struct SystemImporter;

impl Importer for SystemImporter {
    fn discover(&self) -> Vec<InstalledBrowser> {
        match std::env::var_os("HOME") {
            Some(home) if cfg!(target_os = "macos") => core_import::discover(Path::new(&home)),
            _ => Vec::new(),
        }
    }

    fn read(&self, request: &ImportRequest) -> Result<ImportData, ImportError> {
        core_import::read_profile(request, &core_import::Keychain)
    }

    fn write_cookies(
        &self,
        browser_root: &Path,
        target: &Profile,
        cookies: Vec<ImportedCookie>,
        cx: &mut App,
    ) -> Task<Result<CookieWrite, String>> {
        if cookies.is_empty() {
            return Task::ready(Ok(CookieWrite::default()));
        }
        // Chromium may not run yet: nothing opened a page.
        let engine = Engine::global(cx);
        engine.update(cx, |engine, cx| {
            engine.ensure(browser_root.to_path_buf(), cx)
        });
        let target = target.clone();
        cx.spawn(async move |cx| {
            let mut waited = Duration::ZERO;
            loop {
                match engine.read_with(cx, |engine, _| engine.state.clone()) {
                    EngineState::Ready => break,
                    EngineState::Failed(why) => return Err(why),
                    EngineState::Idle if waited < Duration::from_secs(20) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(50))
                            .await;
                        waited += Duration::from_millis(50);
                    }
                    EngineState::Idle => return Err("Chromium did not start".into()),
                }
            }
            let (tx, rx) = async_channel::bounded(1);
            cx.update(|_| write(&target, cookies, tx));
            rx.recv()
                .await
                .map_err(|_| "Chromium stopped before the cookies were written".to_owned())
        })
    }

    fn save_logins(
        &self,
        target: &Profile,
        logins: Vec<ImportedLogin>,
        never_save: Vec<String>,
        cx: &mut App,
    ) -> Task<Result<VaultMerge, String>> {
        save_into_vault(target, logins, never_save, cx)
    }
}

/// Merges imported logins into `target`'s password vault (K56, K61) by
/// [`core_import::merge_logins`]'s rule: one per site and user name, the
/// newer password wins; the "never save" sites join the vault's list.
pub(crate) fn save_into_vault(
    target: &Profile,
    logins: Vec<ImportedLogin>,
    never_save: Vec<String>,
    cx: &mut App,
) -> Task<Result<VaultMerge, String>> {
    let Some(store) = passwords::store_name(target) else {
        return Task::ready(Err("this profile keeps no passwords".into()));
    };
    let unlocking = passwords::unlock(cx);
    cx.spawn(async move |cx| {
        match unlocking.await {
            passwords::VaultState::Open => {}
            passwords::VaultState::Unavailable(why) => return Err(why),
            _ => return Err("the password vault did not open".into()),
        }
        cx.update(|cx| -> Result<VaultMerge, String> {
            let now = super::history::now_ms();
            let report =
                passwords::with_vault(&store, cx, |vault| vault.import(&logins, &never_save, now))
                    .unwrap_or_else(|| Err("the password vault did not open".into()))?;
            passwords::changed(cx);
            Ok(VaultMerge {
                added: report.added,
                updated: report.updated,
            })
        })
    })
}

#[cfg(target_os = "macos")]
fn write(target: &Profile, cookies: Vec<ImportedCookie>, tx: async_channel::Sender<CookieWrite>) {
    let cookies = cookies.iter().map(new_cookie).collect();
    monocode_cef::set_cookies(target, cookies, move |w| {
        let _ = tx.try_send(CookieWrite {
            written: w.written,
            failed: w.failed,
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn write(_: &Profile, cookies: Vec<ImportedCookie>, tx: async_channel::Sender<CookieWrite>) {
    let _ = tx.try_send(CookieWrite {
        written: 0,
        failed: cookies.len(),
    });
}

/// An imported cookie as Chromium sets it: a host-only one without a
/// domain, a domain one with its leading dot.
#[cfg(target_os = "macos")]
fn new_cookie(c: &ImportedCookie) -> monocode_cef::NewCookie {
    use core_import::SameSite;
    use monocode_cef::CookieSameSite as Cef;
    monocode_cef::NewCookie {
        url: c.url(),
        name: c.name.clone(),
        value: c.value.clone(),
        domain: if c.host_only() {
            String::new()
        } else {
            c.host_key.clone()
        },
        path: c.path.clone(),
        secure: c.secure,
        http_only: c.http_only,
        same_site: match c.same_site {
            SameSite::Unspecified => Cef::Unspecified,
            SameSite::None => Cef::NoRestriction,
            SameSite::Lax => Cef::Lax,
            SameSite::Strict => Cef::Strict,
        },
        expires: c.expires,
        creation: c.creation,
        last_access: c.last_access,
    }
}

#[cfg(test)]
mod vault_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn login(user: &str, password: &str, modified: i64) -> ImportedLogin {
        ImportedLogin {
            origin: "https://example.com".into(),
            signon_realm: "https://example.com/".into(),
            username: user.into(),
            password: password.into(),
            created_ms: 1,
            last_used_ms: 0,
            password_modified_ms: modified,
            times_used: 0,
        }
    }

    fn save(
        target: &Profile,
        logins: Vec<ImportedLogin>,
        never_save: Vec<String>,
        cx: &mut gpui::TestAppContext,
    ) -> Result<VaultMerge, String> {
        let out = Rc::new(RefCell::new(None));
        let sink = out.clone();
        cx.update(|cx| {
            let saving = save_into_vault(target, logins, never_save, cx);
            cx.spawn(async move |_| {
                let saved = saving.await;
                *sink.borrow_mut() = Some(saved);
            })
            .detach();
        });
        cx.run_until_parked();
        out.borrow_mut().take().expect("the save finished")
    }

    #[gpui::test]
    fn imported_logins_go_into_the_profiles_vault(cx: &mut gpui::TestAppContext) {
        // The vault is locked at first: the import unlocks it.
        let first = save(
            &Profile::Default,
            vec![login("me", "old", 10)],
            vec!["https://never.example".into()],
            cx,
        );
        assert_eq!(
            first,
            Ok(VaultMerge {
                added: 1,
                updated: 0
            })
        );
        // Importing again merges: the newer password wins, a new user joins.
        let again = save(
            &Profile::Default,
            vec![login("me", "new", 20), login("you", "x", 0)],
            Vec::new(),
            cx,
        );
        assert_eq!(
            again,
            Ok(VaultMerge {
                added: 1,
                updated: 1
            })
        );
        cx.update(|cx| {
            let saved = passwords::with_vault("default", cx, |vault| {
                vault.credentials("https://example.com")
            })
            .unwrap()
            .unwrap();
            let me = saved.iter().find(|(l, _)| l.username == "me").unwrap();
            assert_eq!(me.1, "new");
            assert_eq!(saved.len(), 2);
            let never = passwords::with_vault("default", cx, |vault| {
                vault.is_never("https://never.example")
            });
            assert_eq!(never, Some(Ok(true)));
        });
    }

    #[gpui::test]
    fn a_throwaway_profile_keeps_no_imported_logins(cx: &mut gpui::TestAppContext) {
        let saved = save(
            &Profile::Ephemeral("temp-1".into()),
            vec![login("me", "pw", 1)],
            Vec::new(),
            cx,
        );
        assert!(saved.is_err());
        cx.update(|cx| assert!(passwords::vaults(cx).is_none(), "nothing unlocked"));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use core_import::SameSite;

    fn cookie(host_key: &str, secure: bool, same_site: SameSite) -> ImportedCookie {
        ImportedCookie {
            name: "sid".into(),
            value: "v".into(),
            host_key: host_key.into(),
            path: "/app".into(),
            secure,
            http_only: true,
            same_site,
            expires: Some(13_400_000_000_000_000),
            creation: 13_300_000_000_000_000,
            last_access: 13_350_000_000_000_000,
        }
    }

    #[test]
    fn domain_and_host_only_cookies_map_onto_chromiums_fields() {
        let domain = new_cookie(&cookie(".example.com", true, SameSite::None));
        assert_eq!(domain.url, "https://example.com/app");
        assert_eq!(domain.domain, ".example.com");
        assert_eq!(
            domain.same_site,
            monocode_cef::CookieSameSite::NoRestriction
        );
        assert_eq!(domain.expires, Some(13_400_000_000_000_000));
        assert!(domain.secure && domain.http_only);
        let host = new_cookie(&cookie("app.example.com", false, SameSite::Strict));
        assert_eq!(host.url, "http://app.example.com/app");
        assert_eq!(host.domain, "", "host-only: no domain");
        assert_eq!(host.same_site, monocode_cef::CookieSameSite::Strict);
        assert_eq!(host.creation, 13_300_000_000_000_000);
    }
}
