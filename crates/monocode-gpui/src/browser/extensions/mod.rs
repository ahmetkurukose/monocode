//! The browser's own extension layer (docs/native/browser/context.md K69):
//! Chrome Web Store extensions run without Chromium's extension system.
//! The browser-independent half is `monocode_core::browser::extensions`;
//! the engine half (scheme, fetch proxy, popup window) is
//! `monocode_cef::extensions`. This is the app's half:
//!
//! - [`serve`]: answers `mcext://` on CEF's IO thread: files with
//!   web-accessible enforcement and the manifest's CSP, the shim, the
//!   bridge (calls, events, fetch) which it hands to the main thread.
//! - [`instance`]: one running extension in one browser profile: its
//!   contexts (background worker, pages, content scripts), message routing
//!   and ports, its hidden background host (restarted when it crashes),
//!   offscreen document, popup, alarms and action state.
//! - [`dispatch`]: the `chrome.*` calls, answered over the app's model.
//! - [`pages`]: content scripts in every page (isolated and main world, in
//!   cross-process frames too, over CDP), and the page events behind
//!   `webNavigation` and `webRequest`.
//! - [`tabs`]: the app's windows and browser pages as Chrome's tabs.
//! - [`toolbar`]: the action buttons and their popups.
//!
//! Scope (K15, K22): extensions are installed once for the app; each
//! browser profile that shows a page runs its own instance, with its own
//! storage and sign-ins (the shared profile, and each isolated project
//! profile, like Chrome profiles). Throwaway agent profiles run none.
//!
//! Main thread only, except [`serve`].

mod dispatch;
mod instance;
mod pages;
mod serve;
mod tabs;
pub(crate) mod toolbar;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{App, AsyncApp, Global};
use monocode_core::browser::extensions::{
    self as core, manifest::Manifest, registry::Registry, webstore,
};

pub(crate) use instance::InstanceKey;
pub(in crate::browser) use pages::attach;
pub(crate) use tabs::{PageTab, WindowTab};

/// Updates are checked this long after start, then every [`UPDATE_EVERY`].
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(60);
const UPDATE_EVERY: Duration = Duration::from_secs(5 * 60 * 60);

/// The profile a page browses with, as the layer keys it: `None` the
/// shared one, an isolated project profile's id otherwise.
pub(crate) type ProfileKey = Option<String>;

pub(crate) fn profile_key(profile: &monocode_cef::Profile) -> Option<ProfileKey> {
    match profile {
        monocode_cef::Profile::Default => Some(None),
        monocode_cef::Profile::Isolated(id) => Some(Some(id.clone())),
        monocode_cef::Profile::Ephemeral(_) => None,
    }
}

pub(crate) fn profile_of(key: &ProfileKey) -> monocode_cef::Profile {
    match key {
        None => monocode_cef::Profile::Default,
        Some(id) => monocode_cef::Profile::Isolated(id.clone()),
    }
}

/// A profile's folder name for extension data.
pub(crate) fn profile_folder(key: &ProfileKey) -> String {
    key.clone().unwrap_or_else(|| "default".into())
}

/// An installed extension as the app runs it.
pub(crate) struct Ext {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Arc<Manifest>,
    /// What the user granted (manifest permissions at install or the last
    /// approved update).
    pub permissions: Vec<String>,
    pub hosts: core::pattern::PatternSet,
}

impl Ext {
    pub fn has(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

/// What Settings shows of an extension.
#[derive(Clone, Debug)]
pub(crate) struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub enabled: bool,
    pub warnings: Vec<String>,
    pub unsupported: Vec<String>,
    pub held: Option<core::registry::Held>,
    pub icon: Option<PathBuf>,
}

/// What an install, update or removal is doing, for Settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Work {
    #[default]
    Idle,
    Busy(String),
    Done(String),
    Failed(String),
}

pub(crate) struct Runtime {
    pub app: AsyncApp,
    pub registry: Arc<Mutex<Registry>>,
    /// Enabled extensions, by id.
    pub exts: HashMap<String, Arc<Ext>>,
    pub instances: HashMap<InstanceKey, instance::Instance>,
    pub tabs: tabs::Tracker,
    pub pages: pages::Pages,
    pub work: Work,
    /// Bumped on every change Settings or the toolbar shows.
    pub revision: u64,
    /// The UI locale extensions see.
    pub locale: String,
}

