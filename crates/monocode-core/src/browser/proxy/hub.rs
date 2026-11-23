//! The endpoint's main-thread side. For each WebSocket client it is the
//! browser target Chrome would be (`Target.*`, `Browser.getVersion`) over the
//! pages of the client's scope, and it routes the client's page sessions to
//! the pages' [`CdpClient`]s.
//!
//! A page session is the client's own DevTools session on the page when
//! Chromium gives one, else the page's root session, shared with our tools
//! behind a synthetic id (see [`super`]). A page's target id is Chromium's
//! own: Playwright takes the main frame's id for the target's.
//!
//! For clients that want tab targets (Puppeteer: `Target.setAutoAttach` with
//! pages filtered out), each page also has a tab target whose session
//! attaches the page.
//!
//! Pages come and go without telling us: every connection looks at its
//! scope's pages every [`POLL`] and reports the difference.
//!
//! Restored tabs not loaded yet (K20) are listed under a stand-in id
//! ([`Unloaded`]) and load when a client attaches them, or when it
//! auto-attaches every page (Playwright's `connectOverCDP`).

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    future::Future,
    rc::{Rc, Weak},
    sync::{Mutex, PoisonError, mpsc},
    time::Duration,
};

use futures_util::{
    FutureExt as _,
    future::{self, Either, LocalBoxFuture},
};
use serde_json::{Value, json};

use super::Input;
use crate::browser::{
    CdpClient, CdpError, CdpEvent, Host, Page, PageId, PageInfo, Scope, Subscription,
};

const POLL: Duration = Duration::from_millis(300);
/// How long we wait on a page for what we ask it ourselves.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// What a client may not turn off on the session it shares with our tools
/// (K38): our console and network log need them.
const SWALLOWED: &[&str] = &[
    "Runtime.disable",
    "Network.disable",
    "Log.disable",
    "Page.disable",
    "DOM.disable",
    "Accessibility.disable",
];

/// Browser-wide methods a page session must not reach: they would close the
/// app's browser or step out of the scope.
const REFUSED_ON_PAGES: &[&str] = &[
    "Browser.close",
    "Browser.crash",
    "Browser.crashGpuProcess",
    "Browser.grantPermissions",
    "Browser.resetPermissions",
    "Browser.setPermission",
    // The window is the app's.
    "Browser.setWindowBounds",
    "Browser.setContentsSize",
    "Browser.setDockTile",
    "Browser.executeBrowserCommand",
];

/// The `Target.*` methods a page session keeps; the rest act on other targets.
const TARGET_ON_PAGES: &[&str] = &[
    "Target.setAutoAttach",
    "Target.getTargetInfo",
    "Target.detachFromTarget",
];

#[derive(Clone, Debug)]
pub(super) enum Kind {
    /// `/devtools/browser/<id>`: the browser target.
    Browser,
    /// `/devtools/page/<targetId>`: one page, no session ids.
    Page(String),
}

/// A page of the scope, as a target.
#[derive(Clone)]
struct Target {
    page: Rc<dyn Page>,
    cdp: Rc<CdpClient>,
    id: String,
    info: PageInfo,
}

/// A page for HTTP discovery.
pub(super) struct Listed {
    pub id: String,
    pub url: String,
    pub title: String,
}

thread_local! {
    static CLIENTS: RefCell<HashMap<u64, Rc<Client>>> = RefCell::default();
    /// Each page's target id, for the client it was learned on.
    static TARGET_IDS: RefCell<HashMap<PageId, (Weak<CdpClient>, String)>> = RefCell::default();
}

static VERSION: Mutex<Option<Value>> = Mutex::new(None);

/// A filter of `Target.setDiscoverTargets`, `setAutoAttach`, `getTargets`:
/// the first entry that matches a type decides.
#[derive(Clone, Debug)]
struct Filter(Vec<(Option<String>, bool)>);

impl Filter {
    fn parse(value: Option<&Value>) -> Filter {
        let Some(entries) = value.and_then(Value::as_array) else {
            // Chromium's default: everything but the browser and tabs.
            return Filter(vec![
                (Some("browser".into()), true),
                (Some("tab".into()), true),
                (None, false),
            ]);
        };
        Filter(
            entries
                .iter()
                .map(|e| {
                    let kind = e.get("type").and_then(Value::as_str).map(str::to_owned);
                    (
                        kind,
                        e.get("exclude").and_then(Value::as_bool).unwrap_or(false),
                    )
                })
                .collect(),
        )
    }

    fn wants(&self, kind: &str) -> bool {
        self.0
            .iter()
            .find(|(k, _)| k.as_deref().is_none_or(|k| k == kind))
            .is_some_and(|(_, exclude)| !exclude)
    }
}

/// A client's session on a page. Dropping it ends Chromium's session.
struct PageSession {
    /// The id the client uses; `None` for a page connection's session.
    visible: Option<String>,
    /// Chromium's id of the client's own session; `None` when it shares the
    /// page's root session.
    own: Option<String>,
    /// The browser or tab session it was attached from: its detach goes there.
    parent: Option<String>,
    target: String,
    page: PageId,
    cdp: Rc<CdpClient>,
    /// Child sessions (iframes, workers) attached under it, which the client
    /// addresses directly.
    children: Rc<RefCell<HashSet<String>>>,
    _events: Subscription,
    _drawn: Box<dyn Any>,
}

impl Drop for PageSession {
    fn drop(&mut self) {
        if let Some(own) = self.own.as_ref().filter(|_| !self.cdp.is_gone()) {
            drop(
                self.cdp
                    .call("Target.detachFromTarget", json!({"sessionId": own}), None),
            );
        }
    }
}

/// A tab target's session (Puppeteer): it attaches its page when asked.
struct Tab {
    target: String,
    parent: Option<String>,
    page_session: Option<String>,
}

#[derive(Default)]
struct State {
    closed: bool,
    /// `Target.setDiscoverTargets` is on, with this filter.
    discover: Option<Filter>,
    /// `Target.setAutoAttach` per browser-level session (`None`: the root).
    auto: HashMap<Option<String>, Filter>,
    browser_sessions: HashSet<String>,
    tabs: HashMap<String, Tab>,
    /// Page sessions by the id the client uses.
    sessions: HashMap<String, Rc<PageSession>>,
    /// A page connection's session.
    root: Option<Rc<PageSession>>,
    /// A page connection's messages from before `root` existed.
    pending: Option<Vec<Value>>,
    /// Every target seen, with the info last reported.
    known: HashMap<String, Value>,
    /// Pages seen loaded or loaded for the client ([`load_all`]).
    seen: HashSet<PageId>,
    /// Auto-attaches started, per (browser-level session, target); true once
    /// announced.
    attaching: HashMap<(Option<String>, String), Rc<Cell<bool>>>,
}

