//! Navigation and waiting, over CDP `Page.*` (the `Page` trait has no
//! navigation on purpose), and the URL policy (K40).

use std::pin::pin;

use futures_util::future::{Either, select};
use serde_json::{Value, json};

use super::{Ctx, MAX_WAIT_MS, Reply, invalid, quote};
use crate::browser::{CdpError, ToolOutput};

/// Polling step of every wait.
const STEP_MS: u64 = 100;

/// Schemes an agent may open. Everything else is refused, `file:`,
/// `chrome:` and `devtools:` in particular (K40), and `javascript:`, which
/// would run code without the approval `browser_eval` asks for (K25).
const ALLOWED: &[&str] = &["http", "https", "about", "data", "blob"];

/// The URL to open for what the agent typed, or `browser_url_refused`.
pub fn normalize(raw: &str) -> Result<String, ToolOutput> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid("url is empty"));
    }
    let has_scheme = raw.contains("://")
        || raw.split_once(':').is_some_and(|(scheme, rest)| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                && !scheme.contains('.')
                && !rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                && scheme != "localhost"
        });
    let url = if has_scheme {
        raw.to_owned()
    } else {
        let host = raw.split(['/', ':', '?', '#']).next().unwrap_or("");
        let local = host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || host.starts_with("127.")
            || host == "0.0.0.0"
            || raw.starts_with('[');
        format!("{}://{raw}", if local { "http" } else { "https" })
    };
    let parsed = url::Url::parse(&url).map_err(|e| invalid(format!("{raw} is not a URL: {e}")))?;
    let scheme = parsed.scheme();
    if !ALLOWED.contains(&scheme) || (scheme == "about" && parsed.path() != "blank") {
        return Err(ToolOutput::error(
            "browser_url_refused",
            format!("{scheme}: URLs are not opened by agents; use http(s)"),
        ));
    }
    Ok(parsed.into())
}

/// `browser_wait`'s time.
pub fn wait_ms(args: &Value) -> u64 {
    args.get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .clamp(100, MAX_WAIT_MS)
}

/// The main frame's loader id: which document the page shows (K33).
pub async fn loader(ctx: &Ctx) -> Result<Option<String>, ToolOutput> {
    let tree = ctx.call("Page.getFrameTree", json!({})).await?;
    Ok(tree["frameTree"]["frame"]["loaderId"]
        .as_str()
        .map(str::to_owned))
}

pub struct Location {
    pub url: String,
    pub title: String,
    pub ready: String,
}

/// How long `browser_goto` waits for the server's first answer.
const NAVIGATE_MS: u64 = 20_000;

/// The error code on Chromium's error page (e.g. `DNS_PROBE_FINISHED_NXDOMAIN`).
const ERROR_TEXT: &str = "(document.querySelector('.error-code') || {}).textContent || ''";

const LOCATION: &str = "[location.href, document.title, document.readyState]";

/// Where the page is; `None` while it cannot answer (mid-navigation).
pub async fn location(ctx: &Ctx) -> Result<Option<Location>, ToolOutput> {
    let answer = ctx
        .act_raw(
            "Runtime.evaluate",
            json!({"expression": LOCATION, "returnByValue": true}),
        )
        .await?;
    let value = match answer {
        Ok(value) => value,
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(_) => return Ok(None),
    };
    let parts = &value["result"]["value"];
    let part = |i: usize| parts[i].as_str().unwrap_or("").to_owned();
    if !parts.is_array() {
        return Ok(None);
    }
    Ok(Some(Location {
        url: part(0),
        title: part(1),
        ready: part(2),
    }))
}

