//! A page's console and network log (context.md K36), kept from the page's
//! first load: ring buffers fed by CDP events.
//!
//! Console: `Runtime.consoleAPICalled`, `Runtime.exceptionThrown`,
//! `Log.entryAdded`, and a mark for each main-frame navigation. Network:
//! request metadata only; bodies are fetched on demand with
//! `Network.getResponseBody`. Events of child sessions (iframes) are left out.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::{Rc, Weak},
};

use serde_json::{Value, json};

use super::cdp::{CdpClient, CdpError, CdpEvent, Subscription};

pub const CONSOLE_ENTRIES: usize = 500;
pub const NETWORK_ENTRIES: usize = 300;
/// A console message longer than this is cut.
const MAX_MESSAGE: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Log,
    Warning,
    Error,
    /// A main-frame navigation mark.
    Navigation,
}

impl Level {
    pub fn parse(name: &str) -> Option<Level> {
        Some(match name {
            "debug" | "verbose" => Level::Debug,
            "log" | "info" => Level::Log,
            "warning" | "warn" => Level::Warning,
            "error" => Level::Error,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Log => "log",
            Level::Warning => "warning",
            Level::Error => "error",
            Level::Navigation => "navigation",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleEntry {
    pub level: Level,
    pub text: String,
    /// `url:line` of where it came from.
    pub source: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub time: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkEntry {
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// CDP's resource type: `Document`, `XHR`, `Fetch`, `Script`, ...
    pub kind: String,
    pub status: Option<i64>,
    pub mime: Option<String>,
    /// Bytes over the wire, once finished.
    pub size: Option<f64>,
    pub failed: Option<String>,
    pub finished: bool,
    /// Milliseconds since the Unix epoch.
    pub time: f64,
}

pub struct PageLog {
    cdp: Weak<CdpClient>,
    console: RefCell<VecDeque<ConsoleEntry>>,
    network: RefCell<VecDeque<NetworkEntry>>,
    subscription: RefCell<Option<Subscription>>,
    /// Console entries ever logged: marks for [`PageLog::console_since`].
    console_total: Cell<u64>,
    /// What unfinished requests received so far, for their size.
    received: RefCell<HashMap<String, Received>>,
    /// Methods from `Network.requestWillBeSentExtraInfo` of requests the log
    /// has not seen start (a new page's first document).
    extra_methods: RefCell<HashMap<String, String>>,
    /// The method of the main frame's navigation the page asked for, when
    /// its reason says it (a form's GET or POST).
    asked_method: RefCell<Option<String>>,
    /// The main frame's id, once a navigation of it was heard.
    main_frame: RefCell<Option<String>>,
}

/// Bytes a request received, from `Network.dataReceived` and its answer.
#[derive(Default)]
struct Received {
    /// The answer's headers, over the wire.
    headers: f64,
    encoded: f64,
    decoded: f64,
    content_length: Option<f64>,
}

/// The method of a request from what its answer says: HTTP/2's `:method`
/// header, or HTTP/1's request line.
fn method_of_response(response: &Value) -> Option<String> {
    if let Some(method) = response["requestHeaders"][":method"].as_str() {
        return Some(method.to_owned());
    }
    let line = response["requestHeadersText"].as_str()?.lines().next()?;
    let method = line.split_whitespace().next()?;
    (!method.is_empty() && method.chars().all(|c| c.is_ascii_uppercase())).then(|| method.into())
}

/// A header's value, matched without case.
fn header(headers: &Value, name: &str) -> Option<String> {
    headers.as_object()?.iter().find_map(|(key, value)| {
        key.eq_ignore_ascii_case(name)
            .then(|| value.as_str().map(str::to_owned))
            .flatten()
    })
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

/// A `Runtime.RemoteObject` as console text.
fn remote_text(object: &Value) -> String {
    if let Some(Value::String(s)) = object.get("value") {
        return s.clone();
    }
    if let Some(value) = object.get("value") {
        return value.to_string();
    }
    for key in ["unserializableValue", "description"] {
        if let Some(s) = object.get(key).and_then(Value::as_str) {
            return s.to_owned();
        }
    }
    object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("undefined")
        .to_owned()
}

fn source(url: Option<&str>, line: Option<i64>) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    Some(match line {
        Some(line) => format!("{url}:{}", line + 1),
        None => url.to_owned(),
    })
}

impl PageLog {
    /// Enables the domains the log needs on `cdp` and starts recording.
    pub fn start(cdp: &Rc<CdpClient>) -> Rc<PageLog> {
        let log = Rc::new(PageLog {
            cdp: Rc::downgrade(cdp),
            console: RefCell::default(),
            network: RefCell::default(),
            subscription: RefCell::default(),
            console_total: Cell::new(0),
            received: RefCell::default(),
            extra_methods: RefCell::default(),
            asked_method: RefCell::default(),
            main_frame: RefCell::default(),
        });
        let weak = Rc::downgrade(&log);
        *log.subscription.borrow_mut() = Some(cdp.subscribe(move |event| {
            if let Some(log) = weak.upgrade() {
                log.on_event(event);
            }
        }));
        // The messages go out now; their answers do not matter.
        for method in [
            "Runtime.enable",
            "Log.enable",
            "Network.enable",
            "Page.enable",
        ] {
            drop(cdp.call(method, json!({}), None));
        }
        log
    }

    fn on_event(&self, event: &CdpEvent) {
        if event.session_id.is_some() {
            return;
        }
        let p = &event.params;
        let str_of = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        match event.method.as_str() {
            "Runtime.consoleAPICalled" => {
                let level = match p["type"].as_str().unwrap_or("log") {
                    "error" | "assert" => Level::Error,
                    "warning" => Level::Warning,
                    "debug" => Level::Debug,
                    _ => Level::Log,
                };
                let text = p["args"]
                    .as_array()
                    .map(|args| args.iter().map(remote_text).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                let frame = &p["stackTrace"]["callFrames"][0];
                self.push_console(ConsoleEntry {
                    level,
                    text,
                    source: source(frame["url"].as_str(), frame["lineNumber"].as_i64()),
                    time: p["timestamp"].as_f64().unwrap_or_else(now_ms),
                });
            }
            "Runtime.exceptionThrown" => {
                let details = &p["exceptionDetails"];
                let text = details["exception"]["description"]
                    .as_str()
                    .or(details["text"].as_str())
                    .unwrap_or("Uncaught exception")
                    .to_owned();
                self.push_console(ConsoleEntry {
                    level: Level::Error,
                    text,
                    source: source(details["url"].as_str(), details["lineNumber"].as_i64()),
                    time: p["timestamp"].as_f64().unwrap_or_else(now_ms),
                });
            }
            "Log.entryAdded" => {
                let entry = &p["entry"];
                self.push_console(ConsoleEntry {
                    level: Level::parse(entry["level"].as_str().unwrap_or("info"))
                        .unwrap_or(Level::Log),
                    text: str_of(entry, "text").unwrap_or_default(),
                    source: source(entry["url"].as_str(), entry["lineNumber"].as_i64()),
                    time: entry["timestamp"].as_f64().unwrap_or_else(now_ms),
                });
            }
            "Page.frameNavigated" => {
                let frame = &p["frame"];
                if frame.get("parentId").is_none() {
                    self.settle_before(frame["loaderId"].as_str().unwrap_or(""));
                    *self.main_frame.borrow_mut() = str_of(frame, "id");
                }
                if frame.get("parentId").is_none()
                    && p["type"].as_str() != Some("BackForwardCacheRestore")
                {
                    self.push_console(ConsoleEntry {
                        level: Level::Navigation,
                        text: format!("navigated to {}", frame["url"].as_str().unwrap_or("")),
                        source: None,
                        time: now_ms(),
                    });
                }
            }
            "Network.requestWillBeSent" => {
                let id = str_of(p, "requestId").unwrap_or_default();
                if let Some(redirect) = p.get("redirectResponse") {
                    self.received.borrow_mut().remove(&id);
                    self.update(&id, |e| {
                        e.status = redirect["status"].as_i64();
                        e.mime = str_of(redirect, "mimeType");
                        e.size = redirect["encodedDataLength"].as_f64().filter(|s| *s > 0.0);
                        e.finished = true;
                    });
                }
                let request = &p["request"];
                if p["type"] == "Document" {
                    // Heard: what the page asked for is no longer needed.
                    self.asked_method.borrow_mut().take();
                }
                self.push_network(NetworkEntry {
                    request_id: id,
                    method: str_of(request, "method").unwrap_or_else(|| "GET".into()),
                    url: str_of(request, "url").unwrap_or_default(),
                    kind: str_of(p, "type").unwrap_or_else(|| "Other".into()),
                    status: None,
                    mime: None,
                    size: None,
                    failed: None,
                    finished: false,
                    time: p["wallTime"].as_f64().map_or_else(now_ms, |s| s * 1000.0),
                });
            }
            "Network.responseReceived" => {
                let response = &p["response"];
                let id = p["requestId"].as_str().unwrap_or("");
                if p["type"].as_str() == Some("Document") && !self.knows(id) {
                    // A new page's first document: Chromium starts its
                    // request with the page, before the log listens, and
                    // only its answer reaches the log. Its method is `?`
                    // unless something says it: a page opened by URL is a
                    // GET, a form posting into a new tab a POST.
                    let method = method_of_response(response)
                        .or_else(|| self.extra_methods.borrow_mut().remove(id))
                        .or_else(|| self.asked_method.borrow_mut().take())
                        .unwrap_or_else(|| "?".into());
                    self.push_network(NetworkEntry {
                        request_id: id.to_owned(),
                        method,
                        url: str_of(response, "url").unwrap_or_default(),
                        kind: "Document".into(),
                        status: None,
                        mime: None,
                        size: None,
                        failed: None,
                        finished: false,
                        time: now_ms(),
                    });
                }
                self.update(id, |e| {
                    e.status = response["status"].as_i64();
                    e.mime = str_of(response, "mimeType");
                });
                let mut received = self.received.borrow_mut();
                if received.len() >= NETWORK_ENTRIES && !received.contains_key(id) {
                    received.clear();
                }
                let bytes = received.entry(id.to_owned()).or_default();
                bytes.headers = response["encodedDataLength"].as_f64().unwrap_or(0.0);
                bytes.content_length = header(&response["headers"], "content-length")
                    .and_then(|length| length.trim().parse().ok());
            }
            "Network.requestWillBeSentExtraInfo" => {
                let id = p["requestId"].as_str().unwrap_or("");
                let Some(method) = p["headers"][":method"].as_str() else {
                    return;
                };
                if self.knows(id) {
                    // A first document logged from its answer before this.
                    self.update(id, |e| {
                        if e.method == "?" {
                            e.method = method.to_owned();
                        }
                    });
                } else {
                    let mut methods = self.extra_methods.borrow_mut();
                    if methods.len() >= NETWORK_ENTRIES {
                        methods.clear();
                    }
                    methods.insert(id.to_owned(), method.to_owned());
                }
            }
            "Page.frameRequestedNavigation" => {
                let main = self.main_frame.borrow();
                let elsewhere = main.as_deref().is_some_and(|m| p["frameId"] != m)
                    || p["disposition"].as_str().is_some_and(|d| d != "currentTab");
                if elsewhere {
                    return;
                }
                // Only a form says its method; other reasons may re-send a POST.
                let method = match p["reason"].as_str() {
                    Some("formSubmissionGet") => Some("GET"),
                    Some("formSubmissionPost") => Some("POST"),
                    _ => None,
                };
                *self.asked_method.borrow_mut() = method.map(str::to_owned);
            }
            "Network.dataReceived" => {
                let id = p["requestId"].as_str().unwrap_or("");
                let mut received = self.received.borrow_mut();
                if received.len() >= NETWORK_ENTRIES && !received.contains_key(id) {
                    received.clear();
                }
                let bytes = received.entry(id.to_owned()).or_default();
                bytes.encoded += p["encodedDataLength"].as_f64().unwrap_or(0.0);
                bytes.decoded += p["dataLength"].as_f64().unwrap_or(0.0);
            }
            "Network.loadingFinished" => {
                let id = p["requestId"].as_str().unwrap_or("");
                let received = self.received.borrow_mut().remove(id).unwrap_or_default();
                // CEF's documents and cached answers may say 0 here though
                // their bytes came: then what arrived is the size.
                let size = [
                    p["encodedDataLength"].as_f64().unwrap_or(0.0),
                    received.headers + received.encoded,
                    received.decoded,
                    received.content_length.unwrap_or(0.0),
                ]
                .into_iter()
                .find(|&size| size > 0.0);
                self.update(id, |e| {
                    e.finished = true;
                    e.size = size.or_else(|| {
                        // Nothing to say otherwise: no body, or unknown.
                        (matches!(e.status, Some(204 | 205 | 304)) || e.method == "HEAD")
                            .then_some(0.0)
                    });
                });
            }
            "Network.loadingFailed" => {
                let id = p["requestId"].as_str().unwrap_or("");
                self.received.borrow_mut().remove(id);
                self.update(id, |e| {
                    e.finished = true;
                    e.failed = Some(if p["canceled"].as_bool() == Some(true) {
                        "canceled".into()
                    } else {
                        str_of(p, "errorText").unwrap_or_else(|| "failed".into())
                    });
                });
            }
            _ => {}
        }
    }

    fn push_network(&self, entry: NetworkEntry) {
        let mut network = self.network.borrow_mut();
        if network.len() >= NETWORK_ENTRIES {
            network.pop_front();
        }
        network.push_back(entry);
    }

    /// Whether a request of `request_id` is in the log.
    fn knows(&self, request_id: &str) -> bool {
        (self.network.borrow().iter()).any(|e| e.request_id == request_id)
    }

    fn push_console(&self, mut entry: ConsoleEntry) {
        entry.text = cut(entry.text, MAX_MESSAGE);
        let mut console = self.console.borrow_mut();
        if console.len() >= CONSOLE_ENTRIES {
            console.pop_front();
        }
        console.push_back(entry);
        self.console_total.set(self.console_total.get() + 1);
    }

    /// A mark of the console as it is now, for [`PageLog::console_since`].
    pub fn console_mark(&self) -> u64 {
        self.console_total.get()
    }

    /// The console entries logged after `mark` (those still kept), oldest
    /// first.
    pub fn console_since(&self, mark: u64) -> Vec<ConsoleEntry> {
        let new = self.console_total.get().saturating_sub(mark) as usize;
        let console = self.console.borrow();
        let skip = console.len().saturating_sub(new);
        console.iter().skip(skip).cloned().collect()
    }

    /// The main frame moved to another document, whose request is
    /// `loader`: what the old one still waited for will not end in its log,
    /// so it is over (canceled, if it had no answer yet).
    fn settle_before(&self, loader: &str) {
        for entry in self.network.borrow_mut().iter_mut() {
            if !entry.finished && entry.request_id != loader {
                entry.finished = true;
                if entry.status.is_none() && entry.failed.is_none() {
                    entry.failed = Some("canceled".into());
                }
            }
        }
    }

    /// Changes the newest entry of `request_id`.
    fn update(&self, request_id: &str, change: impl FnOnce(&mut NetworkEntry)) {
        if let Some(entry) = self
            .network
            .borrow_mut()
            .iter_mut()
            .rev()
            .find(|e| e.request_id == request_id)
        {
            change(entry);
        }
    }

    /// The newest `limit` console entries at `level` or above (navigation
    /// marks always), oldest first.
    pub fn console(&self, limit: usize, level: Option<Level>) -> Vec<ConsoleEntry> {
        let console = self.console.borrow();
        let mut out: Vec<ConsoleEntry> = console
            .iter()
            .rev()
            .filter(|e| level.is_none_or(|l| e.level >= l))
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// The newest `limit` requests whose URL contains `filter` or whose kind
    /// is `filter` (both without case), oldest first.
    pub fn network(&self, limit: usize, filter: Option<&str>) -> Vec<NetworkEntry> {
        let filter = filter.map(str::to_lowercase);
        let network = self.network.borrow();
        let mut out: Vec<NetworkEntry> = network
            .iter()
            .rev()
            .filter(|e| {
                filter
                    .as_deref()
                    .is_none_or(|f| e.url.to_lowercase().contains(f) || e.kind.to_lowercase() == f)
            })
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// Requests started and not finished (long-lived event streams left out).
    pub fn in_flight(&self) -> usize {
        self.network
            .borrow()
            .iter()
            .filter(|e| !e.finished && e.kind != "EventSource")
            // An answered document streams into its frame. An iframe in
            // another process finishes it in that frame's session, which
            // the log does not hear, and a new page's renderer may not be
            // listening yet when its first document ends.
            .filter(|e| !(e.kind == "Document" && e.status.is_some()))
            .count()
    }

    /// The response body of a logged request: `(body, base64)`.
    pub async fn response_body(&self, request_id: &str) -> Result<(String, bool), CdpError> {
        let cdp = self.cdp.upgrade().ok_or(CdpError::Gone)?;
        let result = cdp
            .call(
                "Network.getResponseBody",
                json!({"requestId": request_id}),
                None,
            )
            .await?;
        Ok((
            result["body"].as_str().unwrap_or("").to_owned(),
            result["base64Encoded"].as_bool().unwrap_or(false),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tools::fake::Script;

    #[test]
    fn console_keeps_the_newest_and_marks_navigations() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        let enabled: Vec<String> = script.methods();
        assert!(enabled.contains(&"Runtime.enable".to_owned()));
        assert!(enabled.contains(&"Network.enable".to_owned()));
        for i in 0..CONSOLE_ENTRIES + 5 {
            script.emit(
                "Runtime.consoleAPICalled",
                json!({"type": "log", "timestamp": 1.0, "args": [
                    {"type": "string", "value": format!("line {i}")},
                    {"type": "number", "value": 3},
                    {"type": "object", "description": "Object"},
                ]}),
            );
        }
        script.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "F", "url": "https://example.com/"}}),
        );
        script.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "C", "parentId": "F", "url": "https://ads.example/"}}),
        );
        script.emit(
            "Runtime.exceptionThrown",
            json!({"timestamp": 2.0, "exceptionDetails": {"text": "Uncaught",
                "exception": {"description": "TypeError: x is undefined"},
                "url": "https://example.com/app.js", "lineNumber": 9}}),
        );
        script.emit(
            "Log.entryAdded",
            json!({"entry": {"level": "warning", "text": "Mixed content", "timestamp": 3.0}}),
        );
        // A child session's message is not the page's.
        script.emit_in(
            "S1",
            "Runtime.consoleAPICalled",
            json!({"type": "error", "args": []}),
        );

        let all = log.console(1000, None);
        assert_eq!(all.len(), CONSOLE_ENTRIES);
        assert_eq!(all[0].text, "line 8 3 Object");
        let errors = log.console(10, Level::parse("error"));
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].level, Level::Navigation);
        assert_eq!(errors[0].text, "navigated to https://example.com/");
        assert_eq!(errors[1].text, "TypeError: x is undefined");
        assert_eq!(
            errors[1].source.as_deref(),
            Some("https://example.com/app.js:10")
        );
        let warnings = log.console(2, Level::parse("warning"));
        assert_eq!(warnings.last().unwrap().text, "Mixed content");
    }

