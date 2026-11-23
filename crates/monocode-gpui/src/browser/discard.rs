//! Memory Saver and Battery Saver on a page (context.md K64, K52): the
//! page's side of [`lifecycle`]. Its pane's shell ticks it
//! (`shell/browser_lifecycle.rs`); the page gathers its leases, asks its
//! document what only the document knows, and discards or freezes itself.
//!
//! A discarded page closes its Chromium browser and keeps its tab: the
//! address, the title, the zoom and where it was scrolled to. It has no
//! view then, like a tab restored at launch (K20), so showing it or an
//! agent's command loads it again the same way. Its back and forward
//! history is lost: CEF cannot restore it.

use super::*;
use lifecycle::{Life, Policy, ProbeAnswer, Signals, Step};
use serde_json::json;

/// The world the edit watcher and the probe run in: one per document,
/// apart from the page's own scripts, which can neither see nor fake it.
const WORLD: &str = "monocode-life";

/// Notes the user's edits until the form is submitted. A new document (a
/// navigation) starts clean.
const WATCH: &str = r#"(() => {
  if (globalThis.__monocodeLife) return;
  const life = (globalThis.__monocodeLife = { edited: false });
  const edit = (event) => {
    if (event.isTrusted) life.edited = true;
  };
  addEventListener("input", edit, true);
  addEventListener("change", edit, true);
  addEventListener("submit", () => { life.edited = false; }, true);
})()"#;

/// What the probe asks the document.
const PROBE: &str = r#"(() => {
  const life = globalThis.__monocodeLife;
  const audible = [...document.querySelectorAll("video, audio")].some(
    (m) => !m.paused && !m.ended && !m.muted && m.volume > 0,
  );
  return {
    watched: !!life,
    edited: !!(life && life.edited),
    audible,
    x: scrollX,
    y: scrollY,
  };
})()"#;

/// A probe not answered in this long counts as no answer.
const PROBE_PATIENCE: Duration = Duration::from_secs(3);

/// What a discarded page keeps beyond its address and title.
pub(super) struct Discarded {
    /// The address it was discarded at, and where it was scrolled to there.
    url: String,
    scroll: Option<(f64, f64)>,
}

/// Starts the edit watcher in every document of the page from now on, the
/// current one included. Once per CDP client.
pub(super) fn watch_edits(cdp: &CdpClient) {
    drop(cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": WATCH, "worldName": WORLD, "runImmediately": true}),
        None,
    ));
}

/// Asks the page's document what it holds; `None` when it cannot say.
async fn ask(cdp: Rc<CdpClient>) -> Option<ProbeAnswer> {
    let tree = cdp.call("Page.getFrameTree", json!({}), None).await.ok()?;
    let frame = tree["frameTree"]["frame"]["id"].as_str()?.to_owned();
    // The watcher's world: a world's name gives the same one back.
    let world = cdp
        .call(
            "Page.createIsolatedWorld",
            json!({"frameId": frame, "worldName": WORLD, "grantUniveralAccess": false}),
            None,
        )
        .await
        .ok()?;
    let context = world["executionContextId"].as_i64()?;
    let probe = cdp
        .call(
            "Runtime.evaluate",
            json!({"expression": PROBE, "contextId": context, "returnByValue": true}),
            None,
        )
        .await
        .ok()?;
    let value = &probe["result"]["value"];
    let watched = value["watched"].as_bool()?;
    if !watched {
        // A document from before the watcher: watched from now on.
        drop(cdp.call(
            "Runtime.evaluate",
            json!({"expression": WATCH, "contextId": context}),
            None,
        ));
    }
    // `beforeunload` handlers, the page's own (DevTools' listener list,
    // which the page cannot see being read).
    let window = cdp
        .call(
            "Runtime.evaluate",
            json!({"expression": "window", "objectGroup": WORLD}),
            None,
        )
        .await
        .ok()?;
    let listeners = match window["result"]["objectId"].as_str() {
        Some(id) => {
            cdp.call(
                "DOMDebugger.getEventListeners",
                json!({"objectId": id}),
                None,
            )
            .await
        }
        None => return None,
    };
    drop(cdp.call(
        "Runtime.releaseObjectGroup",
        json!({"objectGroup": WORLD}),
        None,
    ));
    let before_unload = (listeners.ok()?["listeners"].as_array()?.iter())
        .any(|listener| listener["type"] == "beforeunload");
    Some(ProbeAnswer {
        audible: value["audible"].as_bool().unwrap_or(true),
        edited: value["edited"].as_bool().unwrap_or(true),
        before_unload,
        watched,
        scroll: (
            value["x"].as_f64().unwrap_or(0.),
            value["y"].as_f64().unwrap_or(0.),
        ),
    })
}

impl BrowserPage {
    /// What keeps the page live that the app knows of; `pinned` is its
    /// tab's, which the shell knows.
    fn signals(&self, pinned: bool) -> Signals {
        use monocode_cef::DownloadState;
        let docked = self.devtools.is_some();
        let detached = (self.host.view.borrow().as_ref()).is_some_and(|h| h.view.has_dev_tools());
        Signals {
            shown: self.host.shown.get() != Placement::Hidden,
            commands: self.host.shared.leases.get() > 0,
            agent_recent: self.active_agent().is_some(),
            devtools: docked || detached,
            capture: self.host.capturing.get().is_some(),
            download: self.download_wait.is_some()
                || (self.downloads.iter()).any(|d| d.state == DownloadState::InProgress),
            dialog: !self.prompts.is_empty(),
            pinned,
        }
    }

