//! Importing from Chrome, Arc and Brave (docs/native/browser/context.md
//! K61): what Settings' "Import from another browser" runs.
//!
//! Reading the source profile (`monocode_core::browser::import`) happens on
//! a background thread, since the Keychain prompt holds it; the cookies are
//! written into the chosen Chromium profile on the main thread, and the
//! logins go to the password vault. An [`Importer`] does both, so the
//! Settings flow can be tested with a fake one.

use super::{Engine, EngineState, Profile};
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
    /// Hands the logins to the password vault of `target`; `None` while
    /// there is no vault to take them.
    fn save_logins(
        &self,
        target: &Profile,
        logins: Vec<ImportedLogin>,
        never_save: Vec<String>,
        cx: &mut App,
    ) -> Option<VaultMerge>;
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
        _target: &Profile,
        logins: Vec<ImportedLogin>,
        never_save: Vec<String>,
        _cx: &mut App,
    ) -> Option<VaultMerge> {
        // TODO(vault, K56/K61): merge `logins` into the password vault of
        // `_target` with `monocode_core::browser::import::merge_logins`
        // (keyed by origin and username; the newer password wins), add the
        // `never_save` origins to its never-save list, and return what was
        // added and updated. Until the vault lands nothing is kept: the
        // plain-text logins are dropped here.
        drop((logins, never_save));
        None
    }
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
