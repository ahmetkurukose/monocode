//! Reading a page: snapshots (K31–K33, K42), screenshots (K37), eval, the
//! console and network log (K36), and answering dialogs.

use std::{collections::HashSet, io::Cursor};

use base64::Engine as _;
use serde_json::{Value, json};

use super::{
    Ctx, Reply, cap, invalid, quote,
    snapshot::{self, AxTree, Frame, Mode, Options},
    state::Shot,
};
use crate::browser::{CdpError, Content, Page, ToolOutput, log::Level};

/// Child frames a snapshot walks at most.
const MAX_FRAMES: usize = 20;
/// The longest edge of a screenshot (K37).
const MAX_EDGE: f64 = 1568.0;
/// A full-page screenshot covers at most this many CSS pixels down.
const MAX_FULL_HEIGHT: f64 = 16_384.0;
const JPEG_QUALITY: u8 = 80;

/// Elements the accessibility tree misses: styled `div` buttons and the
/// like (after Orca's `snapshot-cursor-interactive-elements.ts`). At most
/// 50, none inside a real control.
const SWEEP: &str = r#"(() => {
  const ROLES = new Set(['button', 'link', 'textbox', 'checkbox', 'radio', 'tab', 'menuitem',
    'option', 'switch', 'slider', 'combobox', 'searchbox', 'spinbutton', 'treeitem',
    'menuitemcheckbox', 'menuitemradio', 'listbox']);
  const TAGS = new Set(['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'A', 'OPTION']);
  const INSIDE = 'a[href], button, input, select, textarea, [role=button], [role=link]';
  const found = [];
  const seen = new Set();
  const pointer = el => { try { return getComputedStyle(el).cursor === 'pointer'; } catch (e) { return false; } };
  const check = el => {
    if (found.length >= 50 || seen.has(el)) return;
    seen.add(el);
    if (TAGS.has(el.tagName)) return;
    const role = el.getAttribute('role');
    if (role && ROLES.has(role)) return;
    if (el.parentElement && el.parentElement.closest(INSIDE)) return;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    const text = (el.getAttribute('aria-label') || el.getAttribute('alt') || el.getAttribute('title') || el.textContent || '').trim();
    if (text) found.push(el);
  };
  document.querySelectorAll('[onclick], [tabindex]:not([tabindex="-1"]), [contenteditable="true"], [contenteditable=""]').forEach(check);
  let looked = 0;
  for (const el of document.querySelectorAll('div, span, li, td, img, svg, label')) {
    if (found.length >= 50 || ++looked > 5000) break;
    if (pointer(el) && !(el.parentElement && pointer(el.parentElement))) check(el);
  }
  return found;
})()"#;

const SWEEP_TEXT: &str = "function() { return this.map(el => (el.getAttribute('aria-label') || \
el.getAttribute('alt') || el.getAttribute('title') || el.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 80)); }";