/// Evaluates `expression` by value: `Ok(Err(text))` when it threw or the
/// page could not run it.
pub async fn probe(ctx: &Ctx, expression: &str) -> Result<Result<Value, String>, ToolOutput> {
    let answer = ctx
        .act_raw(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
        )
        .await?;
    Ok(match answer {
        Ok(value) => match value.get("exceptionDetails") {
            Some(details) => Err(details["exception"]["description"]
                .as_str()
                .or(details["text"].as_str())
                .unwrap_or("exception")
                .to_owned()),
            None => Ok(value["result"]["value"].clone()),
        },
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(error) => Err(error.to_string()),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    DomContentLoaded,
    /// The `load` event: everything the document needs.
    Loaded,
    NetworkIdle,
}

/// Waits for the page to reach `load`: first for a new document (the one
/// `expect`ed, or any other than `before`), then for its ready state. False
/// when `budget_ms` ran out first.
pub async fn settle(
    ctx: &Ctx,
    before: Option<String>,
    expect: Option<String>,
    load: Load,
    budget_ms: u64,
) -> Result<bool, ToolOutput> {
    let mut waited = 0;
    if expect.is_some() || before.is_some() {
        // A history step or reload may stay in the same document.
        let switch_ms = if expect.is_some() {
            budget_ms
        } else {
            budget_ms.min(1_500)
        };
        loop {
            let now = loader(ctx).await?;
            let switched = match &expect {
                Some(expect) => now.as_ref() == Some(expect),
                None => now != before,
            };
            if switched {
                break;
            }
            if waited >= switch_ms {
                if expect.is_some() {
                    return Ok(false);
                }
                break;
            }
            ctx.sleep(STEP_MS).await;
            waited += STEP_MS;
        }
    }
    let mut quiet = 0;
    loop {
        let ready = location(ctx).await?.map(|l| l.ready).unwrap_or_default();
        let mut done = match load {
            Load::DomContentLoaded => ready == "interactive" || ready == "complete",
            Load::Loaded | Load::NetworkIdle => ready == "complete",
        };
        if done && load == Load::NetworkIdle {
            let idle = ctx.page.log().is_none_or(|log| log.in_flight() == 0);
            quiet = if idle { quiet + 1 } else { 0 };
            done = quiet >= 5;
        }
        if done {
            return Ok(true);
        }
        if waited >= budget_ms {
            return Ok(false);
        }
        ctx.sleep(STEP_MS).await;
        waited += STEP_MS;
    }
}

/// A new tab: waits for it to leave `about:blank` and load.
pub async fn opened(ctx: &Ctx, url: &str, budget_ms: u64) -> Result<bool, ToolOutput> {
    let mut waited = 0;
    loop {
        if let Some(at) = location(ctx).await? {
            if (at.url != "about:blank" || url == "about:blank") && at.ready == "complete" {
                return Ok(true);
            }
        }
        if waited >= budget_ms {
            return Ok(false);
        }
        ctx.sleep(STEP_MS).await;
        waited += STEP_MS;
    }
}

/// "Page 3 is at URL "Title"." and whether it still loads.
pub async fn report(ctx: &Ctx, done: bool) -> Result<String, ToolOutput> {
    let (url, title) = match location(ctx).await? {
        Some(at) => (at.url, at.title),
        None => {
            let info = ctx.page.info();
            (info.url, info.title)
        }
    };
    if url.starts_with("chrome-error://") {
        // Chromium's own error page: the title names the host, the page the error.
        let error = probe(ctx, ERROR_TEXT).await?.ok();
        let error = error.as_ref().and_then(Value::as_str).unwrap_or("").trim();
        return Ok(format!(
            "Page {} could not load {}: Chromium shows its error page{}.",
            ctx.id,
            quote(&title, 200),
            if error.is_empty() {
                String::new()
            } else {
                format!(" ({error})")
            }
        ));
    }
    let mut out = format!("Page {} is at {url} {}.", ctx.id, quote(&title, 200));
    if !done {
        out.push_str(" It is still loading; browser_wait with load waits for it.");
    }
    Ok(out)
}

pub async fn goto(ctx: &Ctx, args: &Value) -> Reply {
    let Some(raw) = args.get("url").and_then(Value::as_str) else {
        return Err(invalid("url is required"));
    };
    let url = normalize(raw)?;
    // `Page.navigate` answers once the server does: a host whose name never
    // resolves keeps it waiting for as long as DNS does.
    let navigate = ctx.act("Page.navigate", json!({"url": url}));
    let result = match select(pin!(navigate), pin!(ctx.sleep(NAVIGATE_MS))).await {
        Either::Left((result, _)) => result?,
        Either::Right(_) => {
            return Ok(ToolOutput::text(format!(
                "{} {url} has not answered in {} s.",
                report(ctx, false).await?,
                NAVIGATE_MS / 1000
            )));
        }
    };
    if let Some(error) = result["errorText"].as_str().filter(|e| !e.is_empty()) {
        return Err(invalid(format!("could not open {url}: {error}")));
    }
    let expect = result["loaderId"].as_str().map(str::to_owned);
    let done = settle(ctx, None, expect, Load::Loaded, 20_000).await?;
    Ok(ToolOutput::text(report(ctx, done).await?))
}

pub async fn history(ctx: &Ctx, step: i64) -> Reply {
    let history = ctx.call("Page.getNavigationHistory", json!({})).await?;
    let at = history["currentIndex"].as_i64().unwrap_or(0) + step;
    let entries = history["entries"].as_array().cloned().unwrap_or_default();
    let Some(entry) = usize::try_from(at).ok().and_then(|i| entries.get(i)) else {
        return Err(invalid(format!(
            "page {} has no {} page in its history",
            ctx.id,
            if step < 0 { "earlier" } else { "later" }
        )));
    };
    let before = loader(ctx).await?;
    ctx.act(
        "Page.navigateToHistoryEntry",
        json!({"entryId": entry["id"]}),
    )
    .await?;
    let done = settle(ctx, before, None, Load::Loaded, 20_000).await?;
    Ok(ToolOutput::text(report(ctx, done).await?))
}

pub async fn reload(ctx: &Ctx) -> Reply {
    let before = loader(ctx).await?;
    ctx.act("Page.reload", json!({})).await?;
    let done = settle(ctx, before, None, Load::Loaded, 20_000).await?;
    Ok(ToolOutput::text(report(ctx, done).await?))
}

/// A URL pattern: a glob over the whole URL when it has `*`, else a part.
fn url_matches(pattern: &str, url: &str) -> bool {
    if !pattern.contains('*') {
        return url.contains(pattern);
    }
    let glob = pattern
        .split('*')
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(".*");
    regex::Regex::new(&format!("^{glob}$")).is_ok_and(|re| re.is_match(url))
}

pub async fn wait(ctx: &Ctx, args: &Value) -> Reply {
    let budget = wait_ms(args);
    let text = args.get("text").and_then(Value::as_str);
    let url = args.get("url").and_then(Value::as_str);
    let selector = args.get("selector").and_then(Value::as_str);
    let load = match args.get("load").and_then(Value::as_str) {
        None => None,
        Some("load") => Some(Load::Loaded),
        Some("domcontentloaded") => Some(Load::DomContentLoaded),
        Some("networkidle") => Some(Load::NetworkIdle),
        Some(other) => {
            return Err(invalid(format!(
                "load is load, domcontentloaded or networkidle, not {other}"
            )));
        }
    };
    let what = if let Some(text) = text {
        format!("text {}", quote(text, 100))
    } else if let Some(url) = url {
        format!("a URL like {url}")
    } else if let Some(selector) = selector {
        format!("selector {selector}")
    } else if let Some(load) = load {
        if settle(ctx, None, None, load, budget).await? {
            return Ok(ToolOutput::text(report(ctx, true).await?));
        }
        return Err(waited_out(ctx, budget, "the load state").await);
    } else {
        return Err(invalid("give text, url, selector or load"));
    };
    let expression = if let Some(text) = text {
        format!(
            "(document.body ? document.body.innerText : '').includes({})",
            json!(text)
        )
    } else if let Some(selector) = selector {
        format!("!!document.querySelector({})", json!(selector))
    } else {
        String::new()
    };
    let mut waited = 0;
    loop {
        let met = if let Some(pattern) = url {
            location(ctx)
                .await?
                .is_some_and(|at| url_matches(pattern, &at.url))
        } else {
            match probe(ctx, &expression).await? {
                Ok(value) => value == Value::Bool(true),
                Err(error) if error.contains("is not a valid selector") => {
                    return Err(invalid(format!("{selector:?} is not a valid CSS selector")));
                }
                Err(_) => false,
            }
        };
        if met {
            return Ok(ToolOutput::text(format!(
                "Found {what}. {}",
                report(ctx, true).await?
            )));
        }
        if waited >= budget {
            return Err(waited_out(ctx, budget, &what).await);
        }
        ctx.sleep(STEP_MS).await;
        waited += STEP_MS;
    }
}

async fn waited_out(ctx: &Ctx, budget: u64, what: &str) -> ToolOutput {
    let at = match location(ctx).await {
        Ok(Some(at)) => format!(" The page is at {}.", at.url),
        _ => String::new(),
    };
    ToolOutput::error(
        "browser_timeout",
        format!(
            "waited {:.1} s for {what} on page {}.{at}",
            budget as f64 / 1000.0,
            ctx.id
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(url: &str) -> bool {
        normalize(url)
            .is_err_and(|e| super::super::fake::text(&e).starts_with("browser_url_refused"))
    }

    #[test]
    fn urls_get_a_scheme_and_the_policy() {
        assert_eq!(normalize("example.com").unwrap(), "https://example.com/");
        assert_eq!(
            normalize("localhost:3000/a").unwrap(),
            "http://localhost:3000/a"
        );
        assert_eq!(
            normalize("127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080/"
        );
        assert_eq!(
            normalize("http://x.test/p?q=1").unwrap(),
            "http://x.test/p?q=1"
        );
        assert_eq!(normalize("about:blank").unwrap(), "about:blank");
        assert!(normalize("data:text/html,<p>hi</p>").is_ok());
        for url in [
            "file:///etc/passwd",
            "FILE:///etc/passwd",
            "chrome://settings",
            "devtools://devtools/bundled/inspector.html",
            "javascript:alert(1)",
            "view-source:file:///etc/passwd",
            "about:settings",
            "chrome-extension://abc/x.html",
        ] {
            assert!(refused(url), "{url} should be refused");
        }
        assert!(normalize("  ").is_err());
    }

    #[test]
    fn url_patterns() {
        assert!(url_matches("/done", "https://a.test/done?x"));
        assert!(url_matches(
            "https://a.test/*/done",
            "https://a.test/x/y/done"
        ));
        assert!(!url_matches(
            "https://a.test/*/done",
            "https://a.test/x/done?y"
        ));
    }
}
