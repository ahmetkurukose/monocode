//! One extension running in one browser profile: its contexts and how
//! messages reach them, its hidden background host, offscreen document,
//! storage, alarms and action state.
//!
//! Contexts: the background worker, extension pages (popup, tabs, frames
//! in web pages, the offscreen document) poll the bridge; content scripts
//! and externally connectable pages are execution contexts of a page,
//! reached over its CDP channel (`pages.rs`). `runtime` messages go to
//! the contexts that listen, the first answer wins; ports link two ends.
//!
//! The background host is a page no one sees, its worker the extension's
//! service worker; it lives as long as the instance (Chrome keeps a busy
//! worker alive too) and comes back when its renderer crashes, at most
//! [`RESTARTS`] times in [`RESTART_WINDOW`]. `runtime.onInstalled` fires
//! when the profile meets a version first, `onStartup` once per app run.

use std::{
    any::Any,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use monocode_cef::extensions::Responder;
use monocode_core::browser::{
    CdpClient,
    extensions::{self as core, api, storage::Storage},
};
use serde_json::{Value, json};

use super::{Ext, ProfileKey, serve::Bridge, spawn, with};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstanceKey {
    pub profile: ProfileKey,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Background,
    Popup,
    Offscreen,
    /// An extension page as a tab's document.
    Tab,
    /// An extension page in a frame of a web page (web-accessible).
    Frame,
    Content,
    External,
}

impl Kind {
    fn parse(s: &str) -> Self {
        match s {
            "background" => Kind::Background,
            "content" => Kind::Content,
            "external" => Kind::External,
            _ => Kind::Tab,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Background => "background",
            Kind::Popup => "popup",
            Kind::Offscreen => "offscreen",
            Kind::Tab => "tab",
            Kind::Frame => "frame",
            Kind::Content => "content",
            Kind::External => "external",
        }
    }

    /// `runtime.getContexts`' `contextType`.
    pub fn context_type(self) -> Option<&'static str> {
        Some(match self {
            Kind::Background => "BACKGROUND",
            Kind::Popup => "POPUP",
            Kind::Offscreen => "OFFSCREEN_DOCUMENT",
            Kind::Tab | Kind::Frame => "TAB",
            Kind::Content | Kind::External => return None,
        })
    }

    /// The extension's own pages (not scripts in web pages).
    pub fn is_extension_page(self) -> bool {
        !matches!(self, Kind::Content | Kind::External)
    }
}

pub(crate) enum Route {
    /// The bridge's long poll.
    Poll {
        queue: VecDeque<Value>,
        poller: Option<(Responder, u64)>,
    },
    /// An execution context of a page, over CDP.
    Cdp {
        tab: i64,
        session: Option<String>,
        context: i64,
        recv: String,
    },
}

pub(crate) struct Ctx {
    pub kind: Kind,
    pub url: String,
    pub top: bool,
    pub tab: Option<i64>,
    pub frame_id: i64,
    pub route: Route,
    pub listening: HashSet<String>,
    pub seen: Instant,
    pub browser_id: Option<i32>,
}

pub(crate) struct Pending {
    pub targets: Vec<String>,
    pub reply: async_channel::Sender<Result<Value, String>>,
}

/// A page the layer hosts for the extension, out of sight.
pub(crate) struct HostPage {
    pub view: Rc<monocode_cef::BrowserView>,
    pub browser_id: i32,
    pub url: String,
    /// Development hook only: its CDP client.
    pub cdp: Option<Rc<CdpClient>>,
    pub _attachment: Option<Box<dyn Any>>,
}

impl Drop for HostPage {
    fn drop(&mut self) {
        super::serve::remove_host(self.browser_id);
        self.view.close();
    }
}

pub(crate) struct Alarm {
    pub scheduled: f64,
    pub period: Option<f64>,
    pub generation: u64,
}