struct Client {
    host: Rc<dyn Host>,
    scope: Scope,
    /// The app's own DevTools frontend (`super::devtools_base`).
    internal: bool,
    context: String,
    browser_id: String,
    kind: Kind,
    out: mpsc::Sender<Input>,
    state: RefCell<State>,
}

/// One command of the client.
struct Request {
    id: Value,
    method: String,
    params: Value,
    session: Option<String>,
    raw: Value,
}

type Failure = (i64, String);

fn not_found(method: &str) -> Failure {
    (-32601, format!("'{method}' wasn't found"))
}

fn invalid(message: &str) -> Failure {
    (-32602, message.to_owned())
}

fn failed(message: impl Into<String>) -> Failure {
    (-32000, message.into())
}

impl Client {
    fn send(&self, message: Value) {
        let _ = self.out.send(Input::Send(message));
    }

    fn reply(&self, request: &Request, result: Result<Value, Failure>) {
        let mut message = match result {
            Ok(result) => json!({"id": request.id, "result": result}),
            Err((code, message)) => {
                json!({"id": request.id, "error": {"code": code, "message": message}})
            }
        };
        if let Some(session) = &request.session {
            message["sessionId"] = session.as_str().into();
        }
        self.send(message);
    }

    fn event(&self, session: Option<&str>, method: &str, params: Value) {
        let mut message = json!({"method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        self.send(message);
    }

    fn closed(&self) -> bool {
        self.state.borrow().closed
    }

    fn info(&self, target: &Target, tab: bool) -> Value {
        self.target_info(&target.id, &target.info, tab, true)
    }

    fn unloaded_info(&self, page: &Unloaded, tab: bool) -> Value {
        self.target_info(&page.id, &page.info, tab, false)
    }

    fn target_info(&self, id: &str, info: &PageInfo, tab: bool, attached: bool) -> Value {
        json!({
            "targetId": if tab { tab_id(id) } else { id.to_owned() },
            "type": if tab { "tab" } else { "page" },
            "title": if info.title.is_empty() { &info.url } else { &info.title },
            "url": info.url,
            "attached": attached,
            "canAccessOpener": false,
            "browserContextId": self.context,
        })
    }

    fn browser_info(&self) -> Value {
        json!({
            "targetId": self.browser_id,
            "type": "browser",
            "title": "",
            "url": "",
            "attached": true,
            "canAccessOpener": false,
        })
    }
}

fn tab_id(target: &str) -> String {
    format!("TAB-{target}")
}

/// Puts `session` on a message, or takes the session off.
fn retag(message: &mut Value, session: Option<&str>) {
    match (session, message.as_object_mut()) {
        (Some(session), Some(object)) => {
            object.insert("sessionId".into(), session.into());
        }
        (None, Some(object)) => {
            object.remove("sessionId");
        }
        _ => {}
    }
}

fn session_id() -> String {
    format!("{:032X}", rand::random::<u128>())
}

async fn timed<T>(
    host: &Rc<dyn Host>,
    work: impl Future<Output = T>,
    limit: Duration,
) -> Option<T> {
    match future::select(std::pin::pin!(work), host.sleep(limit)).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

/// The page's target id: Chromium's, which is also its main frame's id.
async fn target_id(host: &Rc<dyn Host>, page: PageId, cdp: &Rc<CdpClient>) -> Option<String> {
    let cached = TARGET_IDS.with_borrow(|ids| {
        ids.get(&page)
            .filter(|(client, _)| client.upgrade().is_some_and(|c| Rc::ptr_eq(&c, cdp)))
            .map(|(_, id)| id.clone())
    });
    if cached.is_some() {
        return cached;
    }
    let info = timed(
        host,
        cdp.call("Target.getTargetInfo", json!({}), None),
        CALL_TIMEOUT,
    )
    .await?;
    let id = match info {
        Ok(info) => info["targetInfo"]["targetId"].as_str().map(str::to_owned),
        Err(CdpError::Protocol { .. }) => None,
        Err(_) => return None,
    };
    let id = match id {
        Some(id) => id,
        None => {
            let tree = timed(
                host,
                cdp.call("Page.getFrameTree", json!({}), None),
                CALL_TIMEOUT,
            )
            .await?
            .ok()?;
            tree["frameTree"]["frame"]["id"].as_str()?.to_owned()
        }
    };
    TARGET_IDS.with_borrow_mut(|ids| {
        ids.retain(|_, (client, _)| client.strong_count() > 0);
        ids.insert(page, (Rc::downgrade(cdp), id.clone()));
    });
    Some(id)
}

/// A restored tab not loaded yet (K20): no browser, so no target id from
/// Chromium. It is listed under a stand-in id with its saved address and
/// title, and loads when a client attaches it; then its real target
/// replaces the stand-in (destroyed, then created).
struct Unloaded {
    page: Rc<dyn Page>,
    id: String,
    info: PageInfo,
}

/// How long a restored tab may take to load for a client (Chromium may
/// have to start first).
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

fn unloaded_id(page: PageId) -> String {
    format!("MONOCODE-UNLOADED-{page}")
}

/// The scope's pages without a browser yet.
fn unloaded(host: &Rc<dyn Host>, scope: &Scope) -> Vec<Unloaded> {
    host.pages(scope)
        .into_iter()
        .filter(|page| page.cdp().is_none())
        .map(|page| {
            let info = page.info();
            Unloaded {
                id: unloaded_id(info.id),
                info,
                page,
            }
        })
        .collect()
}

/// Loads a restored tab and returns its target.
async fn load(host: &Rc<dyn Host>, scope: &Scope, page: &Rc<dyn Page>) -> Result<Target, String> {
    let id = page.info().id;
    timed(host, page.ensure_loaded(), LOAD_TIMEOUT)
        .await
        .ok_or("the page took too long to load")??;
    targets(host, scope)
        .await
        .into_iter()
        .find(|t| t.info.id == id)
        .ok_or_else(|| "the page did not answer".to_owned())
}

/// Loads the scope's restored tabs, for a client that attaches every page.
/// Each page once per client: one that did not load, or that the client saw
/// loaded before (its renderer crashed since), is not loaded again here.
/// False when there was none to load.
async fn load_all(client: &Rc<Client>) -> bool {
    let (host, scope) = (&client.host, &client.scope);
    let pages: Vec<Unloaded> = {
        let mut state = client.state.borrow_mut();
        unloaded(host, scope)
            .into_iter()
            .filter(|p| state.seen.insert(p.info.id))
            .collect()
    };
    if pages.is_empty() {
        return false;
    }
    // One that does not load stays listed under its stand-in id.
    future::join_all(pages.iter().map(|p| load(host, scope, &p.page))).await;
    true
}

/// The client saw these pages loaded.
fn seen(client: &Client, now: &[Target]) {
    let mut state = client.state.borrow_mut();
    state.seen.extend(now.iter().map(|t| t.info.id));
}

/// The scope's pages that have a live DevTools channel. Restored tabs not
/// loaded yet (K20) are [`Unloaded`].
async fn targets(host: &Rc<dyn Host>, scope: &Scope) -> Vec<Target> {
    let pages: Vec<(Rc<dyn Page>, Rc<CdpClient>)> = host
        .pages(scope)
        .into_iter()
        .filter_map(|page| {
            let cdp = page.cdp()?;
            (!cdp.is_gone()).then_some((page, cdp))
        })
        .collect();
    let ids = future::join_all(
        pages
            .iter()
            .map(|(page, cdp)| target_id(host, page.info().id, cdp)),
    )
    .await;
    pages
        .into_iter()
        .zip(ids)
        .filter_map(|((page, cdp), id)| {
            (!cdp.is_gone()).then_some(())?;
            Some(Target {
                info: page.info(),
                id: id?,
                page,
                cdp,
            })
        })
        .collect()
}

fn fallback_version() -> Value {
    json!({
        "protocolVersion": "1.3",
        "product": "Chrome/0.0.0.0",
        "revision": "",
        "userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/0.0.0.0 Safari/537.36",
        "jsVersion": "",
    })
}

/// `Browser.getVersion`, asked of a page once; made up while no page is open.
async fn version(host: &Rc<dyn Host>, scope: &Scope) -> Value {
    if let Some(version) = VERSION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    {
        return version;
    }
    for target in targets(host, scope).await {
        let cdp = &target.cdp;
        let mut version = match timed(
            host,
            cdp.call("Browser.getVersion", json!({}), None),
            CALL_TIMEOUT,
        )
        .await
        {
            Some(Ok(v)) if v["product"].is_string() && v["userAgent"].is_string() => Some(v),
            _ => None,
        };
        if version.is_none() {
            let expression = json!({"expression": "navigator.userAgent", "returnByValue": true});
            let answer = timed(
                host,
                cdp.call("Runtime.evaluate", expression, None),
                CALL_TIMEOUT,
            )
            .await;
            let agent = match &answer {
                Some(Ok(answer)) => answer["result"]["value"].as_str(),
                _ => None,
            };
            if let Some(agent) = agent {
                let product = agent
                    .split(' ')
                    .find(|word| word.starts_with("Chrome/"))
                    .unwrap_or("Chrome/0.0.0.0");
                version = Some(json!({
                    "protocolVersion": "1.3",
                    "product": product,
                    "revision": "",
                    "userAgent": agent,
                    "jsVersion": "",
                }));
            }
        }
        if let Some(version) = version {
            *VERSION.lock().unwrap_or_else(PoisonError::into_inner) = Some(version.clone());
            return version;
        }
    }
    fallback_version()
}

// HTTP discovery.

pub(super) fn version_of(host: Rc<dyn Host>, scope: Scope) -> LocalBoxFuture<'static, Value> {
    async move { version(&host, &scope).await }.boxed_local()
}

fn listed_as(id: &str, info: &PageInfo) -> Listed {
    Listed {
        id: id.to_owned(),
        url: info.url.clone(),
        title: if info.title.is_empty() {
            info.url.clone()
        } else {
            info.title.clone()
        },
    }
}

fn listed(target: &Target) -> Listed {
    listed_as(&target.id, &target.info)
}

pub(super) fn list(host: Rc<dyn Host>, scope: Scope) -> LocalBoxFuture<'static, Vec<Listed>> {
    async move {
        let mut list: Vec<Listed> = targets(&host, &scope).await.iter().map(listed).collect();
        list.extend(
            unloaded(&host, &scope)
                .iter()
                .map(|p| listed_as(&p.id, &p.info)),
        );
        list
    }
    .boxed_local()
}

pub(super) fn has_target(
    host: Rc<dyn Host>,
    scope: Scope,
    id: String,
) -> LocalBoxFuture<'static, bool> {
    async move {
        targets(&host, &scope).await.iter().any(|t| t.id == id)
            || unloaded(&host, &scope).iter().any(|p| p.id == id)
    }
    .boxed_local()
}

