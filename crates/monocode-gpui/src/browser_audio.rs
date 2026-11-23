//! A page's sound (Faz 4a): whether it plays any, for the speaker on its
//! tab (`monocode_core::browser::audio` tells), and the tab's mute, which
//! Chromium applies to the page and the pane saves. A new view of the page
//! (a reload after a crash, another profile) is muted again as it is made.
use super::*;

/// What a page's hosts share of its sound.
#[derive(Default)]
pub(super) struct Audio {
    audible: Cell<bool>,
    muted: Cell<bool>,
}

impl Audio {
    /// The page started or stopped playing sound: its tab repaints.
    pub(super) fn set_audible(&self, audible: bool, shared: &Shared) {
        if self.audible.replace(audible) != audible {
            shared.repaint();
        }
    }
}

impl Host<monocode_cef::BrowserView> {
    /// Mutes a new view as the tab is, and watches the page's sound for as
    /// long as the returned value lives.
    pub(super) fn watch_audio(
        &self,
        cdp: &Rc<CdpClient>,
    ) -> monocode_core::browser::audio::AudioWatch {
        let muted = self.shared.audio.muted.get();
        if muted {
            self.with_view(|view| view.set_audio_muted(true));
        }
        let shared = Rc::downgrade(&self.shared);
        monocode_core::browser::audio::watch(cdp, move |audible| {
            if let Some(shared) = shared.upgrade() {
                shared.audio.set_audible(audible, &shared);
            }
        })
    }
}

impl BrowserPage {
    /// The page plays sound now (muted or not).
    pub(crate) fn audible(&self) -> bool {
        self.host.shared.audio.audible.get()
    }

    pub(crate) fn muted(&self) -> bool {
        self.host.shared.audio.muted.get()
    }

    /// Whether Chromium mutes the page's view, if it has one (dev hook).
    pub(crate) fn view_muted(&self) -> Option<bool> {
        (self.host.view.borrow().as_ref()).map(|hosted| hosted.view.is_audio_muted())
    }

    /// The saved mute, before the page has a view.
    pub(crate) fn restore_muted(&mut self, muted: bool) {
        self.host.shared.audio.muted.set(muted);
    }

    /// Mutes the tab or lets it play; the pane saves it.
    pub(crate) fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        if self.host.shared.audio.muted.replace(muted) == muted {
            return;
        }
        self.host.with_view(|view| view.set_audio_muted(muted));
        cx.emit(PageChanged);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_audible_for_test(&self, audible: bool) {
        let shared = &self.host.shared;
        shared.audio.set_audible(audible, shared);
    }
}