pub(crate) struct Instance {
    pub key: InstanceKey,
    pub ext: Arc<Ext>,
    pub ctxs: HashMap<String, Ctx>,
    /// A port's ends: the opener, then the receivers.
    pub ports: HashMap<String, Vec<String>>,
    pub pending: HashMap<String, Pending>,
    pub storage: Option<Storage>,
    /// `storage.session` reaches content scripts too.
    pub session_untrusted: bool,
    pub background: Option<HostPage>,
    pub offscreen: Option<HostPage>,
    pub popup: Option<super::toolbar::Popup>,
    pub action: super::toolbar::ActionState,
    pub alarms: HashMap<String, Alarm>,
    pub privacy: HashMap<String, Value>,
    /// What the background hears at its next hello.
    pub lifecycle: Option<Value>,
    pub stopping: bool,
    pub restarts: Vec<Instant>,
    pub next: u64,
    pub started: Instant,
    /// How long the background took to say hello, the first time.
    pub ready_in: Option<Duration>,
    /// Development hook: recent calls.
    pub calls: VecDeque<String>,
}

/// A crashing background comes back this often in this long, no more.
const RESTARTS: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(300);
/// A polling context not heard of for this long is gone.
const STALE: Duration = Duration::from_secs(70);
const POLL: Duration = Duration::from_secs(25);

pub(crate) fn epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64() * 1000.)
}

impl Instance {
    pub fn next_id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    /// `runtime.MessageSender` for a message or port from `ctx`.
    pub fn sender(&self, ctx: &str, tabs: &super::tabs::Tracker) -> Value {
        let Some(c) = self.ctxs.get(ctx) else {
            return json!({"id": self.ext.id});
        };
        let origin = core::pattern::origin_of(&c.url).unwrap_or_default();
        let mut v = json!({
            "id": self.ext.id, "url": c.url, "origin": origin,
            "documentId": ctx, "documentLifecycle": "active",
        });
        if let Some(tab) = c.tab {
            if let Some(t) = tabs.tab_json(&self.key.profile, tab, true) {
                v["tab"] = t;
            }
            v["frameId"] = c.frame_id.into();
        }
        if c.kind == Kind::External {
            v.as_object_mut().expect("object").remove("id");
        }
        v
    }