    #[test]
    fn network_follows_requests_and_redirects() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        let sent = |id: &str, url: &str, kind: &str| {
            script.emit(
                "Network.requestWillBeSent",
                json!({"requestId": id, "type": kind, "wallTime": 10.0,
                    "request": {"url": url, "method": "GET"}}),
            );
        };
        sent("1", "https://example.com/", "Document");
        script.emit(
            "Network.requestWillBeSent",
            json!({"requestId": "1", "type": "Document", "wallTime": 10.5,
                "redirectResponse": {"status": 301, "mimeType": "text/html"},
                "request": {"url": "https://www.example.com/", "method": "GET"}}),
        );
        script.emit(
            "Network.responseReceived",
            json!({"requestId": "1", "response": {"status": 200, "mimeType": "text/html"}}),
        );
        script.emit(
            "Network.loadingFinished",
            json!({"requestId": "1", "encodedDataLength": 1234.0}),
        );
        sent("2", "https://api.example.com/items", "Fetch");
        sent("3", "https://example.com/live", "EventSource");
        assert_eq!(log.in_flight(), 1);
        script.emit(
            "Network.loadingFailed",
            json!({"requestId": "2", "errorText": "net::ERR_FAILED"}),
        );
        assert_eq!(log.in_flight(), 0);

