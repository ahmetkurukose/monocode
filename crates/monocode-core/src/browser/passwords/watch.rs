//! The page side of the password manager: the watcher (`watch.js`) in an
//! isolated world of every document, its reports, and filling a login
//! there (context.md K56, K57). The page's own scripts can neither see the
//! watcher nor call it; what it reports reaches only the app's own CDP
//! session (the external endpoint's clients have sessions of their own,
//! and the shared-session fallback drops our bindings' calls).

use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::browser::{CdpClient, CdpError, Subscription};

/// The isolated world the watcher runs and fills in.
pub const WORLD: &str = "monocode-passwords";
/// The binding it reports through (JSON text).
pub const BINDING: &str = "__monocodePasswordsReport";
const WATCHER: &str = include_str!("watch.js");

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Username,
    Password,
}

/// What the watcher says.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// A user name or password field got the focus; `rect` is in CSS
    /// pixels of its frame's viewport (`top`: the page's own frame).
    Focus {
        kind: FieldKind,
        field: String,
        rect: Rect,
        top: bool,
    },
    Blur {
        field: String,
    },
    /// A form of the document at `url` was sent with a password.
    Submit {
        url: String,
        username: String,
        password: String,
    },
    /// A user name was sent on its own (a two-step sign-in's first step).
    Username {
        url: String,
        username: String,
    },
    /// The sent form's password field went away in its document.
    Gone,
    /// Whether the (top) document shows a password field.
    Forms {
        password: bool,
    },
}

/// A [`Signal`] and the document (execution context) it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub context: i64,
    pub signal: Signal,
}

/// Reads one binding payload.
pub fn parse(payload: &str) -> Option<Signal> {
    let v: Value = serde_json::from_str(payload).ok()?;
    let text = |name: &str| v[name].as_str().unwrap_or("").to_owned();
    Some(match v["t"].as_str()? {
        "focus" => Signal::Focus {
            kind: match v["kind"].as_str()? {
                "password" => FieldKind::Password,
                "username" => FieldKind::Username,
                _ => return None,
            },
            field: text("field"),
            rect: serde_json::from_value(v["rect"].clone()).unwrap_or_default(),
            top: v["top"].as_bool().unwrap_or(false),
        },
        "blur" => Signal::Blur {
            field: text("field"),
        },
        "submit" => Signal::Submit {
            url: text("url"),
            username: text("username"),
            password: text("password"),
        },
        "username" => Signal::Username {
            url: text("url"),
            username: text("username"),
        },
        "gone" => Signal::Gone,
        "forms" => Signal::Forms {
            password: v["password"].as_bool()?,
        },
        _ => return None,
    })
}

/// Watches a page's forms while it lives; dropping it stops.
pub struct PasswordWatch {
    _events: Subscription,
}