    /// Contexts that hear `event` (`runtime.onMessage`, …), but `except`.
    pub fn listeners(&self, event: &str, except: &str) -> Vec<String> {
        let now = Instant::now();
        (self.ctxs.iter())
            .filter(|(id, c)| {
                id.as_str() != except
                    && c.listening.contains(event)
                    && (matches!(c.route, Route::Cdp { .. })
                        || c.kind == Kind::Background
                        || now.duration_since(c.seen) < STALE)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn listens(&self, event: &str) -> bool {
        self.ctxs.values().any(|c| c.listening.contains(event))
    }
}

/// Starts the extension in the profile unless it runs (CEF must run).
pub(crate) fn ensure(key: &InstanceKey) -> bool {
    if !super::super::engine_started() {
        return false;
    }
    let Some(ext) = with(|rt| {
        if rt.instances.contains_key(key) {
            return None;
        }
        rt.exts.get(&key.id).cloned()
    })
    .flatten() else {
        return with(|rt| rt.instances.contains_key(key)).unwrap_or(false);
    };
    start(key, ext);
    true
}

fn start(key: &InstanceKey, ext: Arc<Ext>) {
    let started = Instant::now();
    let Some(data_dir) = with(|rt| {
        rt.registry
            .lock()
            .unwrap()
            .data_dir(&super::profile_folder(&key.profile), &key.id)
    }) else {
        return;
    };
    let unlimited = ext.has("unlimitedStorage");
    let storage = match Storage::open(&data_dir.join("storage.sqlite"), unlimited) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("browser: extension {}: storage unavailable: {e}", key.id);
            None
        }
    };
    // onInstalled when this profile meets the version first; onStartup
    // when it ran here before (Chrome: once per browser start).
    let state_file = data_dir.join("runtime.json");
    let previous: Option<String> = std::fs::read(&state_file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["version"].as_str().map(str::to_owned));
    let version = ext.manifest.version.clone();
    let lifecycle = match &previous {
        None => json!({"installed": "install"}),
        Some(p) if *p != version => json!({"installed": "update", "previousVersion": p}),
        Some(_) => json!({"startup": true}),
    };
    let _ = std::fs::create_dir_all(&data_dir);
    let _ = std::fs::write(&state_file, json!({"version": version}).to_string());
    let instance = Instance {
        key: key.clone(),
        ext: ext.clone(),
        ctxs: HashMap::new(),
        ports: HashMap::new(),
        pending: HashMap::new(),
        storage,
        session_untrusted: false,
        background: None,
        offscreen: None,
        popup: None,
        action: super::toolbar::ActionState::default(),
        alarms: HashMap::new(),
        privacy: HashMap::new(),
        lifecycle: Some(lifecycle),
        stopping: false,
        restarts: Vec::new(),
        next: 0,
        started,
        ready_in: None,
        calls: VecDeque::new(),
    };
    with(|rt| rt.instances.insert(key.clone(), instance));
    log::warn!(
        "browser: extension {} {} starts in profile {}",
        ext.manifest.name,
        version,
        super::profile_folder(&key.profile)
    );
    if ext.manifest.background.is_some() {
        open_background(key);
    }
    super::changed();
}

/// Stops the extension in the profile: its hosts close, its contexts hear
/// nothing more.
pub(crate) fn stop(key: &InstanceKey) {
    let Some(mut instance) = with(|rt| rt.instances.remove(key)).flatten() else {
        return;
    };
    instance.stopping = true;
    for (_, ctx) in instance.ctxs.drain() {
        if let Route::Poll {
            poller: Some((r, _)),
            ..
        } = ctx.route
        {
            r.json(410, &json!([]));
        }
    }
    for (_, pending) in instance.pending.drain() {
        let _ = pending
            .reply
            .try_send(Err("Extension context invalidated.".into()));
    }
    if let Some(popup) = instance.popup.take() {
        popup.close();
    }
    drop(instance);
    super::changed();
}

// ---- hosts ----

fn open_background(key: &InstanceKey) {
    let url = format!("{}__mcext/background.html", core::base_url(&key.id));
    match open_host(key, &url, false) {
        Some(host) => {
            let id = host.browser_id;
            with(|rt| {
                if let Some(i) = rt.instances.get_mut(key) {
                    i.background = Some(host);
                }
            });
            log::info!("browser: extension {}: background host {id}", key.id);
        }
        None => log::warn!("browser: extension {}: no background host", key.id),
    }
}

/// A hidden page for the extension at `url`, and its events heard.
pub(crate) fn open_host(key: &InstanceKey, url: &str, clipboard: bool) -> Option<HostPage> {
    super::serve::expect(url);
    let options = monocode_cef::CreateOptions {
        profile: super::profile_of(&key.profile),
        clipboard,
    };
    let (view, events) = match monocode_cef::BrowserView::create_hidden(url, &options) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("browser: extension {}: {e}", key.id);
            return None;
        }
    };
    let view = Rc::new(view);
    let browser_id = view.browser_id();
    super::serve::add_host(browser_id, &key.id);
    let (cdp, attachment) = match super::dev::dir() {
        Some(_) => dev_cdp(&view),
        None => (None, None),
    };
    let key = key.clone();
    let host_url = url.to_owned();
    spawn(async move {
        while let Ok(event) = events.recv().await {
            use monocode_cef::BrowserEvent as E;
            match event {
                E::RenderProcessGone => {
                    log::warn!("browser: extension {}: host {browser_id} crashed", key.id);
                    host_gone(&key, browser_id, &host_url);
                }
                E::Closed => {
                    host_gone(&key, browser_id, &host_url);
                    break;
                }
                E::PopupRequested { url, .. } => open_tab(&key, &url, true),
                E::PopupOpened { id, url, .. } => {
                    drop(monocode_cef::take_popup(id));
                    open_tab(&key, &url, true);
                }
                // Dialogs in a page no one sees are dismissed (dropped).
                _ => {}
            }
        }
    });
    Some(HostPage {
        view,
        browser_id,
        url: url.to_owned(),
        cdp,
        _attachment: attachment,
    })
}