async fn open_target(host: &Rc<dyn Host>, scope: &Scope, url: &str) -> Result<Target, String> {
    let page = host.open(scope, url, false).await?;
    let cdp = page.cdp().ok_or("the new page has no DevTools channel")?;
    let id = target_id(host, page.info().id, &cdp)
        .await
        .ok_or("the new page did not answer")?;
    Ok(Target {
        info: page.info(),
        id,
        page,
        cdp,
    })
}

pub(super) fn open_page(
    host: Rc<dyn Host>,
    scope: Scope,
    url: String,
) -> LocalBoxFuture<'static, Result<Listed, String>> {
    async move { open_target(&host, &scope, &url).await.map(|t| listed(&t)) }.boxed_local()
}

pub(super) fn close_page(
    host: Rc<dyn Host>,
    scope: Scope,
    id: String,
) -> LocalBoxFuture<'static, bool> {
    async move {
        let found = targets(&host, &scope)
            .await
            .into_iter()
            .find(|t| t.id == id)
            .map(|t| t.info.id)
            .or_else(|| {
                let pages = unloaded(&host, &scope);
                pages.iter().find(|p| p.id == id).map(|p| p.info.id)
            });
        found.is_some_and(|page| host.close(&scope, page))
    }
    .boxed_local()
}

// WebSocket clients.

