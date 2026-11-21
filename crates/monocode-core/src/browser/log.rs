//! A page's console and network log (context.md K36), kept from the page's
//! first load: ring buffers fed by CDP events.
//!
//! Console: `Runtime.consoleAPICalled`, `Runtime.exceptionThrown`,
//! `Log.entryAdded`, and a mark for each main-frame navigation. Network:
//! request metadata only; bodies are fetched on demand with
//! `Network.getResponseBody`. Events of child sessions (iframes) are left out.

use std::{
    cell::RefCell,
    collections::VecDeque,
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
                    self.update(&id, |e| {
                        e.status = redirect["status"].as_i64();
                        e.mime = str_of(redirect, "mimeType");
                        e.finished = true;
                    });
                }
                let request = &p["request"];
                let mut network = self.network.borrow_mut();
                if network.len() >= NETWORK_ENTRIES {
                    network.pop_front();
                }
                network.push_back(NetworkEntry {
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
                self.update(p["requestId"].as_str().unwrap_or(""), |e| {
                    e.status = response["status"].as_i64();
                    e.mime = str_of(response, "mimeType");
                });
            }
            "Network.loadingFinished" => {
                self.update(p["requestId"].as_str().unwrap_or(""), |e| {
                    e.finished = true;
                    e.size = p["encodedDataLength"].as_f64();
                });
            }
            "Network.loadingFailed" => {
                self.update(p["requestId"].as_str().unwrap_or(""), |e| {
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

    fn push_console(&self, mut entry: ConsoleEntry) {
        entry.text = cut(entry.text, MAX_MESSAGE);
        let mut console = self.console.borrow_mut();
        if console.len() >= CONSOLE_ENTRIES {
            console.pop_front();
        }
        console.push_back(entry);
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
