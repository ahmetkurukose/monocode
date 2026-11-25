//! What a page plays (`monocode_core::browser::playback` tells), for
//! Browser mode's media card, and automatic Picture-in-Picture: a page
//! that stops being shown while its video plays with sound opens
//! Chromium's own PiP window, which closes when the page shows again
//! (docs/native/modes.md, "Media and Picture-in-Picture").
//!
//! The PiP window's Back to Tab closes it without pausing; its Close
//! pauses. So a PiP that ends on its own while the page is out of sight
//! and still playing was Back to Tab: the page asks to be shown
//! ([`MediaReturn`]).
use monocode_core::browser::playback::{self as watcher, Action, Playback, PlaybackWatch};

use super::*;

/// The page's PiP window asked to go back to its tab: the shell shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MediaReturn;

impl EventEmitter<MediaReturn> for BrowserPage {}

/// What a page's hosts share of what it plays.
#[derive(Default)]
pub(super) struct Media {
    state: RefCell<Option<Playback>>,
    /// The watcher of the page's current view.
    watch: RefCell<Weak<PlaybackWatch>>,
    /// The page is on screen (shown or under an overlay).
    on_screen: Cell<bool>,
    /// We asked its PiP to close: that is not Back to Tab.
    closing: Cell<bool>,
    returned: RefCell<Option<async_channel::Sender<()>>>,
}

/// Whether a page out of sight opens its video in PiP (Settings → Browser).
fn auto_pip(cx: &App) -> bool {
    cx.try_global::<crate::settings::Preferences>()
        .is_none_or(|p| p.browser_auto_pip)
}

/// Whether a page that stops being seen with `state` goes into PiP: a
/// video playing with sound, not in PiP already.
fn leaves_in_pip(state: Option<&Playback>) -> bool {
    state.is_some_and(|s| s.playing && s.video && s.sound && !s.pip)
}

impl Media {
    /// The page's view went (closed, crashed): it plays nothing.
    pub(super) fn gone(&self, shared: &Shared) {
        self.closing.set(false);
        if self.state.replace(None).is_some() {
            shared.repaint();
        }
    }

    /// The page's script reported a new state.
    fn changed(&self, state: Option<Playback>, shared: &Shared) {
        let was = self.state.replace(state.clone());
        let left_pip = was.is_some_and(|s| s.pip) && !state.as_ref().is_some_and(|s| s.pip);
        if left_pip {
            let ours = self.closing.replace(false);
            let playing = state.as_ref().is_some_and(|s| s.playing);
            if !ours
                && playing
                && !self.on_screen.get()
                && let Some(sender) = self.returned.borrow().as_ref()
            {
                let _ = sender.try_send(());
            }
        }
        shared.repaint();
    }
}

impl<V> Host<V> {
    /// Runs `f` with the page's watcher and CDP client, if it has them.
    fn with_playback(&self, f: impl FnOnce(&PlaybackWatch, &CdpClient) -> bool) -> bool {
        let Some(watch) = self.shared.playback.watch.borrow().upgrade() else {
            return false;
        };
        let agent = self.agent.borrow();
        agent.as_ref().is_some_and(|link| f(&watch, &link.cdp))
    }

    /// The sweep moved the page from `prev` to `next`: leaving the screen
    /// with a video playing opens PiP; coming back closes it.
    pub(super) fn follow_playback(&self, prev: Placement, next: Placement, cx: &App) {
        let seen = |p: Placement| matches!(p, Placement::Shown | Placement::Covered);
        let media = &self.shared.playback;
        media.on_screen.set(seen(next));
        if seen(prev) == seen(next) {
            return;
        }
        let state = media.state.borrow().clone();
        if !seen(next) {
            if leaves_in_pip(state.as_ref()) && auto_pip(cx) {
                self.with_playback(|watch, cdp| watch.picture_in_picture(cdp, true));
            }
        } else if state.is_some_and(|s| s.pip) {
            media.closing.set(true);
            self.with_playback(|watch, cdp| watch.picture_in_picture(cdp, false));
        }
    }
}