/// A client connected: from now on its messages come to [`message`]. The
/// future runs while the client is connected. `internal`: the app's own
/// DevTools frontend.
#[allow(clippy::too_many_arguments)]
pub(super) fn open(
    host: Rc<dyn Host>,
    id: u64,
    scope: Scope,
    context: String,
    browser_id: String,
    kind: Kind,
    internal: bool,
    out: mpsc::Sender<Input>,
) -> LocalBoxFuture<'static, ()> {
    let client = Rc::new(Client {
        host,
        scope,
        internal,
        context,
        browser_id,
        state: RefCell::new(State {
            pending: matches!(kind, Kind::Page(_)).then(Vec::new),
            ..State::default()
        }),
        kind,
        out,
    });
    CLIENTS.with_borrow_mut(|clients| clients.insert(id, client.clone()));
    async move {
        let mut started = Vec::new();
        if let Kind::Page(wanted) = &client.kind {
            let (host, scope) = (&client.host, &client.scope);
            let mut found = targets(host, scope)
                .await
                .into_iter()
                .find(|t| t.id == *wanted);
            if found.is_none() {
                // A restored tab's stand-in id: the page loads now.
                let page = unloaded(host, scope).into_iter().find(|p| p.id == *wanted);
                if let Some(page) = page {
                    found = load(host, scope, &page.page).await.ok();
                }
            }
            let session = match found {
                Some(target) => attach(&client, &target, None, true).await.ok(),
                None => None,
            };
            let Some(session) = session else {
                let _ = client.out.send(Input::Close);
                return;
            };
            let pending = {
                let mut state = client.state.borrow_mut();
                state.root = Some(session);
                state.pending.take().unwrap_or_default()
            };
            started.extend(pending.into_iter().filter_map(|m| on_message(&client, m)));
        }
        future::join(future::join_all(started), watch(client)).await;
    }
    .boxed_local()
}

/// The own session of the latest internal page client (the app's docked
/// DevTools) on the page whose channel is `cdp`.
pub(super) fn internal_session(cdp: &Rc<CdpClient>) -> Option<String> {
    CLIENTS.with_borrow(|clients| {
        (clients.iter())
            .filter(|(_, client)| client.internal && matches!(client.kind, Kind::Page(_)))
            .filter_map(|(id, client)| {
                let state = client.state.borrow();
                let root = state.root.as_ref()?;
                (Rc::ptr_eq(&root.cdp, cdp) && !state.closed)
                    .then(|| root.own.clone())
                    .flatten()
                    .map(|own| (*id, own))
            })
            .max_by_key(|(id, _)| *id)
            .map(|(_, own)| own)
    })
}

/// One message of client `id`.
pub(super) fn message(id: u64, message: Value) -> LocalBoxFuture<'static, ()> {
    let client = CLIENTS.with_borrow(|clients| clients.get(&id).cloned());
    client
        .and_then(|client| on_message(&client, message))
        .unwrap_or_else(|| async {}.boxed_local())
}

/// Client `id` went away: its sessions end.
pub(super) fn close(id: u64) {
    let Some(client) = CLIENTS.with_borrow_mut(|clients| clients.remove(&id)) else {
        return;
    };
    let state = std::mem::take(&mut *client.state.borrow_mut());
    client.state.borrow_mut().closed = true;
    drop(state);
}

/// Reports the scope's pages as they come, change and go.
async fn watch(client: Rc<Client>) {
    loop {
        client.host.sleep(POLL).await;
        if client.closed() {
            return;
        }
        refresh(&client).await;
    }
}

async fn refresh(client: &Rc<Client>) {
    let (host, scope) = (&client.host, &client.scope);
    let mut now = targets(host, scope).await;
    seen(client, &now);
    // A client that attaches every page gets restored tabs loaded.
    if matches!(client.kind, Kind::Browser)
        && !client.state.borrow().auto.is_empty()
        && load_all(client).await
    {
        now = targets(host, scope).await;
    }
    if client.closed() {
        return;
    }
    if let Kind::Page(_) = &client.kind {
        // A stand-in id's page is known by its real one once attached.
        let attached = client
            .state
            .borrow()
            .root
            .as_ref()
            .map(|s| s.target.clone());
        if attached.is_none_or(|id| !now.iter().any(|t| t.id == id)) {
            // Chrome closes a page's socket when the page goes.
            let _ = client.out.send(Input::Close);
        }
        return;
    }
    let waiting = unloaded(host, scope);
    let current: HashSet<&str> = (now.iter().map(|t| t.id.as_str()))
        .chain(waiting.iter().map(|p| p.id.as_str()))
        .collect();
    let gone: HashSet<String> = {
        let state = client.state.borrow();
        state
            .known
            .keys()
            .chain(state.sessions.values().map(|s| &s.target))
            .chain(state.tabs.values().map(|t| &t.target))
            .filter(|id| !current.contains(id.as_str()))
            .cloned()
            .collect()
    };
    for id in gone {
        target_gone(client, &id);
    }
    for target in &now {
        announce(client, target).await;
    }
    for page in &waiting {
        let (info, tab) = (
            client.unloaded_info(page, false),
            client.unloaded_info(page, true),
        );
        discovered(client, &page.id, info, tab);
    }
}

/// Reports a target that is new or changed to a client that discovers.
fn discovered(client: &Client, id: &str, info: Value, tab: Value) {
    let change = {
        let mut state = client.state.borrow_mut();
        let change = match state.known.get(id) {
            None => Some("Target.targetCreated"),
            Some(old) if old["url"] != info["url"] || old["title"] != info["title"] => {
                Some("Target.targetInfoChanged")
            }
            Some(_) => None,
        };
        if change.is_some() {
            state.known.insert(id.to_owned(), info.clone());
        }
        change.zip(state.discover.clone())
    };
    if let Some((method, filter)) = change {
        if filter.wants("page") {
            client.event(None, method, json!({"targetInfo": info}));
        }
        if filter.wants("tab") {
            client.event(None, method, json!({"targetInfo": tab}));
        }
    }
}

/// Reports a target that is new or changed, and auto-attaches it.
async fn announce(client: &Rc<Client>, target: &Target) {
    discovered(
        client,
        &target.id,
        client.info(target, false),
        client.info(target, true),
    );
    let keys: Vec<Option<String>> = client.state.borrow().auto.keys().cloned().collect();
    for key in keys {
        auto_attach(client, &key, target).await;
    }
}