/// A soft CDP answer: `None` on a protocol error, `Err` when the page went.
async fn soft(
    ctx: &Ctx,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Option<Value>, ToolOutput> {
    match ctx.cdp.call(method, params, session).await {
        Ok(value) => Ok(Some(value)),
        Err(CdpError::Gone) => Err(super::page_gone(ctx.id)),
        Err(_) => Ok(None),
    }
}

/// `(frame id, url)` of every frame under `tree` (a `Page.FrameTree`).
fn child_frames(tree: &Value, out: &mut Vec<(String, String)>) {
    for child in tree["childFrames"].as_array().into_iter().flatten() {
        let frame = &child["frame"];
        out.push((
            frame["id"].as_str().unwrap_or("").to_owned(),
            frame["url"].as_str().unwrap_or("").to_owned(),
        ));
        child_frames(child, out);
    }
}

async fn frame_owner(ctx: &Ctx, frame_id: &str) -> Result<Option<i64>, ToolOutput> {
    Ok(
        soft(ctx, None, "DOM.getFrameOwner", json!({"frameId": frame_id}))
            .await?
            .and_then(|o| o["backendNodeId"].as_i64()),
    )
}

/// The page's frames: the main one, same-process iframes (fetched by frame
/// id), out-of-process ones (in their own sessions, K42).
async fn frames(ctx: &Ctx, frame_tree: &Value) -> Result<Vec<Frame>, ToolOutput> {
    let _ = soft(ctx, None, "Accessibility.enable", json!({})).await?;
    let main = ctx.call("Accessibility.getFullAXTree", json!({})).await?;
    let main = AxTree::parse(&main);
    let known: HashSet<i64> = main.nodes.iter().filter_map(|n| n.backend).collect();
    let mut frames = vec![Frame {
        tree: main,
        url: frame_tree["frame"]["url"].as_str().unwrap_or("").to_owned(),
        ..Frame::default()
    }];
    let mut listed = Vec::new();
    child_frames(frame_tree, &mut listed);
    let children = ctx.state.children.borrow().clone();
    let mut done = HashSet::new();
    for (id, url) in listed.into_iter().take(MAX_FRAMES) {
        if let Some(child) = children.iter().find(|c| c.target_id == id) {
            done.insert(child.session.clone());
            if let Some(frame) = remote_frame(ctx, &child.session, &id, &child.url).await? {
                frames.push(frame);
            }
            continue;
        }
        let Some(tree) = soft(
            ctx,
            None,
            "Accessibility.getFullAXTree",
            json!({"frameId": id}),
        )
        .await?
        else {
            continue;
        };
        let tree = AxTree::parse(&tree);
        // Already part of the main tree.
        if tree
            .root()
            .and_then(|r| r.backend)
            .is_some_and(|b| known.contains(&b))
        {
            continue;
        }
        let owner = frame_owner(ctx, &id).await?;
        frames.push(Frame {
            tree,
            session: None,
            frame_id: Some(id),
            owner,
            url,
        });
    }
    for child in children.iter().filter(|c| !done.contains(&c.session)) {
        if frames.len() > MAX_FRAMES {
            break;
        }
        if let Some(frame) = remote_frame(ctx, &child.session, &child.target_id, &child.url).await?
        {
            frames.push(frame);
        }
    }
    Ok(frames)
}

async fn remote_frame(
    ctx: &Ctx,
    session: &str,
    frame_id: &str,
    url: &str,
) -> Result<Option<Frame>, ToolOutput> {
    let _ = soft(ctx, Some(session), "Accessibility.enable", json!({})).await?;
    let Some(tree) = soft(ctx, Some(session), "Accessibility.getFullAXTree", json!({})).await?
    else {
        return Ok(None);
    };
    Ok(Some(Frame {
        tree: AxTree::parse(&tree),
        session: Some(session.to_owned()),
        frame_id: Some(frame_id.to_owned()),
        owner: frame_owner(ctx, frame_id).await?,
        url: url.to_owned(),
    }))
}

/// The backend id of the first element `selector` matches.
async fn select_root(ctx: &Ctx, selector: &str) -> Result<i64, ToolOutput> {
    let found = ctx
        .call(
            "Runtime.evaluate",
            json!({"expression": format!("document.querySelector({})", json!(selector)), "objectGroup": "monocode"}),
        )
        .await?;
    if found.get("exceptionDetails").is_some() {
        return Err(invalid(format!("{selector:?} is not a valid CSS selector")));
    }
    let Some(object) = found["result"]["objectId"].as_str() else {
        return Err(invalid(format!("no element matches {selector:?}")));
    };
    let node = ctx
        .call("DOM.describeNode", json!({"objectId": object}))
        .await?;
    node["node"]["backendNodeId"]
        .as_i64()
        .ok_or_else(|| invalid(format!("no element matches {selector:?}")))
}

/// The DOM sweep's finds: backend id and text.
async fn sweep(ctx: &Ctx) -> Result<Vec<(i64, String)>, ToolOutput> {
    let group = json!("monocode-sweep");
    let Some(found) = soft(
        ctx,
        None,
        "Runtime.evaluate",
        json!({"expression": SWEEP, "objectGroup": group}),
    )
    .await?
    else {
        return Ok(Vec::new());
    };
    let Some(array) = found["result"]["objectId"].as_str() else {
        return Ok(Vec::new());
    };
    let texts = soft(
        ctx,
        None,
        "Runtime.callFunctionOn",
        json!({"objectId": array, "functionDeclaration": SWEEP_TEXT, "returnByValue": true}),
    )
    .await?;
    let properties = soft(
        ctx,
        None,
        "Runtime.getProperties",
        json!({"objectId": array, "ownProperties": true}),
    )
    .await?;
    let mut out = Vec::new();
    if let (Some(texts), Some(properties)) = (texts, properties) {
        let texts = texts["result"]["value"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for property in properties["result"].as_array().into_iter().flatten() {
            let Some(index) = property["name"]
                .as_str()
                .and_then(|n| n.parse::<usize>().ok())
            else {
                continue;
            };
            let Some(object) = property["value"]["objectId"].as_str() else {
                continue;
            };
            let node = soft(ctx, None, "DOM.describeNode", json!({"objectId": object})).await?;
            if let Some(backend) = node.and_then(|n| n["node"]["backendNodeId"].as_i64()) {
                let text = texts
                    .get(index)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                out.push((backend, text));
            }
        }
    }
    let _ = soft(
        ctx,
        None,
        "Runtime.releaseObjectGroup",
        json!({"objectGroup": group}),
    )
    .await?;
    Ok(out)
}

/// One reading of a page's accessibility outline.
pub struct Reading {
    pub loader: Option<String>,
    pub title: String,
    pub url: String,
    pub lines: Vec<String>,
}

/// What snapshots are compared by: only those of alike options (see
/// [`Shot::key`]).
pub fn shot_key(mode: Mode, selector: Option<&str>, depth: Option<usize>) -> String {
    format!("{mode:?}|{}|{depth:?}", selector.unwrap_or(""))
}

/// Reads the page's outline; its refs go into the page's table.
pub async fn read(
    ctx: &Ctx,
    mode: Mode,
    selector: Option<&str>,
    max_depth: Option<usize>,
) -> Result<Reading, ToolOutput> {
    let tree = ctx.call("Page.getFrameTree", json!({})).await?;
    let frame_tree = &tree["frameTree"];
    let loader = frame_tree["frame"]["loaderId"].as_str().map(str::to_owned);
    {
        let mut refs = ctx.state.refs.borrow_mut();
        if refs.loader != loader {
            refs.reset(loader.clone());
        }
    }
    let frames = frames(ctx, frame_tree).await?;
    let root = match selector {
        Some(selector) => Some(select_root(ctx, selector).await?),
        None => None,
    };
    let clickables = sweep(ctx).await?;
    let options = Options {
        mode,
        root,
        max_depth,
    };
    let lines = snapshot::render(
        &frames,
        &clickables,
        &options,
        &mut ctx.state.refs.borrow_mut(),
    )
    .ok_or_else(|| invalid("the selector's element is not in the accessibility tree"))?;
    let title = frames[0]
        .tree
        .root()
        .map(|r| r.name.clone())
        .unwrap_or_default();
    Ok(Reading {
        loader,
        title,
        url: frames[0].url.clone(),
        lines,
    })
}

pub async fn snapshot(ctx: &Ctx, args: &Value) -> Reply {
    let mode = match args.get("mode").and_then(Value::as_str) {
        None | Some("interactive") => Mode::Interactive,
        Some("full") => Mode::Full,
        Some(other) => return Err(invalid(format!("mode is interactive or full, not {other}"))),
    };
    let max_depth = match args.get("depth") {
        None | Some(Value::Null) => None,
        Some(depth) => Some(
            depth
                .as_u64()
                .ok_or_else(|| invalid("depth must be a whole number"))? as usize,
        ),
    };
    let selector = args
        .get("selector")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let delta = args.get("delta").and_then(Value::as_bool).unwrap_or(false);
    let reading = read(ctx, mode, selector, max_depth).await?;
    let shot = Shot {
        loader: reading.loader.clone(),
        key: shot_key(mode, selector, max_depth),
        lines: reading.lines.clone(),
    };
    let before = ctx.state.seen.replace(Some(shot.clone()));
    let mut out = format!(
        "Page {} {} {}{}",
        ctx.id,
        quote(&reading.title, 200),
        reading.url,
        selector.map(|s| format!(", under {s}")).unwrap_or_default()
    );
    if delta {
        match before {
            Some(before) if before.loader == shot.loader && before.key == shot.key => {
                let changes = snapshot::delta(&before.lines, &shot.lines);
                if changes.is_empty() {
                    out.push_str(": nothing changed since your last snapshot.");
                    return Ok(ToolOutput::text(out));
                }
                // A delta longer than the page says less than the page.
                if changes.len() <= shot.lines.len() {
                    out.push_str(
                        ", what changed since your last snapshot (+ added, - removed, ~ changed):\n",
                    );
                    out.push_str(&changes.join("\n"));
                    return Ok(ToolOutput::text(cap(
                        out,
                        "take a snapshot without delta, narrowed with selector or depth",
                    )));
                }
                out.push_str("\n(Most of it changed since your last snapshot; all of it follows.)");
            }
            Some(before) if before.loader == shot.loader => out.push_str(
                "\n(Your last snapshot of this page had another mode, selector or depth; all of it follows.)",
            ),
            Some(_) => out.push_str(
                "\n(It is a new document since your last snapshot; all of it follows.)",
            ),
            None => out.push_str("\n(There is no earlier snapshot to compare; all of it follows.)"),
        }
    }
    out.push('\n');
    if reading.lines.is_empty() {
        out.push_str(match mode {
            Mode::Interactive => "(nothing interactive here; try mode full)",
            Mode::Full => "(nothing here)",
        });
    } else {
        out.push_str(&reading.lines.join("\n"));
    }
    Ok(ToolOutput::text(cap(
        out,
        "narrow it with selector (a CSS selector of the part you need) or depth",
    )))
}

/// `jpeg` scaled down so its longest edge is at most [`MAX_EDGE`].
fn fit(jpeg: Vec<u8>) -> Result<(Vec<u8>, u32, u32), String> {
    let reader = image::ImageReader::new(Cursor::new(&jpeg))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    let longest = width.max(height) as f64;
    if longest <= MAX_EDGE + 1.0 {
        return Ok((jpeg, width, height));
    }
    let image = image::load_from_memory(&jpeg).map_err(|e| e.to_string())?;
    let factor = MAX_EDGE / longest;
    let (w, h) = (
        ((width as f64 * factor).round() as u32).max(1),
        ((height as f64 * factor).round() as u32).max(1),
    );
    let small = image
        .resize_exact(w, h, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&small)
        .map_err(|e| e.to_string())?;
    Ok((out, w, h))
}

pub async fn screenshot(ctx: &Ctx, args: &Value) -> Reply {
    let full = args
        .get("full_page")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Saved only inside the project (K51).
    let save = match args
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
    {
        Some(raw) => Some(super::files::writable(&ctx.scope.project, raw)?),
        None => None,
    };
    let metrics = ctx.call("Page.getLayoutMetrics", json!({})).await?;
    let view = &metrics["cssVisualViewport"];
    let number = |v: &Value, key: &str, default: f64| v[key].as_f64().unwrap_or(default);
    let ratio = match soft(
        ctx,
        None,
        "Runtime.evaluate",
        json!({"expression": "devicePixelRatio", "returnByValue": true}),
    )
    .await?
    {
        Some(r) => r["result"]["value"]
            .as_f64()
            .filter(|r| *r > 0.0)
            .unwrap_or(1.0),
        None => 1.0,
    };
    let (x, y, width, height) = if full {
        let content = &metrics["cssContentSize"];
        (
            0.0,
            0.0,
            number(content, "width", 800.0),
            number(content, "height", 600.0).min(MAX_FULL_HEIGHT),
        )
    } else {
        (
            number(view, "pageX", 0.0),
            number(view, "pageY", 0.0),
            number(view, "clientWidth", 800.0),
            number(view, "clientHeight", 600.0),
        )
    };
    let scale = (MAX_EDGE / (width.max(height).max(1.0) * ratio)).min(1.0);
    let shot = ctx
        .call(
            "Page.captureScreenshot",
            json!({
                "format": "jpeg",
                "quality": JPEG_QUALITY,
                "clip": {"x": x, "y": y, "width": width, "height": height, "scale": scale},
                "captureBeyondViewport": full,
                "fromSurface": true,
            }),
        )
        .await?;
    let data = shot["data"].as_str().unwrap_or("");
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| invalid(format!("the screenshot was not readable: {e}")))?;
    let (jpeg, w, h) =
        fit(jpeg).map_err(|e| invalid(format!("the screenshot was not readable: {e}")))?;
    let mut said = format!(
        "{} of page {} ({w}×{h} px){}.",
        if full {
            "The whole page"
        } else {
            "The visible part"
        },
        ctx.id,
        if full && height >= MAX_FULL_HEIGHT {
            format!(", its first {MAX_FULL_HEIGHT} CSS px")
        } else {
            String::new()
        }
    );
    if let Some(path) = save {
        std::fs::write(&path, &jpeg)
            .map_err(|e| invalid(format!("could not write {}: {e}", path.display())))?;
        said.push_str(&format!(" Saved to {}.", path.display()));
    }
    Ok(ToolOutput {
        content: vec![
            Content::Text(said),
            Content::Image {
                mime: "image/jpeg".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&jpeg),
            },
        ],
        is_error: false,
    })
}

