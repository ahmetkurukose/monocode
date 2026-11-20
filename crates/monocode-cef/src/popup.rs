//! New windows a page asks for (docs/native/browser/context.md K14, §3.1/8).
//!
//! Every page is its own tab in the host, so no popup gets a window:
//! - A link with `target=_blank` or a plain `window.open` (`OnBeforePopup`),
//!   and a middle or ⌘-click (`OnOpenURLFromTab`), is cancelled and reported
//!   ([`BrowserEvent::PopupRequested`]); the host opens the address as a new
//!   page.
//! - A popup that asks for a size (`window.open(url, name, "width=…")`), as
//!   sign-in flows do, needs `window.opener` and `postMessage` to reach the
//!   page that opened it, which a reopened address would lose. CEF creates
//!   it for real, as a child of a parent view made here on the spot (the
//!   host's tab for it does not exist yet). Once created it waits in
//!   [`take_popup`] for the host to move it into a new tab
//!   ([`BrowserEvent::PopupOpened`]).
//! - Script popups without a user gesture are limited per page
//!   ([`PopupLimiter`]); the rest are dropped.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use cef::*;

use crate::view::{BrowserEvent, BrowserView, PageState};

/// At most this many gestureless popups per [`WINDOW`], per page.
const LIMIT: usize = 4;
const WINDOW: Duration = Duration::from_secs(2);

/// Counts a page's gestureless popups in a sliding window.
#[derive(Debug, Default)]
pub(crate) struct PopupLimiter {
    recent: VecDeque<Instant>,
}