thread_local! {
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

/// Runs `f` on the runtime; `None` before [`init`] (or while it is busy:
/// callers never nest).
pub(crate) fn with<R>(f: impl FnOnce(&mut Runtime) -> R) -> Option<R> {
    RUNTIME.with(|r| {
        let mut slot = r.try_borrow_mut().ok()?;
        slot.as_mut().map(f)
    })
}

pub(crate) fn spawn(fut: impl std::future::Future<Output = ()> + 'static) {
    if let Some(app) = with(|rt| rt.app.clone()) {
        app.foreground_executor().spawn(fut).detach();
    }
}

pub(crate) async fn sleep(d: Duration) {
    if let Some(app) = with(|rt| rt.app.clone()) {
        app.background_executor().timer(d).await;
    }
}

/// Repaints the windows (the toolbar, Settings).
pub(crate) fn changed() {
    let Some(app) = with(|rt| {
        rt.revision += 1;
        rt.app.clone()
    }) else {
        return;
    };
    let executor = app.foreground_executor().clone();
    executor
        .spawn(async move {
            app.update(|cx| cx.refresh_windows());
        })
        .detach();
}

struct Started;
impl Global for Started {}

/// Once, with the data mode's folder (`None`: no folder, no extensions).
pub(crate) fn init(data_dir: Option<PathBuf>, cx: &mut App) {
    if cx.has_global::<Started>() {
        return;
    }
    cx.set_global(Started);
    let Some(data_dir) = data_dir else { return };
    let registry = Registry::open(&data_dir);
    let locale = sys_locale();
    RUNTIME.with(|r| {
        *r.borrow_mut() = Some(Runtime {
            app: cx.to_async(),
            registry: Arc::new(Mutex::new(registry)),
            exts: HashMap::new(),
            instances: HashMap::new(),
            tabs: tabs::Tracker::default(),
            pages: pages::Pages::default(),
            work: Work::Idle,
            revision: 0,
            locale,
        })
    });
    reload_exts();
    serve::start(cx);
    // Updates: a minute after start, then every five hours (Orca's rhythm).
    let app = cx.to_async();
    cx.foreground_executor()
        .spawn(async move {
            app.background_executor().timer(FIRST_UPDATE_CHECK).await;
            loop {
                check_updates(false);
                app.background_executor().timer(UPDATE_EVERY).await;
            }
        })
        .detach();
    tabs::start_watching();
    dev::start();
}

fn sys_locale() -> String {
    std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(|s| s.replace('_', "-")))
        .filter(|l| !l.is_empty() && l != "C")
        .unwrap_or_else(|| "en-US".into())
}

/// Reads the enabled extensions from the registry again, and restarts
/// the instances whose version or state changed.
pub(crate) fn reload_exts() {
    let Some((registry, locale)) = with(|rt| (rt.registry.clone(), rt.locale.clone())) else {
        return;
    };
    let enabled = registry.lock().unwrap().enabled();
    let mut exts = HashMap::new();
    for (installed, dir) in enabled {
        match Manifest::load(&dir, &locale) {
            Ok(manifest) => {
                let hosts = core::pattern::PatternSet::parse(
                    installed.granted_hosts.iter().map(String::as_str),
                );
                exts.insert(
                    installed.id.clone(),
                    Arc::new(Ext {
                        id: installed.id.clone(),
                        dir,
                        manifest: Arc::new(manifest),
                        permissions: installed.granted_permissions.clone(),
                        hosts,
                    }),
                );
            }
            Err(e) => log::warn!("browser: extension {} does not load: {e}", installed.id),
        }
    }
    serve::publish(&exts);
    // Instances of extensions that went or changed stop; pages get the
    // current set at their next document.
    let stale: Vec<InstanceKey> = with(|rt| {
        let stale = (rt.instances.iter())
            .filter(|(key, i)| {
                exts.get(&key.id)
                    .is_none_or(|e| e.manifest.version != i.ext.manifest.version)
            })
            .map(|(key, _)| key.clone())
            .collect();
        rt.exts = exts;
        stale
    })
    .unwrap_or_default();
    for key in stale {
        instance::stop(&key);
    }
    pages::refresh_all();
    changed();
}

/// Whether an enabled password manager extension runs (K60).
pub(crate) fn password_manager_enabled() -> bool {
    with(|rt| (rt.exts.keys()).any(|id| core::PASSWORD_MANAGERS.contains(&id.as_str())))
        .unwrap_or(false)
}

// ---- Settings: what is installed, install, update, remove ----

