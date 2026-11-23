//! The app's password vaults (docs/native/browser/context.md K55, K56,
//! K60): one per browser profile in `<data>/Browser Passwords`, beside
//! Chromium's own `Browser` folder; memory when the data mode has no folder.
//! The vault key comes from the Keychain once per run, off the main thread
//! (the system may ask the user): when Chromium starts if passwords are
//! saved already, else when the first one is saved. Refused, passwords are
//! unavailable until the user tries again; nothing is lost or rewritten.
//!
//! Pages (`browser_passwords.rs`) offer to save sign-ins and fill saved
//! ones; Settings → Passwords lists them; agents fill them without seeing
//! them (K57). [`password_manager_enabled`] says whether any of it runs.
use super::*;
use monocode_core::browser::passwords::{Vaults, keychain};

/// What the vaults are now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VaultState {
    /// Not asked for the key yet this run.
    Locked,
    /// Waiting on the Keychain (maybe on the user).
    Unlocking,
    Open,
    /// The key was refused or lost: why.
    Unavailable(String),
}

pub(crate) struct Passwords {
    dir: Option<PathBuf>,
    state: VaultState,
    vaults: Option<Rc<RefCell<Vaults>>>,
    /// The last successful Touch ID check: reveals and copies within
    /// [`REAUTH_FOR`] ask no more.
    authenticated: Option<Instant>,
    /// Bumped on every change, so views can tell.
    pub(crate) revision: u64,
}

struct PasswordsGlobal(Rc<RefCell<Passwords>>);
impl Global for PasswordsGlobal {}

/// A Touch ID check holds this long (Orca's 5 minutes).
pub(crate) const REAUTH_FOR: Duration = Duration::from_secs(5 * 60);

/// Where the vaults live, once (the first shell to make a page says).
pub(crate) fn init(dir: Option<PathBuf>, cx: &mut App) {
    if !cx.has_global::<PasswordsGlobal>() {
        cx.set_global(PasswordsGlobal(Rc::new(RefCell::new(Passwords {
            dir,
            state: VaultState::Locked,
            vaults: None,
            authenticated: None,
            revision: 0,
        }))));
    }
}

fn global(cx: &mut App) -> Rc<RefCell<Passwords>> {
    init(None, cx);
    cx.global::<PasswordsGlobal>().0.clone()
}

/// Whether the browser's own password manager runs (K60): the user's
/// "Offer to save passwords" setting. Phase 5 turns it off while the
/// 1Password extension is installed (the setting turns it back on).
pub(crate) fn password_manager_enabled(cx: &App) -> bool {
    let offered = cx
        .try_global::<crate::settings::Preferences>()
        .is_none_or(|p| p.browser_offer_passwords);
    offered && !password_extension_installed()
}

/// A password manager extension (1Password) runs in the browser: ours
/// steps aside (K60). Extensions come in phase 5.
fn password_extension_installed() -> bool {
    false
}

/// The vault a profile's pages use; `None` for a throwaway one (K22).
pub(crate) fn store_name(profile: &monocode_cef::Profile) -> Option<String> {
    history::store_name(profile)
}

pub(crate) fn state(cx: &mut App) -> VaultState {
    global(cx).borrow().state.clone()
}

/// Whether any password was ever saved in this data mode.
pub(crate) fn saved_any(cx: &mut App) -> bool {
    global(cx)
        .borrow()
        .dir
        .as_deref()
        .is_some_and(Vaults::exist_in)
}

/// The open vaults; `None` while locked (it does not unlock them).
pub(crate) fn vaults(cx: &mut App) -> Option<Rc<RefCell<Vaults>>> {
    global(cx).borrow().vaults.clone()
}

/// Something changed: views showing passwords repaint.
pub(crate) fn changed(cx: &mut App) {
    global(cx).borrow_mut().revision += 1;
    cx.refresh_windows();
}

/// Asks for the key when passwords are saved already, so pages can offer
/// and fill them (Chromium started).
pub(crate) fn warm(cx: &mut App) {
    if saved_any(cx) && state(cx) == VaultState::Locked {
        unlock(cx).detach();
    }
}

