//! Extensions in web pages, over each page's CDP channel (K69, K42):
//!
//! - Content scripts: every enabled extension's programs
//!   (`monocode_core::browser::extensions::scripts::content_programs`) are
//!   added before the page's own scripts, the isolated one in a world of
//!   the extension's own with its binding, the main-world one (`world:
//!   "MAIN"` scripts, the externally connectable relay) in the page's.
//!   Cross-process frames (`all_frames` into an iframe of another site)
//!   are auto-attached sessions of the page and get the same, paused at
//!   their start until they have it.
//! - Nothing of the layer reaches the page's main world but those scripts:
//!   the binding lives in the isolated world only (`__monocode…`, which the
//!   external CDP endpoint never shows, K57).
//! - The page's events become `webNavigation` and `webRequest` events of
//!   the instances that listen (observation only).
//!
//! Frames: the main frame is 0, others are numbered as they appear.

use std::{
    any::Any,
    collections::HashMap,
    rc::{Rc, Weak},
};

use monocode_core::browser::{
    CdpClient, CdpEvent, Subscription,
    extensions::{self as core, scripts},
};
use serde_json::{Value, json};

use super::{
    InstanceKey, ProfileKey,
    instance::{self, Kind, Meta, Route},
    spawn, with,
};

/// A frame of a page.
#[derive(Clone, Debug)]
pub(crate) struct FrameInfo {
    pub id: i64,
    pub parent: Option<String>,
    pub url: String,
    /// The session the frame's document runs in (`None`: the page's).
    pub session: Option<String>,
}

/// An execution context of a page.
#[derive(Clone, Debug)]
struct ContextInfo {
    frame: String,
    /// Its world's name; empty for the page's own.
    world: String,
    default: bool,
}

pub(crate) struct PageLink {
    pub cdp: Weak<CdpClient>,
    pub profile: ProfileKey,
    /// Frames by CDP frame id.
    pub frames: HashMap<String, FrameInfo>,
    contexts: HashMap<(Option<String>, i64), ContextInfo>,
    next_frame: i64,
    /// The extensions injected, with their program generation.
    injected: HashMap<String, u64>,
    /// webRequest: a request's frame and type, by id.
    requests: HashMap<String, (String, String)>,
}

#[derive(Default)]
pub(crate) struct Pages {
    pub by_tab: HashMap<i64, PageLink>,
    /// Program generation: bumps when the enabled set changes.
    generation: u64,
}

impl Pages {
    pub fn dev_json(&self) -> Value {
        json!(self.by_tab.iter().map(|(tab, p)| json!({
            "tab": tab,
            "profile": super::profile_folder(&p.profile),
            "frames": p.frames.values().map(|f| json!({"id": f.id, "url": f.url, "oopif": f.session.is_some()})).collect::<Vec<_>>(),
            "injected": p.injected.keys().collect::<Vec<_>>(),
        })).collect::<Vec<_>>())
    }
}

impl PageLink {
    fn frame_num(&mut self, frame: &str) -> i64 {
        if let Some(f) = self.frames.get(frame) {
            return f.id;
        }
        self.next_frame += 1;
        let id = self.next_frame;
        self.frames.insert(
            frame.to_owned(),
            FrameInfo {
                id,
                parent: None,
                url: String::new(),
                session: None,
            },
        );
        id
    }
}

/// The programs of an extension, for pages.
struct Programs {
    world: String,
    binding: String,
    isolated: Option<String>,
    main: Option<String>,
}

fn programs(ext: &super::Ext, locale: &str) -> Programs {
    let names = scripts::Names::new(&ext.id, &secret());
    let config = scripts::ShimConfig {
        id: &ext.id,
        manifest: &ext.manifest,
        locale,
        expose: super::dev::dir().is_some(),
    };
    let (isolated, main) = scripts::content_programs(&config, &ext.dir, &names);
    Programs {
        world: names.world,
        binding: names.binding,
        isolated,
        main,
    }
}

/// A per-run secret in the names of the layer's page globals.
fn secret() -> String {
    static SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SECRET
        .get_or_init(|| core::random_token()[..12].to_owned())
        .clone()
}