/// Starts the watcher in every document of the page whose channel is
/// `cdp` (its log has turned `Runtime` on); `heard` gets each report, on
/// the main thread.
pub fn watch(cdp: &Rc<CdpClient>, heard: impl Fn(Report) + 'static) -> PasswordWatch {
    let events = cdp.subscribe(move |event| {
        // Our own session only: child sessions' contexts are not ours.
        if event.session_id.is_some()
            || event.method != "Runtime.bindingCalled"
            || event.params["name"] != BINDING
        {
            return;
        }
        let (Some(context), Some(signal)) = (
            event.params["executionContextId"].as_i64(),
            event.params["payload"].as_str().and_then(parse),
        ) else {
            return;
        };
        heard(Report { context, signal });
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
    PasswordWatch { _events: events }
}

/// Where to fill.
#[derive(Clone, Debug, PartialEq)]
pub enum FillTarget {
    /// The field the watcher reported focused, in its document.
    Field { context: i64, field: String },
    /// A DOM node (an agent's ref) of the page or of a child session.
    Node {
        backend: i64,
        session: Option<String>,
        frame: Option<String>,
    },
    /// The page's first login form.
    Page,
}

/// What [`fill`] filled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Filled {
    pub username: bool,
    pub password: bool,
}

const FILL_IN_CONTEXT: &str = "function (u, p, f) { var api = globalThis.__monocodePasswords; \
return api ? api.fill(u, p, f, null) : {ok: false, why: 'not ready'}; }";
const FILL_AT_NODE: &str = "function (u, p) { var api = globalThis.__monocodePasswords; \
return api ? api.fill(u, p, null, this) : {ok: false, why: 'not ready'}; }";

fn cdp_text(error: CdpError) -> String {
    error.to_string()
}

/// The watcher's world in `frame` (the main frame when `None`) of `session`.
async fn world(
    cdp: &CdpClient,
    session: Option<&str>,
    frame: Option<String>,
) -> Result<i64, String> {
    let frame = match frame {
        Some(frame) => frame,
        None => {
            let tree = cdp
                .call("Page.getFrameTree", json!({}), session)
                .await
                .map_err(cdp_text)?;
            tree["frameTree"]["frame"]["id"]
                .as_str()
                .ok_or("the page has no frame")?
                .to_owned()
        }
    };
    let made = cdp
        .call(
            "Page.createIsolatedWorld",
            json!({"frameId": frame, "worldName": WORLD, "grantUniveralAccess": false}),
            session,
        )
        .await
        .map_err(cdp_text)?;
    made["executionContextId"]
        .as_i64()
        .ok_or_else(|| "the page made no world to fill in".to_owned())
}

/// Fills `username` and `password` into the login form at `target`, in
/// the watcher's world, so that the page's frameworks see an edit. The
/// password never leaves the app's own CDP session.
pub async fn fill(
    cdp: &CdpClient,
    target: &FillTarget,
    username: &str,
    password: &str,
) -> Result<Filled, String> {
    let args = |extra: Option<&str>| {
        let mut a = vec![json!({"value": username}), json!({"value": password})];
        if let Some(extra) = extra {
            a.push(json!({"value": extra}));
        }
        a
    };
    for attempt in 0..2 {
        let answer = match target {
            FillTarget::Field { context, field } => {
                cdp.call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": FILL_IN_CONTEXT,
                        "executionContextId": context,
                        "arguments": args(Some(field)),
                        "returnByValue": true,
                    }),
                    None,
                )
                .await
            }
            FillTarget::Page => {
                let context = world(cdp, None, None).await?;
                if attempt > 0 {
                    install(cdp, None, context).await?;
                }
                cdp.call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": FILL_IN_CONTEXT,
                        "executionContextId": context,
                        "arguments": args(Some("")),
                        "returnByValue": true,
                    }),
                    None,
                )
                .await
            }
            FillTarget::Node {
                backend,
                session,
                frame,
            } => {
                let session = session.as_deref();
                let context = world(cdp, session, frame.clone()).await?;
                if attempt > 0 {
                    install(cdp, session, context).await?;
                }
                let node = cdp
                    .call(
                        "DOM.resolveNode",
                        json!({"backendNodeId": backend, "executionContextId": context}),
                        session,
                    )
                    .await
                    .map_err(cdp_text)?;
                let object = node["object"]["objectId"]
                    .as_str()
                    .ok_or("the element is gone")?
                    .to_owned();
                cdp.call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": FILL_AT_NODE,
                        "objectId": object,
                        "arguments": args(None),
                        "returnByValue": true,
                    }),
                    session,
                )
                .await
            }
        };
        let answer = answer.map_err(cdp_text)?;
        if let Some(details) = answer.get("exceptionDetails") {
            return Err(details["exception"]["description"]
                .as_str()
                .unwrap_or("the fill failed")
                .to_owned());
        }
        let value = &answer["result"]["value"];
        if value["ok"] == true {
            return Ok(Filled {
                username: value["username"] == true,
                password: value["password"] == true,
            });
        }
        let why = value["why"].as_str().unwrap_or("nothing to fill").to_owned();
        // A world made after the watcher's script: it runs there now.
        if why == "not ready" && attempt == 0 && !matches!(target, FillTarget::Field { .. }) {
            continue;
        }
        return Err(why);
    }
    Err("nothing to fill".into())
}

