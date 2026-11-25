//! What a page plays, for Browser mode's media card and automatic
//! Picture-in-Picture (docs/native/modes.md, "Media and Picture-in-Picture").
//!
//! A script in the top document's main world picks the page's media
//! element (the one playing, else the last one chosen, else one that has
//! played) and reports it through a binding whenever its state changes:
//! whether it plays, whether it is a video, whether it is in
//! Picture-in-Picture, its title (the Media Session's, else the page's)
//! and whether the page handles Previous and Next Track. The Media
//! Session's action handlers are only seen in the main world, so the
//! script wraps `setActionHandler` before the page's own scripts run.
//!
//! Actions go back as fixed expressions ([`command`], [`picture_in_picture`])
//! with a user gesture, which `play()` and `requestPictureInPicture()` need.
//! The script takes the binding off the global first, in every frame, so
//! neither the page nor its iframes can report for it; the command it
//! leaves behind only moves the page's own media.
//!
//! Not seen: media in iframes or shadow roots, and elements never put in
//! the document.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use serde::Deserialize;
use serde_json::json;

use super::{CdpClient, CdpEvent, Subscription};

/// The binding the script reports through, in the main world.
const BINDING: &str = "__monocodePlaybackReport";
/// The command the script leaves on the top document's window.
const COMMAND: &str = "__monocodePlayback";

/// Runs in every document of the page, in its main world, before the
/// page's own scripts.
const WATCHER: &str = r#"(function () {
  var report = globalThis.__monocodePlaybackReport;
  try { delete globalThis.__monocodePlaybackReport; } catch (e) {}
  if (typeof report !== "function" || window.top !== window || window.__monocodePlayback) return;
  var handlers = new Map();
  var session = navigator.mediaSession;
  if (session && typeof session.setActionHandler === "function") {
    var set = session.setActionHandler.bind(session);
    session.setActionHandler = function (action, handler) {
      set(action, handler);
      if (handler) handlers.set(action, handler); else handlers.delete(action);
    };
  }
  var current = null, last = "", started = new WeakSet();
  function choose() {
    var list = Array.prototype.slice.call(document.querySelectorAll("video, audio"));
    return list.find(function (e) { return !e.paused && !e.ended; })
      || (list.indexOf(current) >= 0 ? current : null)
      || list.find(function (e) { return e.currentTime > 0; })
      || list.find(function (e) { return e.readyState >= 1; })
      || null;
  }
  function update() {
    current = choose();
    if (current && !current.paused && !current.ended) started.add(current);
    var meta = session && session.metadata;
    var state = {
      active: current ? started.has(current) && !current.ended
        : !!(session && session.playbackState !== "none" && meta),
      playing: current ? !current.paused && !current.ended
        : !!(session && session.playbackState === "playing"),
      video: !!current && current.tagName === "VIDEO" && current.videoWidth > 0,
      sound: !!current && !current.muted && current.volume > 0,
      pip: !!document.pictureInPictureElement,
      title: String((meta && meta.title) || document.title || "").slice(0, 512),
      artist: String((meta && meta.artist) || "").slice(0, 512),
      previous: handlers.has("previoustrack"),
      next: handlers.has("nexttrack")
    };
    var json = JSON.stringify(state);
    if (json !== last) { last = json; report(json); }
  }
  var soon = null;
  function later(delay) {
    if (soon !== null) clearTimeout(soon);
    soon = setTimeout(function () { soon = null; update(); }, delay);
  }
  ["play", "playing", "pause", "ended", "emptied", "loadedmetadata", "volumechange", "enterpictureinpicture"]
    .forEach(function (type) { document.addEventListener(type, function () { later(0); }, true); });
  // Chromium's Close pauses before the window goes; Back to Tab does not.
  // Waiting a moment tells the two apart.
  document.addEventListener("leavepictureinpicture", function () { later(300); }, true);
  setInterval(update, 1000);
  Object.defineProperty(window, "__monocodePlayback", {
    value: function (action) {
      current = choose();
      var name = action;
      if (action === "toggle") {
        var paused = current ? current.paused : !(session && session.playbackState === "playing");
        name = paused ? "play" : "pause";
      }
      var handler = handlers.get(name);
      if (handler) { try { handler({ action: name }); } catch (e) {} }
      else if (current) {
        if (name === "play") current.play().catch(function () {});
        if (name === "pause") current.pause();
      }
      later(0);
      return true;
    }
  });
  update();
})()"#;