impl PopupLimiter {
    /// Whether one more popup may open at `now`; counts it if so.
    pub(crate) fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= LIMIT {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

/// How a page asked for the new window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opened {
    /// A link or a plain `window.open`, to show at once.
    ForegroundTab,
    /// A middle or ⌘-click: open it without switching to it.
    BackgroundTab,
    /// `window.open` with a size or position: a popup proper.
    Popup,
    /// Anything else (⇧-click, a new window).
    Other,
}

impl Opened {
    fn from_disposition(disposition: WindowOpenDisposition) -> Self {
        if disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB {
            Opened::BackgroundTab
        } else if disposition == WindowOpenDisposition::NEW_FOREGROUND_TAB {
            Opened::ForegroundTab
        } else if disposition == WindowOpenDisposition::NEW_POPUP {
            Opened::Popup
        } else {
            Opened::Other
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Over the limit: nothing opens.
    Drop,
    /// Cancel, and let the host open the address in a new tab.
    Reopen { foreground: bool },
    /// Let CEF create it, keeping its opener, for the host to adopt.
    Adopt { foreground: bool },
}

/// What to do with a popup. The new tab takes focus only when the user
/// asked for it; a script's popup opens behind. `allow` asks the page's
/// limiter, only for gestureless popups.
pub(crate) fn plan(
    opened: Opened,
    user_gesture: bool,
    can_adopt: bool,
    allow: impl FnOnce() -> bool,
) -> Plan {
    if !user_gesture && !allow() {
        return Plan::Drop;
    }
    let foreground = user_gesture && opened != Opened::BackgroundTab;
    if opened == Opened::Popup && can_adopt {
        Plan::Adopt { foreground }
    } else {
        Plan::Reopen { foreground }
    }
}

/// A popup CEF is creating, until its browser exists.
struct Pending {
    /// The opener, which hears that the popup is ready.
    opener: PageState,
    events: async_channel::Receiver<BrowserEvent>,
    url: String,
    foreground: bool,
    #[cfg(target_os = "macos")]
    parent: objc2::rc::Retained<objc2_app_kit::NSView>,
}

/// A popup's id in [`PENDING`]: the opener's browser and CEF's popup id.
pub(crate) type PopupKey = (i32, i32);

thread_local! {
    static PENDING: RefCell<HashMap<PopupKey, Pending>> = RefCell::new(HashMap::new());
    /// Created popups the host has not adopted yet, by [`BrowserEvent::PopupOpened`] id.
    static READY: RefCell<HashMap<u64, (BrowserView, async_channel::Receiver<BrowserEvent>)>> =
        RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// The popup announced by [`BrowserEvent::PopupOpened`], once. Its view sits
/// in a hidden parent of its own; [`BrowserView::reparent`] moves it into
/// the host's tab. Dropping it closes the popup.
pub fn take_popup(id: u64) -> Option<(BrowserView, async_channel::Receiver<BrowserEvent>)> {
    READY.with(|r| r.borrow_mut().remove(&id))
}

fn string(value: Option<&CefString>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

/// `OnBeforePopup` for the page whose state is `state`. Returns CEF's
/// answer: 1 cancels the popup, 0 lets CEF create it with `window_info`
/// and `client` as set here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn before_popup(
    state: &PageState,
    browser: Option<&mut Browser>,
    popup_id: i32,
    target_url: Option<&CefString>,
    disposition: WindowOpenDisposition,
    user_gesture: bool,
    features: Option<&PopupFeatures>,
    window_info: Option<&mut WindowInfo>,
    client: Option<&mut Option<Client>>,
) -> std::os::raw::c_int {
    let url = string(target_url);
    let opened = Opened::from_disposition(disposition);
    let can_adopt = cfg!(target_os = "macos") && window_info.is_some() && client.is_some();
    let plan = plan(opened, user_gesture, can_adopt, || {
        state
            .popups
            .lock()
            .is_ok_and(|mut limiter| limiter.allow(Instant::now()))
    });
    match plan {
        Plan::Drop => 1,
        Plan::Reopen { foreground } => {
            state.send(BrowserEvent::PopupRequested {
                url,
                user_gesture,
                foreground,
            });
            1
        }
        Plan::Adopt { foreground } => {
            let opener = browser.map(|b| b.identifier()).unwrap_or_default();
            let (Some(window_info), Some(client)) = (window_info, client) else {
                return 1;
            };
            match adopt(state, (opener, popup_id), url.clone(), foreground, features) {
                Some((info, popup_client)) => {
                    *window_info = info;
                    *client = Some(popup_client);
                    0
                }
                None => {
                    state.send(BrowserEvent::PopupRequested {
                        url,
                        user_gesture,
                        foreground,
                    });
                    1
                }
            }
        }
    }
}

/// `OnOpenURLFromTab`: a middle, ⌘ or ⇧ click on a link, which Chromium
/// would otherwise follow in the same page. 1 cancels that navigation.
pub(crate) fn open_url_from_tab(
    state: &PageState,
    target_url: Option<&CefString>,
    disposition: WindowOpenDisposition,
    user_gesture: bool,
) -> std::os::raw::c_int {
    let opened = Opened::from_disposition(disposition);
    if disposition == WindowOpenDisposition::CURRENT_TAB {
        return 0;
    }
    // Never adopted: there is no popup, only an address.
    match plan(opened, user_gesture, false, || {
        state
            .popups
            .lock()
            .is_ok_and(|mut limiter| limiter.allow(Instant::now()))
    }) {
        Plan::Drop => 1,
        Plan::Reopen { foreground } | Plan::Adopt { foreground } => {
            state.send(BrowserEvent::PopupRequested {
                url: string(target_url),
                user_gesture,
                foreground,
            });
            1
        }
    }
}

#[cfg(target_os = "macos")]
fn adopt(
    opener: &PageState,
    key: PopupKey,
    url: String,
    foreground: bool,
    features: Option<&PopupFeatures>,
) -> Option<(WindowInfo, Client)> {
    let size = |set: i32, value: i32, default: i32| {
        if set != 0 && value > 0 {
            value
        } else {
            default
        }
    };
    let (width, height) = features.map_or((800, 600), |f| {
        (
            size(f.width_set, f.width, 800),
            size(f.height_set, f.height, 600),
        )
    });
    let parent = crate::mac::view::hidden_parent(f64::from(width), f64::from(height))?;
    let (state, events) = PageState::new(Some(key));
    let window_info = WindowInfo {
        runtime_style: RuntimeStyle::ALLOY,
        ..Default::default()
    }
    .set_as_child(
        objc2::rc::Retained::as_ptr(&parent) as _,
        &Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
    );
    let client = crate::view::page_client(state);
    PENDING.with(|p| {
        p.borrow_mut().insert(
            key,
            Pending {
                opener: opener.clone(),
                events,
                url,
                foreground,
                parent,
            },
        )
    });
    Some((window_info, client))
}

#[cfg(not(target_os = "macos"))]
fn adopt(
    _: &PageState,
    _: PopupKey,
    _: String,
    _: bool,
    _: Option<&PopupFeatures>,
) -> Option<(WindowInfo, Client)> {
    None
}

/// `OnAfterCreated` of a popup's own client: it is ready for a tab.
pub(crate) fn created(key: PopupKey, browser: Browser, state: &PageState) {
    let Some(pending) = PENDING.with(|p| p.borrow_mut().remove(&key)) else {
        return;
    };
    let id = NEXT_ID.with(|n| n.replace(n.get() + 1));
    let view = BrowserView::from_browser(browser, state);
    // A superview does not outlive its last reference; the view keeps its
    // first one until the host moves it into a tab.
    #[cfg(target_os = "macos")]
    view.hold(pending.parent);
    READY.with(|r| r.borrow_mut().insert(id, (view, pending.events)));
    pending.opener.send(BrowserEvent::PopupOpened {
        id,
        url: pending.url,
        foreground: pending.foreground,
    });
}

/// `OnBeforePopupAborted`: CEF gave up creating it.
pub(crate) fn aborted(key: PopupKey) {
    PENDING.with(|p| p.borrow_mut().remove(&key));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gestureless_popups_are_limited_to_four_per_two_seconds() {
        let mut limiter = PopupLimiter::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert!((0..4).all(|i| limiter.allow(at(i * 100))));
        assert!(!limiter.allow(at(500)));
        assert!(!limiter.allow(at(1999)));
        // The first one left the window; one more fits.
        assert!(limiter.allow(at(2000)));
        assert!(!limiter.allow(at(2050)));
        assert!(limiter.allow(at(4500)));
    }

    #[test]
    fn a_popup_takes_focus_only_when_the_user_asked_for_it() {
        let yes = || true;
        assert_eq!(
            plan(Opened::ForegroundTab, true, true, yes),
            Plan::Reopen { foreground: true }
        );
        assert_eq!(
            plan(Opened::BackgroundTab, true, true, yes),
            Plan::Reopen { foreground: false }
        );
        assert_eq!(
            plan(Opened::ForegroundTab, false, true, yes),
            Plan::Reopen { foreground: false }
        );
        assert_eq!(
            plan(Opened::Popup, true, true, yes),
            Plan::Adopt { foreground: true }
        );
        assert_eq!(
            plan(Opened::Popup, true, false, yes),
            Plan::Reopen { foreground: true }
        );
        assert_eq!(plan(Opened::Popup, false, true, || false), Plan::Drop);
        // A gesture is never counted against the limit.
        assert_eq!(
            plan(Opened::Other, true, true, || panic!("asked the limiter")),
            Plan::Reopen { foreground: true }
        );
    }
}
