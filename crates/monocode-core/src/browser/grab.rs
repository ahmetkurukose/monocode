//! Element grab (context.md K49): the user picks an element of a page and it
//! goes to an agent's composer as a Markdown block and a cropped image.
//!
//! The picker runs in an isolated world of the page's main frame
//! (`grab/picker.js`), so the page's scripts neither see nor change it; its
//! overlay sits in a closed shadow root and the top layer. React's component
//! names and source file come from the page's own world (`grab/react.js`),
//! where React keeps its fiber on the node. Everything the page gives is
//! redacted here again, whatever the script already left out.
//!
//! Main thread only, like [`CdpClient`].

use std::{rc::Rc, sync::LazyLock};

use base64::Engine;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::cdp::CdpClient;

const PICKER: &str = include_str!("grab/picker.js");
const REACT: &str = include_str!("grab/react.js");
/// The isolated world's name, as DevTools lists it.
const WORLD: &str = "monocode-grab";
const OBJECT_GROUP: &str = "monocode-grab";

/// The element's HTML is cut at this many bytes.
pub const MAX_HTML: usize = 4 * 1024;
/// A larger crop is sent smaller, as JPEG, or not at all.
pub const MAX_IMAGE: usize = 2 * 1024 * 1024;
/// Short texts (the element's own, each nearby one) are cut at this many chars.
const MAX_TEXT: usize = 200;
const MAX_NEARBY: usize = 8;
const REDACTED: &str = "[redacted]";

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub ratio: f64,
}

/// A form field: what kind, and whether it has a value (never the value).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Field {
    #[serde(rename = "type")]
    pub kind: String,
    pub filled: bool,
}

/// What the picker says about an element, then what CDP adds.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Element {
    pub url: String,
    pub title: String,
    pub tag: String,
    pub selector: String,
    /// Ancestors, outermost first.
    pub path: Vec<String>,
    pub in_shadow: bool,
    /// The `role` and `aria-label` attributes.
    pub role: String,
    pub label: String,
    pub text: String,
    pub field: Option<Field>,
    pub nearby: Vec<String>,
    pub attributes: Vec<(String, String)>,
    pub styles: Vec<(String, String)>,
    pub html: String,
    pub fixed: bool,
    /// In CSS pixels, relative to the viewport.
    pub rect: Rect,
    pub viewport: Viewport,
    /// From the accessibility tree.
    #[serde(skip)]
    pub ax_role: String,
    #[serde(skip)]
    pub ax_name: String,
    /// React components around it, outermost first.
    #[serde(skip)]
    pub components: Vec<String>,
    #[serde(skip)]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

impl Image {
    pub fn extension(&self) -> &'static str {
        if self.mime == "image/png" {
            "png"
        } else {
            "jpg"
        }
    }
}

/// A grabbed element: redacted, capped, ready for the composer.
#[derive(Clone, Debug, PartialEq)]
pub struct Grab {
    pub element: Element,
    pub image: Option<Image>,
}

impl Grab {
    pub fn markdown(&self) -> String {
        markdown(&self.element)
    }

    /// A short name for files and notices: `button "Save"`.
    pub fn label(&self) -> String {
        let e = &self.element;
        let name = [&e.ax_name, &e.label, &e.text]
            .into_iter()
            .find(|s| !s.trim().is_empty())
            .map(|s| clamp(s.trim(), 40));
        match name {
            Some(name) => format!("{} \"{name}\"", e.tag),
            None => e.tag.clone(),
        }
    }

    /// A file name stem: `browser-button-save`.
    pub fn file_stem(&self) -> String {
        let mut stem = String::from("browser-");
        let mut dash = false;
        for c in self.label().chars().flat_map(char::to_lowercase) {
            if c.is_ascii_alphanumeric() {
                stem.push(c);
                dash = false;
            } else if !dash && !stem.ends_with('-') {
                stem.push('-');
                dash = true;
            }
            if stem.len() >= 40 {
                break;
            }
        }
        stem.trim_end_matches('-').to_owned()
    }
}

