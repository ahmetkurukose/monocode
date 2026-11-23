//! A page's passkey requests (docs/native/browser/context.md K58), after
//! Orca's `browser-passkey-request-watch.ts`.
//!
//! Chromium shows its own WebAuthn dialog (the phone's QR code), but CEF
//! tells the app nothing about the request. So the page tells: a script in
//! its main world wraps `navigator.credentials.get` and `create` and
//! reports, through a binding, when a request starts waiting and when it
//! settles. Conditional requests (passkey autofill) block nothing and are
//! not reported. The wrapper also swaps the request's `AbortSignal`, so the
//! app can cancel what waits ([`cancel`]): the page sees the user's own
//! "cancelled" (`NotAllowedError`) and offers another way to sign in.
//!
//! The script runs in the page's world, where the page could call the
//! binding too; it takes the binding off the page's global first. What a
//! page could fake this way is only its own request's state.
//!
//! Not seen: cross-process iframes (their sessions are not the page's).

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use serde::Deserialize;
use serde_json::json;

use super::{CdpClient, Subscription};

/// The binding the wrapper reports through, in the page's main world.
const BINDING: &str = "__monocodePasskey";
/// What cancels the document's waiting requests; returns how many.
const CANCEL: &str = "__monocodePasskeyCancel";

/// Runs in every document of the page, in its main world, before the
/// page's own scripts.
const WATCHER: &str = r#"(function () {
  if (typeof CredentialsContainer === "undefined" || window.__monocodePasskeyCancel) return;
  var report = globalThis.__monocodePasskey;
  if (typeof report !== "function") return;
  try { delete globalThis.__monocodePasskey; } catch (e) {}
  var pending = new Map();
  var next = 0;
  var cancelled = function () {
    return new DOMException("The passkey request was cancelled.", "NotAllowedError");
  };
  var watch = function (kind, original) {
    return function (options) {
      // Conditional requests wait on autofill and block nothing.
      if (!options || !options.publicKey || options.mediation === "conditional") {
        return original.apply(this, arguments);
      }
      var controller = new AbortController();
      var request;
      try {
        var signal = options.signal
          ? AbortSignal.any([options.signal, controller.signal])
          : controller.signal;
        // Swap only the signal; every other member reads from the page's object.
        var withSignal = new Proxy(options, {
          get: function (target, key) { return key === "signal" ? signal : Reflect.get(target, key); }
        });
        request = original.call(this, withSignal);
      } catch (error) {
        return Promise.reject(error);
      }
      var id = ++next;
      var origin = location.origin;
      var say = function (state) {
        try { report(JSON.stringify({ id: id, kind: kind, state: state, origin: origin })); } catch (e) {}
      };
      pending.set(id, controller);
      say("pending");
      var settle = function () { if (pending.delete(id)) say("settled"); };
      // A derived promise keeps an unhandled rejection unhandled for the page.
      return request.finally(settle);
    };
  };
  var credentials = CredentialsContainer.prototype;
  credentials.get = watch("get", credentials.get);
  credentials.create = watch("create", credentials.create);
  Object.defineProperty(window, "__monocodePasskeyCancel", {
    value: function () {
      var n = pending.size;
      pending.forEach(function (controller) { controller.abort(cancelled()); });
      return n;
    }
  });
})()"#;

/// `navigator.credentials.get` (sign in) or `create` (a new passkey).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasskeyKind {
    Get,
    Create,
}

impl PasskeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PasskeyKind::Get => "get",
            PasskeyKind::Create => "create",
        }
    }
}

/// Why a waiting request cannot reach the phone: hybrid sign-in needs the
/// Mac to hear the phone over Bluetooth (K58).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyBlock {
    /// macOS does not let the app use Bluetooth (denied or restricted).
    BluetoothDenied,
    /// Bluetooth is off.
    BluetoothOff,
}

impl PasskeyBlock {
    pub fn as_str(self) -> &'static str {
        match self {
            PasskeyBlock::BluetoothDenied => "bluetooth_denied",
            PasskeyBlock::BluetoothOff => "bluetooth_off",
        }
    }
}

/// A request's record, for the agent that drives the page (K45, K58).
#[derive(Clone, Debug, PartialEq)]
pub struct PasskeyNote {
    /// From the same counter as the page's permission notes
    /// ([`super::sites::next_note_seq`]).
    pub seq: u64,
    pub kind: PasskeyKind,
    pub origin: String,
    /// Still waiting (false: it settled: signed in, failed or cancelled).
    pub pending: bool,
    pub reason: Option<PasskeyBlock>,
}

