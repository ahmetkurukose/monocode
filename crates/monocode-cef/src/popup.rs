//! New windows a page asks for (docs/native/browser/context.md K14, §3.1/8).
//!
//! Every page is its own tab in the host, so no popup gets a window:
//! - Whatever reaches `OnBeforePopup` (`window.open`, with a size or not, a
//!   link with `target=_blank`) may need `window.opener` and `postMessage` to
//!   reach the page that opened it, as sign-in flows do; a reopened address
//!   would lose them. CEF creates it for real, as a child of a parent view
//!   made here on the spot (the host's tab for it does not exist yet). Once
//!   created it waits in [`take_popup`] for the host to move it into a new
//!   tab ([`BrowserEvent::PopupOpened`]). Where the page asked for
//!   `noopener`, Chromium creates it without an opener itself; CEF does not
//!   tell which.
//! - A middle or ⌘-click (`OnOpenURLFromTab`) has no opener. It is cancelled
//!   and reported ([`BrowserEvent::PopupRequested`]); the host opens the
//!   address as a new page. So is a popup that may not be scripted or cannot
//!   be adopted here.
//! - A created popup no tab took closes when its opener closes, or after
//!   [`UNADOPTED_FOR`] ([`Unadopted`]).
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
/// A created popup no tab took by then closes.
pub(crate) const UNADOPTED_FOR: Duration = Duration::from_secs(10);

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
    /// A middle or ⌘-click, or a script's window opened from one: open it
    /// without switching to it.
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
/// asked for it; a script's popup opens behind. `can_adopt`: CEF would create
/// it, scriptable, for a tab to adopt (`OnBeforePopup` only). `allow` asks
/// the page's limiter, only for gestureless popups.
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
    if can_adopt {
        Plan::Adopt { foreground }
    } else {
        Plan::Reopen { foreground }
    }
}

/// Created popups waiting for a tab, by [`BrowserEvent::PopupOpened`] id.
/// One whose opener closed, or that waited [`UNADOPTED_FOR`], is discarded.
pub(crate) struct Unadopted<T> {
    waiting: HashMap<u64, Waiting<T>>,
}

struct Waiting<T> {
    item: T,
    /// The opener's browser id.
    opener: i32,
    since: Instant,
    orphaned: bool,
}

impl<T> Default for Unadopted<T> {
    fn default() -> Self {
        Self {
            waiting: HashMap::new(),
        }
    }
}

impl<T> Unadopted<T> {
    /// `orphaned`: its opener closed while CEF created it.
    pub(crate) fn insert(&mut self, id: u64, opener: i32, item: T, orphaned: bool, now: Instant) {
        let waiting = Waiting {
            item,
            opener,
            since: now,
            orphaned,
        };
        self.waiting.insert(id, waiting);
    }

    pub(crate) fn take(&mut self, id: u64) -> Option<T> {
        self.waiting.remove(&id).map(|w| w.item)
    }

    pub(crate) fn opener_closed(&mut self, opener: i32) {
        for waiting in self.waiting.values_mut() {
            if waiting.opener == opener {
                waiting.orphaned = true;
            }
        }
    }

    /// Takes out the ones to close at `now`.
    pub(crate) fn discard(&mut self, now: Instant) -> Vec<T> {
        let gone: Vec<u64> = (self.waiting.iter())
            .filter(|(_, w)| w.orphaned || now.duration_since(w.since) >= UNADOPTED_FOR)
            .map(|(id, _)| *id)
            .collect();
        gone.into_iter().filter_map(|id| self.take(id)).collect()
    }

    pub(crate) fn drain(&mut self) -> Vec<T> {
        self.waiting.drain().map(|(_, w)| w.item).collect()
    }
}

/// A popup CEF is creating, until its browser exists.
struct Pending {
    /// The opener, which hears that the popup is ready.
    opener: PageState,
    events: async_channel::Receiver<BrowserEvent>,
    url: String,
    foreground: bool,
    /// The opener closed first: the popup closes as soon as it exists.
    orphaned: bool,
    #[cfg(target_os = "macos")]
    parent: objc2::rc::Retained<objc2_app_kit::NSView>,
}

/// A popup's id in [`PENDING`]: the opener's browser and CEF's popup id.
pub(crate) type PopupKey = (i32, i32);

type Popup = (BrowserView, async_channel::Receiver<BrowserEvent>);

