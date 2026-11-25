//! The shell's side of Memory Saver and Battery Saver (context.md K64,
//! K52): a timer that looks at every browser page of the window now and
//! then, with the settings and the battery's state of the moment
//! (`browser/lifecycle.rs` decides, the page acts), and the dimmed tile of a
//! discarded page's tab.
use super::*;
use crate::browser::lifecycle::{self, Policy};

impl Shell {
    /// Starts the window's lifecycle ticks. Once per window.
    pub(super) fn start_browser_lifecycle(&mut self, cx: &mut Context<Self>) {
        if cfg!(test) || !cfg!(target_os = "macos") {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(policy) = this.update(cx, |this, cx| this.browser_lifecycle_tick(cx)) else {
                    return;
                };
                cx.background_executor().timer(policy.tick()).await;
            }
        })
        .detach();
    }

    /// The settings and the battery now.
    fn browser_lifecycle_policy(&self) -> Policy {
        let prefs = &self.workspace.preferences;
        Policy::new(
            prefs.browser_memory_saver,
            lifecycle::discard_secs_override(),
            prefs.browser_battery_saver,
            prefs.browser_battery_saver && lifecycle::battery_low(),
        )
    }

    fn browser_lifecycle_tick(&mut self, cx: &mut Context<Self>) -> Policy {
        let policy = self.browser_lifecycle_policy();
        let pages: Vec<(i64, Entity<crate::browser::BrowserPage>)> = (self.browser_pages.iter())
            .map(|(pane, (page, _))| (*pane, page.clone()))
            .collect();
        for (pane, page) in pages {
            let pinned = (self.workspace.group_of(pane))
                .is_some_and(|ix| self.workspace.groups[ix].pinned.contains(&pane));
            page.update(cx, |page, cx| page.lifecycle_tick(pinned, &policy, cx));
        }
        // Browser mode's Today tabs left unseen go to the archive.
        self.browse_archive_idle(cx);
        policy
    }

    /// Whether the page of `pane` is discarded (its tab's tile dims).
    pub(super) fn browser_discarded(&self, pane: i64, cx: &App) -> bool {
        (self.browser_pages.get(&pane)).is_some_and(|(page, _)| page.read(cx).discarded())
    }
}
