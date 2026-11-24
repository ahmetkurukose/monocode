//! `mcext://<id>/…` on CEF's IO thread (K69). Every request is decided by
//! `monocode_core::browser::extensions::access`: the extension's own
//! contexts load anything, web pages only its web-accessible resources,
//! and the layer's files (`__mcext/…`) only the extension. Pages get the
//! shim as their first script and the manifest's CSP. The bridge (calls,
//! the event poll, the fetch proxy) also needs the run's token, which only
//! the shim carries; calls and polls go to the main thread.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, Instant},
};

use gpui::App;
use monocode_cef::extensions::{
    self as engine, ExtRequest, ExtensionRequests, FetchSink, FetchSpec, Head, Load, Responder,
    Writer,
};
use monocode_core::browser::extensions::{
    self as core,
    access::{self, Access, Requester, Who},
    scripts,
};
use serde_json::Value;

use super::{Ext, InstanceKey, ProfileKey};

/// A bridge message for the main thread.
pub(crate) enum Bridge {
    Call {
        key: InstanceKey,
        ctx: String,
        api: String,
        args: Vec<Value>,
        browser_id: Option<i32>,
        frame_url: Option<String>,
        responder: Responder,
    },
    Poll {
        key: InstanceKey,
        ctx: String,
        browser_id: Option<i32>,
        frame_url: Option<String>,
        responder: Responder,
    },
}

struct Served {
    ext: Arc<Ext>,
    shim: String,
    background_host: String,
    worker: String,
}

fn served() -> &'static RwLock<HashMap<String, Arc<Served>>> {
    static SERVED: OnceLock<RwLock<HashMap<String, Arc<Served>>>> = OnceLock::new();
    SERVED.get_or_init(Default::default)
}

/// The run's bridge token: only the shim knows it.
pub(crate) fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(core::random_token)
}

/// Browsers the layer hosts (background, popup, offscreen), by id.
fn hosts() -> &'static Mutex<HashMap<i32, String>> {
    static HOSTS: OnceLock<Mutex<HashMap<i32, String>>> = OnceLock::new();
    HOSTS.get_or_init(Default::default)
}

/// Top-level addresses the app opens itself, for a short while.
fn expected() -> &'static Mutex<Vec<(String, Instant)>> {
    static EXPECTED: OnceLock<Mutex<Vec<(String, Instant)>>> = OnceLock::new();
    EXPECTED.get_or_init(Default::default)
}

const EXPECTED_FOR: Duration = Duration::from_secs(30);

/// The app is about to load `url` top-level (a tab, a host page).
pub(crate) fn expect(url: &str) {
    let mut list = expected().lock().unwrap();
    list.retain(|(_, at)| at.elapsed() < EXPECTED_FOR);
    list.push((url.to_owned(), Instant::now()));
}

fn take_expected(url: &str) -> bool {
    let mut list = expected().lock().unwrap();
    list.retain(|(_, at)| at.elapsed() < EXPECTED_FOR);
    match list.iter().position(|(u, _)| u == url) {
        Some(at) => {
            list.remove(at);
            true
        }
        None => false,
    }
}

/// Frames this extension's own documents load in, for a short while: their
/// first requests may come before Chromium gives the frame its new URL.
fn own_frames() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static OWN: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    OWN.get_or_init(Default::default)
}

const OWN_FRAME_FOR: Duration = Duration::from_secs(60);

/// Extension `id`'s document loads in frame `frame`.
fn note_own_frame(frame: &str, id: &str) {
    let mut own = own_frames().lock().unwrap();
    own.retain(|_, (_, at)| at.elapsed() < OWN_FRAME_FOR);
    own.insert(frame.to_owned(), (id.to_owned(), Instant::now()));
}

/// Whether frame `frame` loads a document of extension `id` (its URL is
/// not known yet).
fn is_own_frame(frame: &str, id: &str) -> bool {
    let own = own_frames().lock().unwrap();
    own.get(frame)
        .is_some_and(|(owner, at)| owner == id && at.elapsed() < OWN_FRAME_FOR)
}