/// Runs the watcher in a world that lacks it.
async fn install(cdp: &CdpClient, session: Option<&str>, context: i64) -> Result<(), String> {
    cdp.call(
        "Runtime.evaluate",
        json!({"expression": WATCHER, "contextId": context}),
        session,
    )
    .await
    .map(|_| ())
    .map_err(cdp_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tools::fake::Script;
    use std::cell::RefCell;

    #[test]
    fn the_watcher_reports_only_through_its_binding_in_its_world() {
        let (cdp, script) = Script::client();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        let _watch = watch(&cdp, move |report| log.borrow_mut().push(report));
        let binding = &script.calls("Runtime.addBinding")[0];
        assert_eq!(binding["executionContextName"], WORLD);
        let added = &script.calls("Page.addScriptToEvaluateOnNewDocument")[0];
        assert_eq!(added["worldName"], WORLD);
        assert_eq!(added["runImmediately"], true);

        let call = |name: &str, payload: Value| {
            script.emit(
                "Runtime.bindingCalled",
                json!({"name": name, "payload": payload.to_string(), "executionContextId": 4}),
            )
        };
        call(
            BINDING,
            json!({"t": "submit", "url": "https://a.test/", "username": "alice", "password": "pw"}),
        );
        call("__other", json!({"t": "gone"}));
        call(BINDING, json!({"t": "nonsense"}));
        script.emit_in(
            "IFRAME",
            "Runtime.bindingCalled",
            json!({"name": BINDING, "payload": "{\"t\":\"gone\"}", "executionContextId": 9}),
        );
        call(BINDING, json!({"t": "forms", "password": false}));
        assert_eq!(
            *heard.borrow(),
            [
                Report {
                    context: 4,
                    signal: Signal::Submit {
                        url: "https://a.test/".into(),
                        username: "alice".into(),
                        password: "pw".into()
                    }
                },
                Report {
                    context: 4,
                    signal: Signal::Forms { password: false }
                }
            ]
        );
    }

    #[test]
    fn reports_parse_into_signals() {
        assert_eq!(
            parse(r#"{"t":"focus","kind":"password","field":"f1","rect":{"x":1,"y":2,"width":3,"height":4},"top":true}"#),
            Some(Signal::Focus {
                kind: FieldKind::Password,
                field: "f1".into(),
                rect: Rect { x: 1., y: 2., width: 3., height: 4. },
                top: true
            })
        );
        assert_eq!(
            parse(r#"{"t":"blur","field":"f1"}"#),
            Some(Signal::Blur { field: "f1".into() })
        );
        assert_eq!(
            parse(r#"{"t":"username","url":"https://a.test/","username":"al"}"#),
            Some(Signal::Username {
                url: "https://a.test/".into(),
                username: "al".into()
            })
        );
        assert_eq!(parse(r#"{"t":"gone"}"#), Some(Signal::Gone));
        assert_eq!(parse(r#"{"t":"focus","kind":"other"}"#), None);
        assert_eq!(parse("not json"), None);
    }

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        futures_executor::block_on(future)
    }

    #[test]
    fn a_fill_runs_in_the_watchers_world_with_the_values_as_arguments() {
        let (cdp, script) = Script::client();
        script.reply(
            "Page.getFrameTree",
            json!({"frameTree": {"frame": {"id": "F"}}}),
        );
        script.reply("Page.createIsolatedWorld", json!({"executionContextId": 12}));
        script.reply(
            "Runtime.callFunctionOn",
            json!({"result": {"type": "object", "value": {"ok": true, "username": true, "password": true}}}),
        );
        let filled = run(fill(&cdp, &FillTarget::Page, "alice", "s3cret!")).unwrap();
        assert_eq!(
            filled,
            Filled {
                username: true,
                password: true
            }
        );
        let world = &script.calls("Page.createIsolatedWorld")[0];
        assert_eq!((world["frameId"].as_str(), world["worldName"].as_str()), (Some("F"), Some(WORLD)));
        let call = &script.calls("Runtime.callFunctionOn")[0];
        assert_eq!(call["executionContextId"], 12);
        assert_eq!(call["arguments"][1]["value"], "s3cret!");
        // The password is an argument, never part of code the page could see.
        assert!(!call["functionDeclaration"].as_str().unwrap().contains("s3cret"));
    }

    #[test]
    fn a_fill_at_an_agents_ref_resolves_the_node_in_the_world() {
        let (cdp, script) = Script::client();
        script.reply("Page.createIsolatedWorld", json!({"executionContextId": 5}));
        script.on("DOM.resolveNode", |p, _| {
            Ok(json!({"object": {"objectId": format!("o-{}-{}", p["backendNodeId"], p["executionContextId"])}}))
        });
        // The first world lacks the watcher: it is installed, then filled.
        let calls = Rc::new(std::cell::Cell::new(0));
        let seen = calls.clone();
        script.on("Runtime.callFunctionOn", move |_, _| {
            seen.set(seen.get() + 1);
            Ok(if seen.get() == 1 {
                json!({"result": {"value": {"ok": false, "why": "not ready"}}})
            } else {
                json!({"result": {"value": {"ok": true, "username": false, "password": true}}})
            })
        });
        let target = FillTarget::Node {
            backend: 33,
            session: None,
            frame: Some("F2".into()),
        };
        let filled = run(fill(&cdp, &target, "", "pw12")).unwrap();
        assert!(filled.password && !filled.username);
        assert_eq!(script.calls("Page.createIsolatedWorld")[0]["frameId"], "F2");
        assert_eq!(script.calls("Runtime.callFunctionOn")[1]["objectId"], "o-33-5");
        let installed = &script.calls("Runtime.evaluate")[0];
        assert_eq!(installed["contextId"], 5);

        // Nothing to fill: the watcher's reason.
        script.reply(
            "Runtime.callFunctionOn",
            json!({"result": {"value": {"ok": false, "why": "no login form"}}}),
        );
        let field = FillTarget::Field {
            context: 3,
            field: "f1".into(),
        };
        assert_eq!(run(fill(&cdp, &field, "a", "b")).unwrap_err(), "no login form");
    }
}