/// A hosted page went (crashed, closed): the background comes back.
fn host_gone(key: &InstanceKey, browser_id: i32, url: &str) {
    enum Which {
        Background,
        Offscreen,
        Other,
    }
    let which = with(|rt| {
        let i = rt.instances.get_mut(key)?;
        let which = if i
            .background
            .as_ref()
            .is_some_and(|h| h.browser_id == browser_id)
        {
            Which::Background
        } else if i
            .offscreen
            .as_ref()
            .is_some_and(|h| h.browser_id == browser_id)
        {
            Which::Offscreen
        } else {
            Which::Other
        };
        // Its contexts are gone with it.
        let gone: Vec<String> = (i.ctxs.iter())
            .filter(|(_, c)| c.browser_id == Some(browser_id))
            .map(|(id, _)| id.clone())
            .collect();
        Some((which, gone, i.stopping))
    })
    .flatten();
    let Some((which, gone, stopping)) = which else {
        return;
    };
    for ctx in gone {
        forget_ctx(key, &ctx);
    }
    match which {
        Which::Offscreen => {
            with(|rt| rt.instances.get_mut(key).map(|i| i.offscreen.take()));
        }
        Which::Background if !stopping => {
            let allowed = with(|rt| {
                let i = rt.instances.get_mut(key)?;
                i.restarts.retain(|t| t.elapsed() < RESTART_WINDOW);
                i.restarts.push(Instant::now());
                let n = i.restarts.len();
                // Dropped outside the borrow below.
                Some((n <= RESTARTS, n, i.background.take()))
            })
            .flatten();
            let Some((allowed, n, old)) = allowed else {
                return;
            };
            drop(old);
            if !allowed {
                log::warn!(
                    "browser: extension {}: background crashed {n} times in 5 minutes; not restarted",
                    key.id
                );
                return;
            }
            let key = key.clone();
            let delay = Duration::from_millis(500 * (1 << (n.min(5) - 1)));
            let _ = url;
            spawn(async move {
                super::sleep(delay).await;
                if with(|rt| rt.instances.contains_key(&key)) == Some(true) {
                    open_background(&key);
                }
            });
        }
        _ => {}
    }
}

/// Development hook: a CDP client on a hosted page, its worker attached,
/// their console in the app's log.
fn dev_cdp(view: &Rc<monocode_cef::BrowserView>) -> (Option<Rc<CdpClient>>, Option<Box<dyn Any>>) {
    let weak = Rc::downgrade(view);
    let cdp = CdpClient::new(Box::new(move |m: &[u8]| {
        weak.upgrade().is_some_and(|v| v.devtools_send(m))
    }));
    let sink = Rc::downgrade(&cdp);
    let Some(attachment) = view.devtools_attach(Box::new(move |m| {
        if let Some(cdp) = sink.upgrade() {
            cdp.on_message(m);
        }
    })) else {
        return (None, None);
    };
    let subscription = cdp.subscribe(|event| {
        if event.method == "Runtime.consoleAPICalled" || event.method == "Runtime.exceptionThrown" {
            let text = event.params.to_string();
            log::warn!(
                "browser: extension console: {}",
                text.chars().take(600).collect::<String>()
            );
        }
    });
    for call in [
        cdp.call("Runtime.enable", json!({}), None),
        cdp.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            None,
        ),
    ] {
        spawn(async move {
            let _ = call.await;
        });
    }
    let c = cdp.clone();
    let worker_sub = cdp.subscribe(move |event| {
        if event.method == "Target.attachedToTarget"
            && let Some(session) = event.params["sessionId"].as_str()
        {
            let call = c.call("Runtime.enable", json!({}), Some(session));
            spawn(async move {
                let _ = call.await;
            });
        }
    });
    (
        Some(cdp),
        Some(Box::new((attachment, subscription, worker_sub))),
    )
}