/// Keeps a page known; dropping it forgets the page.
struct PageGuard {
    tab: i64,
    _subscription: Subscription,
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        let tab = self.tab;
        let profile = with(|rt| rt.pages.by_tab.remove(&tab).map(|p| p.profile)).flatten();
        if let Some(profile) = profile {
            let keys: Vec<InstanceKey> = with(|rt| {
                (rt.instances.keys())
                    .filter(|k| k.profile == profile)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
            for key in keys {
                instance::forget_where(&key, |c| c.tab == Some(tab));
            }
        }
    }
}

/// Called when a page's view gets its CDP channel (`Host::attach_agent`),
/// after the app's own watchers: the passkey watcher wraps WebAuthn
/// before an extension's main-world script does (K58), so a request the
/// extension declines still reaches Chromium's QR sign-in through it.
pub(in crate::browser) fn attach(
    host: &Rc<super::super::Host<monocode_cef::BrowserView>>,
    cdp: &Rc<CdpClient>,
) -> Option<Box<dyn Any>> {
    let profile = super::profile_key(&host.options.borrow().profile)?;
    let tab = host.shared.tab_id.0;
    with(|rt| {
        rt.pages.by_tab.insert(
            tab,
            PageLink {
                cdp: Rc::downgrade(cdp),
                profile: profile.clone(),
                frames: HashMap::new(),
                contexts: HashMap::new(),
                next_frame: 0,
                injected: HashMap::new(),
                requests: HashMap::new(),
            },
        )
    })?;
    let subscription = cdp.subscribe(move |event| on_event(tab, event));
    // Frames of other sites: their own sessions, paused until injected.
    let auto = cdp.call(
        "Target.setAutoAttach",
        json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
               "filter": [{"type": "iframe", "exclude": false}, {"exclude": true}]}),
        None,
    );
    let tree = cdp.call("Page.getFrameTree", json!({}), None);
    spawn(async move {
        let _ = auto.await;
        if let Ok(tree) = tree.await {
            let main = tree["frameTree"]["frame"]["id"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let url = tree["frameTree"]["frame"]["url"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            with(|rt| {
                if let Some(p) = rt.pages.by_tab.get_mut(&tab) {
                    p.frames.insert(
                        main,
                        FrameInfo {
                            id: 0,
                            parent: None,
                            url,
                            session: None,
                        },
                    );
                }
            });
        }
    });
    inject(tab, None);
    Some(Box::new(PageGuard {
        tab,
        _subscription: subscription,
    }))
}

/// Every page gets the current set of extensions at its next document
/// (and the ones not injected yet, now).
pub(crate) fn refresh_all() {
    let tabs: Vec<i64> = with(|rt| {
        rt.pages.generation += 1;
        rt.pages.by_tab.keys().copied().collect()
    })
    .unwrap_or_default();
    for tab in tabs {
        inject(tab, None);
    }
}

/// Injects what is missing into the page's root (or a frame's session).
fn inject(tab: i64, session: Option<String>) {
    let Some((cdp, profile, exts, locale, generation, have)) = with(|rt| {
        let p = rt.pages.by_tab.get(&tab)?;
        let cdp = p.cdp.upgrade()?;
        let exts: Vec<std::sync::Arc<super::Ext>> = rt.exts.values().cloned().collect();
        Some((
            cdp,
            p.profile.clone(),
            exts,
            rt.locale.clone(),
            rt.pages.generation,
            p.injected.clone(),
        ))
    })
    .flatten() else {
        return;
    };
    for ext in exts {
        if !scripts::touches_pages(&ext.manifest) {
            continue;
        }
        if session.is_none() && have.contains_key(&ext.id) {
            continue;
        }
        let key = InstanceKey {
            profile: profile.clone(),
            id: ext.id.clone(),
        };
        // The extension runs in the profile before its scripts do.
        instance::ensure(&key);
        let p = programs(&ext, &locale);
        let s = session.as_deref();
        let mut calls = vec![cdp.call(
            "Runtime.addBinding",
            json!({"name": p.binding, "executionContextName": p.world}),
            s,
        )];
        if let Some(source) = &p.isolated {
            calls.push(cdp.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": source, "worldName": p.world, "runImmediately": true}),
                s,
            ));
        }
        if let Some(source) = &p.main {
            calls.push(cdp.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": source, "runImmediately": true}),
                s,
            ));
        }
        if session.is_none() {
            with(|rt| {
                if let Some(page) = rt.pages.by_tab.get_mut(&tab) {
                    page.injected.insert(ext.id.clone(), generation);
                }
            });
        }
        let id = ext.id.clone();
        spawn(async move {
            for call in calls {
                if let Err(e) = call.await {
                    log::info!("browser: extension {id}: inject into tab {tab} failed: {e}");
                }
            }
        });
    }
}