thread_local! {
    static PENDING: RefCell<HashMap<PopupKey, Pending>> = RefCell::new(HashMap::new());
    /// Created popups the host has not adopted yet.
    static READY: RefCell<Unadopted<Popup>> = RefCell::default();
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// The popup announced by [`BrowserEvent::PopupOpened`], once. Its view sits
/// in a hidden parent of its own; [`BrowserView::reparent`] moves it into
/// the host's tab. Dropping it closes the popup.
pub fn take_popup(id: u64) -> Option<(BrowserView, async_channel::Receiver<BrowserEvent>)> {
    READY.with(|r| r.borrow_mut().take(id))
}

fn string(value: Option<&CefString>) -> String {
    value.map(|s| s.to_string()).unwrap_or_default()
}

/// `OnBeforePopup` for the page whose state is `state`. Returns CEF's
/// answer: 1 cancels the popup, 0 lets CEF create it with `window_info`
/// and `client` as set here. `scriptable`: CEF's `no_javascript_access` is
/// not set.
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
    scriptable: bool,
) -> std::os::raw::c_int {
    let url = string(target_url);
    let opened = Opened::from_disposition(disposition);
    let can_adopt =
        cfg!(target_os = "macos") && scriptable && window_info.is_some() && client.is_some();
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
                orphaned: false,
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
    let orphaned = pending.orphaned;
    let now = Instant::now();
    READY.with(|r| (r.borrow_mut()).insert(id, key.0, (view, pending.events), orphaned, now));
    if orphaned {
        // Closed after this callback, not inside it.
        discard_later(Duration::ZERO);
        return;
    }
    discard_later(UNADOPTED_FOR);
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

/// `OnBeforeClose` of a page: the popups it opened that no tab took close,
/// and the ones still being created close once they exist.
pub(crate) fn opener_closed(opener: i32) {
    let mut any = PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        let mine = pending.iter_mut().filter(|(key, _)| key.0 == opener);
        mine.map(|(_, popup)| popup.orphaned = true).count() > 0
    });
    any |= READY.with(|r| {
        let mut ready = r.borrow_mut();
        ready.opener_closed(opener);
        !ready.waiting.is_empty()
    });
    if any {
        discard_later(Duration::ZERO);
    }
}

/// Closes the popups [`Unadopted::discard`] gives up on after `delay`, on
/// CEF's UI thread (the main thread), outside any CEF callback.
fn discard_later(delay: Duration) {
    let mut task = DiscardTask::new();
    let delay = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
    cef::post_delayed_task(ThreadId::UI, Some(&mut task), delay);
}

wrap_task! {
    struct DiscardTask;

    impl Task {
        fn execute(&self) {
            let gone = READY.with(|r| r.borrow_mut().discard(Instant::now()));
            close(gone);
        }
    }
}

/// Drops popups, which closes them. Their views go within an autorelease
/// pool, so that CEF hears of it before this returns.
fn close(popups: Vec<Popup>) {
    if popups.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| drop(popups));
    #[cfg(not(target_os = "macos"))]
    drop(popups);
}

/// Before CEF shuts down: every popup waiting for a tab closes, and the ones
/// still being created are forgotten.
pub(crate) fn forget_all() {
    PENDING.with(|p| {
        for pending in p.borrow_mut().values_mut() {
            pending.orphaned = true;
        }
    });
    let all = READY.with(|r| r.borrow_mut().drain());
    close(all);
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
            plan(Opened::ForegroundTab, true, false, yes),
            Plan::Reopen { foreground: true }
        );
        assert_eq!(
            plan(Opened::BackgroundTab, true, false, yes),
            Plan::Reopen { foreground: false }
        );
        assert_eq!(
            plan(Opened::ForegroundTab, false, false, yes),
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
            plan(Opened::Other, true, false, || panic!("asked the limiter")),
            Plan::Reopen { foreground: true }
        );
    }

    #[test]
    fn every_popup_cef_would_create_keeps_its_opener() {
        let yes = || true;
        // A plain `window.open` or a `target=_blank` link, one opening
        // behind, and one without a gesture: all adopted.
        assert_eq!(
            plan(Opened::ForegroundTab, true, true, yes),
            Plan::Adopt { foreground: true }
        );
        assert_eq!(
            plan(Opened::BackgroundTab, true, true, yes),
            Plan::Adopt { foreground: false }
        );
        assert_eq!(
            plan(Opened::Other, false, true, yes),
            Plan::Adopt { foreground: false }
        );
        // Still limited without a gesture.
        assert_eq!(
            plan(Opened::ForegroundTab, false, true, || false),
            Plan::Drop
        );
    }

    #[test]
    fn unadopted_popups_close_with_their_opener_or_after_a_while() {
        let start = Instant::now();
        let mut ready = Unadopted::default();
        ready.insert(1, 10, "a", false, start);
        ready.insert(2, 10, "b", false, start);
        ready.insert(3, 20, "c", false, start);
        // Its opener closed while CEF created it.
        ready.insert(4, 30, "d", true, start);
        assert_eq!(ready.discard(start), ["d"]);

        assert_eq!(ready.take(2), Some("b"));
        assert_eq!(ready.take(2), None);
        ready.opener_closed(10);
        assert_eq!(ready.discard(start), ["a"]);

        let soon = start + UNADOPTED_FOR - Duration::from_millis(1);
        assert!(ready.discard(soon).is_empty());
        ready.insert(5, 20, "e", false, soon);
        assert_eq!(ready.discard(start + UNADOPTED_FOR), ["c"]);
        assert_eq!(ready.drain(), ["e"]);
    }
}