/// Opens `url` (an extension page or a web address) in a new tab.
pub(crate) fn open_tab(key: &InstanceKey, url: &str, active: bool) {
    let url = if url.contains("://") || url.starts_with("about:") {
        url.to_owned()
    } else {
        format!("{}{}", core::base_url(&key.id), url.trim_start_matches('/'))
    };
    if url.starts_with(&core::base_url(&key.id)) {
        super::serve::expect(&url);
    }
    let Some(app) = with(|rt| rt.app.clone()) else {
        return;
    };
    let profile = key.profile.clone();
    app.update(|cx| crate::shell::browser_extensions::open_extension_tab(url, profile, active, cx));
}

// ---- contexts ----

/// What the bridge or a binding says about a context.
pub(crate) struct Meta {
    pub ctx: String,
    pub kind: Kind,
    pub url: String,
    pub top: bool,
    pub browser_id: Option<i32>,
    pub route: Option<Route>,
    pub tab: Option<i64>,
    pub frame_id: Option<i64>,
    /// The context says what it is (`ctx.hello`): its kind and place are
    /// taken again, a poll may have come first.
    pub hello: bool,
}

/// Knows `meta.ctx` from now on (and refreshes when it was seen).
pub(crate) fn ensure_ctx(key: &InstanceKey, meta: Meta) {
    with(|rt| {
        let tab_of_browser = meta
            .browser_id
            .and_then(|b| rt.tabs.tab_of_browser(&key.profile, b));
        let Some(i) = rt.instances.get_mut(key) else {
            return;
        };
        let earlier = match i.ctxs.get_mut(&meta.ctx) {
            Some(c) if !meta.hello => {
                c.seen = Instant::now();
                if !meta.url.is_empty() {
                    c.url = meta.url;
                }
                return;
            }
            Some(_) => i.ctxs.remove(&meta.ctx),
            None => None,
        };
        let host_kind = meta.browser_id.and_then(|b| {
            if i.background.as_ref().is_some_and(|h| h.browser_id == b) {
                Some(Kind::Background)
            } else if i.offscreen.as_ref().is_some_and(|h| h.browser_id == b) {
                Some(Kind::Offscreen)
            } else if i.popup.as_ref().is_some_and(|p| p.browser_id() == b) {
                Some(Kind::Popup)
            } else {
                None
            }
        });
        let (kind, tab) = match (meta.kind, host_kind) {
            (Kind::Content | Kind::External, _) => (meta.kind, meta.tab),
            (Kind::Background, Some(Kind::Background)) => (Kind::Background, None),
            (_, Some(k)) => (k, None),
            // A worker of a page in a tab, or the page itself.
            (Kind::Background, None) => (Kind::Frame, tab_of_browser),
            (_, None) if meta.top => (Kind::Tab, tab_of_browser),
            _ => (Kind::Frame, tab_of_browser),
        };
        let frame_id = meta.frame_id.unwrap_or_else(|| {
            if tab.is_some() && meta.top {
                0
            } else {
                i.next += 1;
                1000 + i.next as i64
            }
        });
        if kind == Kind::Background && i.ready_in.is_none() {
            i.ready_in = Some(i.started.elapsed());
            log::warn!(
                "browser: extension {}: background ready in {:?}",
                key.id,
                i.started.elapsed()
            );
        }
        let (route, listening) = match earlier {
            Some(c) => (c.route, c.listening),
            None => (
                meta.route.unwrap_or(Route::Poll {
                    queue: VecDeque::new(),
                    poller: None,
                }),
                HashSet::new(),
            ),
        };
        i.ctxs.insert(
            meta.ctx,
            Ctx {
                kind,
                url: meta.url,
                top: meta.top,
                tab,
                frame_id,
                route,
                listening,
                seen: Instant::now(),
                browser_id: meta.browser_id,
            },
        );
    });
}