impl PasskeyNote {
    /// The record as agents read it.
    pub fn record(&self) -> serde_json::Value {
        let mut record = json!({
            "kind": self.kind.as_str(),
            "origin": self.origin,
            "state": if self.pending { "pending" } else { "settled" },
        });
        if let Some(reason) = self.reason {
            record["reason"] = reason.as_str().into();
        }
        record
    }

    /// One line: `passkeyRequest {…}: what it means.`
    pub fn describe(&self) -> String {
        let meaning = match (self.pending, self.reason) {
            (false, _) => "the passkey request ended (signed in, failed or cancelled)",
            (true, Some(PasskeyBlock::BluetoothDenied)) => {
                "the page waits for a passkey, but macOS does not let Monocode use Bluetooth, which signing in with a phone needs; the user can allow it in System Settings (the page shows a bar), or cancel the request with browser_dialog dismiss"
            }
            (true, Some(PasskeyBlock::BluetoothOff)) => {
                "the page waits for a passkey, but Bluetooth is off, which signing in with a phone needs; ask the user to turn it on, or cancel the request with browser_dialog dismiss"
            }
            (true, None) => {
                "the page waits for a passkey: Chromium's dialog asks the user to scan a QR code with their phone; only the user can finish it. Ask them, or cancel the request with browser_dialog dismiss"
            }
        };
        format!("passkeyRequest {}: {meaning}.", self.record())
    }
}

/// One change the page reported.
#[derive(Clone, Debug, PartialEq)]
pub struct PasskeyChange {
    pub kind: PasskeyKind,
    pub origin: String,
    /// It started waiting (false: it settled, or its document went).
    pub pending: bool,
}

#[derive(Deserialize)]
struct Report {
    id: u64,
    kind: PasskeyKind,
    state: String,
    origin: String,
}

/// The requests that wait, per document (execution context).
#[derive(Default)]
struct Waiting {
    by_context: HashMap<i64, HashMap<u64, (PasskeyKind, String)>>,
}