/// The object group of what `browser_eval` holds on to while it reads it.
const EVAL_GROUP: &str = "monocode-eval";

pub async fn eval(ctx: &Ctx, args: &Value) -> Reply {
    let Some(expression) = args.get("expression").and_then(Value::as_str) else {
        return Err(invalid("expression is required"));
    };
    let await_promise = args
        .get("await_promise")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Run once, by reference: what cannot be returned as JSON (`window`,
    // a DOM node, a cycle) is then described instead of failing, and the
    // expression's side effects never happen twice.
    let answer = ctx
        .act(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": await_promise,
                "userGesture": true,
                "objectGroup": EVAL_GROUP,
            }),
        )
        .await?;
    if let Some(details) = answer.get("exceptionDetails") {
        let text = details["exception"]["description"]
            .as_str()
            .or(details["text"].as_str())
            .unwrap_or("exception");
        return Err(invalid(cap(
            format!("The expression threw: {text}"),
            "the exception is long",
        )));
    }
    let mut result = answer["result"].clone();
    if let Some(object) = result["objectId"].as_str() {
        let json = ctx
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object,
                    "functionDeclaration": "function () { return this; }",
                    "returnByValue": true,
                }),
            )
            .await;
        // A node or a function comes back as `{}`: its description says more.
        let opaque = result["subtype"] == "node"
            || result["type"] == "function"
            || (result["className"].as_str()).is_some_and(|class| class != "Object");
        let by_value = json.ok().filter(|json| {
            let value = &json["result"]["value"];
            json.get("exceptionDetails").is_none()
                && !value.is_null()
                && !(opaque && value.as_object().is_some_and(|o| o.is_empty()))
        });
        if let Some(json) = by_value {
            result = json["result"].clone();
        }
        drop(ctx.cdp.call(
            "Runtime.releaseObjectGroup",
            json!({"objectGroup": EVAL_GROUP}),
            None,
        ));
    }
    let text = if let Some(value) = result["unserializableValue"].as_str() {
        value.to_owned()
    } else if result["type"] == "undefined" {
        "undefined".to_owned()
    } else if let Some(value) = result.get("value") {
        serde_json::to_string_pretty(value).unwrap_or_default()
    } else {
        result["description"]
            .as_str()
            .or(result["type"].as_str())
            .unwrap_or("undefined")
            .to_owned()
    };
    Ok(ToolOutput::text(cap(
        text,
        "return less, e.g. pick the fields you need or slice arrays",
    )))
}