/// Waits until the vaults are no longer unlocking.
fn settled(passwords: Rc<RefCell<Passwords>>, cx: &mut App) -> gpui::Task<VaultState> {
    cx.spawn(async move |cx| {
        loop {
            let state = passwords.borrow().state.clone();
            if state != VaultState::Unlocking {
                return state;
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
    })
}

/// Asks the Keychain for the key, off the main thread; again after it was
/// refused. The returned task resolves when the state is settled either
/// way; dropping it does not stop the unlocking.
pub(crate) fn unlock(cx: &mut App) -> gpui::Task<VaultState> {
    let passwords = global(cx);
    let (dir, now) = {
        let p = passwords.borrow();
        (p.dir.clone(), p.state.clone())
    };
    match now {
        VaultState::Open => return gpui::Task::ready(VaultState::Open),
        // Someone asked already: wait for their answer.
        VaultState::Unlocking => return settled(passwords, cx),
        VaultState::Locked | VaultState::Unavailable(_) => {}
    }
    passwords.borrow_mut().state = VaultState::Unlocking;
    let exist = dir.as_deref().is_some_and(Vaults::exist_in);
    let source = key_source(dir.as_deref());
    let key = cx
        .background_executor()
        .spawn(async move { keychain::get_or_create(&*source, exist) });
    let done = passwords.clone();
    cx.spawn(async move |cx| {
        let key = key.await;
        cx.update(|cx| {
            {
                let mut p = done.borrow_mut();
                p.state = match key {
                    Ok(key) => {
                        p.vaults = Some(Rc::new(RefCell::new(Vaults::new(p.dir.clone(), key))));
                        VaultState::Open
                    }
                    Err(error) => {
                        log::warn!("browser passwords: {error}");
                        VaultState::Unavailable(unavailable_text(&error))
                    }
                };
                p.revision += 1;
            }
            cx.refresh_windows();
        })
    })
    .detach();
    settled(passwords, cx)
}

/// The Keychain item of the data folder the vaults are in (tests: memory).
fn key_source(dir: Option<&Path>) -> Box<dyn keychain::KeySource> {
    if cfg!(test) {
        return Box::new(keychain::MemoryKey::default());
    }
    keychain::default_source(dir.and_then(Path::parent))
}

fn unavailable_text(error: &keychain::KeyError) -> String {
    match error {
        keychain::KeyError::Lost => "The key of your saved passwords is no longer in the Keychain, so they cannot be opened.".into(),
        keychain::KeyError::Unavailable(why) => format!(
            "Monocode could not read its key from the Keychain ({why}). If you clicked Deny, try again and allow it."
        ),
    }
}

/// Runs `f` on `profile`'s vault, if open.
pub(crate) fn with_vault<T>(
    profile: &str,
    cx: &mut App,
    f: impl FnOnce(&monocode_core::browser::passwords::Vault) -> Result<T, String>,
) -> Option<Result<T, String>> {
    let vaults = vaults(cx)?;
    let mut vaults = vaults.borrow_mut();
    Some(vaults.vault(profile).and_then(f))
}

/// Opens the vaults with a key in memory (tests).
#[cfg(test)]
pub(crate) fn open_for_test(cx: &mut App) {
    let passwords = global(cx);
    let mut p = passwords.borrow_mut();
    let key = monocode_core::browser::passwords::VaultKey::generate().unwrap();
    p.vaults = Some(Rc::new(RefCell::new(Vaults::new(None, key))));
    p.state = VaultState::Open;
}

/// Whether a Touch ID check passed in the last [`REAUTH_FOR`].
pub(crate) fn recently_authenticated(cx: &mut App) -> bool {
    (global(cx).borrow().authenticated).is_some_and(|at| at.elapsed() < REAUTH_FOR)
}

/// Asks the user to prove it is them (Touch ID, or the login password)
/// before a password shows or is copied; true once they did. Passes
/// without asking within [`REAUTH_FOR`] of the last check, and where the
/// system has no such check.
pub(crate) fn authenticate(reason: &str, cx: &mut App) -> gpui::Task<bool> {
    if recently_authenticated(cx) {
        return gpui::Task::ready(true);
    }
    let passwords = global(cx);
    let answer = device_owner::ask(reason);
    cx.spawn(async move |_| {
        let ok = answer.recv().await.unwrap_or(false);
        if ok {
            passwords.borrow_mut().authenticated = Some(Instant::now());
        }
        ok
    })
}

mod device_owner {
    /// LocalAuthentication's `deviceOwnerAuthentication`: Touch ID, a
    /// watch, or the account password.
    #[cfg(target_os = "macos")]
    pub(super) fn ask(reason: &str) -> async_channel::Receiver<bool> {
        use objc2::{msg_send, rc::Retained, runtime::AnyClass, runtime::AnyObject};
        use objc2_foundation::NSString;

        #[link(name = "LocalAuthentication", kind = "framework")]
        unsafe extern "C" {}

        let (tx, rx) = async_channel::bounded::<bool>(1);
        if cfg!(test) {
            let _ = tx.try_send(true);
            return rx;
        }
        let Some(class) = AnyClass::get(c"LAContext") else {
            let _ = tx.try_send(true);
            return rx;
        };
        let context: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
        let Some(context) = context else {
            let _ = tx.try_send(true);
            return rx;
        };
        let reason = NSString::from_str(reason);
        // The context lives until its answer.
        let kept = std::sync::Mutex::new(Some(context.clone()));
        let block = block2::RcBlock::new(
            move |ok: objc2::runtime::Bool, _error: *mut AnyObject| {
                let _ = tx.try_send(ok.as_bool());
                drop(kept.lock().ok().and_then(|mut k| k.take()));
            },
        );
        // LAPolicyDeviceOwnerAuthentication = 2.
        let _: () = unsafe {
            msg_send![&*context, evaluatePolicy: 2isize, localizedReason: &*reason, reply: &*block]
        };
        rx
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn ask(_: &str) -> async_channel::Receiver<bool> {
        let (tx, rx) = async_channel::bounded::<bool>(1);
        let _ = tx.try_send(true);
        rx
    }
}