/// Forgets a context: its ports close at the other end, the answers it
/// owed become "no answer".
pub(crate) fn forget_ctx(key: &InstanceKey, ctx: &str) {
    let Some((ports, pending)) = with(|rt| {
        let i = rt.instances.get_mut(key)?;
        if let Some(c) = i.ctxs.remove(ctx)
            && let Route::Poll {
                poller: Some((r, _)),
                ..
            } = c.route
        {
            r.json(200, &json!([]));
        }
        let ports: Vec<(String, Vec<String>)> = (i.ports.iter())
            .filter(|(_, ends)| ends.iter().any(|e| e == ctx))
            .map(|(id, ends)| (id.clone(), ends.clone()))
            .collect();
        for (id, _) in &ports {
            i.ports.remove(id);
        }
        let pending: Vec<String> = (i.pending.iter())
            .filter(|(_, p)| p.targets.iter().any(|t| t == ctx))
            .map(|(id, _)| id.clone())
            .collect();
        Some((ports, pending))
    })
    .flatten() else {
        return;
    };
    for (port, ends) in ports {
        for end in ends.iter().filter(|e| *e != ctx) {
            deliver(key, end, json!({"t": "port.disconnect", "port": port}));
        }
    }
    for rid in pending {
        respond(key, &rid, ctx, false, Value::Null);
    }
}

/// Forgets the contexts `pred` picks.
pub(crate) fn forget_where(key: &InstanceKey, pred: impl Fn(&Ctx) -> bool) {
    let gone: Vec<String> = with(|rt| {
        (rt.instances.get(key).into_iter())
            .flat_map(|i| i.ctxs.iter())
            .filter(|(_, c)| pred(c))
            .map(|(id, _)| id.clone())
            .collect()
    })
    .unwrap_or_default();
    for ctx in gone {
        forget_ctx(key, &ctx);
    }
}

/// Sends `message` to a context.
pub(crate) fn deliver(key: &InstanceKey, ctx: &str, message: Value) {
    let cdp_route = with(|rt| {
        let i = rt.instances.get_mut(key)?;
        let c = i.ctxs.get_mut(ctx)?;
        match &mut c.route {
            Route::Poll { queue, poller } => {
                queue.push_back(message.clone());
                if let Some((r, _)) = poller.take() {
                    let batch: Vec<Value> = queue.drain(..).collect();
                    r.json(200, &Value::Array(batch));
                }
                None
            }
            Route::Cdp {
                tab,
                session,
                context,
                recv,
            } => Some((*tab, session.clone(), *context, recv.clone())),
        }
    })
    .flatten();
    if let Some((tab, session, context, recv)) = cdp_route {
        super::pages::evaluate_in(tab, session, context, &recv, &message);
    }
}

/// Sends event `name` to every context of the instance listening.
pub(crate) fn broadcast(key: &InstanceKey, name: &str, args: Value, filter: bool) {
    let targets = with(|rt| rt.instances.get(key).map(|i| i.listeners(name, "")))
        .flatten()
        .unwrap_or_default();
    if targets.is_empty() {
        return;
    }
    let message = json!({"t": "event", "name": name, "args": args, "filter": filter});
    for ctx in targets {
        deliver(key, &ctx, message.clone());
    }
}

pub(crate) fn respond(key: &InstanceKey, rid: &str, from: &str, has: bool, value: Value) {
    with(|rt| {
        let Some(i) = rt.instances.get_mut(key) else {
            return;
        };
        let Some(p) = i.pending.get_mut(rid) else {
            return;
        };
        if !p.targets.iter().any(|t| t == from) {
            return;
        }
        p.targets.retain(|t| t != from);
        if has {
            let p = i.pending.remove(rid).expect("pending");
            let _ = p.reply.try_send(Ok(value));
        } else if p.targets.is_empty() {
            let p = i.pending.remove(rid).expect("pending");
            let _ = p.reply.try_send(Ok(json!({"__mcextNone": true})));
        }
    });
}