/// Attaches `target` for the browser-level session `key` that auto-attaches,
/// once; returns when it is announced.
async fn auto_attach(client: &Rc<Client>, key: &Option<String>, target: &Target) {
    let Some(filter) = client.state.borrow().auto.get(key).cloned() else {
        return;
    };
    let mine = {
        let mut state = client.state.borrow_mut();
        let slot = (key.clone(), target.id.clone());
        match state.attaching.get(&slot) {
            Some(done) => Err(done.clone()),
            None => {
                let done = Rc::new(Cell::new(false));
                state.attaching.insert(slot, done.clone());
                Ok(done)
            }
        }
    };
    match mine {
        Ok(done) => {
            if filter.wants("page") {
                let _ = attach(client, target, key.clone(), false).await;
            } else if filter.wants("tab") {
                attach_tab(client, target, key.clone());
            }
            done.set(true);
        }
        // Another task attaches it: wait for its announcement, which must
        // come before, say, the answer to `Target.createTarget`.
        Err(done) => {
            for _ in 0..1000 {
                if done.get() || client.closed() {
                    break;
                }
                client.host.sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// Opens a session on `target` for the client and, unless it is a page
/// connection's, announces it on `parent`.
async fn attach(
    client: &Rc<Client>,
    target: &Target,
    parent: Option<String>,
    page_connection: bool,
) -> Result<Rc<PageSession>, String> {
    let call = target.cdp.call(
        "Target.attachToTarget",
        json!({"targetId": target.id, "flatten": true}),
        None,
    );
    let own = match timed(&client.host, call, CALL_TIMEOUT).await {
        None => return Err("the page did not answer".into()),
        Some(Ok(answer)) => answer["sessionId"].as_str().map(str::to_owned),
        // Chromium would not give a session of its own: share the root one.
        Some(Err(CdpError::Protocol { .. })) => None,
        Some(Err(error)) => return Err(error.to_string()),
    };
    let visible = (!page_connection).then(|| own.clone().unwrap_or_else(session_id));
    let children = Rc::new(RefCell::new(HashSet::new()));
    let events = {
        let (out, own, visible, children, id) = (
            client.out.clone(),
            own.clone(),
            visible.clone(),
            children.clone(),
            target.id.clone(),
        );
        target.cdp.subscribe(move |event| {
            if let Some(message) = route_event(event, &own, visible.as_deref(), &children, &id) {
                let _ = out.send(Input::Send(message));
            }
        })
    };
    let session = Rc::new(PageSession {
        visible: visible.clone(),
        own,
        parent: parent.clone(),
        target: target.id.clone(),
        page: target.info.id,
        cdp: target.cdp.clone(),
        children,
        _events: events,
        _drawn: target.page.keep_drawn(),
    });
    if client.closed() {
        return Err("the client went away".into());
    }
    let info = client.info(target, false);
    {
        let mut state = client.state.borrow_mut();
        state
            .known
            .entry(target.id.clone())
            .or_insert_with(|| info.clone());
        if let Some(visible) = &visible {
            state.sessions.insert(visible.clone(), session.clone());
        }
    }
    if let Some(visible) = &visible {
        client.event(
            parent.as_deref(),
            "Target.attachedToTarget",
            json!({"sessionId": visible, "targetInfo": info, "waitingForDebugger": false}),
        );
    }
    Ok(session)
}

/// A page event, as the client of one session should see it, if at all.
fn route_event(
    event: &CdpEvent,
    own: &Option<String>,
    visible: Option<&str>,
    children: &RefCell<HashSet<String>>,
    target: &str,
) -> Option<Value> {
    let tag = if event.session_id == *own {
        // On the shared root session, our own sessions on the page are not
        // the client's business.
        let about_self =
            event.params["targetInfo"]["targetId"] == target || event.params["targetId"] == target;
        if own.is_none() && event.method.starts_with("Target.") && about_self {
            return None;
        }
        visible.map(str::to_owned)
    } else {
        let session = event
            .session_id
            .as_ref()
            .filter(|s| children.borrow().contains(*s))?;
        Some(session.clone())
    };
    if let Some(child) = event.params["sessionId"].as_str() {
        match event.method.as_str() {
            "Target.attachedToTarget" => {
                children.borrow_mut().insert(child.to_owned());
            }
            "Target.detachedFromTarget" => {
                children.borrow_mut().remove(child);
            }
            _ => {}
        }
    }
    let mut message = json!({"method": event.method, "params": event.params});
    retag(&mut message, tag.as_deref());
    Some(message)
}

/// Opens a tab target's session and announces it on `parent`.
fn attach_tab(client: &Rc<Client>, target: &Target, parent: Option<String>) -> String {
    let session = session_id();
    {
        let mut state = client.state.borrow_mut();
        state
            .known
            .entry(target.id.clone())
            .or_insert_with(|| client.info(target, false));
        state.tabs.insert(
            session.clone(),
            Tab {
                target: target.id.clone(),
                parent: parent.clone(),
                page_session: None,
            },
        );
    }
    client.event(
        parent.as_deref(),
        "Target.attachedToTarget",
        json!({"sessionId": session, "targetInfo": client.info(target, true), "waitingForDebugger": false}),
    );
    session
}

/// Ends one of the client's sessions and says so; false if it has none by
/// that id.
fn detach(client: &Client, session: &str) -> bool {
    let mut state = client.state.borrow_mut();
    if let Some(page) = state.sessions.remove(session) {
        for tab in state.tabs.values_mut() {
            if tab.page_session.as_deref() == Some(session) {
                tab.page_session = None;
            }
        }
        drop(state);
        client.event(
            page.parent.as_deref(),
            "Target.detachedFromTarget",
            json!({"sessionId": session, "targetId": page.target}),
        );
        return true;
    }
    if let Some(tab) = state.tabs.remove(session) {
        let page = tab
            .page_session
            .as_ref()
            .and_then(|p| state.sessions.remove(p));
        drop(state);
        if let Some(page) = page {
            client.event(
                Some(session),
                "Target.detachedFromTarget",
                json!({"sessionId": page.visible, "targetId": page.target}),
            );
        }
        client.event(
            tab.parent.as_deref(),
            "Target.detachedFromTarget",
            json!({"sessionId": session, "targetId": tab_id(&tab.target)}),
        );
        return true;
    }
    if state.browser_sessions.remove(session) {
        let key = Some(session.to_owned());
        state.auto.remove(&key);
        state.attaching.retain(|(k, _), _| *k != key);
        let below: Vec<String> = state
            .sessions
            .iter()
            .filter(|(_, s)| s.parent == key)
            .map(|(id, _)| id.clone())
            .chain(
                state
                    .tabs
                    .iter()
                    .filter(|(_, t)| t.parent == key)
                    .map(|(id, _)| id.clone()),
            )
            .collect();
        drop(state);
        for id in below {
            detach(client, &id);
        }
        client.event(
            None,
            "Target.detachedFromTarget",
            json!({"sessionId": session, "targetId": client.browser_id}),
        );
        return true;
    }
    false
}

/// A target went away (closed, or its DevTools agent did).
fn target_gone(client: &Client, id: &str) {
    let sessions: Vec<String> = {
        let state = client.state.borrow();
        state
            .tabs
            .iter()
            .filter(|(_, t)| t.target == id)
            .map(|(s, _)| s.clone())
            .chain(
                state
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.target == id)
                    .map(|(s, _)| s.clone()),
            )
            .collect()
    };
    for session in sessions {
        detach(client, &session);
    }
    let discover = {
        let mut state = client.state.borrow_mut();
        state.attaching.retain(|(_, target), _| target != id);
        state.known.remove(id).and(state.discover.clone())
    };
    if let Some(filter) = discover {
        if filter.wants("page") {
            client.event(None, "Target.targetDestroyed", json!({"targetId": id}));
        }
        if filter.wants("tab") {
            client.event(
                None,
                "Target.targetDestroyed",
                json!({"targetId": tab_id(id)}),
            );
        }
    }
}

enum Route {
    Browser,
    Tab,
    Page(Rc<PageSession>),
    /// A child session (iframe, worker) under a page session.
    Child(Rc<PageSession>),
    Unknown,
}

/// Handles one message: what can be done at once is done before this
/// returns, so messages to a page keep their order; the rest is the future.
fn on_message(client: &Rc<Client>, raw: Value) -> Option<LocalBoxFuture<'static, ()>> {
    {
        let mut state = client.state.borrow_mut();
        if let Some(pending) = &mut state.pending {
            pending.push(raw);
            return None;
        }
    }
    let request = Request {
        id: raw["id"].clone(),
        method: raw["method"].as_str().unwrap_or_default().to_owned(),
        params: raw.get("params").cloned().unwrap_or_else(|| json!({})),
        session: raw
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw,
    };
    let route = {
        let state = client.state.borrow();
        match (&client.kind, &request.session) {
            (Kind::Browser, None) => Route::Browser,
            (Kind::Page(_), None) => state.root.clone().map_or(Route::Unknown, Route::Page),
            (_, Some(s)) if state.browser_sessions.contains(s) => Route::Browser,
            (_, Some(s)) if state.tabs.contains_key(s) => Route::Tab,
            (_, Some(s)) => match state.sessions.get(s) {
                Some(page) => Route::Page(page.clone()),
                None => state
                    .sessions
                    .values()
                    .chain(state.root.as_ref())
                    .find(|p| p.children.borrow().contains(s))
                    .cloned()
                    .map_or(Route::Unknown, Route::Child),
            },
        }
    };
    match route {
        Route::Browser => browser_command(client, request),
        Route::Tab => tab_command(client, request),
        Route::Page(session) => page_command(client, &session, request),
        Route::Child(session) => child_command(client, &session, request),
        Route::Unknown => {
            client.reply(
                &request,
                Err((-32001, "Session with given id not found.".into())),
            );
            None
        }
    }
}

fn ok(client: &Client, request: &Request) -> Option<LocalBoxFuture<'static, ()>> {
    client.reply(request, Ok(json!({})));
    None
}

/// The browser target, emulated: Chromium's own is not ours to give out.
fn browser_command(client: &Rc<Client>, request: Request) -> Option<LocalBoxFuture<'static, ()>> {
    let client = client.clone();
    let key = request.session.clone();
    let params = &request.params;
    match request.method.as_str() {
        "Browser.getVersion" => Some(
            async move {
                let version = version(&client.host, &client.scope).await;
                client.reply(&request, Ok(version));
            }
            .boxed_local(),
        ),
        "Browser.close" | "Browser.crash" | "Browser.crashGpuProcess" => {
            let refusal =
                failed("Monocode's browser is not closed over CDP; close its pages instead");
            client.reply(&request, Err(refusal));
            None
        }
        // Downloads go where the user's do; activating never steals focus.
        "Browser.setDownloadBehavior"
        | "Runtime.runIfWaitingForDebugger"
        | "Target.activateTarget" => ok(&client, &request),
        "Target.getBrowserContexts" => {
            // Like Chrome, without the default context.
            client.reply(&request, Ok(json!({"browserContextIds": []})));
            None
        }
        "Target.createBrowserContext" | "Target.disposeBrowserContext" => {
            let refusal = failed(
                "Monocode's browser has one browser context per project; new ones are not supported",
            );
            client.reply(&request, Err(refusal));
            None
        }
        "Target.setDiscoverTargets" => {
            if !params["discover"].as_bool().unwrap_or(false) {
                client.state.borrow_mut().discover = None;
                return ok(&client, &request);
            }
            let filter = Filter::parse(params.get("filter"));
            Some(
                async move {
                    let now = targets(&client.host, &client.scope).await;
                    let waiting = unloaded(&client.host, &client.scope);
                    client.state.borrow_mut().discover = Some(filter.clone());
                    let infos = (now.iter())
                        .map(|t| (&t.id, client.info(t, false), client.info(t, true)))
                        .chain(waiting.iter().map(|p| {
                            let info = client.unloaded_info(p, false);
                            (&p.id, info, client.unloaded_info(p, true))
                        }));
                    for (id, info, tab) in infos {
                        client
                            .state
                            .borrow_mut()
                            .known
                            .insert(id.clone(), info.clone());
                        if filter.wants("tab") {
                            client.event(None, "Target.targetCreated", json!({"targetInfo": tab}));
                        }
                        if filter.wants("page") {
                            client.event(None, "Target.targetCreated", json!({"targetInfo": info}));
                        }
                    }
                    client.reply(&request, Ok(json!({})));
                }
                .boxed_local(),
            )
        }
        "Target.getTargets" => {
            let filter = Filter::parse(params.get("filter"));
            Some(
                async move {
                    let mut infos = Vec::new();
                    for target in targets(&client.host, &client.scope).await {
                        if filter.wants("tab") {
                            infos.push(client.info(&target, true));
                        }
                        if filter.wants("page") {
                            infos.push(client.info(&target, false));
                        }
                    }
                    for page in unloaded(&client.host, &client.scope) {
                        if filter.wants("tab") {
                            infos.push(client.unloaded_info(&page, true));
                        }
                        if filter.wants("page") {
                            infos.push(client.unloaded_info(&page, false));
                        }
                    }
                    client.reply(&request, Ok(json!({"targetInfos": infos})));
                }
                .boxed_local(),
            )
        }
        "Target.getTargetInfo" => {
            let Some(wanted) = params["targetId"].as_str().map(str::to_owned) else {
                client.reply(&request, Ok(json!({"targetInfo": client.browser_info()})));
                return None;
            };
            Some(
                async move {
                    let info = if wanted == client.browser_id {
                        Some(client.browser_info())
                    } else {
                        let found =
                            (targets(&client.host, &client.scope).await.iter()).find_map(|t| {
                                if t.id == wanted {
                                    Some(client.info(t, false))
                                } else {
                                    (tab_id(&t.id) == wanted).then(|| client.info(t, true))
                                }
                            });
                        found.or_else(|| {
                            unloaded(&client.host, &client.scope).iter().find_map(|p| {
                                if p.id == wanted {
                                    Some(client.unloaded_info(p, false))
                                } else {
                                    (tab_id(&p.id) == wanted).then(|| client.unloaded_info(p, true))
                                }
                            })
                        })
                    };
                    let answer = info
                        .map(|info| json!({"targetInfo": info}))
                        .ok_or_else(|| invalid("No target with given id found"));
                    client.reply(&request, answer);
                }
                .boxed_local(),
            )
        }
        "Target.setAutoAttach" => {
            if !params["autoAttach"].as_bool().unwrap_or(false) {
                client.state.borrow_mut().auto.remove(&key);
                return ok(&client, &request);
            }
            if params["flatten"] != true {
                client.reply(&request, Err(failed("Only flatten mode is supported")));
                return None;
            }
            let filter = Filter::parse(params.get("filter"));
            client.state.borrow_mut().auto.insert(key.clone(), filter);
            Some(
                async move {
                    // Every page, restored tabs too: they load now.
                    seen(&client, &targets(&client.host, &client.scope).await);
                    load_all(&client).await;
                    for target in targets(&client.host, &client.scope).await {
                        auto_attach(&client, &key, &target).await;
                    }
                    client.reply(&request, Ok(json!({})));
                }
                .boxed_local(),
            )
        }
        "Target.attachToTarget" => {
            let Some(wanted) = params["targetId"].as_str().map(str::to_owned) else {
                client.reply(&request, Err(invalid("Invalid parameters: targetId")));
                return None;
            };
            if params["flatten"] != true {
                client.reply(&request, Err(failed("Only flatten mode is supported")));
                return None;
            }
            Some(
                async move {
                    let (host, scope) = (&client.host, &client.scope);
                    let mut now = targets(host, scope).await;
                    let mut wanted = wanted;
                    // A restored tab loads, and is attached by its real id.
                    let stand_in = unloaded(host, scope)
                        .into_iter()
                        .find(|p| p.id == wanted || tab_id(&p.id) == wanted);
                    if let Some(page) = stand_in {
                        match load(host, scope, &page.page).await {
                            Ok(target) => {
                                let tab = wanted != page.id;
                                wanted = if tab {
                                    tab_id(&target.id)
                                } else {
                                    target.id.clone()
                                };
                                now.push(target);
                            }
                            Err(error) => {
                                client.reply(&request, Err(failed(error)));
                                return;
                            }
                        }
                    }
                    let answer = if let Some(target) = now.iter().find(|t| t.id == wanted) {
                        attach(&client, target, key, false)
                            .await
                            .map(|s| json!({"sessionId": s.visible}))
                            .map_err(failed)
                    } else if let Some(target) = now.iter().find(|t| tab_id(&t.id) == wanted) {
                        Ok(json!({"sessionId": attach_tab(&client, target, key)}))
                    } else {
                        Err(invalid("No target with given id found"))
                    };
                    client.reply(&request, answer);
                }
                .boxed_local(),
            )
        }
        "Target.attachToBrowserTarget" => {
            let session = session_id();
            client
                .state
                .borrow_mut()
                .browser_sessions
                .insert(session.clone());
            client.event(
                key.as_deref(),
                "Target.attachedToTarget",
                json!({"sessionId": session, "targetInfo": client.browser_info(), "waitingForDebugger": false}),
            );
            client.reply(&request, Ok(json!({"sessionId": session})));
            None
        }
        "Target.detachFromTarget" => {
            let session = params["sessionId"].as_str().map(str::to_owned).or_else(|| {
                let wanted = params["targetId"].as_str()?;
                let state = client.state.borrow();
                state
                    .sessions
                    .iter()
                    .find(|(_, s)| s.target == wanted && s.parent == key)
                    .map(|(id, _)| id.clone())
            });
            let answer = match session {
                Some(session) if detach(&client, &session) => Ok(json!({})),
                _ => Err(invalid("No session with given id")),
            };
            client.reply(&request, answer);
            None
        }
        "Target.createTarget" => {
            let url = params["url"]
                .as_str()
                .filter(|u| !u.is_empty())
                .unwrap_or("about:blank")
                .to_owned();
            if let Some(context) = params["browserContextId"]
                .as_str()
                .filter(|c| *c != client.context)
            {
                let refusal = invalid(&format!("Failed to find browser context with id {context}"));
                client.reply(&request, Err(refusal));
                return None;
            }
            Some(
                async move {
                    match open_target(&client.host, &client.scope, &url).await {
                        Ok(target) => {
                            // Chrome announces the new target before it answers.
                            announce(&client, &target).await;
                            client.reply(&request, Ok(json!({"targetId": target.id})));
                        }
                        Err(error) => client.reply(&request, Err(failed(error))),
                    }
                }
                .boxed_local(),
            )
        }
        "Target.closeTarget" => {
            let wanted = params["targetId"].as_str().unwrap_or_default().to_owned();
            Some(
                async move {
                    let now = targets(&client.host, &client.scope).await;
                    let waiting = unloaded(&client.host, &client.scope);
                    let found = (now.iter().map(|t| (&t.id, t.info.id)))
                        .chain(waiting.iter().map(|p| (&p.id, p.info.id)))
                        .find(|(id, _)| **id == wanted || tab_id(id) == wanted);
                    let Some((_, page)) = found else {
                        client.reply(&request, Err(invalid("No target with given id found")));
                        return;
                    };
                    let closed = client.host.close(&client.scope, page);
                    client.reply(&request, Ok(json!({"success": closed})));
                    if closed {
                        refresh(&client).await;
                    }
                }
                .boxed_local(),
            )
        }
        "Storage.getCookies" | "Storage.setCookies" | "Storage.clearCookies" => Some(
            async move {
                let answer = cookies(&client, &request).await;
                client.reply(&request, answer);
            }
            .boxed_local(),
        ),
        method => {
            client.reply(&request, Err(not_found(method)));
            None
        }
    }
}