// ---- Redaction (K49) ----

/// Names (of attributes, `data-*` keys, query parameters) that suggest a secret.
static SECRET_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)token|secret|passw|api[-_]?key|apikey|access[-_]?key|private[-_]?key|auth|csrf|xsrf|session|credential|signature|nonce|otp|^sig$|^key$|^code$",
    )
    .expect("secret name pattern")
});

/// Values that look like a secret whatever their name.
static SECRET_VALUES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let rules: [(&str, &str); 9] = [
        // `Authorization`-style credentials.
        (
            r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9._~+/=-]{8,}",
            "$1 [redacted]",
        ),
        // JWTs.
        (
            r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*",
            REDACTED,
        ),
        // Well-known key shapes: Stripe, OpenAI/Anthropic, GitHub, Slack, AWS, Google.
        (
            r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{8,}|\bsk-[A-Za-z0-9_-]{16,}",
            REDACTED,
        ),
        (
            r"\bgh[pousr]_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}",
            REDACTED,
        ),
        (r"\bxox[abposr]-[A-Za-z0-9-]{10,}", REDACTED),
        (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", REDACTED),
        (r"\bAIza[0-9A-Za-z_-]{30,}", REDACTED),
        // Secret query parameters keep their name.
        (
            r"(?i)([?&;](?:[a-z0-9_-]*(?:token|secret|passw|api[-_]?key|auth|csrf|session|signature|sig|code|key)[a-z0-9_-]*)=)[^&#\s'\x22<>]+",
            "${1}[redacted]",
        ),
        // Long hex strings (hashes, keys).
        (r"\b[0-9a-fA-F]{32,}\b", REDACTED),
    ];
    rules
        .into_iter()
        .map(|(pattern, with)| (Regex::new(pattern).expect("secret value pattern"), with))
        .collect()
});

/// Long base64 runs: mixed case and digits, which words and slugs lack.
static BASE64: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_-]{32,}={0,2}").expect("base64 pattern"));

/// An HTML attribute: its name and value.
static HTML_ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(\s)([^\s"'<>/=]+)(\s*=\s*)("[^"]*"|'[^']*'|[^\s"'<>]+)"#)
        .expect("attribute pattern")
});

/// Whether an attribute (or `data-*`) name suggests a secret.
pub fn secret_name(name: &str) -> bool {
    let name = name.strip_prefix("data-").unwrap_or(name);
    SECRET_NAME.is_match(name)
}

fn looks_base64_secret(run: &str) -> bool {
    let body = run.trim_end_matches('=');
    // Paths and slugs: more than a couple of separators.
    if body.matches(['/', '-', '_']).count() > 2 {
        return false;
    }
    body.chars().any(|c| c.is_ascii_uppercase())
        && body.chars().any(|c| c.is_ascii_lowercase())
        && body.chars().any(|c| c.is_ascii_digit())
}

/// `text` with anything that looks like a token or key replaced.
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for (pattern, with) in SECRET_VALUES.iter() {
        if pattern.is_match(&out) {
            out = pattern.replace_all(&out, *with).into_owned();
        }
    }
    BASE64
        .replace_all(&out, |m: &regex::Captures| {
            if looks_base64_secret(&m[0]) {
                REDACTED.to_owned()
            } else {
                m[0].to_owned()
            }
        })
        .into_owned()
}

/// An attribute's value as it may be shown: inputs' values and
/// secret-named attributes never are.
pub fn redact_attribute(name: &str, value: &str) -> String {
    let name = name.to_ascii_lowercase();
    if name == "value" || secret_name(&name) {
        REDACTED.to_owned()
    } else {
        redact(value)
    }
}