/// Waits a little for receivers (the background starting, a content
/// script loading), as Chrome wakes a service worker for a message.
pub(crate) async fn wait_for(pick: impl Fn() -> Vec<String>) -> Vec<String> {
    for _ in 0..50 {
        let targets = pick();
        if !targets.is_empty() {
            return targets;
        }
        super::sleep(Duration::from_millis(100)).await;
    }
    Vec::new()
}

/// Sends `msg` from `from` to `targets`; the first answer.
pub(crate) async fn send_message(
    key: &InstanceKey,
    from: &str,
    targets: Vec<String>,
    msg: Value,
    external: bool,
) -> Result<Value, String> {
    if targets.is_empty() {
        return Err(api::NO_RECEIVER.into());
    }
    let (tx, rx) = async_channel::bounded(1);
    let Some((rid, sender)) = with(|rt| {
        let tabs = &rt.tabs;
        let i = rt.instances.get_mut(key)?;
        let rid = i.next_id("r");
        i.pending.insert(
            rid.clone(),
            Pending {
                targets: targets.clone(),
                reply: tx,
            },
        );
        Some((rid, i.sender(from, tabs)))
    })
    .flatten() else {
        return Err("Extension context invalidated.".into());
    };
    for target in &targets {
        deliver(
            key,
            target,
            json!({"t": "message", "msg": msg, "sender": sender, "rid": rid, "external": external}),
        );
    }
    rx.recv()
        .await
        .unwrap_or(Err("Extension context invalidated.".into()))
}

/// Opens port `port` from `from` to `targets`.
pub(crate) fn connect(
    key: &InstanceKey,
    from: &str,
    targets: Vec<String>,
    port: &str,
    name: &Value,
    external: bool,
) -> Result<(), String> {
    if targets.is_empty() {
        return Err(api::NO_RECEIVER.into());
    }
    let sender = with(|rt| {
        let tabs = &rt.tabs;
        let i = rt.instances.get_mut(key)?;
        let mut ends = vec![from.to_owned()];
        ends.extend(targets.iter().cloned());
        i.ports.insert(port.to_owned(), ends);
        Some(i.sender(from, tabs))
    })
    .flatten()
    .ok_or("Extension context invalidated.")?;
    for target in &targets {
        deliver(
            key,
            target,
            json!({"t": "connect", "port": port, "name": name, "sender": sender, "external": external}),
        );
    }
    Ok(())
}

// ---- the bridge ----

fn meta_from_bridge(
    ctx: &str,
    args: &[Value],
    browser_id: Option<i32>,
    frame_url: Option<String>,
) -> Meta {
    let info = args.first().cloned().unwrap_or_default();
    let hello = !args.is_empty();
    Meta {
        ctx: ctx.to_owned(),
        kind: Kind::parse(info["kind"].as_str().unwrap_or("page")),
        url: info["url"]
            .as_str()
            .map(str::to_owned)
            .or(frame_url)
            .unwrap_or_default(),
        top: info["top"].as_bool().unwrap_or(true),
        browser_id,
        route: None,
        tab: None,
        frame_id: None,
        hello,
    }
}