/// Cookie calls of the browser target, done on a page of the profile.
async fn cookies(client: &Rc<Client>, request: &Request) -> Result<Value, Failure> {
    let now = targets(&client.host, &client.scope).await;
    let page = now
        .iter()
        .find(|t| !t.info.isolated)
        .or(now.first())
        .ok_or_else(|| failed("No page is open to reach the cookies through"))?;
    let (method, params) = match request.method.as_str() {
        "Storage.getCookies" => ("Network.getAllCookies", json!({})),
        "Storage.setCookies" => (
            "Network.setCookies",
            json!({"cookies": request.params["cookies"]}),
        ),
        _ => ("Network.clearBrowserCookies", json!({})),
    };
    let answer = timed(
        &client.host,
        page.cdp.call(method, params, None),
        CALL_TIMEOUT,
    )
    .await
    .ok_or_else(|| failed("The page did not answer"))?
    .map_err(|e| failed(e.to_string()))?;
    Ok(if request.method == "Storage.getCookies" {
        json!({"cookies": answer["cookies"]})
    } else {
        json!({})
    })
}

/// A tab target's session: it only attaches its page.
fn tab_command(client: &Rc<Client>, request: Request) -> Option<LocalBoxFuture<'static, ()>> {
    let session = request.session.clone().unwrap_or_default();
    let wanted = client
        .state
        .borrow()
        .tabs
        .get(&session)
        .map(|t| t.target.clone())?;
    let params = &request.params;
    match request.method.as_str() {
        "Runtime.runIfWaitingForDebugger" => ok(client, &request),
        "Target.getTargetInfo" | "Target.setAutoAttach" => {
            let auto = request.method == "Target.setAutoAttach";
            if auto && !params["autoAttach"].as_bool().unwrap_or(false) {
                return ok(client, &request);
            }
            let filter = Filter::parse(params.get("filter"));
            let client = client.clone();
            Some(
                async move {
                    let now = targets(&client.host, &client.scope).await;
                    let Some(target) = now.iter().find(|t| t.id == wanted) else {
                        client.reply(&request, Err(failed("Target closed")));
                        return;
                    };
                    if !auto {
                        client.reply(
                            &request,
                            Ok(json!({"targetInfo": client.info(target, true)})),
                        );
                        return;
                    }
                    let attached = client
                        .state
                        .borrow()
                        .tabs
                        .get(&session)
                        .is_some_and(|t| t.page_session.is_some());
                    if filter.wants("page") && !attached {
                        if let Ok(page) =
                            attach(&client, target, Some(session.clone()), false).await
                        {
                            if let Some(tab) = client.state.borrow_mut().tabs.get_mut(&session) {
                                tab.page_session = page.visible.clone();
                            }
                        }
                    }
                    client.reply(&request, Ok(json!({})));
                }
                .boxed_local(),
            )
        }
        "Target.detachFromTarget" => {
            let page = client
                .state
                .borrow()
                .tabs
                .get(&session)
                .and_then(|t| t.page_session.clone());
            let answer = match page {
                Some(page) if params["sessionId"] == page.as_str() && detach(client, &page) => {
                    Ok(json!({}))
                }
                _ => Err(invalid("No session with given id")),
            };
            client.reply(&request, answer);
            None
        }
        method => {
            client.reply(&request, Err(not_found(method)));
            None
        }
    }
}