fn clock(ms: f64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M:%S%.3f")
                .to_string()
        })
        .unwrap_or_default()
}

fn limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(default, |l| l as usize)
        .clamp(1, max)
}

fn no_log(page: &dyn Page) -> ToolOutput {
    invalid(format!(
        "page {} has not loaded yet, so it has no log; use it first (browser_snapshot)",
        page.info().id
    ))
}

pub fn console(page: &dyn Page, args: &Value) -> Reply {
    let level = match args.get("level").and_then(Value::as_str) {
        None => None,
        Some(name) => Some(Level::parse(name).ok_or_else(|| {
            invalid(format!("level is debug, log, warning or error, not {name}"))
        })?),
    };
    let log = page.log().ok_or_else(|| no_log(page))?;
    let entries = log.console(limit(args, 50, crate::browser::log::CONSOLE_ENTRIES), level);
    let id = page.info().id;
    if entries.is_empty() {
        return Ok(ToolOutput::text(format!(
            "Page {id} has no console messages{}.",
            if level.is_some() {
                " at that level"
            } else {
                ""
            }
        )));
    }
    let mut out = format!("Console of page {id}, oldest first:\n");
    for entry in entries {
        out.push_str(&format!(
            "{} {:<7} {}",
            clock(entry.time),
            entry.level.name(),
            entry.text
        ));
        if let Some(source) = entry.source {
            out.push_str(&format!("  ({source})"));
        }
        out.push('\n');
    }
    Ok(ToolOutput::text(cap(
        out.trim_end().to_owned(),
        "ask for fewer with limit or level",
    )))
}