/// HTML with values and secret-named attributes redacted, token-like text
/// replaced, and cut at [`MAX_HTML`] bytes.
pub fn redact_html(html: &str) -> String {
    let attributes = HTML_ATTRIBUTE.replace_all(html, |m: &regex::Captures| {
        let name = &m[2];
        let lower = name.to_ascii_lowercase();
        if lower == "value" || secret_name(&lower) {
            format!("{}{name}{}\"{REDACTED}\"", &m[1], &m[3])
        } else {
            m[0].to_owned()
        }
    });
    let out = redact(&attributes);
    if out.len() <= MAX_HTML {
        return out;
    }
    let mut end = MAX_HTML;
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n<!-- cut at {} KB -->", &out[..end], MAX_HTML / 1024)
}

/// A page address without its query, fragment and credentials.
pub fn page_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => redact(url.split(['?', '#']).next().unwrap_or("")),
    }
}

fn clamp(text: &str, max: usize) -> String {
    let squashed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if squashed.chars().count() <= max {
        return squashed;
    }
    let cut: String = squashed.chars().take(max).collect();
    format!("{cut}…")
}

impl Element {
    /// Everything redacted and capped, as it may leave the page.
    pub fn redacted(mut self) -> Self {
        let short = |s: &str| redact(&clamp(s, MAX_TEXT));
        self.url = page_url(&self.url);
        self.title = short(&self.title);
        self.tag = clamp(&self.tag, 40);
        self.selector = redact(&clamp(&self.selector, 700));
        self.path = self.path.iter().take(12).map(|p| short(p)).collect();
        self.role = short(&self.role);
        self.label = short(&self.label);
        self.text = short(&self.text);
        self.nearby = self
            .nearby
            .iter()
            .filter(|t| !t.trim().is_empty())
            .take(MAX_NEARBY)
            .map(|t| short(t))
            .collect();
        self.attributes = self
            .attributes
            .iter()
            .take(30)
            .map(|(name, value)| {
                let value = clamp(value, 300);
                (name.clone(), redact_attribute(name, &value))
            })
            .collect();
        self.styles = self
            .styles
            .iter()
            .take(24)
            .map(|(name, value)| (name.clone(), clamp(value, 120)))
            .collect();
        self.html = redact_html(&self.html);
        self.ax_role = short(&self.ax_role);
        self.ax_name = short(&self.ax_name);
        self.components = self.components.iter().take(6).map(|c| short(c)).collect();
        self.source = self.source.as_deref().map(|s| redact(&clean_source(s)));
        self
    }
}

/// A source path as bundlers write it, made project-relative where it can.
fn clean_source(path: &str) -> String {
    let mut path = path.split(['?', '#']).next().unwrap_or("").to_owned();
    for prefix in [
        "webpack-internal:///./",
        "webpack-internal:///",
        "webpack:///./",
        "webpack:///",
        "turbopack:///[project]/",
        "turbopack:///",
        "file://",
    ] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest.to_owned();
            break;
        }
    }
    clamp(path.trim_start_matches("./"), 300)
}

// ---- Markdown ----

/// A code fence longer than any backtick run in `text`.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// Inline code that survives backticks inside; nothing for nothing.
fn code(text: &str) -> String {
    if text.trim().is_empty() {
        String::new()
    } else if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\\\""))
}