/// One-time keys put in the shim tag of the documents this extension's
/// frames load, for a short while. A frame's first requests come before
/// Chromium gives it its URL; the key in its shim's address says the
/// request is that document's, and only the document has it (a page
/// cannot read a cross-origin frame's document).
fn doc_keys() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static KEYS: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    KEYS.get_or_init(Default::default)
}

const DOC_KEY_FOR: Duration = Duration::from_secs(30);

/// A new key for a document of extension `id`.
fn issue_doc_key(id: &str) -> String {
    let key = core::random_token();
    let mut keys = doc_keys().lock().unwrap();
    keys.retain(|_, (_, at)| at.elapsed() < DOC_KEY_FOR);
    keys.insert(key.clone(), (id.to_owned(), Instant::now()));
    key
}

/// Whether `key` is a fresh key of extension `id`'s, used up.
fn take_doc_key(key: &str, id: &str) -> bool {
    let mut keys = doc_keys().lock().unwrap();
    match keys.get(key) {
        Some((owner, at)) if owner == id && at.elapsed() < DOC_KEY_FOR => {
            keys.remove(key);
            true
        }
        _ => false,
    }
}

pub(crate) fn add_host(browser_id: i32, id: &str) {
    hosts().lock().unwrap().insert(browser_id, id.to_owned());
}

pub(crate) fn remove_host(browser_id: i32) {
    hosts().lock().unwrap().remove(&browser_id);
}

/// The enabled extensions, for the IO thread.
pub(crate) fn publish(exts: &HashMap<String, Arc<Ext>>) {
    let mut map = HashMap::new();
    for (id, ext) in exts {
        let config = scripts::ShimConfig {
            id,
            manifest: &ext.manifest,
            locale: &super::with(|rt| rt.locale.clone()).unwrap_or_else(|| "en-US".into()),
            expose: super::dev::dir().is_some(),
        };
        map.insert(
            id.clone(),
            Arc::new(Served {
                shim: scripts::page_shim(&config, token()),
                background_host: scripts::background_host_script(&ext.manifest),
                worker: scripts::worker_script(&ext.manifest),
                ext: ext.clone(),
            }),
        );
    }
    *served().write().unwrap() = map;
}

static BRIDGE: OnceLock<async_channel::Sender<Bridge>> = OnceLock::new();

/// Installs the handler, and hears the bridge on the main thread.
pub(super) fn start(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded();
    let _ = BRIDGE.set(tx);
    engine::set_handler(Arc::new(Handler));
    cx.foreground_executor()
        .spawn(async move {
            while let Ok(message) = rx.recv().await {
                super::instance::on_bridge(message);
            }
        })
        .detach();
}

struct Handler;

fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let rest = url.strip_prefix(&format!("{}://", core::SCHEME))?;
    let (id, rest) = rest.split_once('/').unwrap_or((rest, ""));
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Some((id, path, query))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_value(query: &str, name: &str) -> String {
    (query.split('&'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(&v.replace('+', " ")))
        .unwrap_or_default()
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

fn refuse(responder: Responder, status: i32) {
    responder.respond(Head::new(status, "text/plain"), Vec::new());
}

/// The shim first, before any of the page's own scripts; `key`, a
/// document's one-time key ([`issue_doc_key`]), in its address.
fn with_shim(html: &[u8], key: Option<&str>) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    let tag = match key {
        Some(key) => format!("<script src=\"/__mcext/shim.js?doc={key}\"></script>"),
        None => "<script src=\"/__mcext/shim.js\"></script>".to_owned(),
    };
    let lower = text.to_ascii_lowercase();
    let at = lower
        .find("<head")
        .and_then(|h| lower[h..].find('>').map(|e| h + e + 1))
        .or_else(|| {
            lower
                .find("<html")
                .and_then(|h| lower[h..].find('>').map(|e| h + e + 1))
        })
        .unwrap_or(0);
    format!("{}{tag}{}", &text[..at], &text[at..]).into_bytes()
}

/// A file of the extension; `None`: not there (or not a safe path).
fn file(dir: &Path, path: &str) -> Option<Vec<u8>> {
    let relative = Path::new(path);
    if path.is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    let full = dir.join(relative);
    // No symlinks out of the package.
    let real = full.canonicalize().ok()?;
    if !real.starts_with(dir.canonicalize().ok()?) {
        return None;
    }
    std::fs::read(real).ok()
}

impl ExtensionRequests for Handler {
    fn handle(&self, request: ExtRequest, responder: Responder) {
        let Some((id, path, query)) =
            split_url(&request.url).map(|(i, p, q)| (i.to_owned(), p.to_owned(), q.to_owned()))
        else {
            return refuse(responder, 404);
        };
        let (id, query) = (id.as_str(), query.as_str());
        let Some(served) = served().read().unwrap().get(id).cloned() else {
            return refuse(responder, 404);
        };
        // Throwaway profiles run no extension (K22).
        if request.profile.as_deref() == Some("") {
            return refuse(responder, 403);
        }
        let path = percent_decode(&path);
        let from_host = (request.browser_id)
            .is_some_and(|b| hosts().lock().unwrap().get(&b).is_some_and(|h| h == id));
        let expected = request.load == Load::MainFrame && take_expected(&request.url);
        let own_origin = monocode_core::browser::extensions::origin(id);
        // The frame's new document is this extension's, from before Chromium
        // gave the frame its URL: a request of that document.
        let unknown_frame = request.frame_url.as_deref().is_none_or(str::is_empty);
        // A main frame's site for cookies is its own document: this
        // extension's page, opened top-level (a tab it navigates to).
        let own_top = request.parent_url.is_none()
            && (request.first_party == own_origin
                || request
                    .first_party
                    .strip_prefix(&own_origin)
                    .is_some_and(|rest| rest.starts_with('/')));
        // A frame's own shim, by its document's key: the frame is this
        // extension's from now on (its scripts and styles follow).
        let keyed = path == "__mcext/shim.js"
            && request.load == Load::Resource
            && unknown_frame
            && take_doc_key(&query_value(query, "doc"), id);
        if keyed && let Some(frame) = &request.frame_id {
            note_own_frame(frame, id);
        }
        let own_frame = matches!(request.load, Load::Resource | Load::Worker)
            && unknown_frame
            && (own_top
                || keyed
                || (request.frame_id.as_deref()).is_some_and(|f| is_own_frame(f, id)));
        let requester = Requester {
            load: Some(match request.load {
                Load::MainFrame => access::Load::MainFrame,
                Load::SubFrame => access::Load::SubFrame,
                Load::Worker => access::Load::Worker,
                Load::Resource => access::Load::Resource,
            }),
            frame_url: if own_frame {
                Some(own_origin.as_str())
            } else {
                request.frame_url.as_deref()
            },
            parent_url: request.parent_url.as_deref(),
            referrer: &request.referrer,
            from_host,
            expected,
        };
        let who = access::who(id, &requester);
        let manifest = &served.ext.manifest;
        if access::decide(manifest, &path, &who) == Access::Deny {
            log::warn!(
                "browser: extension {id}: refused {path} to {who:?} ({:?}, frame {:?} {:?}, parent {:?}, first party {:?}, referrer {:?})",
                request.load,
                request.frame_url,
                request.frame_id,
                request.parent_url,
                request.first_party,
                request.referrer
            );
            return refuse(responder, 403);
        }
        if matches!(request.load, Load::MainFrame | Load::SubFrame)
            && let Some(frame) = &request.frame_id
        {
            note_own_frame(frame, id);
        }
        let key = InstanceKey {
            profile: request.profile.clone() as ProfileKey,
            id: id.to_owned(),
        };
        let csp = manifest.page_csp.clone();
        let script = |body: &str| {
            let head = Head::new(200, "text/javascript")
                .header("Content-Security-Policy", &csp)
                .header("Cache-Control", "no-store");
            (head, body.as_bytes().to_vec())
        };
        match path.as_str() {
            "__mcext/api" | "__mcext/events" | "__mcext/fetch" => {
                if request.header("x-mcext-token") != Some(token()) {
                    return refuse(responder, 403);
                }
                bridge(&path, query, request, key, &served, responder);
            }
            "__mcext/shim.js" => {
                let (head, body) = script(&served.shim);
                responder.respond(head, body);
            }
            "__mcext/background.html" => {
                let head = Head::new(200, "text/html").header("Content-Security-Policy", &csp);
                responder.respond(head, scripts::BACKGROUND_HTML.as_bytes().to_vec());
            }
            "__mcext/background-host.js" => {
                let (head, body) = script(&served.background_host);
                responder.respond(head, body);
            }
            "__mcext/background-worker.js" => {
                let (head, body) = script(&served.worker);
                responder.respond(head, body);
            }
            _ if path.starts_with(scripts::RESERVED) => refuse(responder, 404),
            _ => {
                let path = if path.is_empty() {
                    "index.html".into()
                } else {
                    path
                };
                let Some(bytes) = file(&served.ext.dir, &path) else {
                    return refuse(responder, 404);
                };
                let kind = mime(&path);
                let mut head = Head::new(200, kind);
                if matches!(who, Who::Web(_)) {
                    // Listed for this page: it may read it (fetch, fonts).
                    head = head.header("Access-Control-Allow-Origin", "*");
                }
                let body = if kind == "text/html" {
                    head = head.header("Content-Security-Policy", &csp);
                    // Only a document gets a key: a page may fetch a listed
                    // page and read it, but never a frame's document.
                    let document = matches!(request.load, Load::MainFrame | Load::SubFrame);
                    let key = document.then(|| issue_doc_key(id));
                    with_shim(&bytes, key.as_deref())
                } else {
                    if kind == "text/javascript" {
                        head = head.header("Content-Security-Policy", &csp);
                    }
                    bytes
                };
                responder.respond(head, body);
            }
        }
    }
}

fn bridge(
    path: &str,
    query: &str,
    request: ExtRequest,
    key: InstanceKey,
    served: &Served,
    responder: Responder,
) {
    let send = |message: Bridge| {
        if let Some(tx) = BRIDGE.get() {
            let _ = tx.try_send(message);
        }
    };
    match path {
        "__mcext/api" => {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            send(Bridge::Call {
                key,
                ctx: body["ctx"].as_str().unwrap_or("").to_owned(),
                api: body["api"].as_str().unwrap_or("").to_owned(),
                args: body["args"].as_array().cloned().unwrap_or_default(),
                browser_id: request.browser_id,
                frame_url: request.frame_url,
                responder,
            });
        }
        "__mcext/events" => send(Bridge::Poll {
            key,
            ctx: query_value(query, "ctx"),
            browser_id: request.browser_id,
            frame_url: request.frame_url,
            responder,
        }),
        _ => proxy_fetch(request, key, served, responder),
    }
}

/// The extension's cross-origin `fetch`, where it has host access.
fn proxy_fetch(request: ExtRequest, key: InstanceKey, served: &Served, responder: Responder) {
    let spec: Value = request
        .header("x-mcext-fetch")
        .and_then(|h| serde_json::from_str(&percent_decode(h)).ok())
        .unwrap_or_default();
    let url = spec["url"].as_str().unwrap_or("").to_owned();
    if !served.ext.hosts.matches(&url) {
        let head = Head::new(200, "text/plain").header("x-mcext-fallback", "1");
        return responder.respond(head, Vec::new());
    }
    let headers = (spec["headers"].as_array().into_iter().flatten())
        .filter_map(|pair| {
            let (name, value) = (pair[0].as_str()?, pair[1].as_str()?);
            // The engine sets these itself.
            (!["host", "content-length", "connection", "cookie"]
                .contains(&name.to_ascii_lowercase().as_str()))
            .then(|| (name.to_owned(), value.to_owned()))
        })
        .collect();
    let fetch = FetchSpec {
        url,
        method: spec["method"].as_str().unwrap_or("GET").to_owned(),
        headers,
        body: request.body,
        credentials: spec["credentials"].as_str() == Some("include"),
        stop_on_redirect: matches!(spec["redirect"].as_str(), Some("manual" | "error")),
        skip_cache: matches!(
            spec["cache"].as_str(),
            Some("no-store" | "reload" | "no-cache")
        ),
    };
    let slot = Arc::new(Mutex::new(None::<engine::FetchHandle>));
    let cancel = slot.clone();
    responder.on_cancel(move || {
        if let Some(handle) = cancel.lock().unwrap().take() {
            handle.cancel();
        }
    });
    let sink = Sink {
        responder: Some(responder),
        writer: None,
    };
    let handle = engine::fetch(key.profile, fetch, Box::new(sink));
    *slot.lock().unwrap() = Some(handle);
}

struct Sink {
    responder: Option<Responder>,
    writer: Option<Writer>,
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl FetchSink for Sink {
    fn head(
        &mut self,
        status: i32,
        status_text: String,
        headers: Vec<(String, String)>,
        url: String,
    ) {
        let Some(responder) = self.responder.take() else {
            return;
        };
        let list: Vec<Value> = (headers.into_iter())
            .map(|(k, v)| Value::Array(vec![k.into(), v.into()]))
            .collect();
        let head = Head::new(200, "application/octet-stream")
            .header("Cache-Control", "no-store")
            .header("x-mcext-status", &status.to_string())
            .header("x-mcext-status-text", &enc(&status_text))
            .header("x-mcext-url", &enc(&url))
            .header("x-mcext-headers", &enc(&Value::Array(list).to_string()));
        self.writer = Some(responder.start(head));
    }

    fn data(&mut self, bytes: &[u8]) {
        if let Some(writer) = &self.writer {
            writer.write(bytes);
        }
    }

    fn done(&mut self, result: Result<(), String>) {
        match (&self.writer, result) {
            (Some(writer), Ok(())) => writer.finish(None),
            // ERR_FAILED: the page's body read fails.
            (Some(writer), Err(_)) => writer.finish(Some(-2)),
            (None, result) => {
                if let Some(responder) = self.responder.take() {
                    let why = result.err().unwrap_or_default();
                    let head = Head::new(502, "text/plain");
                    responder.respond(head, why.into_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_and_pages_get_the_shim_first() {
        assert_eq!(
            split_url("mcext://abc/popup.html?x=1#y"),
            Some(("abc", "popup.html", "x=1"))
        );
        assert_eq!(split_url("mcext://abc"), Some(("abc", "", "")));
        assert_eq!(split_url("https://abc/"), None);
        let html = with_shim(
            b"<!doctype html><html><head><title>x</title><script src=a.js></script>",
            None,
        );
        let html = String::from_utf8(html).unwrap();
        assert!(html.starts_with(
            "<!doctype html><html><head><script src=\"/__mcext/shim.js\"></script><title>"
        ));
        let bare = String::from_utf8(with_shim(b"<div>", None)).unwrap();
        assert!(bare.starts_with("<script src=\"/__mcext/shim.js\">"));
        let keyed = String::from_utf8(with_shim(b"<head>", Some("k1"))).unwrap();
        assert_eq!(
            keyed,
            "<head><script src=\"/__mcext/shim.js?doc=k1\"></script>"
        );
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
        assert_eq!(query_value("ctx=a%2Fb&x=1", "ctx"), "a/b");
        assert_eq!(enc("a b/é"), "a%20b%2F%C3%A9");
    }

    #[test]
    fn a_document_key_is_its_extensions_and_works_once() {
        let key = issue_doc_key("ext-a");
        assert!(!take_doc_key(&key, "ext-b"));
        assert!(take_doc_key(&key, "ext-a"));
        assert!(!take_doc_key(&key, "ext-a"));
        assert!(!take_doc_key("", "ext-a"));
    }

    #[test]
    fn files_stay_inside_the_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.js"), "1").unwrap();
        assert_eq!(file(dir.path(), "a.js").as_deref(), Some(&b"1"[..]));
        assert!(file(dir.path(), "../a.js").is_none());
        assert!(file(dir.path(), "/etc/hosts").is_none());
        assert!(file(dir.path(), "").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", dir.path().join("link")).unwrap();
            assert!(file(dir.path(), "link").is_none());
        }
    }
}
