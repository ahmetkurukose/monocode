//! A page's camera and microphone requests, held until macOS has decided
//! (docs/native/browser/context.md §11, "Mikrofon takılması").
//!
//! Chromium's audio service reads the microphone before it asks the app
//! (CEF's permission handler): while macOS has not decided whether the app
//! may use it, coreaudiod asks the user then and Chromium waits inside
//! CoreAudio. The page, and its later camera requests, wait with no word
//! from the app. So the page tells first: a script in its main world holds
//! each `getUserMedia` call with audio or video and reports it through a
//! binding; the app asks macOS itself, with a bar that says so, and then
//! lets the call go on ([`answer`]), or refuses it (a background tab an
//! agent drives, K63). Without an answer in a minute the call goes on as
//! it would have.
//!
//! As with the passkey watcher (`super::passkeys`), the script takes the
//! binding off the page's global first; what a page could fake is only its
//! own request. Cross-process iframes are not held.

use std::rc::{Rc, Weak};

use serde::Deserialize;
use serde_json::json;

use super::{CdpClient, CdpEvent, Subscription};

/// The binding the script reports through, in the page's main world.
const BINDING: &str = "__monocodeMedia";
/// What lets a held call go on: `(id, allowed)`.
const READY: &str = "__monocodeMediaReady";

/// Runs in every document of the page, in its main world, before the
/// page's own scripts.
const WATCHER: &str = r#"(function () {
  if (typeof MediaDevices === "undefined" || window.__monocodeMediaReady) return;
  var report = globalThis.__monocodeMedia;
  if (typeof report !== "function") return;
  try { delete globalThis.__monocodeMedia; } catch (e) {}
  var original = MediaDevices.prototype.getUserMedia;
  if (typeof original !== "function") return;
  var held = new Map();
  var next = 0;
  MediaDevices.prototype.getUserMedia = function (constraints) {
    var self = this;
    var args = arguments;
    var audio = !!(constraints && constraints.audio);
    var video = !!(constraints && constraints.video);
    if (!audio && !video) return original.apply(self, args);
    var id = ++next;
    return new Promise(function (resolve) {
      var go = function (allowed) {
        if (held.delete(id)) resolve(allowed);
      };
      held.set(id, go);
      // Never longer than a minute: then as if nobody held it.
      setTimeout(function () { go(true); }, 60000);
      try {
        report(JSON.stringify({ id: id, audio: audio, video: video, origin: location.origin }));
      } catch (e) {
        go(true);
      }
    }).then(function (allowed) {
      if (!allowed) throw new DOMException("Permission denied", "NotAllowedError");
      return original.apply(self, args);
    });
  };
  Object.defineProperty(window, "__monocodeMediaReady", {
    value: function (id, allowed) {
      var go = held.get(id);
      if (go) go(allowed);
      return !!go;
    }
  });
})()"#;

/// One `getUserMedia` call the page holds.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaAsk {
    /// The document (execution context) that holds it.
    pub context: i64,
    pub id: u64,
    pub audio: bool,
    pub video: bool,
    pub origin: String,
}

impl MediaAsk {
    /// What it asks for, as permission names: "camera", "microphone".
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.video {
            names.push("camera");
        }
        if self.audio {
            names.push("microphone");
        }
        names
    }
}

#[derive(Deserialize)]
struct Report {
    id: u64,
    #[serde(default)]
    audio: bool,
    #[serde(default)]
    video: bool,
    #[serde(default)]
    origin: String,
}

/// The call an event of the page reports, if it is one.
fn parse(event: &CdpEvent) -> Option<MediaAsk> {
    // Our own session only: child sessions' contexts are not ours.
    if event.session_id.is_some()
        || event.method != "Runtime.bindingCalled"
        || event.params["name"] != BINDING
    {
        return None;
    }
    let context = event.params["executionContextId"].as_i64()?;
    let report: Report = serde_json::from_str(event.params["payload"].as_str()?).ok()?;
    if !report.audio && !report.video {
        return None;
    }
    Some(MediaAsk {
        context,
        id: report.id,
        audio: report.audio,
        video: report.video,
        origin: report.origin,
    })
}

/// Holds the page's camera and microphone requests while it lives;
/// dropping it stops.
pub struct MediaWatch {
    _events: Subscription,
}