/// What a page or child session may not do; `None` when it may.
fn refusal(method: &str, params: &Value, children: &RefCell<HashSet<String>>) -> Option<Failure> {
    if REFUSED_ON_PAGES.contains(&method) {
        return Some(failed(format!(
            "'{method}' is not allowed on Monocode's pages"
        )));
    }
    if !method.starts_with("Target.") || method == "Target.activateTarget" {
        return None;
    }
    let allowed = TARGET_ON_PAGES.contains(&method)
        && match method {
            "Target.getTargetInfo" => params.get("targetId").is_none(),
            "Target.detachFromTarget" => params["sessionId"]
                .as_str()
                .is_some_and(|s| children.borrow().contains(s)),
            _ => true,
        };
    (!allowed).then(|| failed(format!("'{method}' is not allowed on a page session here")))
}

fn page_command(
    client: &Rc<Client>,
    session: &Rc<PageSession>,
    request: Request,
) -> Option<LocalBoxFuture<'static, ()>> {
    let shared = session.own.is_none();
    match request.method.as_str() {
        // Never steals the user's focus.
        "Page.bringToFront" | "Target.activateTarget" => return ok(client, &request),
        // The browser's downloads are the user's.
        "Browser.setDownloadBehavior" => return ok(client, &request),
        "Page.close" => {
            client.host.close(&client.scope, session.page);
            return ok(client, &request);
        }
        method if shared && SWALLOWED.contains(&method) => return ok(client, &request),
        method => {
            if let Some(refusal) = refusal(method, &request.params, &session.children) {
                client.reply(&request, Err(refusal));
                return None;
            }
        }
    }
    let mut message = request.raw.clone();
    if request.method == "Target.setAutoAttach" && !auto_attach_kept(&mut message) {
        return ok(client, &request);
    }
    if shared && request.method == "Runtime.enable" {
        // Runtime is on already for our log, and enabling it again does not
        // report the existing contexts, which the client needs.
        drop(session.cdp.call("Runtime.disable", json!({}), None));
    }
    retag(&mut message, session.own.as_deref());
    forward(
        client,
        &session.cdp,
        message,
        session.visible.clone(),
        &request,
    );
    None
}