impl Waiting {
    /// One event of the page; what it changed.
    fn on_event(&mut self, method: &str, params: &serde_json::Value) -> Vec<PasskeyChange> {
        let ended = |requests: HashMap<u64, (PasskeyKind, String)>| {
            let mut ended: Vec<_> = requests.into_iter().collect();
            ended.sort_by_key(|(id, _)| *id);
            (ended.into_iter())
                .map(|(_, (kind, origin))| PasskeyChange {
                    kind,
                    origin,
                    pending: false,
                })
                .collect()
        };
        match method {
            "Runtime.bindingCalled" if params["name"] == BINDING => {
                let Some(context) = params["executionContextId"].as_i64() else {
                    return Vec::new();
                };
                let Some(report) = (params["payload"].as_str())
                    .and_then(|p| serde_json::from_str::<Report>(p).ok())
                else {
                    return Vec::new();
                };
                let requests = self.by_context.entry(context).or_default();
                let pending = report.state == "pending";
                let known = if pending {
                    let fresh = !requests.contains_key(&report.id);
                    requests.insert(report.id, (report.kind, report.origin.clone()));
                    fresh
                } else {
                    requests.remove(&report.id).is_some()
                };
                if requests.is_empty() {
                    self.by_context.remove(&context);
                }
                if !known {
                    return Vec::new();
                }
                vec![PasskeyChange {
                    kind: report.kind,
                    origin: report.origin,
                    pending,
                }]
            }
            "Runtime.executionContextDestroyed" => {
                let Some(context) = params["executionContextId"].as_i64() else {
                    return Vec::new();
                };
                self.by_context
                    .remove(&context)
                    .map(ended)
                    .unwrap_or_default()
            }
            "Runtime.executionContextsCleared" => {
                let all = std::mem::take(&mut self.by_context);
                let mut contexts: Vec<_> = all.into_iter().collect();
                contexts.sort_by_key(|(context, _)| *context);
                contexts.into_iter().flat_map(|(_, r)| ended(r)).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Watches a page's passkey requests while it lives; dropping it stops.
pub struct PasskeyWatch {
    _events: Subscription,
}

/// Starts watching the page whose channel is `cdp` (its log has turned
/// `Runtime` on); `changed` hears each change, on the main thread.
pub fn watch(cdp: &Rc<CdpClient>, changed: impl Fn(PasskeyChange) + 'static) -> PasskeyWatch {
    let waiting = RefCell::new(Waiting::default());
    let events = cdp.subscribe(move |event| {
        // Our own session only: child sessions' contexts are not ours.
        if event.session_id.is_some() {
            return;
        }
        let changes = waiting.borrow_mut().on_event(&event.method, &event.params);
        for change in changes {
            changed(change);
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
    PasskeyWatch { _events: events }
}

/// Cancels the passkey requests the page's main document waits on; they
/// fail as if the user cancelled them. Resolves with how many there were.
pub async fn cancel(cdp: &CdpClient) -> Result<u64, super::CdpError> {
    let answer = cdp
        .call(
            "Runtime.evaluate",
            json!({
                "expression": format!("typeof {CANCEL} === 'function' ? {CANCEL}() : 0"),
                "returnByValue": true,
            }),
            None,
        )
        .await?;
    Ok(answer["result"]["value"].as_u64().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tools::fake::Script;

    fn payload(id: u64, kind: &str, state: &str) -> String {
        json!({"id": id, "kind": kind, "state": state, "origin": "https://accounts.google.com"})
            .to_string()
    }

    #[test]
    fn requests_are_heard_from_start_to_end() {
        let (cdp, script) = Script::client();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        let _watch = watch(&cdp, move |change| {
            log.borrow_mut().push((change.kind, change.pending))
        });
        // The binding and the script are the page's main world's.
        let binding = &script.calls("Runtime.addBinding")[0];
        assert_eq!(binding["name"], BINDING);
        assert!(binding.get("executionContextName").is_none());
        let added = &script.calls("Page.addScriptToEvaluateOnNewDocument")[0];
        assert!(added.get("worldName").is_none());
        assert_eq!(added["runImmediately"], true);

        let call = |context: i64, payload: String| {
            script.emit(
                "Runtime.bindingCalled",
                json!({"name": BINDING, "payload": payload, "executionContextId": context}),
            )
        };
        call(3, payload(1, "get", "pending"));
        // Said twice, heard once; a settle of a request never heard is not.
        call(3, payload(1, "get", "pending"));
        call(3, payload(9, "get", "settled"));
        call(3, payload(1, "get", "settled"));
        assert_eq!(
            *heard.borrow(),
            [(PasskeyKind::Get, true), (PasskeyKind::Get, false)]
        );
        // A document that goes ends its requests; so does a navigation.
        call(4, payload(1, "create", "pending"));
        script.emit(
            "Runtime.executionContextDestroyed",
            json!({"executionContextId": 4}),
        );
        call(5, payload(1, "get", "pending"));
        script.emit("Runtime.executionContextsCleared", json!({}));
        assert_eq!(
            heard.borrow()[2..],
            [
                (PasskeyKind::Create, true),
                (PasskeyKind::Create, false),
                (PasskeyKind::Get, true),
                (PasskeyKind::Get, false),
            ]
        );
        // Other bindings, garbage and iframes' sessions are not ours.
        script.emit(
            "Runtime.bindingCalled",
            json!({"name": "other", "payload": payload(2, "get", "pending"), "executionContextId": 6}),
        );
        call(6, "not json".into());
        script.emit_in(
            "IFRAME",
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": payload(3, "get", "pending"), "executionContextId": 6}),
        );
        assert_eq!(heard.borrow().len(), 6);
    }

    #[test]
    fn notes_tell_agents_what_waits_and_why() {
        let mut note = PasskeyNote {
            seq: 1,
            kind: PasskeyKind::Get,
            origin: "https://accounts.google.com".into(),
            pending: true,
            reason: Some(PasskeyBlock::BluetoothDenied),
        };
        let line = note.describe();
        assert!(line.starts_with(
            r#"passkeyRequest {"kind":"get","origin":"https://accounts.google.com","state":"pending","reason":"bluetooth_denied"}: "#
        ), "{line}");
        assert!(line.contains("browser_dialog dismiss"));
        note.pending = false;
        note.reason = None;
        assert!(note.describe().contains(r#""state":"settled""#));
        assert!(!note.describe().contains("reason"));
    }

    #[test]
    fn the_watcher_wraps_get_and_create_and_can_cancel() {
        for needle in [
            "credentials.get = watch(\"get\"",
            "credentials.create = watch(\"create\"",
            "mediation === \"conditional\"",
            "AbortSignal.any",
            "delete globalThis.__monocodePasskey",
            CANCEL,
        ] {
            assert!(WATCHER.contains(needle), "{needle}");
        }
    }
}