/// The recv function name of an extension's contexts in pages.
fn recv_name(id: &str, external: bool) -> String {
    let names = scripts::Names::new(id, &secret());
    if external {
        format!("{}_ext", names.recv)
    } else {
        names.recv
    }
}

fn on_event(tab: i64, event: &CdpEvent) {
    let session = event.session_id.clone();
    let p = &event.params;
    match event.method.as_str() {
        "Runtime.bindingCalled" => {
            let name = p["name"].as_str().unwrap_or("");
            if !name.starts_with("__monocodeExt_") {
                return;
            }
            let (Some(context), Some(payload)) = (
                p["executionContextId"].as_i64(),
                p["payload"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok()),
            ) else {
                return;
            };
            from_binding(tab, session, context, name, payload);
        }
        "Runtime.executionContextCreated" => {
            let c = &p["context"];
            let (Some(id), Some(frame)) = (c["id"].as_i64(), c["auxData"]["frameId"].as_str())
            else {
                return;
            };
            let info = ContextInfo {
                frame: frame.to_owned(),
                world: c["name"].as_str().unwrap_or("").to_owned(),
                default: c["auxData"]["isDefault"].as_bool().unwrap_or(false),
            };
            with(|rt| {
                if let Some(page) = rt.pages.by_tab.get_mut(&tab) {
                    page.contexts.insert((session, id), info);
                }
            });
        }
        "Runtime.executionContextDestroyed" => {
            let Some(id) = p["executionContextId"].as_i64() else {
                return;
            };
            with(|rt| {
                rt.pages
                    .by_tab
                    .get_mut(&tab)
                    .map(|page| page.contexts.remove(&(session.clone(), id)))
            });
            for_instances(tab, |key| {
                let s = session.clone();
                instance::forget_where(
                    key,
                    |c| matches!(&c.route, Route::Cdp { tab: t, session: cs, context, .. } if *t == tab && *cs == s && *context == id),
                )
            });
        }
        "Runtime.executionContextsCleared" => {
            with(|rt| {
                rt.pages
                    .by_tab
                    .get_mut(&tab)
                    .map(|page| page.contexts.retain(|(s, _), _| *s != session))
            });
            for_instances(tab, |key| {
                let s = session.clone();
                instance::forget_where(
                    key,
                    |c| matches!(&c.route, Route::Cdp { tab: t, session: cs, .. } if *t == tab && *cs == s),
                )
            });
        }
        "Target.attachedToTarget" => {
            let Some(child) = p["sessionId"].as_str().map(str::to_owned) else {
                return;
            };
            let info = &p["targetInfo"];
            if info["type"] != "iframe" {
                resume(tab, &child);
                return;
            }
            let frame = info["targetId"].as_str().unwrap_or("").to_owned();
            let url = info["url"].as_str().unwrap_or("").to_owned();
            with(|rt| {
                if let Some(page) = rt.pages.by_tab.get_mut(&tab) {
                    page.frame_num(&frame);
                    if let Some(f) = page.frames.get_mut(&frame) {
                        f.session = Some(child.clone());
                        f.url = url;
                    }
                }
            });
            attach_frame(tab, child);
        }
        "Target.detachedFromTarget" => {
            let Some(child) = p["sessionId"].as_str().map(str::to_owned) else {
                return;
            };
            with(|rt| {
                if let Some(page) = rt.pages.by_tab.get_mut(&tab) {
                    page.frames
                        .retain(|_, f| f.session.as_deref() != Some(child.as_str()));
                    page.contexts
                        .retain(|(s, _), _| s.as_deref() != Some(child.as_str()));
                }
            });
            for_instances(tab, |key| {
                let c2 = child.clone();
                instance::forget_where(
                    key,
                    |c| matches!(&c.route, Route::Cdp { tab: t, session: Some(s), .. } if *t == tab && *s == c2),
                )
            });
        }
        "Page.frameAttached" => {
            let (Some(frame), parent) = (p["frameId"].as_str(), p["parentFrameId"].as_str()) else {
                return;
            };
            with(|rt| {
                if let Some(page) = rt.pages.by_tab.get_mut(&tab) {
                    page.frame_num(frame);
                    if let Some(f) = page.frames.get_mut(frame) {
                        f.parent = parent.map(str::to_owned);
                    }
                }
            });
        }
        "Page.frameDetached" => {
            if let Some(frame) = p["frameId"].as_str() {
                let reason = p["reason"].as_str().unwrap_or("");
                // A frame swapped into another process keeps its number.
                if reason != "swap" {
                    with(|rt| {
                        rt.pages
                            .by_tab
                            .get_mut(&tab)
                            .map(|page| page.frames.remove(frame))
                    });
                }
            }
        }
        "Page.frameNavigated" => {
            let f = &p["frame"];
            let (Some(frame), url) = (f["id"].as_str(), f["url"].as_str().unwrap_or("")) else {
                return;
            };
            let main = f["parentId"].is_null() && session.is_none();
            let num = with(|rt| {
                let page = rt.pages.by_tab.get_mut(&tab)?;
                let id = if main {
                    page.frames.retain(|_, fr| fr.id != 0);
                    page.frames.insert(
                        frame.to_owned(),
                        FrameInfo {
                            id: 0,
                            parent: None,
                            url: url.to_owned(),
                            session: None,
                        },
                    );
                    0
                } else {
                    let id = page.frame_num(frame);
                    if let Some(fr) = page.frames.get_mut(frame) {
                        fr.url = url.to_owned();
                        if fr.parent.is_none() {
                            fr.parent = f["parentId"].as_str().map(str::to_owned);
                        }
                    }
                    id
                };
                Some(id)
            })
            .flatten();
            if let Some(num) = num {
                navigation_event(
                    tab,
                    "webNavigation.onCommitted",
                    num,
                    url,
                    json!({"transitionType": "link", "transitionQualifiers": []}),
                );
            }
        }
        "Page.navigatedWithinDocument" => {
            let (Some(frame), url) = (p["frameId"].as_str(), p["url"].as_str().unwrap_or(""))
            else {
                return;
            };
            let num = frame_of(tab, frame);
            let name = if p["navigationType"] == "fragment" {
                "webNavigation.onReferenceFragmentUpdated"
            } else {
                "webNavigation.onHistoryStateUpdated"
            };
            if let Some(num) = num {
                navigation_event(
                    tab,
                    name,
                    num,
                    url,
                    json!({"transitionType": "link", "transitionQualifiers": []}),
                );
            }
        }
        "Page.frameStartedNavigating" => {
            let (Some(frame), url) = (p["frameId"].as_str(), p["url"].as_str().unwrap_or(""))
            else {
                return;
            };
            if let Some(num) = frame_of(tab, frame).or((session.is_none()).then_some(0)) {
                navigation_event(tab, "webNavigation.onBeforeNavigate", num, url, json!({}));
            }
        }
        "Page.domContentEventFired" if session.is_none() => {
            let url = main_url(tab);
            navigation_event(tab, "webNavigation.onDOMContentLoaded", 0, &url, json!({}));
        }
        "Page.loadEventFired" if session.is_none() => {
            let url = main_url(tab);
            navigation_event(tab, "webNavigation.onCompleted", 0, &url, json!({}));
        }
        "Network.requestWillBeSent" if session.is_none() => {
            let r = &p["request"];
            let main = p["frameId"].as_str().and_then(|f| frame_of(tab, f)) == Some(0);
            let kind = request_type(p["type"].as_str().unwrap_or("Other"), main);
            let frame = p["frameId"].as_str().unwrap_or("").to_owned();
            if let Some(id) = p["requestId"].as_str() {
                with(|rt| {
                    rt.pages.by_tab.get_mut(&tab).map(|page| {
                        page.requests
                            .insert(id.to_owned(), (frame.clone(), kind.clone()))
                    })
                });
            }
            let details = json!({"url": r["url"], "method": r["method"], "type": kind});
            request_event(tab, "webRequest.onBeforeRequest", p, details.clone());
            request_event(tab, "webRequest.onSendHeaders", p, details);
        }
        "Network.responseReceived" if session.is_none() => {
            let r = &p["response"];
            let details = json!({"url": r["url"], "statusCode": r["status"], "statusLine": format!("HTTP/1.1 {} {}", r["status"], r["statusText"].as_str().unwrap_or("")), "fromCache": r["fromDiskCache"].as_bool().unwrap_or(false), "ip": r["remoteIPAddress"]});
            request_event(tab, "webRequest.onHeadersReceived", p, details.clone());
            request_event(tab, "webRequest.onResponseStarted", p, details);
        }
        "Network.loadingFinished" if session.is_none() => {
            request_event(tab, "webRequest.onCompleted", p, json!({"statusCode": 200}));
            forget_request(tab, p);
        }
        "Network.loadingFailed" if session.is_none() => {
            let details = json!({"error": p["errorText"], "fromCache": false});
            request_event(tab, "webRequest.onErrorOccurred", p, details);
            if p["type"] == "Document" {
                if let Some(num) = p["frameId"].as_str().and_then(|f| frame_of(tab, f)) {
                    navigation_event(
                        tab,
                        "webNavigation.onErrorOccurred",
                        num,
                        &main_url(tab),
                        json!({"error": p["errorText"]}),
                    );
                }
            }
            forget_request(tab, p);
        }
        _ => {}
    }
}