/// The element as the agent reads it.
pub fn markdown(e: &Element) -> String {
    let mut out = String::from("Element picked in the browser");
    let page = match (e.title.trim(), e.url.trim()) {
        ("", "") => String::new(),
        ("", url) => format!(" on <{url}>"),
        (title, "") => format!(" on \"{title}\""),
        (title, url) => format!(" on \"{title}\" <{url}>"),
    };
    out.push_str(&page);
    out.push_str(":\n\n");
    let mut line = |label: &str, value: String| {
        if !value.trim().is_empty() {
            out.push_str(&format!("- {label}: {value}\n"));
        }
    };
    line("Element", code(&format!("<{}>", e.tag)));
    line("Selector", code(&e.selector));
    let role = if e.ax_role.is_empty() {
        &e.role
    } else {
        &e.ax_role
    };
    let name = if e.ax_name.is_empty() {
        &e.label
    } else {
        &e.ax_name
    };
    line(
        "Accessibility",
        match (role.is_empty(), name.is_empty()) {
            (true, true) => String::new(),
            (false, true) => role.clone(),
            (true, false) => format!("name {}", quoted(name)),
            (false, false) => format!("{role} {}", quoted(name)),
        },
    );
    if !e.components.is_empty() {
        let chain: Vec<String> = e.components.iter().map(|c| format!("<{c}>")).collect();
        line("React", code(&chain.join(" › ")));
    }
    line("Source", e.source.as_deref().map(code).unwrap_or_default());
    let r = &e.rect;
    let mut size = format!(
        "{}×{} at ({}, {}) in a {}×{} viewport",
        r.width.round(),
        r.height.round(),
        r.x.round(),
        r.y.round(),
        e.viewport.width.round(),
        e.viewport.height.round()
    );
    if e.fixed {
        size.push_str(", fixed");
    }
    if e.in_shadow {
        size.push_str(", inside a shadow root");
    }
    line("Box", size);
    line("Inside", code(&e.path.join(" > ")));
    if let Some(field) = &e.field {
        line(
            "Field",
            format!(
                "{}, {}",
                field.kind,
                if field.filled { "filled in" } else { "empty" }
            ),
        );
    }
    if !e.text.is_empty() {
        line("Text", quoted(&e.text));
    }
    let nearby: Vec<String> = e.nearby.iter().map(|t| quoted(t)).collect();
    line("Nearby text", nearby.join(" · "));
    let attributes: Vec<String> = e
        .attributes
        .iter()
        .map(|(k, v)| format!("{k}={}", quoted(v)))
        .collect();
    line("Attributes", code(&attributes.join(" ")));
    let styles: Vec<String> = e.styles.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    line("Styles", code(&styles.join("; ")));
    if !e.html.trim().is_empty() {
        let fence = fence(&e.html);
        out.push_str(&format!("\n{fence}html\n{}\n{fence}\n", e.html.trim_end()));
    }
    out
}

// ---- Screenshot ----

/// The part of `rect` inside the viewport, in page coordinates (what
/// `Page.captureScreenshot`'s clip takes); `None` when nothing shows.
pub fn clip(rect: &Rect, view: &Viewport) -> Option<Rect> {
    let width = if view.width > 0. {
        view.width
    } else {
        f64::MAX
    };
    let height = if view.height > 0. {
        view.height
    } else {
        f64::MAX
    };
    let left = rect.x.max(0.);
    let top = rect.y.max(0.);
    let right = (rect.x + rect.width).min(width);
    let bottom = (rect.y + rect.height).min(height);
    if right - left < 1. || bottom - top < 1. {
        return None;
    }
    Some(Rect {
        x: left + view.scroll_x,
        y: top + view.scroll_y,
        width: right - left,
        height: bottom - top,
    })
}

/// `area` in CSS pixels as `Page.captureScreenshot`'s clip wants it under
/// the browser's zoom, which counts it unzoomed.
pub fn zoomed(area: Rect, zoom: f64) -> Rect {
    let zoom = if zoom.is_finite() && zoom > 0. {
        zoom
    } else {
        1.
    };
    Rect {
        x: area.x * zoom,
        y: area.y * zoom,
        width: area.width * zoom,
        height: area.height * zoom,
    }
}

/// The browser's zoom of the page (1 at 100%).
async fn page_zoom(cdp: &CdpClient) -> f64 {
    let metrics = cdp.call("Page.getLayoutMetrics", json!({}), None).await;
    (metrics.ok())
        .and_then(|m| m["cssVisualViewport"]["zoom"].as_f64())
        .unwrap_or(1.)
}

/// The encodings tried in turn until one fits [`MAX_IMAGE`]: format,
/// JPEG quality, scale.
const SHOTS: [(&str, u8, f64); 4] = [
    ("png", 0, 1.),
    ("jpeg", 85, 1.),
    ("jpeg", 75, 0.5),
    ("jpeg", 65, 0.25),
];