pub(crate) fn on_bridge(message: Bridge) {
    match message {
        Bridge::Call {
            key,
            ctx,
            api,
            args,
            browser_id,
            frame_url,
            responder,
        } => {
            if ctx.is_empty() || !ensure(&key) {
                return responder.json(
                    200,
                    &json!({"ok": false, "error": "Extension context invalidated."}),
                );
            }
            let hello = api == "ctx.hello";
            let meta =
                meta_from_bridge(&ctx, if hello { &args } else { &[] }, browser_id, frame_url);
            ensure_ctx(&key, meta);
            spawn(async move {
                let result = super::dispatch::call(&key, &ctx, &api, args).await;
                let body = match result {
                    Ok(value) => json!({"ok": true, "value": value}),
                    Err(error) => json!({"ok": false, "error": error}),
                };
                responder.json(200, &body);
            });
        }
        Bridge::Poll {
            key,
            ctx,
            browser_id,
            frame_url,
            responder,
        } => {
            if ctx.is_empty() || !ensure(&key) {
                return responder.json(410, &json!([]));
            }
            ensure_ctx(&key, meta_from_bridge(&ctx, &[], browser_id, frame_url));
            let token = with(|rt| {
                let i = rt.instances.get_mut(&key)?;
                i.next += 1;
                let token = i.next;
                let c = i.ctxs.get_mut(&ctx)?;
                let Route::Poll { queue, poller } = &mut c.route else {
                    return None;
                };
                if let Some((old, _)) = poller.take() {
                    old.json(200, &json!([]));
                }
                if !queue.is_empty() {
                    let batch: Vec<Value> = queue.drain(..).collect();
                    responder.json(200, &Value::Array(batch));
                    return None;
                }
                *poller = Some((responder, token));
                Some(token)
            })
            .flatten();
            if let Some(token) = token {
                spawn(async move {
                    super::sleep(POLL).await;
                    with(|rt| {
                        let c = rt.instances.get_mut(&key)?.ctxs.get_mut(&ctx)?;
                        if let Route::Poll { poller, .. } = &mut c.route
                            && poller.as_ref().is_some_and(|(_, t)| *t == token)
                        {
                            let (r, _) = poller.take().expect("poller");
                            r.json(200, &json!([]));
                        }
                        Some(())
                    });
                });
            }
        }
    }
}

// ---- alarms ----

pub(crate) fn schedule_alarm(key: &InstanceKey, name: &str) {
    let Some((delay, generation)) = with(|rt| {
        let a = rt.instances.get(key)?.alarms.get(name)?;
        Some(((a.scheduled - epoch_ms()).max(0.), a.generation))
    })
    .flatten() else {
        return;
    };
    let key = key.clone();
    let name = name.to_owned();
    spawn(async move {
        super::sleep(Duration::from_millis(delay as u64)).await;
        let fire = with(|rt| {
            let i = rt.instances.get_mut(&key)?;
            let a = i.alarms.get_mut(&name)?;
            if a.generation != generation {
                return None;
            }
            let info =
                json!({"name": name, "scheduledTime": a.scheduled, "periodInMinutes": a.period});
            match a.period {
                Some(p) => {
                    a.scheduled = epoch_ms() + p * 60_000.;
                    Some((info, true))
                }
                None => {
                    i.alarms.remove(&name);
                    Some((info, false))
                }
            }
        })
        .flatten();
        if let Some((info, again)) = fire {
            broadcast(&key, "alarms.onAlarm", json!([info]), false);
            if again {
                schedule_alarm(&key, &name);
            }
        }
    });
}

/// What the development hook writes to `state.json`.
pub(crate) fn dev_state(rt: &mut super::Runtime) -> Value {
    json!({
        "instances": rt.instances.values().map(|i| json!({
            "id": i.key.id,
            "profile": super::profile_folder(&i.key.profile),
            "version": i.ext.manifest.version,
            "background": i.background.as_ref().map(|h| h.browser_id),
            "offscreen": i.offscreen.as_ref().map(|h| h.url.clone()),
            "popup": i.popup.as_ref().map(|p| p.browser_id()),
            "readyMs": i.ready_in.map(|d| d.as_millis() as u64),
            "restarts": i.restarts.len(),
            "action": i.action.dev_json(),
            "alarms": i.alarms.keys().collect::<Vec<_>>(),
            "calls": i.calls.iter().rev().take(80).collect::<Vec<_>>(),
            "ctxs": i.ctxs.iter().map(|(id, c)| json!({
                "ctx": id, "kind": c.kind.as_str(), "url": c.url, "top": c.top, "tab": c.tab, "frame": c.frame_id,
                "listening": c.listening.iter().collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "tabs": rt.tabs.dev_json(),
        "pages": rt.pages.dev_json(),
    })
}