pub(crate) fn listings() -> Vec<Listing> {
    let Some((registry, locale)) = with(|rt| (rt.registry.clone(), rt.locale.clone())) else {
        return Vec::new();
    };
    let registry = registry.lock().unwrap();
    (registry.list().iter())
        .map(|e| {
            let dir = registry.root().join(&e.id).join(&e.version);
            let manifest = Manifest::load(&dir, &locale).ok();
            Listing {
                id: e.id.clone(),
                name: manifest
                    .as_ref()
                    .map_or_else(|| e.id.clone(), |m| m.name.clone()),
                description: manifest
                    .as_ref()
                    .map(|m| m.description.clone())
                    .unwrap_or_default(),
                version: e.version.clone(),
                enabled: e.enabled,
                warnings: core::permissions::warnings(&e.granted_permissions, &e.granted_hosts),
                unsupported: manifest
                    .as_ref()
                    .map(|m| m.unsupported())
                    .unwrap_or_default(),
                held: e.held.clone(),
                icon: manifest
                    .as_ref()
                    .and_then(|m| m.icon(32).map(|p| dir.join(p))),
            }
        })
        .collect()
}

pub(crate) fn work() -> Work {
    with(|rt| rt.work.clone()).unwrap_or_default()
}

fn set_work(work: Work) {
    with(|rt| rt.work = work);
    changed();
}

/// Installs the extension a store URL or id names, from the Web Store.
pub(crate) fn install(input: &str) -> Result<(), String> {
    let id = webstore::parse_store_input(input)
        .ok_or("Paste a Chrome Web Store link or an extension id")?;
    let registry = with(|rt| rt.registry.clone()).ok_or("Extensions are not available")?;
    set_work(Work::Busy(format!("Downloading {id}…")));
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("extension-install".into())
        .spawn(move || {
            let result = webstore::download(&webstore::download_url(&id), None)
                .and_then(|crx| registry.lock().unwrap().install_crx(&crx, Some(&id), true));
            let _ = tx.send_blocking(result);
        })
        .map_err(|e| e.to_string())?;
    spawn(async move {
        let result = rx.recv().await.unwrap_or(Err("the install stopped".into()));
        match result {
            Ok(outcome) => {
                use core::registry::Outcome;
                let text = match outcome {
                    Outcome::Installed(v) => format!("Installed version {v}."),
                    Outcome::Updated { from, to } => format!("Updated from {from} to {to}."),
                    Outcome::Current => "Already installed and up to date.".into(),
                    Outcome::Held(h) => format!(
                        "Version {} waits for your approval: it asks for more ({}).",
                        h.version,
                        h.warnings.join("; ")
                    ),
                };
                set_work(Work::Done(text));
                reload_exts();
            }
            Err(e) => set_work(Work::Failed(e)),
        }
    });
    Ok(())
}

pub(crate) fn set_enabled(id: &str, enabled: bool) {
    let Some(registry) = with(|rt| rt.registry.clone()) else {
        return;
    };
    if let Err(e) = registry.lock().unwrap().set_enabled(id, enabled) {
        set_work(Work::Failed(e));
    }
    reload_exts();
}

pub(crate) fn uninstall(id: &str) {
    let Some(registry) = with(|rt| rt.registry.clone()) else {
        return;
    };
    // Its own origin's data in Chromium's profiles (IndexedDB, caches),
    // through a page of each profile that has one.
    let clients: Vec<std::rc::Rc<monocode_core::browser::CdpClient>> = with(|rt| {
        let mut seen = std::collections::HashSet::new();
        (rt.pages.by_tab.values())
            .filter(|p| seen.insert(p.profile.clone()))
            .filter_map(|p| p.cdp.upgrade())
            .collect()
    })
    .unwrap_or_default();
    for cdp in clients {
        let call = cdp.call(
            "Storage.clearDataForOrigin",
            serde_json::json!({"origin": core::origin(id), "storageTypes": "all"}),
            None,
        );
        spawn(async move {
            let _ = call.await;
        });
    }
    // Its instances stop first: their storage files close before they go.
    with(|rt| rt.exts.remove(id));
    let keys: Vec<InstanceKey> = with(|rt| {
        rt.instances
            .keys()
            .filter(|k| k.id == id)
            .cloned()
            .collect()
    })
    .unwrap_or_default();
    for key in keys {
        instance::stop(&key);
    }
    let id = id.to_owned();
    spawn(async move {
        // The stopped hosts let go of their files.
        sleep(Duration::from_millis(300)).await;
        match registry.lock().unwrap().uninstall(&id) {
            Ok(()) => set_work(Work::Done("Removed, with its data.".into())),
            Err(e) => set_work(Work::Failed(e)),
        }
        reload_exts();
    });
}