async fn screenshot(cdp: &CdpClient, area: Rect) -> Option<Image> {
    for (format, quality, scale) in SHOTS {
        let mut params = json!({
            "format": format,
            "clip": {"x": area.x, "y": area.y, "width": area.width, "height": area.height, "scale": scale},
            "fromSurface": true,
        });
        if format == "jpeg" {
            params["quality"] = quality.into();
        }
        let Ok(shot) = cdp.call("Page.captureScreenshot", params, None).await else {
            return None;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(shot["data"].as_str().unwrap_or(""))
            .ok()?;
        if bytes.len() <= MAX_IMAGE {
            let mime = if format == "png" {
                "image/png"
            } else {
                "image/jpeg"
            };
            return Some(Image { mime, bytes });
        }
    }
    None
}

// ---- Driving the page ----

async fn call(cdp: &CdpClient, method: &str, params: Value) -> Result<Value, String> {
    cdp.call(method, params, None)
        .await
        .map_err(|e| format!("the page did not answer ({e})"))
}

/// `Runtime.evaluate` in `context`: the value, or what the script threw.
async fn evaluate(
    cdp: &CdpClient,
    context: i64,
    expression: &str,
    await_promise: bool,
) -> Result<Value, String> {
    let result = call(
        cdp,
        "Runtime.evaluate",
        json!({
            "expression": expression,
            "contextId": context,
            "returnByValue": true,
            "awaitPromise": await_promise,
            "silent": true,
        }),
    )
    .await?;
    if let Some(details) = result.get("exceptionDetails") {
        let text = details["exception"]["description"]
            .as_str()
            .or(details["text"].as_str())
            .unwrap_or("the script failed");
        return Err(text.to_owned());
    }
    Ok(result["result"]["value"].clone())
}

/// A new isolated world in the page's main frame, with the picker's code.
async fn install(cdp: &CdpClient) -> Result<i64, String> {
    let tree = call(cdp, "Page.getFrameTree", json!({})).await?;
    let frame = tree["frameTree"]["frame"]["id"]
        .as_str()
        .ok_or("the page has no frame yet")?
        .to_owned();
    let world = call(
        cdp,
        "Page.createIsolatedWorld",
        json!({"frameId": frame, "worldName": WORLD}),
    )
    .await?;
    let context = world["executionContextId"]
        .as_i64()
        .ok_or("the page gave no script context")?;
    evaluate(cdp, context, PICKER, false).await?;
    Ok(context)
}

/// The node's role and name in the accessibility tree.
async fn accessibility(cdp: &CdpClient, backend: i64) -> Option<(String, String)> {
    let tree = call(
        cdp,
        "Accessibility.getPartialAXTree",
        json!({"backendNodeId": backend, "fetchRelatives": false}),
    )
    .await
    .ok()?;
    let node = tree["nodes"].as_array()?.first()?;
    let text = |v: &Value| v["value"].as_str().unwrap_or("").to_owned();
    Some((text(&node["role"]), text(&node["name"])))
}

/// React's components around the node and the nearest source file, read in
/// the page's own world.
async fn react(cdp: &CdpClient, backend: i64) -> Option<(Vec<String>, Option<String>)> {
    let main = call(
        cdp,
        "DOM.resolveNode",
        json!({"backendNodeId": backend, "objectGroup": OBJECT_GROUP}),
    )
    .await
    .ok()?;
    let object = main["object"]["objectId"].as_str()?;
    let found = call(
        cdp,
        "Runtime.callFunctionOn",
        json!({"functionDeclaration": REACT, "objectId": object, "returnByValue": true}),
    )
    .await
    .ok()?;
    let found = &found["result"]["value"];
    let components = (found["components"].as_array().into_iter().flatten())
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    Some((components, found["source"].as_str().map(str::to_owned)))
}

/// The picked element (`__monocodeGrab.picked`) described: what the
/// picker sees, its accessibility role and name, React, and a crop.
async fn describe(cdp: &CdpClient, context: i64) -> Result<Grab, String> {
    let picked = call(
        cdp,
        "Runtime.evaluate",
        json!({
            "expression": "window.__monocodeGrab && window.__monocodeGrab.picked",
            "contextId": context,
            "objectGroup": OBJECT_GROUP,
        }),
    )
    .await?;
    let object = picked["result"]["objectId"]
        .as_str()
        .ok_or("nothing was picked")?
        .to_owned();
    let seen = call(
        cdp,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": "function () { return window.__monocodeGrab.capture(this); }",
            "objectId": object,
            "returnByValue": true,
        }),
    )
    .await?;
    let mut element: Element = serde_json::from_value(seen["result"]["value"].clone())
        .map_err(|_| "the element could not be read".to_owned())?;
    if element.tag.is_empty() {
        return Err("the element could not be read".into());
    }
    let node = call(cdp, "DOM.describeNode", json!({"objectId": object})).await;
    if let Some(backend) = node.ok().and_then(|n| n["node"]["backendNodeId"].as_i64()) {
        if let Some((role, name)) = accessibility(cdp, backend).await {
            element.ax_role = role;
            element.ax_name = name;
        }
        if let Some((components, source)) = react(cdp, backend).await {
            element.components = components;
            element.source = source;
        }
    }
    // The overlay is gone; let the page draw once without it.
    let _ = evaluate(
        cdp,
        context,
        "new Promise(function (r) { setTimeout(r, 150); requestAnimationFrame(function () { requestAnimationFrame(r); }); })",
        true,
    )
    .await;
    let image = match clip(&element.rect, &element.viewport) {
        Some(area) => screenshot(cdp, zoomed(area, page_zoom(cdp).await)).await,
        None => None,
    };
    let _ = call(
        cdp,
        "Runtime.releaseObjectGroup",
        json!({"objectGroup": OBJECT_GROUP}),
    )
    .await;
    Ok(Grab {
        element: element.redacted(),
        image,
    })
}

