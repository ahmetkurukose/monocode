//! A page's passkey requests (docs/native/browser/context.md K58).
//!
//! Chromium shows its own WebAuthn dialog; signing in with a phone (the QR
//! code, "hybrid") needs the Mac to hear the phone over Bluetooth. The page
//! tells when a request starts and ends (`monocode_core::browser::passkeys`);
//! when one starts, the app asks macOS whether it may use Bluetooth and
//! whether Bluetooth is on. If not, a bar under the toolbar (never over the
//! page) says so and opens System Settings. Before macOS has been asked
//! there is nothing to say: Chromium asks it when the user picks the phone.
//!
//! Agents read every start and end (`passkeyRequest` in action effects and
//! `browser_tabs`) and may cancel what waits (`browser_dialog` dismiss).
use super::*;
use monocode_core::browser::passkeys::{self, PasskeyBlock, PasskeyChange, PasskeyNote};

/// Notes a page keeps for agents.
const NOTES: usize = 20;

/// What a page's hosts share of its passkey requests.
#[derive(Default)]
pub(super) struct Passkeys {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// The requests that wait, oldest first.
    pending: Vec<PasskeyNote>,
    notes: VecDeque<PasskeyNote>,
    /// What the bar says; the user can close it.
    bar: Option<PasskeyBlock>,
}

impl State {
    fn note(&mut self, note: PasskeyNote) {
        self.notes.push_back(note);
        while self.notes.len() > NOTES {
            self.notes.pop_front();
        }
    }

    /// One change the page reported; `block` is what keeps a new request
    /// from the phone.
    fn on_change(&mut self, change: PasskeyChange, block: impl FnOnce() -> Option<PasskeyBlock>) {
        let seq = monocode_core::browser::sites::next_note_seq();
        if change.pending {
            let reason = block();
            let note = PasskeyNote {
                seq,
                kind: change.kind,
                origin: change.origin,
                pending: true,
                reason,
            };
            self.pending.push(note.clone());
            self.note(note);
            self.bar = reason;
            return;
        }
        let ix = self
            .pending
            .iter()
            .position(|p| p.kind == change.kind && p.origin == change.origin);
        let Some(ix) = ix else { return };
        let ended = self.pending.remove(ix);
        self.note(PasskeyNote {
            seq,
            pending: false,
            ..ended
        });
        if self.pending.is_empty() {
            self.bar = None;
        }
    }

    /// The page's documents went (it navigated away, crashed or closed).
    fn clear(&mut self) {
        for ended in std::mem::take(&mut self.pending) {
            let seq = monocode_core::browser::sites::next_note_seq();
            self.note(PasskeyNote {
                seq,
                pending: false,
                ..ended
            });
        }
        self.bar = None;
    }
}

impl Passkeys {
    fn on_change(&self, change: PasskeyChange) {
        self.state
            .borrow_mut()
            .on_change(change, bluetooth::blocks_phone);
    }

    /// The page's DevTools agent went: what waited ended with it.
    pub(super) fn clear(&self) {
        self.state.borrow_mut().clear();
    }
}

impl Host<monocode_cef::BrowserView> {
    /// Watches the page's passkey requests for as long as the returned
    /// value lives.
    pub(super) fn watch_passkeys(&self, cdp: &Rc<CdpClient>) -> passkeys::PasskeyWatch {
        let shared = Rc::downgrade(&self.shared);
        passkeys::watch(cdp, move |change| {
            if let Some(shared) = shared.upgrade() {
                shared.passkeys.on_change(change);
                shared.repaint();
            }
        })
    }
}

impl BrowserPage {
    // ---- For the agent tools ----

    pub(crate) fn passkey_notes(&self) -> Vec<PasskeyNote> {
        let state = self.host.shared.passkeys.state.borrow();
        state.notes.iter().cloned().collect()
    }

    pub(crate) fn pending_passkey(&self) -> Option<PasskeyNote> {
        let state = self.host.shared.passkeys.state.borrow();
        state.pending.last().cloned()
    }

    /// Cancels what the page waits on; resolves with whether it had any.
    pub(crate) fn cancel_passkey(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool>>> {
        let cdp = self.cdp();
        Box::pin(async move {
            match cdp {
                Some(cdp) => passkeys::cancel(&cdp).await.is_ok_and(|n| n > 0),
                None => false,
            }
        })
    }

    // ---- What the page shows ----

    /// The bar under the toolbar while a passkey request cannot reach the
    /// phone. It takes room from the page instead of covering it.
    pub(super) fn passkey_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let block = self.host.shared.passkeys.state.borrow().bar?;
        let (text, settings) = match block {
            PasskeyBlock::BluetoothDenied => (
                "To sign in with your phone, Monocode needs Bluetooth. Allow it in System Settings → Privacy & Security → Bluetooth, then try again (macOS may ask you to reopen Monocode).",
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth",
            ),
            PasskeyBlock::BluetoothOff => (
                "To sign in with your phone, turn on Bluetooth, then try again.",
                "x-apple.systempreferences:com.apple.Bluetooth-Settings.extension",
            ),
        };
        Some(
            sites::bar()
                .id("browser-passkey-bar")
                .child(icon("alert-circle", 14.).text_color(crate::modal::danger(1.)))
                .child(div().flex_1().min_w_0().text_size(px(12.)).child(text))
                .child(
                    modal::ghost_button("browser-passkey-settings", "Open Settings")
                        .h(px(26.))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(settings)),
                )
                .child(
                    icon_button("browser-passkey-close", "Close", "x").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.host.shared.passkeys.state.borrow_mut().bar = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element(),
        )
    }
}

