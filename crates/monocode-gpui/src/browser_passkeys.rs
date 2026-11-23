//! A page's passkey requests (docs/native/browser/context.md K58).
//!
//! Chromium shows its own WebAuthn dialog; signing in with a phone (the QR
//! code, "hybrid") needs the Mac to hear the phone over Bluetooth. The page
//! tells when a request starts and ends (`monocode_core::browser::passkeys`);
//! when one starts, the app asks macOS whether it may use Bluetooth and
//! whether Bluetooth is on. If not, a bar under the toolbar (never over the
//! page) says so and opens System Settings.
//!
//! Before macOS has asked the user at all, Chromium does not listen for the
//! phone in that request: it asks macOS when the user picks the phone, and
//! even when the user allows it, it listens only from the next request on
//! (its scan starts when Bluetooth's power changes, which it does not). The
//! bar says so then too: allow it, and start signing in again.
//!
//! Agents read every start and end (`passkeyRequest` in action effects and
//! `browser_tabs`) and may cancel what waits (`browser_dialog` dismiss).
use super::*;
use monocode_core::browser::passkeys::{self, PasskeyBlock, PasskeyChange, PasskeyNote};

/// Notes a page keeps for agents.
const NOTES: usize = 20;

/// What macOS says about Bluetooth for signing in with a phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bluetooth {
    /// Allowed and on.
    Ready,
    /// macOS has not asked the user yet.
    NotAsked,
    Blocked(PasskeyBlock),
}

/// What the bar says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bar {
    NotAsked,
    Blocked(PasskeyBlock),
}

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
    bar: Option<Bar>,
}

impl State {
    fn note(&mut self, note: PasskeyNote) {
        self.notes.push_back(note);
        while self.notes.len() > NOTES {
            self.notes.pop_front();
        }
    }

    /// One change the page reported; `bluetooth` tells whether a new
    /// request can reach the phone.
    fn on_change(&mut self, change: PasskeyChange, bluetooth: impl FnOnce() -> Bluetooth) {
        let seq = monocode_core::browser::sites::next_note_seq();
        if change.pending {
            let (reason, bar) = match bluetooth() {
                Bluetooth::Ready => (None, None),
                Bluetooth::NotAsked => (None, Some(Bar::NotAsked)),
                Bluetooth::Blocked(block) => (Some(block), Some(Bar::Blocked(block))),
            };
            let note = PasskeyNote {
                seq,
                kind: change.kind,
                origin: change.origin,
                pending: true,
                reason,
            };
            self.pending.push(note.clone());
            self.note(note);
            self.bar = bar;
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
        self.state.borrow_mut().on_change(change, bluetooth::state);
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
        let bar = self.host.shared.passkeys.state.borrow().bar?;
        let privacy = "x-apple.systempreferences:com.apple.preference.security?Privacy_Bluetooth";
        let (text, settings) = match bar {
            Bar::NotAsked => (
                "To sign in with your phone, Monocode needs Bluetooth. If macOS asks, allow it, then cancel and start signing in again: this attempt cannot reach your phone.",
                privacy,
            ),
            Bar::Blocked(PasskeyBlock::BluetoothDenied) => (
                "To sign in with your phone, Monocode needs Bluetooth. Allow it in System Settings → Privacy & Security → Bluetooth, then try again (macOS may ask you to reopen Monocode).",
                privacy,
            ),
            Bar::Blocked(PasskeyBlock::BluetoothOff) => (
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
    use super::Bluetooth;

    /// Whether a phone can reach the Mac now. Never asks the user.
    #[cfg(target_os = "macos")]
    pub(super) fn state() -> Bluetooth {
        use super::PasskeyBlock;
        use objc2::{
            msg_send,
            runtime::{AnyClass, AnyObject},
        };
        load(c"/System/Library/Frameworks/CoreBluetooth.framework/CoreBluetooth");
        let Some(manager) = AnyClass::get(c"CBManager") else {
            return Bluetooth::Ready;
        };
        // +[CBManager authorization] (CBManagerAuthorization, an NSInteger):
        // 0 not determined, 1 restricted, 2 denied, 3 allowed.
        let authorization: isize = unsafe { msg_send![manager, authorization] };
        match authorization {
            0 => return Bluetooth::NotAsked,
            1 | 2 => return Bluetooth::Blocked(PasskeyBlock::BluetoothDenied),
            _ => {}
        }
        load(c"/System/Library/Frameworks/IOBluetooth.framework/IOBluetooth");
        let Some(controller) = AnyClass::get(c"IOBluetoothHostController") else {
            return Bluetooth::Ready;
        };
        let default: *mut AnyObject = unsafe { msg_send![controller, defaultController] };
        // SAFETY: null or the shared controller, which lives for the process.
        let Some(default) = (unsafe { default.as_ref() }) else {
            return Bluetooth::Ready;
        };
        // -[IOBluetoothHostController powerState] (BluetoothHCIPowerState, a
        // C enum, encoded `i`): 0 off, 1 on, 0xFF not initialized.
        let power: i32 = unsafe { msg_send![default, powerState] };
        if power == 0 {
            Bluetooth::Blocked(PasskeyBlock::BluetoothOff)
        } else {
            Bluetooth::Ready
        }
    }

    /// Loads a system framework, so its classes exist.
    #[cfg(target_os = "macos")]
    fn load(path: &std::ffi::CStr) {
        // SAFETY: a valid C string; the framework stays loaded on purpose.
        unsafe {
            libc::dlopen(path.as_ptr(), libc::RTLD_LAZY);
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn state() -> Bluetooth {
        Bluetooth::Ready
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
            Bluetooth::Blocked(PasskeyBlock::BluetoothDenied)
        });
        assert_eq!(state.bar, Some(Bar::Blocked(PasskeyBlock::BluetoothDenied)));
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
    fn before_macos_asked_the_bar_says_to_try_again_but_agents_get_no_reason() {
        let mut state = State::default();
        state.on_change(change(PasskeyKind::Get, true), || Bluetooth::NotAsked);
        assert_eq!(state.bar, Some(Bar::NotAsked));
        assert_eq!(state.notes.back().unwrap().reason, None);
    }

    #[test]
    fn a_request_bluetooth_allows_has_no_bar_and_navigation_ends_it() {
        let mut state = State::default();
        state.on_change(change(PasskeyKind::Create, true), || Bluetooth::Ready);
        assert_eq!(state.bar, None);
        assert_eq!(state.pending.len(), 1);
        // An end of a request never heard changes nothing.
        state.on_change(change(PasskeyKind::Get, false), || Bluetooth::Ready);
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
            state.on_change(change(PasskeyKind::Get, true), || Bluetooth::Ready);
            state.on_change(change(PasskeyKind::Get, false), || Bluetooth::Ready);
        }
        assert_eq!(state.notes.len(), NOTES);
    }
}