fn main_url(tab: i64) -> String {
    with(|rt| {
        let page = rt.pages.by_tab.get(&tab)?;
        page.frames
            .values()
            .find(|f| f.id == 0)
            .map(|f| f.url.clone())
    })
    .flatten()
    .unwrap_or_default()
}

fn frame_of(tab: i64, frame: &str) -> Option<i64> {
    with(|rt| rt.pages.by_tab.get(&tab)?.frames.get(frame).map(|f| f.id)).flatten()
}

fn request_type(cdp_type: &str, main: bool) -> String {
    match cdp_type {
        "Document" if main => "main_frame",
        "Document" => "sub_frame",
        "Stylesheet" => "stylesheet",
        "Script" => "script",
        "Image" => "image",
        "Font" => "font",
        "Media" => "media",
        "XHR" | "Fetch" | "EventSource" => "xmlhttprequest",
        "WebSocket" => "websocket",
        "Ping" => "ping",
        "CSPViolationReport" => "csp_report",
        _ => "other",
    }
    .into()
}

fn forget_request(tab: i64, p: &Value) {
    if let Some(id) = p["requestId"].as_str() {
        with(|rt| {
            rt.pages
                .by_tab
                .get_mut(&tab)
                .map(|page| page.requests.remove(id))
        });
    }
}