pub async fn network(page: &dyn Page, args: &Value) -> Reply {
    let log = page.log().ok_or_else(|| no_log(page))?;
    let id = page.info().id;
    if let Some(request) = args.get("body_of").and_then(Value::as_str) {
        let (body, encoded) = match log.response_body(request).await {
            Ok(body) => body,
            Err(CdpError::Gone) => return Err(super::page_gone(id)),
            Err(error) => {
                return Err(invalid(format!("no body for request {request}: {error}")));
            }
        };
        let text = if encoded {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&body)
                .unwrap_or_default();
            match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(bytes) => {
                    return Ok(ToolOutput::text(format!(
                        "The body of request {request} is binary, {} bytes.",
                        bytes.as_bytes().len()
                    )));
                }
            }
        } else {
            body
        };
        return Ok(ToolOutput::text(cap(
            text,
            "the body is long; browser_eval can fetch and pick parts of it",
        )));
    }
    let filter = args
        .get("filter")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty());
    let entries = log.network(
        limit(args, 50, crate::browser::log::NETWORK_ENTRIES),
        filter,
    );
    if entries.is_empty() {
        return Ok(ToolOutput::text(format!(
            "Page {id} has no requests{}.",
            if filter.is_some() { " like that" } else { "" }
        )));
    }
    let mut out = format!("Requests of page {id}, oldest first (id method status type url):\n");
    for e in entries {
        let status = match (&e.failed, e.status, e.finished) {
            (Some(failed), _, _) => format!("failed ({failed})"),
            (None, Some(status), _) => status.to_string(),
            (None, None, false) => "pending".into(),
            (None, None, true) => "-".into(),
        };
        out.push_str(&format!(
            "{} {} {} {} {}",
            e.request_id,
            e.method,
            status,
            e.kind.to_lowercase(),
            short_url(&e.url)
        ));
        // A failed request's size is that of Chromium's error page.
        if let Some(size) = e.size.filter(|_| e.failed.is_none()) {
            out.push_str(&format!(" {} B", size as u64));
        }
        out.push('\n');
    }
    Ok(ToolOutput::text(cap(
        out.trim_end().to_owned(),
        "ask for fewer with limit or filter",
    )))
}