/// What macOS lets the app do with Bluetooth (CoreBluetooth).
mod bluetooth {
    use monocode_core::browser::passkeys::PasskeyBlock;

    /// What keeps a phone from reaching the Mac now: Bluetooth refused or
    /// off. `None` when it is fine, or macOS has not asked the user yet
    /// (Chromium asks when the user picks the phone).
    #[cfg(target_os = "macos")]
    pub(super) fn blocks_phone() -> Option<PasskeyBlock> {
        use objc2::{
            msg_send,
            runtime::{AnyClass, AnyObject},
        };
        load("/System/Library/Frameworks/CoreBluetooth.framework/CoreBluetooth");
        let manager = AnyClass::get(c"CBManager")?;
        // CBManagerAuthorization: 0 not determined, 1 restricted, 2 denied,
        // 3 allowed. Reading it never asks the user.
        let authorization: isize = unsafe { msg_send![manager, authorization] };
        match authorization {
            1 | 2 => return Some(PasskeyBlock::BluetoothDenied),
            3 => {}
            _ => return None,
        }
        load("/System/Library/Frameworks/IOBluetooth.framework/IOBluetooth");
        let controller = AnyClass::get(c"IOBluetoothHostController")?;
        let default: *mut AnyObject = unsafe { msg_send![controller, defaultController] };
        // SAFETY: null or the shared controller, which lives for the process.
        let default = unsafe { default.as_ref() }?;
        // BluetoothHCIPowerState (a C enum, encoded `i`): 0 off, 1 on, 0xFF
        // not initialized.
        let power: i32 = unsafe { msg_send![default, powerState] };
        (power == 0).then_some(PasskeyBlock::BluetoothOff)
    }

    /// Loads a system framework, so its classes exist (once each).
    #[cfg(target_os = "macos")]
    fn load(path: &str) {
        let Ok(path) = std::ffi::CString::new(path) else {
            return;
        };
        // SAFETY: a valid C string; the handle stays loaded on purpose.
        unsafe {
            libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn blocks_phone() -> Option<PasskeyBlock> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::browser::passkeys::PasskeyKind;

    fn change(kind: PasskeyKind, pending: bool) -> PasskeyChange {
        PasskeyChange {
            kind,
            origin: "https://accounts.google.com".into(),
            pending,
        }
    }

    #[test]
    fn a_request_the_phone_cannot_reach_shows_the_bar_until_it_ends() {
        let mut state = State::default();
        state.on_change(change(PasskeyKind::Get, true), || {
            Some(PasskeyBlock::BluetoothDenied)
        });
        assert_eq!(state.bar, Some(PasskeyBlock::BluetoothDenied));
        assert_eq!(state.pending.len(), 1);
        let waiting = state.notes.back().unwrap().clone();
        assert!(waiting.pending);
        assert_eq!(waiting.reason, Some(PasskeyBlock::BluetoothDenied));

        state.on_change(change(PasskeyKind::Get, false), || unreachable!());
        assert_eq!(state.bar, None);
        assert!(state.pending.is_empty());
        let ended = state.notes.back().unwrap();
        assert!(!ended.pending && ended.seq > waiting.seq);
        // The end keeps why it could not reach the phone.
        assert_eq!(ended.reason, Some(PasskeyBlock::BluetoothDenied));
    }

    #[test]
    fn a_request_bluetooth_allows_has_no_bar_and_navigation_ends_it() {
        let mut state = State::default();
        state.on_change(change(PasskeyKind::Create, true), || None);
        assert_eq!(state.bar, None);
        assert_eq!(state.pending.len(), 1);
        // An end of a request never heard changes nothing.
        state.on_change(change(PasskeyKind::Get, false), || None);
        assert_eq!(state.pending.len(), 1);
        state.clear();
        assert!(state.pending.is_empty());
        assert_eq!(state.notes.len(), 2);
        assert!(!state.notes.back().unwrap().pending);
    }

    #[test]
    fn a_page_keeps_its_last_notes_only() {
        let mut state = State::default();
        for _ in 0..NOTES {
            state.on_change(change(PasskeyKind::Get, true), || None);
            state.on_change(change(PasskeyKind::Get, false), || None);
        }
        assert_eq!(state.notes.len(), NOTES);
    }
}