fn for_instances(tab: i64, mut f: impl FnMut(&InstanceKey)) {
    let keys: Vec<InstanceKey> = with(|rt| {
        let profile = rt.pages.by_tab.get(&tab).map(|p| p.profile.clone())?;
        Some(
            (rt.instances.keys())
                .filter(|k| k.profile == profile)
                .cloned()
                .collect(),
        )
    })
    .flatten()
    .unwrap_or_default();
    for key in &keys {
        f(key);
    }
}

/// A `webNavigation` event, for instances with the permission that listen.
fn navigation_event(tab: i64, name: &str, frame: i64, url: &str, extra: Value) {
    let parent = parent_of(tab, frame);
    for_instances(tab, |key| {
        let listens = with(|rt| {
            let i = rt.instances.get(key)?;
            Some(i.ext.has("webNavigation") && i.listens(name))
        })
        .flatten()
        .unwrap_or(false);
        if !listens {
            return;
        }
        let mut details = json!({
            "tabId": tab, "frameId": frame, "parentFrameId": parent, "url": url,
            "timeStamp": instance::epoch_ms(), "processId": -1,
            "frameType": if frame == 0 { "outermost_frame" } else { "sub_frame" },
            "documentLifecycle": "active", "documentId": document_id(tab, frame),
        });
        if let (Value::Object(d), Value::Object(e)) = (&mut details, &extra) {
            for (k, v) in e {
                d.insert(k.clone(), v.clone());
            }
        }
        instance::broadcast(key, name, json!([details]), true);
    });
}

fn parent_of(tab: i64, frame: i64) -> i64 {
    if frame == 0 {
        return -1;
    }
    with(|rt| {
        let page = rt.pages.by_tab.get(&tab)?;
        let (_, f) = page.frames.iter().find(|(_, f)| f.id == frame)?;
        let parent = f.parent.as_ref()?;
        page.frames.get(parent).map(|p| p.id)
    })
    .flatten()
    .unwrap_or(0)
}