/// A URL as the network list shows it: `data:` URLs by their type and
/// length (their payload is often hundreds of KB of base64), others cut at
/// 300 characters.
fn short_url(url: &str) -> String {
    const MAX: usize = 300;
    if let Some(rest) = url.strip_prefix("data:") {
        let kind = rest.split([',', ';']).next().unwrap_or_default();
        let kind = if kind.is_empty() { "text/plain" } else { kind };
        return format!("data:{kind},… ({} characters)", url.chars().count());
    }
    if url.chars().count() <= MAX {
        return url.to_owned();
    }
    let cut: String = url.chars().take(MAX).collect();
    format!("{cut}… ({} characters)", url.chars().count())
}

pub fn dialog(page: &dyn Page, args: &Value) -> Reply {
    let accept = match args.get("action").and_then(Value::as_str) {
        Some("accept") => true,
        Some("dismiss") => false,
        _ => return Err(invalid("action is accept or dismiss")),
    };
    let id = page.info().id;
    let Some(dialog) = page.dialog() else {
        return Err(invalid(format!("page {id} has no dialog open")));
    };
    let text = args.get("text").and_then(Value::as_str);
    if !page.answer_dialog(accept, text) {
        return Err(invalid(format!("page {id} has no dialog open")));
    }
    Ok(ToolOutput::text(format!(
        "{} {} on page {id}{}.",
        if accept { "Accepted" } else { "Dismissed" },
        super::describe_dialog(&dialog),
        match (accept, text) {
            (true, Some(text)) => format!(" with {}", quote(text, 100)),
            _ => String::new(),
        }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_screenshots_are_scaled_to_the_longest_edge() {
        let image = image::RgbImage::from_pixel(3000, 1000, image::Rgb([200, 30, 30]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
            .encode_image(&image)
            .unwrap();
        let (small, w, h) = fit(jpeg).unwrap();
        assert_eq!((w, h), (1568, 523));
        let (_, w2, h2) = fit(small).unwrap();
        assert_eq!((w2, h2), (1568, 523));
    }
}