/// Starts holding the requests of the page whose channel is `cdp` (its log
/// has turned `Runtime` on). `asked` hears each one, on the main thread,
/// and must [`answer`] it; when it gives the request back (`Err`), the call
/// goes on at once.
pub fn watch(
    cdp: &Rc<CdpClient>,
    asked: impl Fn(MediaAsk) -> Result<(), MediaAsk> + 'static,
) -> MediaWatch {
    let client: Weak<CdpClient> = Rc::downgrade(cdp);
    let events = cdp.subscribe(move |event| {
        let Some(ask) = parse(event) else { return };
        if let Err(ask) = asked(ask) {
            if let Some(cdp) = client.upgrade() {
                answer(&cdp, &ask, true);
            }
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
    MediaWatch { _events: events }
}

/// Lets the held call go on (`allowed`), or makes it fail as refused
/// (`NotAllowedError`). A document that went has nothing to answer.
pub fn answer(cdp: &CdpClient, ask: &MediaAsk, allowed: bool) {
    let call = cdp.call(
        "Runtime.evaluate",
        json!({
            "expression": format!(
                "typeof {READY} === 'function' && {READY}({}, {allowed})",
                ask.id
            ),
            "contextId": ask.context,
        }),
        None,
    );
    drop(call);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::browser::tools::fake::Script;

    fn payload(id: u64, audio: bool, video: bool) -> String {
        json!({"id": id, "audio": audio, "video": video, "origin": "https://meet.example"})
            .to_string()
    }

    #[test]
    fn held_requests_are_heard_and_answered_in_their_document() {
        let (cdp, script) = Script::client();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        let _watch = watch(&cdp, move |ask| {
            let keep = ask.audio;
            log.borrow_mut().push(ask.clone());
            // Audio is answered later; the rest is given back.
            if keep { Ok(()) } else { Err(ask) }
        });
        let binding = &script.calls("Runtime.addBinding")[0];
        assert_eq!(binding["name"], BINDING);
        assert!(binding.get("executionContextName").is_none());
        let added = &script.calls("Page.addScriptToEvaluateOnNewDocument")[0];
        assert_eq!(added["runImmediately"], true);
        assert!(added.get("worldName").is_none());

        let call = |context: i64, payload: String| {
            script.emit(
                "Runtime.bindingCalled",
                json!({"name": BINDING, "payload": payload, "executionContextId": context}),
            )
        };
        call(7, payload(1, true, false));
        assert_eq!(
            *heard.borrow(),
            [MediaAsk {
                context: 7,
                id: 1,
                audio: true,
                video: false,
                origin: "https://meet.example".into(),
            }]
        );
        assert_eq!(heard.borrow()[0].names(), ["microphone"]);
        // Kept: nothing answered yet.
        assert!(script.calls("Runtime.evaluate").is_empty());
        answer(&cdp, &heard.borrow()[0], false);
        let evaluated = &script.calls("Runtime.evaluate")[0];
        assert_eq!(evaluated["contextId"], 7);
        assert_eq!(
            evaluated["expression"],
            format!("typeof {READY} === 'function' && {READY}(1, false)")
        );

        // Given back: the call goes on at once.
        call(8, payload(2, false, true));
        let evaluated = &script.calls("Runtime.evaluate")[1];
        assert_eq!(evaluated["contextId"], 8);
        assert!(
            evaluated["expression"]
                .as_str()
                .unwrap()
                .ends_with("(2, true)")
        );
        assert_eq!(heard.borrow()[1].names(), ["camera"]);

        // Other bindings, garbage, nothing asked and iframes' sessions are not ours.
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": "other", "payload": payload(3, true, true), "executionContextId": 7}),
        );
        call(7, "not json".into());
        call(7, payload(4, false, false));
        script.emit_in(
            "IFRAME",
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": payload(5, true, true), "executionContextId": 7}),
        );
        assert_eq!(heard.borrow().len(), 2);
    }

    #[test]
    fn the_watcher_holds_get_user_media_and_can_refuse() {
        for needle in [
            "MediaDevices.prototype.getUserMedia = function",
            "delete globalThis.__monocodeMedia",
            "NotAllowedError",
            "setTimeout(function () { go(true); }, 60000)",
            READY,
        ] {
            assert!(WATCHER.contains(needle), "{needle}");
        }
    }
}