pub(crate) fn document_id(tab: i64, frame: i64) -> String {
    format!("{tab:08X}{frame:08X}")
}

/// A `webRequest` event (observation), for instances with the permission
/// and host access to the URL that listen.
fn request_event(tab: i64, name: &str, p: &Value, mut details: Value) {
    let id = p["requestId"].as_str().unwrap_or("").to_owned();
    let (frame, kind) = with(|rt| rt.pages.by_tab.get(&tab)?.requests.get(&id).cloned())
        .flatten()
        .unwrap_or_default();
    let frame_num = frame_of(tab, &frame).unwrap_or(0);
    let url = details["url"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default();
    for_instances(tab, |key| {
        let wants = with(|rt| {
            let i = rt.instances.get(key)?;
            Some(
                i.ext.has("webRequest")
                    && i.listens(name)
                    && (url.is_empty() || i.ext.hosts.matches(&url)),
            )
        })
        .flatten()
        .unwrap_or(false);
        if !wants {
            return;
        }
        if let Value::Object(d) = &mut details {
            d.insert("requestId".into(), id.clone().into());
            d.insert("tabId".into(), tab.into());
            d.insert("frameId".into(), frame_num.into());
            d.insert("parentFrameId".into(), parent_of(tab, frame_num).into());
            d.insert("type".into(), kind.clone().into());
            d.insert("timeStamp".into(), instance::epoch_ms().into());
            d.insert("documentId".into(), document_id(tab, frame_num).into());
            d.insert(
                "frameType".into(),
                (if frame_num == 0 {
                    "outermost_frame"
                } else {
                    "sub_frame"
                })
                .into(),
            );
            d.entry("method").or_insert("GET".into());
        }
        instance::broadcast(key, name, json!([details.clone()]), true);
    });
}

/// A cross-process frame's session: the scripts, then it may run.
fn attach_frame(tab: i64, session: String) {
    let Some(cdp) = cdp_of(tab) else { return };
    let s = Some(session.as_str());
    let first = [
        cdp.call("Runtime.enable", json!({}), s),
        cdp.call("Page.enable", json!({}), s),
        cdp.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true,
                   "filter": [{"type": "iframe", "exclude": false}, {"exclude": true}]}),
            s,
        ),
    ];
    inject(tab, Some(session.clone()));
    spawn(async move {
        for call in first {
            let _ = call.await;
        }
        resume(tab, &session);
    });
}

fn resume(tab: i64, session: &str) {
    if let Some(cdp) = cdp_of(tab) {
        let call = cdp.call("Runtime.runIfWaitingForDebugger", json!({}), Some(session));
        spawn(async move {
            let _ = call.await;
        });
    }
}

pub(crate) fn cdp_of(tab: i64) -> Option<Rc<CdpClient>> {
    with(|rt| rt.pages.by_tab.get(&tab)?.cdp.upgrade()).flatten()
}