        let all = log.network(10, None);
        assert_eq!(all.len(), 4);
        assert_eq!((all[0].status, all[0].finished), (Some(301), true));
        assert_eq!(all[1].url, "https://www.example.com/");
        assert_eq!((all[1].status, all[1].size), (Some(200), Some(1234.0)));
        assert_eq!(all[1].time, 10_500.0);
        assert_eq!(all[2].failed.as_deref(), Some("net::ERR_FAILED"));
        assert_eq!(log.network(10, Some("fetch")).len(), 1);
        assert_eq!(log.network(10, Some("API.EXAMPLE")).len(), 1);
        assert_eq!(log.network(1, None)[0].request_id, "3");

        for i in 0..NETWORK_ENTRIES + 3 {
            sent(&format!("x{i}"), "https://example.com/x", "Image");
        }
        assert_eq!(log.network(1000, None).len(), NETWORK_ENTRIES);
    }

    #[test]
    fn a_new_pages_first_document_is_logged_from_its_answer() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        // Its request went out before the log listened.
        script.emit(
            "Network.responseReceived",
            json!({"requestId": "L1", "type": "Document", "response":
                {"url": "http://localhost:8765/", "status": 200, "mimeType": "text/html"}}),
        );
        script.emit(
            "Network.requestWillBeSent",
            json!({"requestId": "7.1", "type": "Fetch", "request":
                {"url": "http://localhost:8765/api", "method": "POST"}}),
        );
        script.emit(
            "Network.loadingFinished",
            json!({"requestId": "L1", "encodedDataLength": 1794.0}),
        );
        // Answers of anything else unheard stay out; a known document's
        // answer adds nothing.
        script.emit(
            "Network.responseReceived",
            json!({"requestId": "9.9", "type": "Script", "response": {"url": "x", "status": 200}}),
        );
        script.emit(
            "Network.responseReceived",
            json!({"requestId": "L1", "type": "Document", "response":
                {"url": "http://localhost:8765/", "status": 200, "mimeType": "text/html"}}),
        );
        let all = log.network(10, None);
        assert_eq!(all.len(), 2);
        let first = &all[0];
        assert_eq!(
            (
                first.method.as_str(),
                first.url.as_str(),
                first.kind.as_str()
            ),
            // Nothing says how it was asked for.
            ("?", "http://localhost:8765/", "Document")
        );
        assert_eq!(first.status, Some(200));
        assert_eq!(first.mime.as_deref(), Some("text/html"));
        assert_eq!((first.finished, first.size), (true, Some(1794.0)));
        assert_eq!(all[1].method, "POST");
    }

    #[test]
    fn answered_documents_and_an_old_documents_requests_do_not_keep_the_page_busy() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        let sent = |id: &str, kind: &str| {
            script.emit(
                "Network.requestWillBeSent",
                json!({"requestId": id, "type": kind, "loaderId": "L1",
                    "request": {"url": "http://localhost/", "method": "GET"}}),
            );
        };
        let answered = |id: &str| {
            script.emit(
                "Network.responseReceived",
                json!({"requestId": id, "response": {"status": 200}}),
            );
        };
        // The page's document, and an iframe's from another origin: both
        // answered, neither's end is heard on the page's session.
        sent("L1", "Document");
        answered("L1");
        sent("X", "Document");
        answered("X");
        assert_eq!(log.in_flight(), 0);
        sent("F", "Fetch");
        sent("G", "Fetch");
        answered("G");
        assert_eq!(log.in_flight(), 2);
        // A frame navigating is not the page moving on.
        script.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "C", "parentId": "M", "loaderId": "L9", "url": "x"}}),
        );
        assert_eq!(log.in_flight(), 2);
        // The main frame moves to the document of request L2.
        sent("L2", "Document");
        script.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "M", "loaderId": "L2", "url": "http://localhost/b"}}),
        );
        assert_eq!(log.in_flight(), 1);
        answered("L2");
        assert_eq!(log.in_flight(), 0);
        let all = log.network(10, None);
        let of = |id: &str| all.iter().find(|e| e.request_id == id).unwrap();
        assert_eq!(of("F").failed.as_deref(), Some("canceled"));
        assert_eq!(
            (of("G").failed.as_deref(), of("G").status),
            (None, Some(200))
        );
        assert!(!of("L2").finished);
    }

    #[test]
    fn a_first_documents_method_comes_from_what_says_it() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        let answered = |id: &str, response: Value| {
            let mut response = response;
            response["url"] = "http://localhost/".into();
            response["status"] = 200.into();
            script.emit(
                "Network.responseReceived",
                json!({"requestId": id, "type": "Document", "response": response}),
            );
        };
        // HTTP/2 headers, an HTTP/1 request line.
        answered("A", json!({"requestHeaders": {":method": "POST"}}));
        answered(
            "B",
            json!({"requestHeadersText": "PUT / HTTP/1.1\r\nHost: x\r\n"}),
        );
        // Extra info before the answer, and after it.
        script.emit(
            "Network.requestWillBeSentExtraInfo",
            json!({"requestId": "C", "headers": {":method": "POST"}}),
        );
        answered("C", json!({}));
        answered("D", json!({}));
        script.emit(
            "Network.requestWillBeSentExtraInfo",
            json!({"requestId": "D", "headers": {":method": "GET"}}),
        );
        // A form the page itself submitted.
        script.emit(
            "Page.frameRequestedNavigation",
            json!({"frameId": "F", "reason": "formSubmissionPost", "url": "http://localhost/"}),
        );
        answered("E", json!({}));
        answered("F", json!({}));
        let methods: Vec<String> = log
            .network(10, None)
            .into_iter()
            .map(|e| e.method)
            .collect();
        assert_eq!(methods, ["POST", "PUT", "POST", "GET", "POST", "?"]);
    }

    #[test]
    fn sizes_come_from_what_arrived_when_the_end_says_0() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        for id in ["A", "B", "C", "D", "E"] {
            script.emit(
                "Network.requestWillBeSent",
                json!({"requestId": id, "type": "Document",
                    "request": {"url": "http://localhost/", "method": "GET"}}),
            );
        }
        let answered = |id: &str, status: i64, response: Value| {
            let mut response = response;
            response["status"] = status.into();
            script.emit(
                "Network.responseReceived",
                json!({"requestId": id, "response": response}),
            );
        };
        let data = |id: &str, encoded: f64, decoded: f64| {
            script.emit(
                "Network.dataReceived",
                json!({"requestId": id, "encodedDataLength": encoded, "dataLength": decoded}),
            );
        };
        let finished = |id: &str| {
            script.emit(
                "Network.loadingFinished",
                json!({"requestId": id, "encodedDataLength": 0}),
            );
        };
        answered("A", 200, json!({"encodedDataLength": 200}));
        data("A", 1000.0, 3000.0);
        data("A", 500.0, 1000.0);
        answered("B", 200, json!({}));
        data("B", 0.0, 2048.0);
        answered("C", 200, json!({"headers": {"Content-Length": "77"}}));
        answered("D", 204, json!({}));
        answered("E", 200, json!({}));
        for id in ["A", "B", "C", "D", "E"] {
            finished(id);
        }
        let sizes: Vec<Option<f64>> = log.network(10, None).into_iter().map(|e| e.size).collect();
        assert_eq!(
            sizes,
            [Some(1700.0), Some(2048.0), Some(77.0), Some(0.0), None]
        );
    }

    #[test]
    fn console_marks_give_what_came_after() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        let say = |text: &str| {
            script.emit(
                "Log.entryAdded",
                json!({"entry": {"level": "error", "text": text}}),
            );
        };
        say("old");
        let mark = log.console_mark();
        assert!(log.console_since(mark).is_empty());
        say("new 1");
        say("new 2");
        let since: Vec<String> = log
            .console_since(mark)
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(since, ["new 1", "new 2"]);
    }

    #[test]
    fn bodies_come_from_cdp() {
        let (cdp, script) = Script::client();
        let log = PageLog::start(&cdp);
        script.on("Network.getResponseBody", |p, _| {
            assert_eq!(p["requestId"], "7");
            Ok(json!({"body": "{\"ok\":true}", "base64Encoded": false}))
        });
        let body = futures_util::FutureExt::now_or_never(log.response_body("7"));
        assert_eq!(body, Some(Ok(("{\"ok\":true}".to_owned(), false))));
    }
}