/// Opens Chromium's Picture-in-Picture on the page's playing video.
const ENTER_PIP: &str = r#"(async function () {
  if (document.pictureInPictureElement) return true;
  var video = Array.prototype.slice.call(document.querySelectorAll("video")).find(function (v) {
    return !v.paused && !v.ended && v.readyState >= 1 && v.videoWidth > 0 && !v.disablePictureInPicture;
  });
  if (!video || !document.pictureInPictureEnabled) return false;
  try { await video.requestPictureInPicture(); return true; } catch (e) { return false; }
})()"#;
const EXIT_PIP: &str = "document.pictureInPictureElement ? document.exitPictureInPicture().then(function () { return true; }, function () { return false; }) : false";

/// What the page plays, as its script last reported.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// It has played: the media card shows it, paused or not.
    pub active: bool,
    pub playing: bool,
    /// A video with a picture (not only sound).
    pub video: bool,
    /// Its element is not muted (a muted decorative loop stays in its tab).
    pub sound: bool,
    /// The page's video is in Chromium's Picture-in-Picture window.
    pub pip: bool,
    pub title: String,
    pub artist: String,
    /// The page handles Previous Track (Media Session).
    pub previous: bool,
    pub next: bool,
}

impl Playback {
    fn decode(payload: &str) -> Option<Playback> {
        if payload.len() > 16384 {
            return None;
        }
        let mut state: Playback = serde_json::from_str(payload).ok()?;
        state.title = state.title.chars().take(512).collect();
        state.artist = state.artist.chars().take(512).collect();
        state.playing &= state.active;
        Some(state)
    }
}

/// What the media card and the page ask of the page's media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Toggle,
    Previous,
    Next,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Toggle => "toggle",
            Action::Previous => "previoustrack",
            Action::Next => "nexttrack",
        }
    }
}

/// The top document that reports, and its last state.
#[derive(Default)]
struct Reporter {
    context: Cell<Option<i64>>,
}

/// Watches what a page plays while it lives; dropping it stops.
pub struct PlaybackWatch {
    _events: Subscription,
    reporter: Rc<Reporter>,
}

impl PlaybackWatch {
    /// The document (execution context) commands go to.
    fn context(&self) -> Option<i64> {
        self.reporter.context.get()
    }

    /// Plays, pauses or skips the page's media. False when no document
    /// reports.
    pub fn command(&self, cdp: &CdpClient, action: Action) -> bool {
        self.evaluate(
            cdp,
            format!(
                "typeof {COMMAND} === 'function' && {COMMAND}('{}')",
                action.name()
            ),
        )
    }

    /// Opens (`true`) or closes Chromium's Picture-in-Picture on the page's
    /// playing video.
    pub fn picture_in_picture(&self, cdp: &CdpClient, open: bool) -> bool {
        self.evaluate(cdp, if open { ENTER_PIP } else { EXIT_PIP }.to_string())
    }

    fn evaluate(&self, cdp: &CdpClient, expression: String) -> bool {
        let Some(context) = self.context() else {
            return false;
        };
        let call = cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "contextId": context,
                "userGesture": true,
                "awaitPromise": true,
            }),
            None,
        );
        drop(call);
        true
    }
}

/// The page's new state if `event` changes it: `Some(None)` when the
/// reporting document went.
fn on_event(reporter: &Reporter, event: &CdpEvent) -> Option<Option<Playback>> {
    // Our own session only: child sessions' contexts are not ours.
    if event.session_id.is_some() {
        return None;
    }
    let params = &event.params;
    match event.method.as_str() {
        "Runtime.bindingCalled" if params["name"] == BINDING => {
            let context = params["executionContextId"].as_i64()?;
            let state = Playback::decode(params["payload"].as_str()?)?;
            reporter.context.set(Some(context));
            Some(Some(state))
        }
        "Runtime.executionContextDestroyed"
            if params["executionContextId"].as_i64() == reporter.context.get() =>
        {
            reporter.context.set(None);
            Some(None)
        }
        "Runtime.executionContextsCleared" if reporter.context.get().is_some() => {
            reporter.context.set(None);
            Some(None)
        }
        _ => None,
    }
}