/// A message from a page's isolated world.
fn from_binding(tab: i64, session: Option<String>, context: i64, binding: &str, payload: Value) {
    let Some((profile, frame_num, top)) = with(|rt| {
        let page = rt.pages.by_tab.get_mut(&tab)?;
        let frame = page
            .contexts
            .get(&(session.clone(), context))
            .map(|c| c.frame.clone());
        let num = frame.map(|f| page.frame_num(&f));
        Some((page.profile.clone(), num, payload["top"].as_bool()))
    })
    .flatten() else {
        return;
    };
    // Which extension: the binding names it.
    let Some(id) = with(|rt| {
        (rt.exts.keys())
            .find(|id| binding == format!("__monocodeExt_{}", &id[..id.len().min(12)]))
            .cloned()
    })
    .flatten() else {
        return;
    };
    let key = InstanceKey {
        profile,
        id: id.clone(),
    };
    if !instance::ensure(&key) {
        return;
    }
    let ctx = payload["ctx"].as_str().unwrap_or("").to_owned();
    if ctx.is_empty() {
        return;
    }
    let kind = match payload["api"].as_str() {
        _ if payload.pointer("/args/0/kind").and_then(Value::as_str) == Some("external") => {
            Kind::External
        }
        _ => Kind::Content,
    };
    let known = with(|rt| {
        rt.instances
            .get(&key)
            .map(|i| i.ctxs.get(&ctx).map(|c| c.kind))
    })
    .flatten()
    .flatten();
    let kind = known.unwrap_or(kind);
    let url = payload
        .pointer("/args/0/url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let frame = frame_num.unwrap_or(if top == Some(false) { -1 } else { 0 });
    instance::ensure_ctx(
        &key,
        Meta {
            ctx: ctx.clone(),
            kind,
            url,
            top: top.unwrap_or(frame == 0),
            browser_id: None,
            route: Some(Route::Cdp {
                tab,
                session,
                context,
                recv: recv_name(&id, kind == Kind::External),
            }),
            tab: Some(tab),
            frame_id: Some(frame),
            hello: false,
        },
    );
    if payload["t"] != "call" {
        return;
    }
    let call_id = payload["id"].clone();
    let api = payload["api"].as_str().unwrap_or("").to_owned();
    let args = payload["args"].as_array().cloned().unwrap_or_default();
    // An externally connectable page reaches only messaging.
    if kind == Kind::External
        && !matches!(
            api.as_str(),
            "ctx.hello"
                | "ctx.listen"
                | "runtime.sendMessage"
                | "runtime.connect"
                | "port.post"
                | "port.disconnect"
                | "runtime.respond"
        )
    {
        return instance::deliver(
            &key,
            &ctx,
            json!({"t": "reply", "id": call_id, "ok": false, "error": "Not allowed"}),
        );
    }
    spawn(async move {
        let result = super::dispatch::call(&key, &ctx, &api, args).await;
        let reply = match result {
            Ok(value) => json!({"t": "reply", "id": call_id, "ok": true, "value": value}),
            Err(error) => json!({"t": "reply", "id": call_id, "ok": false, "error": error}),
        };
        instance::deliver(&key, &ctx, reply);
    });
}

/// Runs `recv(message)` in a page's context.
pub(crate) fn evaluate_in(
    tab: i64,
    session: Option<String>,
    context: i64,
    recv: &str,
    message: &Value,
) {
    let Some(cdp) = cdp_of(tab) else { return };
    let expression = format!("globalThis.{recv} && globalThis.{recv}({message})");
    let call = cdp.call(
        "Runtime.evaluate",
        json!({"expression": expression, "contextId": context, "silent": true}),
        session.as_deref(),
    );
    spawn(async move {
        let _ = call.await;
    });
}

/// The frames of a tab (`webNavigation.getAllFrames`).
pub(crate) fn frames(tab: i64) -> Vec<FrameInfo> {
    with(|rt| {
        let page = rt.pages.by_tab.get(&tab)?;
        let mut frames: Vec<FrameInfo> = page.frames.values().cloned().collect();
        frames.sort_by_key(|f| f.id);
        Some(frames)
    })
    .flatten()
    .unwrap_or_default()
}

/// The parent's number of a frame.
pub(crate) fn parent_frame(tab: i64, frame: i64) -> i64 {
    parent_of(tab, frame)
}

/// Where `scripting` runs in a frame: `(session, context)`. The
/// extension's isolated world (made if the frame has none yet) or, for
/// `MAIN`, the page's own.
pub(crate) async fn context_for(
    tab: i64,
    frame: i64,
    ext: &str,
    main: bool,
) -> Option<(Option<String>, Option<i64>)> {
    let world = scripts::Names::new(ext, &secret()).world;
    let found = with(|rt| {
        let page = rt.pages.by_tab.get(&tab)?;
        let (cdp_frame, info) = page.frames.iter().find(|(_, f)| f.id == frame)?;
        let session = info.session.clone();
        let context = (page.contexts.iter())
            .find(|((s, _), c)| {
                c.frame == *cdp_frame
                    && *s == session
                    && if main { c.default } else { c.world == world }
            })
            .map(|((_, id), _)| *id);
        Some((cdp_frame.clone(), session, context))
    })
    .flatten()?;
    let (cdp_frame, session, context) = found;
    if let Some(context) = context {
        return Some((session, Some(context)));
    }
    if main {
        // The main frame's own world, when no context is known.
        return Some((session, None));
    }
    let cdp = cdp_of(tab)?;
    let made = cdp
        .call(
            "Page.createIsolatedWorld",
            json!({"frameId": cdp_frame, "worldName": world, "grantUniveralAccess": false}),
            session.as_deref(),
        )
        .await
        .ok()?;
    Some((session, made["executionContextId"].as_i64()))
}