/// A client's `Target.setAutoAttach` on a page or child session, made safe
/// for our tools, which auto-attach on the page too (K42): nothing waits for
/// a debugger (no one may be left to resume it, and a paused iframe stalls
/// our tools as well), always flatten, and auto-attach is never turned off
/// (false: answer `{}` and send nothing). Child sessions' attach events reach
/// both us and the client.
fn auto_attach_kept(message: &mut Value) -> bool {
    if !message["params"]["autoAttach"].as_bool().unwrap_or(false) {
        return false;
    }
    message["params"]["waitForDebuggerOnStart"] = false.into();
    message["params"]["flatten"] = true.into();
    true
}

fn child_command(
    client: &Rc<Client>,
    session: &Rc<PageSession>,
    request: Request,
) -> Option<LocalBoxFuture<'static, ()>> {
    if let Some(refusal) = refusal(&request.method, &request.params, &session.children) {
        client.reply(&request, Err(refusal));
        return None;
    }
    let mut message = request.raw.clone();
    match request.method.as_str() {
        // Nothing waits: see `auto_attach_kept`.
        "Runtime.runIfWaitingForDebugger" => return ok(client, &request),
        "Target.setAutoAttach" if !auto_attach_kept(&mut message) => return ok(client, &request),
        _ => {}
    }
    forward(
        client,
        &session.cdp,
        message,
        request.session.clone(),
        &request,
    );
    None
}

/// Sends `message` to the page; its answer goes to the client under `visible`.
fn forward(
    client: &Client,
    cdp: &CdpClient,
    message: Value,
    visible: Option<String>,
    request: &Request,
) {
    let out = client.out.clone();
    let sent = cdp.forward(
        message,
        Box::new(move |mut answer| {
            retag(&mut answer, visible.as_deref());
            let _ = out.send(Input::Send(answer));
        }),
    );
    if !sent {
        client.reply(request, Err(failed("Target closed")));
    }
}