impl Host<monocode_cef::BrowserView> {
    /// Watches what the page plays for as long as the returned value lives.
    pub(super) fn watch_playback(&self, cdp: &Rc<CdpClient>) -> Rc<PlaybackWatch> {
        let shared = Rc::downgrade(&self.shared);
        let watch = Rc::new(watcher::watch(cdp, move |state| {
            if let Some(shared) = shared.upgrade() {
                shared.playback.changed(state, &shared);
            }
        }));
        *self.shared.playback.watch.borrow_mut() = Rc::downgrade(&watch);
        watch
    }
}

impl BrowserPage {
    /// Hears the PiP window's Back to Tab from any of the page's views.
    pub(super) fn listen_playback(self, cx: &mut Context<Self>) -> Self {
        let (sender, returns) = async_channel::unbounded::<()>();
        *self.host.shared.playback.returned.borrow_mut() = Some(sender);
        cx.spawn(async move |this, cx| {
            while returns.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.emit(MediaReturn)).is_err() {
                    break;
                }
            }
        })
        .detach();
        self
    }

    /// What the page plays, if it has played anything since it loaded.
    pub(crate) fn playback(&self) -> Option<Playback> {
        self.host.shared.playback.state.borrow().clone()
    }

    /// Plays, pauses or skips the page's media (the media card).
    pub(crate) fn media_action(&self, action: Action) -> bool {
        self.host
            .with_playback(|watch, cdp| watch.command(cdp, action))
    }

    /// Opens the page's playing video in PiP (the media card).
    pub(crate) fn open_picture_in_picture(&self) -> bool {
        self.host
            .with_playback(|watch, cdp| watch.picture_in_picture(cdp, true))
    }

    #[cfg(test)]
    pub(crate) fn set_playback_for_test(&self, state: Option<Playback>) {
        let shared = &self.host.shared;
        shared.playback.changed(state, shared);
    }

    #[cfg(test)]
    pub(crate) fn set_on_screen_for_test(&self, on_screen: bool) {
        self.host.shared.playback.on_screen.set(on_screen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(video: bool) -> Playback {
        Playback {
            active: true,
            playing: true,
            video,
            sound: true,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_video_playing_with_sound_follows_the_user_out() {
        assert!(leaves_in_pip(Some(&playing(true))));
        assert!(!leaves_in_pip(Some(&playing(false))), "sound only");
        assert!(!leaves_in_pip(None));
        let muted = Playback {
            sound: false,
            ..playing(true)
        };
        assert!(!leaves_in_pip(Some(&muted)), "a muted loop stays");
        let paused = Playback {
            playing: false,
            ..playing(true)
        };
        assert!(!leaves_in_pip(Some(&paused)));
        let in_pip = Playback {
            pip: true,
            ..playing(true)
        };
        assert!(!leaves_in_pip(Some(&in_pip)), "already in PiP");
    }

    #[gpui::test]
    fn a_pip_window_that_closes_while_the_video_plays_out_of_sight_returns_to_its_tab(
        cx: &mut gpui::TestAppContext,
    ) {
        let page = cx.new(|cx| {
            BrowserPage::new(
                "https://v.test/".into(),
                String::new(),
                100,
                PathBuf::new(),
                cx,
            )
        });
        let returns = Rc::new(Cell::new(0));
        let count = returns.clone();
        cx.update(|cx| {
            cx.subscribe(&page, move |_, _: &MediaReturn, _| {
                count.set(count.get() + 1)
            })
            .detach()
        });
        let in_pip = Playback {
            pip: true,
            ..playing(true)
        };
        let set = |state: Playback, cx: &mut gpui::TestAppContext| {
            page.read_with(cx, |page, _| page.set_playback_for_test(Some(state)));
            cx.run_until_parked();
        };
        // Back to Tab: PiP ends, the video plays on, the page is away.
        set(in_pip.clone(), cx);
        set(playing(true), cx);
        assert_eq!(returns.get(), 1);
        // Close pauses: no return.
        set(in_pip.clone(), cx);
        set(
            Playback {
                playing: false,
                ..playing(true)
            },
            cx,
        );
        assert_eq!(returns.get(), 1);
        // On screen, PiP ending is the page's own business.
        page.read_with(cx, |page, _| page.set_on_screen_for_test(true));
        set(in_pip, cx);
        set(playing(true), cx);
        assert_eq!(returns.get(), 1);
        assert!(page.read_with(cx, |page, _| page.playback()).is_some());
    }
}