pub(crate) fn approve_update(id: &str) {
    let Some(registry) = with(|rt| rt.registry.clone()) else {
        return;
    };
    let result = registry.lock().unwrap().approve_held(id);
    match result {
        Ok(Some(v)) => set_work(Work::Done(format!("Updated to {v}."))),
        Ok(None) => {}
        Err(e) => set_work(Work::Failed(e)),
    }
    reload_exts();
}

/// Asks the Web Store for updates of every installed extension; an
/// update that adds warnings is held for the user (Settings).
pub(crate) fn check_updates(by_user: bool) {
    let Some(registry) = with(|rt| rt.registry.clone()) else {
        return;
    };
    let installed: Vec<(String, String)> = (registry.lock().unwrap().list().iter())
        .filter(|e| e.from_store)
        .map(|e| (e.id.clone(), e.version.clone()))
        .collect();
    if installed.is_empty() {
        if by_user {
            set_work(Work::Done("Nothing to update.".into()));
        }
        return;
    }
    if by_user {
        set_work(Work::Busy("Checking for updates…".into()));
    }
    let (tx, rx) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("extension-updates".into())
        .spawn(move || {
            let result = webstore::check_updates(&installed).map(|updates| {
                let mut notes = Vec::new();
                for update in updates {
                    let applied = webstore::download(&update.url, update.sha256.as_deref())
                        .and_then(|crx| {
                            registry
                                .lock()
                                .unwrap()
                                .install_crx(&crx, Some(&update.id), true)
                        });
                    notes.push(match applied {
                        Ok(outcome) => format!("{}: {outcome:?}", update.id),
                        Err(e) => format!("{}: {e}", update.id),
                    });
                }
                notes
            });
            let _ = tx.send_blocking(result);
        });
    spawn(async move {
        match rx.recv().await.unwrap_or(Err("the check stopped".into())) {
            Ok(notes) => {
                log::warn!(
                    "browser: extension update check: {} update(s) {notes:?}",
                    notes.len()
                );
                if by_user {
                    set_work(Work::Done(if notes.is_empty() {
                        "Every extension is up to date.".into()
                    } else {
                        notes.join("; ")
                    }));
                }
                if !notes.is_empty() {
                    reload_exts();
                }
            }
            Err(e) => {
                log::warn!("browser: extension update check failed: {e}");
                if by_user {
                    set_work(Work::Failed(format!("The update check failed: {e}")));
                }
            }
        }
    });
}

/// Development hooks for driving the layer from scripts (never in a
/// release's normal use): with `MONOCODE_BROWSER_EXT_DEV=<dir>` the layer
/// writes `<dir>/state.json` (instances, contexts, tabs, action state) and
/// runs commands written to `<dir>/cmd` (one JSON object per line; results
/// in `<dir>/out.jsonl`): `{"install": "<id or url>"}`, `{"enable": id,
/// "on": bool}`, `{"uninstall": id}`, `{"updates": true}`, `{"popup": id}`,
/// `{"eval": expr, "id": id, "in": "background"|"popup"}`,
/// `{"crash": id}` (kills the background's renderer).
pub(crate) mod dev {
    use std::path::PathBuf;

    pub(crate) fn dir() -> Option<PathBuf> {
        std::env::var_os("MONOCODE_BROWSER_EXT_DEV").map(PathBuf::from)
    }

    pub(super) fn start() {
        let Some(dir) = dir() else { return };
        let _ = std::fs::create_dir_all(&dir);
        super::spawn(async move {
            loop {
                super::sleep(std::time::Duration::from_millis(300)).await;
                if let Ok(text) = std::fs::read_to_string(dir.join("cmd")) {
                    let _ = std::fs::remove_file(dir.join("cmd"));
                    for line in text.lines().filter(|l| !l.trim().is_empty()) {
                        match serde_json::from_str::<serde_json::Value>(line) {
                            Ok(cmd) => super::dispatch::dev_command(cmd, dir.clone()),
                            Err(e) => super::dispatch::dev_out(
                                &dir,
                                serde_json::json!({"error": e.to_string()}),
                            ),
                        }
                    }
                }
                if let Some(state) = super::with(super::instance::dev_state) {
                    let _ = std::fs::write(
                        dir.join("state.json"),
                        serde_json::to_vec_pretty(&state).unwrap_or_default(),
                    );
                }
            }
        });
    }
}
