//! Whether a page plays sound, for its tab's audio indicator (Faz 4a).
//!
//! Chromium knows (its tab strip's speaker), but CEF does not say: its only
//! audio hook, `CefAudioHandler`, captures the stream, which mutes the page.
//! So the page tells: a script in an isolated world the page cannot see
//! watches its documents' `<audio>` and `<video>` elements (play, pause,
//! volume, end) and reports through a binding whenever its document turns
//! audible or silent. A page is audible while any of its documents is.
//!
//! Not seen: media elements never put in a document (`new Audio().play()`),
//! ones inside shadow roots, Web Audio, and cross-process iframes.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use serde_json::json;

use super::{CdpClient, Subscription};

/// The isolated world the watcher runs in.
const WORLD: &str = "monocode-audio";
/// The binding it reports through, `"1"` (audible) or `"0"`.
const BINDING: &str = "__monocodeAudio";

/// Runs in every document of the page, in [`WORLD`].
const WATCHER: &str = r#"(function () {
  if (window.__monocodeAudioCheck) { window.__monocodeAudioCheck(); return; }
  var report = globalThis.__monocodeAudio;
  if (typeof report !== "function") return;
  var last = null;
  function audible() {
    var media = document.querySelectorAll("audio, video");
    for (var i = 0; i < media.length; i++) {
      var m = media[i];
      if (!m.paused && !m.ended && !m.muted && m.volume > 0 && m.readyState > 2) return true;
    }
    return false;
  }
  function check() {
    var now = audible();
    if (now !== last) { last = now; report(now ? "1" : "0"); }
  }
  var soon = null;
  function later() { if (soon === null) soon = setTimeout(function () { soon = null; check(); }, 0); }
  ["play", "playing", "pause", "ended", "volumechange", "emptied", "abort", "loadeddata", "waiting"]
    .forEach(function (type) { document.addEventListener(type, later, true); });
  window.__monocodeAudioCheck = check;
  check();
})()"#;

/// Watches a page's sound while it lives; dropping it stops.
pub struct AudioWatch {
    _events: Subscription,
}

/// The documents (execution contexts) that report sound.
#[derive(Default)]
struct Sounding {
    contexts: HashSet<i64>,
    audible: bool,
}

impl Sounding {
    /// One event of the page; the page's new state if it changed.
    fn on_event(&mut self, method: &str, params: &serde_json::Value) -> Option<bool> {
        match method {
            "Runtime.bindingCalled" if params["name"] == BINDING => {
                let context = params["executionContextId"].as_i64()?;
                if params["payload"] == "1" {
                    self.contexts.insert(context);
                } else {
                    self.contexts.remove(&context);
                }
            }
            "Runtime.executionContextDestroyed" => {
                let context = params["executionContextId"].as_i64()?;
                self.contexts.remove(&context);
            }
            "Runtime.executionContextsCleared" => self.contexts.clear(),
            _ => return None,
        }
        let audible = !self.contexts.is_empty();
        (audible != std::mem::replace(&mut self.audible, audible)).then_some(audible)
    }
}

/// Starts watching the page whose channel is `cdp` (its log has turned
/// `Runtime` on); `changed` hears each change, on the main thread.
pub fn watch(cdp: &Rc<CdpClient>, changed: impl Fn(bool) + 'static) -> AudioWatch {
    let sounding = RefCell::new(Sounding::default());
    let events = cdp.subscribe(move |event| {
        // Our own session only: child sessions' contexts are not ours.
        if event.session_id.is_some() {
            return;
        }
        let change = sounding.borrow_mut().on_event(&event.method, &event.params);
        if let Some(audible) = change {
            changed(audible);
        }
    });
    let binding = cdp.call(
        "Runtime.addBinding",
        json!({"name": BINDING, "executionContextName": WORLD}),
        None,
    );
    let script = cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": WATCHER, "worldName": WORLD, "runImmediately": true}),
        None,
    );
    drop((binding, script));
    AudioWatch { _events: events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tools::fake::Script;

    #[test]
    fn a_page_is_audible_while_any_of_its_documents_is() {
        let (cdp, script) = Script::client();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        let _watch = watch(&cdp, move |audible| log.borrow_mut().push(audible));
        // The binding lives in the watcher's world, which runs on documents
        // that exist already and on every new one.
        let binding = &script.calls("Runtime.addBinding")[0];
        assert_eq!(binding["executionContextName"], WORLD);
        let added = &script.calls("Page.addScriptToEvaluateOnNewDocument")[0];
        assert_eq!(added["worldName"], WORLD);
        assert_eq!(added["runImmediately"], true);

        let call = |context: i64, payload: &str| {
            script.emit(
                "Runtime.bindingCalled",
                json!({"name": BINDING, "payload": payload, "executionContextId": context}),
            )
        };
        call(3, "0");
        call(3, "1");
        call(4, "1");
        call(3, "0");
        assert_eq!(*heard.borrow(), [true]);
        // A frame with sound goes away; another binding is not ours; an
        // iframe's session is not the page's.
        script.emit(
            "Runtime.executionContextDestroyed",
            json!({"executionContextId": 4}),
        );
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": "other", "payload": "1", "executionContextId": 9}),
        );
        script.emit_in(
            "IFRAME",
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": "1", "executionContextId": 9}),
        );
        assert_eq!(*heard.borrow(), [true, false]);
        // A navigation clears every document.
        call(5, "1");
        script.emit("Runtime.executionContextsCleared", json!({}));
        assert_eq!(*heard.borrow(), [true, false, true, false]);
    }
}