    /// One look at the page (the shell's lifecycle tick).
    pub(crate) fn lifecycle_tick(&mut self, pinned: bool, policy: &Policy, cx: &mut Context<Self>) {
        if !self.has_view() {
            self.life = Life::default();
            return;
        }
        let now = Instant::now();
        let leases = self.signals(pinned).leases();
        match (self.life).tick(leases, self.host.shared.frozen.get(), policy, now) {
            Step::Stay => {}
            Step::Probe => self.probe(cx),
            Step::Discard => {
                log::info!("browser: Memory Saver discards {}", self.url);
                self.discard(cx);
            }
            Step::Freeze => self.set_frozen(true),
            Step::Thaw => self.set_frozen(false),
        }
    }

    /// Asks the document what it holds (playing media, an edited form, a
    /// `beforeunload` handler) and where it is scrolled.
    fn probe(&mut self, cx: &mut Context<Self>) {
        let Some(cdp) = self.cdp() else {
            self.life.probed(None, Instant::now());
            return;
        };
        self.life.probing = true;
        let (tx, rx) = async_channel::bounded(1);
        let late = tx.clone();
        cx.foreground_executor()
            .spawn(async move {
                let _ = tx.try_send(ask(cdp).await);
            })
            .detach();
        let timer = cx.background_executor().timer(PROBE_PATIENCE);
        cx.foreground_executor()
            .spawn(async move {
                timer.await;
                let _ = late.try_send(None);
            })
            .detach();
        cx.spawn(async move |this, cx| {
            let answer = rx.recv().await.ok().flatten();
            let _ = this.update(cx, |page, _| page.life.probed(answer, Instant::now()));
        })
        .detach();
    }

    /// Closes the page's Chromium browser and keeps its tab; it loads again
    /// when shown or used. False when it had no browser.
    pub(crate) fn discard(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.has_view() {
            return false;
        }
        let scroll = (self.life.last_probe())
            .and_then(|probe| probe.answer)
            .map(|answer| answer.scroll);
        let shared = self.host.shared.clone();
        let options = self.host.options.borrow().clone();
        let old = std::mem::replace(&mut self.host, Host::with_shared(shared.clone()));
        *self.host.options.borrow_mut() = options;
        old.release(cx);
        old.close();
        drop(old);
        shared.frozen.set(false);
        self.discarded = Some(Discarded {
            url: self.url.clone(),
            scroll,
        });
        self.life = Life::default();
        self.loading = false;
        self.progress = 0.;
        self.find = None;
        // The tab shows it.
        cx.emit(PageChanged);
        cx.notify();
        true
    }

    /// Discarded to save memory, and not loaded again yet.
    pub(crate) fn discarded(&self) -> bool {
        self.discarded.is_some() && !self.has_view()
    }

    /// A page loaded again after a discard goes back to where it was
    /// scrolled, if it is still at that address.
    pub(super) fn restore_scroll(&mut self) {
        if self.discarded.is_none() || !self.has_view() {
            return;
        }
        let Some(discarded) = self.discarded.take() else {
            return;
        };
        let Some((x, y)) = discarded.scroll.filter(|&(x, y)| x != 0. || y != 0.) else {
            return;
        };
        if discarded.url != self.url {
            return;
        }
        if let Some(cdp) = self.cdp() {
            drop(cdp.call(
                "Runtime.evaluate",
                json!({"expression": format!("scrollTo({x}, {y})")}),
                None,
            ));
        }
    }

    /// Freezes the page out of sight (Battery Saver), or thaws it.
    fn set_frozen(&mut self, frozen: bool) {
        if frozen && self.host.shown.get() != Placement::Hidden {
            return;
        }
        if !frozen {
            self.host.thaw();
            return;
        }
        let Some(cdp) = self.cdp() else { return };
        if self.host.shared.frozen.replace(true) {
            return;
        }
        log::info!("browser: Battery Saver freezes {}", self.url);
        drop(cdp.call(
            "Page.setWebLifecycleState",
            json!({"state": "frozen"}),
            None,
        ));
    }

    /// Battery Saver froze the page.
    #[cfg(test)]
    pub(crate) fn frozen(&self) -> bool {
        self.host.shared.frozen.get()
    }
}

impl<V> Host<V> {
    /// A frozen page runs again before it shows (K52).
    pub(super) fn thaw(&self) {
        if !self.shared.frozen.replace(false) {
            return;
        }
        if let Some(link) = self.agent.borrow().as_ref() {
            drop(link.cdp.call(
                "Page.setWebLifecycleState",
                json!({"state": "active"}),
                None,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn a_page_without_a_browser_has_nothing_to_discard(cx: &mut gpui::TestAppContext) {
        let page = cx.new(|cx| {
            BrowserPage::new(
                "https://a.test/".into(),
                "A".into(),
                100,
                std::env::temp_dir(),
                cx,
            )
        });
        page.update(cx, |page, cx| {
            assert!(!page.discard(cx));
            assert!(!page.discarded());
            // Ticks leave a page with no browser be, however idle.
            let policy = Policy::new(0, Some(1), true, true);
            page.lifecycle_tick(false, &policy, cx);
            assert!(!page.frozen());
            // Nothing was frozen, so nothing thaws.
            page.host.thaw();
            assert!(!page.frozen());
        });
    }

    #[test]
    fn a_page_out_of_sight_with_nothing_running_has_no_lease() {
        let none = Signals::default().leases();
        assert!(none.is_empty());
        let downloading = Signals {
            download: true,
            ..Default::default()
        };
        assert!(downloading.leases().has(lifecycle::Lease::Download));
    }
}