/// The picker shown over a page, waiting for the user's click.
pub struct Picker {
    cdp: Rc<CdpClient>,
    context: i64,
}

impl Picker {
    /// Shows the picker over the page.
    pub async fn arm(cdp: Rc<CdpClient>) -> Result<Picker, String> {
        let context = install(&cdp).await?;
        evaluate(&cdp, context, "window.__monocodeGrab.arm()", false).await?;
        Ok(Picker { cdp, context })
    }

    /// Waits for the user: the grabbed element, or `None` when they left
    /// (Esc, [`Picker::cancel`], the page navigated).
    pub async fn wait(&self) -> Result<Option<Grab>, String> {
        match evaluate(
            &self.cdp,
            self.context,
            "window.__monocodeGrab.wait()",
            true,
        )
        .await
        {
            Ok(Value::String(how)) if how == "picked" => {
                describe(&self.cdp, self.context).await.map(Some)
            }
            // The world went with a navigation, or the user left.
            _ => Ok(None),
        }
    }

    /// Takes the picker down; [`Picker::wait`] then resolves with `None`.
    pub fn cancel(&self) {
        let done = self.cdp.call(
            "Runtime.evaluate",
            json!({
                "expression": "window.__monocodeGrab && window.__monocodeGrab.cancel()",
                "contextId": self.context,
            }),
            None,
        );
        drop(done);
    }
}

/// The element at `(x, y)` (CSS pixels in the viewport), as the page's
/// context menu's "Send to Agent" asks.
pub async fn grab_at(cdp: Rc<CdpClient>, x: f64, y: f64) -> Result<Grab, String> {
    let context = install(&cdp).await?;
    let found = evaluate(
        &cdp,
        context,
        &format!(
            "(function () {{ var g = window.__monocodeGrab; var el = document.elementFromPoint({x}, {y}); \
             while (el && el.shadowRoot) {{ var i = el.shadowRoot.elementFromPoint({x}, {y}); if (!i || i === el) break; el = i; }} \
             g.picked = el; return !!el; }})()"
        ),
        false,
    )
    .await?;
    if found != Value::Bool(true) {
        return Err("there is no element there".into());
    }
    describe(&cdp, context).await
}

#[cfg(test)]
#[path = "grab_tests.rs"]
mod tests;