/// Starts watching the page whose channel is `cdp` (its log has turned
/// `Runtime` on); `changed` hears each new state, `None` when the page has
/// none (it navigated), on the main thread.
pub fn watch(cdp: &Rc<CdpClient>, changed: impl Fn(Option<Playback>) + 'static) -> PlaybackWatch {
    let reporter = Rc::new(Reporter::default());
    let heard = reporter.clone();
    let last = RefCell::new(None::<Playback>);
    let events = cdp.subscribe(move |event| {
        let Some(state) = on_event(&heard, event) else {
            return;
        };
        if *last.borrow() != state {
            *last.borrow_mut() = state.clone();
            changed(state);
        }
    });
    // Without a context name: the binding is the main world's.
    let binding = cdp.call("Runtime.addBinding", json!({"name": BINDING}), None);
    let script = cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": WATCHER, "runImmediately": true}),
        None,
    );
    drop((binding, script));
    PlaybackWatch {
        _events: events,
        reporter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tools::fake::Script;

    fn report(script: &Script, context: i64, state: serde_json::Value) {
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": state.to_string(), "executionContextId": context}),
        );
    }

    #[test]
    fn the_top_document_reports_and_commands_go_back_to_it() {
        let (cdp, script) = Script::client();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        let watch = watch(&cdp, move |state| log.borrow_mut().push(state));
        let binding = &script.calls("Runtime.addBinding")[0];
        assert_eq!(binding["name"], BINDING);
        assert!(binding.get("executionContextName").is_none());
        let added = &script.calls("Page.addScriptToEvaluateOnNewDocument")[0];
        assert_eq!(added["runImmediately"], true);
        assert!(added.get("worldName").is_none());
        // Nothing reports yet: nowhere to send a command.
        assert!(!watch.command(&cdp, Action::Toggle));

        report(
            &script,
            4,
            json!({"active": true, "playing": true, "video": true, "title": "Song", "next": true}),
        );
        // The same state again is not news.
        report(
            &script,
            4,
            json!({"active": true, "playing": true, "video": true, "title": "Song", "next": true}),
        );
        let playing = Playback {
            active: true,
            playing: true,
            video: true,
            title: "Song".into(),
            next: true,
            ..Default::default()
        };
        assert_eq!(*heard.borrow(), [Some(playing.clone())]);

        assert!(watch.command(&cdp, Action::Next));
        let sent = &script.calls("Runtime.evaluate")[0];
        assert_eq!(sent["contextId"], 4);
        assert_eq!(sent["userGesture"], true);
        assert!(
            sent["expression"]
                .as_str()
                .unwrap()
                .ends_with("__monocodePlayback('nexttrack')")
        );
        assert!(watch.picture_in_picture(&cdp, true));
        let sent = &script.calls("Runtime.evaluate")[1];
        assert!(
            sent["expression"]
                .as_str()
                .unwrap()
                .contains("requestPictureInPicture")
        );

        // Other bindings, garbage and iframes' sessions are not ours.
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": "other", "payload": "{}", "executionContextId": 4}),
        );
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": "nope", "executionContextId": 4}),
        );
        script.emit_in(
            "IFRAME",
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": "{}", "executionContextId": 9}),
        );
        // Another document going is not the reporter's.
        script.emit(
            "Runtime.executionContextDestroyed",
            json!({"executionContextId": 7}),
        );
        assert_eq!(heard.borrow().len(), 1);

        // A navigation ends it.
        script.emit("Runtime.executionContextsCleared", json!({}));
        assert_eq!(*heard.borrow(), [Some(playing), None]);
        assert!(!watch.command(&cdp, Action::Toggle));
    }

    #[test]
    fn a_page_that_never_played_does_not_play() {
        let state = Playback::decode(r#"{"active":false,"playing":true}"#).unwrap();
        assert!(!state.playing);
        let long = "界".repeat(600);
        let state = Playback::decode(&json!({"title": long, "active": true}).to_string()).unwrap();
        assert_eq!(state.title.chars().count(), 512);
        assert!(Playback::decode(&" ".repeat(16385)).is_none());
    }
}
