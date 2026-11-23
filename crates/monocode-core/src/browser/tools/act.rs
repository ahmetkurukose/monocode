//! Element actions over refs (K33) and input events: mouse, keys, text.
//!
//! Input goes to the page's own session even for out-of-process iframes:
//! Chromium routes mouse events by point and key events to the focused
//! frame. Points of such frames are offset by their `<iframe>`'s box.
//! Every action says what it did ([`effects`], K45).

use serde_json::{Value, json};

use super::{
    Ctx, Reply, effects, invalid, keys, nav, quote,
    snapshot::{AxTree, RefEntry},
};
use crate::browser::{CdpError, ToolOutput};

/// `browser_type` takes at most this many characters.
const MAX_TYPED: usize = 5_000;

fn no_ref(message: String) -> ToolOutput {
    ToolOutput::error("browser_no_ref", message)
}

fn stale(message: String) -> ToolOutput {
    ToolOutput::error("browser_stale_ref", message)
}

pub fn label(r: &str, entry: &RefEntry) -> String {
    if entry.name.is_empty() {
        format!("{r} ({})", entry.role)
    } else {
        format!("{r} ({} {})", entry.role, quote(&entry.name, 80))
    }
}

pub fn ref_arg(args: &Value) -> Option<String> {
    args.get("ref")
        .and_then(Value::as_str)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

/// The element `r` points at, found again if the page re-rendered it.
pub async fn resolve(ctx: &Ctx, r: &str) -> Result<RefEntry, ToolOutput> {
    let (entry, loader, empty) = {
        let refs = ctx.state.refs.borrow();
        (refs.get(r).cloned(), refs.loader.clone(), refs.is_empty())
    };
    let Some(entry) = entry else {
        return Err(no_ref(if empty {
            format!(
                "page {} has no refs yet; take a browser_snapshot first",
                ctx.id
            )
        } else {
            format!(
                "{r} is not a ref of page {}'s snapshots; take a new browser_snapshot",
                ctx.id
            )
        }));
    };
    let now = nav::loader(ctx).await?;
    if now != loader {
        ctx.state.refs.borrow_mut().reset(now);
        return Err(stale(format!(
            "page {} navigated since the snapshot; take a new browser_snapshot",
            ctx.id
        )));
    }
    if connected(ctx, &entry, entry.backend).await? {
        return Ok(entry);
    }
    // Re-rendered: the same role and name, the same place among equals.
    if entry.role != "clickable" {
        let mut params = json!({});
        if let (None, Some(frame)) = (&entry.session, &entry.frame_id) {
            params["frameId"] = frame.as_str().into();
        }
        let tree = ctx
            .cdp
            .call(
                "Accessibility.getFullAXTree",
                params,
                entry.session.as_deref(),
            )
            .await;
        match tree {
            Ok(tree) => {
                let tree = AxTree::parse(&tree);
                for backend in tree.candidates(&entry.role, &entry.name, entry.nth) {
                    if connected(ctx, &entry, backend).await? {
                        ctx.state.refs.borrow_mut().moved(r, backend);
                        return Ok(RefEntry { backend, ..entry });
                    }
                }
            }
            Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
            Err(_) => {}
        }
    }
    Err(stale(format!(
        "{} is no longer on the page; take a new browser_snapshot",
        label(r, &entry)
    )))
}

/// Whether `backend` (in `entry`'s session) is in its document.
async fn connected(ctx: &Ctx, entry: &RefEntry, backend: i64) -> Result<bool, ToolOutput> {
    let session = entry.session.as_deref();
    let soft = |result: Result<Value, CdpError>| match result {
        Ok(value) => Ok(Some(value)),
        Err(CdpError::Gone) => Err(super::page_gone(ctx.id)),
        Err(_) => Ok(None),
    };
    let resolved = soft(
        ctx.cdp
            .call(
                "DOM.resolveNode",
                json!({"backendNodeId": backend, "objectGroup": "monocode"}),
                session,
            )
            .await,
    )?;
    let Some(object) = resolved
        .as_ref()
        .and_then(|r| r["object"]["objectId"].as_str())
    else {
        return Ok(false);
    };
    let answer = soft(
        ctx.cdp
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object,
                    "functionDeclaration": "function() { return this.isConnected; }",
                    "returnByValue": true,
                }),
                session,
            )
            .await,
    )?;
    Ok(answer.is_some_and(|a| a["result"]["value"] == true))
}

fn center(quad: &Value) -> Option<(f64, f64)> {
    let q: Vec<f64> = quad.as_array()?.iter().filter_map(Value::as_f64).collect();
    if q.len() < 8 {
        return None;
    }
    Some((
        (q[0] + q[2] + q[4] + q[6]) / 4.0,
        (q[1] + q[3] + q[5] + q[7]) / 4.0,
    ))
}

/// The element's center in the page's viewport, after scrolling it there.
async fn point(ctx: &Ctx, r: &str, entry: &RefEntry) -> Result<(f64, f64), ToolOutput> {
    let session = entry.session.as_deref();
    let node = json!({"backendNodeId": entry.backend});
    if let Err(CdpError::Gone) = ctx
        .cdp
        .call("DOM.scrollIntoViewIfNeeded", node.clone(), session)
        .await
    {
        return Err(super::page_gone(ctx.id));
    }
    let model = match ctx.cdp.call("DOM.getBoxModel", node, session).await {
        Ok(model) => model,
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(_) => {
            return Err(invalid(format!(
                "{} is not visible (it has no box); scroll or open what hides it",
                label(r, entry)
            )));
        }
    };
    let (mut x, mut y) = center(&model["model"]["content"])
        .or_else(|| center(&model["model"]["border"]))
        .ok_or_else(|| invalid(format!("{} has no box", label(r, entry))))?;
    if let (Some(_), Some(owner)) = (session, entry.owner) {
        let frame = ctx
            .call("DOM.getBoxModel", json!({"backendNodeId": owner}))
            .await?;
        let quad = &frame["model"]["content"];
        x += quad[0].as_f64().unwrap_or(0.0);
        y += quad[1].as_f64().unwrap_or(0.0);
    }
    Ok((x, y))
}

async fn mouse(
    ctx: &Ctx,
    kind: &str,
    x: f64,
    y: f64,
    button: &str,
    count: i64,
) -> Result<(), ToolOutput> {
    let buttons = match (kind, button) {
        ("mousePressed", "left") => 1,
        ("mousePressed", "right") => 2,
        ("mousePressed", "middle") => 4,
        _ => 0,
    };
    ctx.act(
        "Input.dispatchMouseEvent",
        json!({"type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": count}),
    )
    .await
    .map(drop)
}

pub async fn click(ctx: &Ctx, args: &Value) -> Reply {
    let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
    if !["left", "right", "middle"].contains(&button) {
        return Err(invalid("button is left, right or middle"));
    }
    let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
    let (x, y, what) = target(ctx, args).await?;
    let before = effects::before(ctx).await?;
    press_at(ctx, x, y, button, double).await?;
    let verb = match (double, button) {
        (true, _) => "Double-clicked",
        (false, "right") => "Right-clicked",
        (false, "middle") => "Middle-clicked",
        _ => "Clicked",
    };
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(format!("{verb} {what}."), true))
}

/// The point a click goes to, from `ref` or `x` and `y`, and what it is.
pub async fn target(ctx: &Ctx, args: &Value) -> Result<(f64, f64, String), ToolOutput> {
    if let Some(r) = ref_arg(args) {
        let entry = resolve(ctx, &r).await?;
        let (x, y) = point(ctx, &r, &entry).await?;
        return Ok((x, y, label(&r, &entry)));
    }
    match (
        args.get("x").and_then(Value::as_f64),
        args.get("y").and_then(Value::as_f64),
    ) {
        (Some(x), Some(y)) => Ok((x, y, format!("({x}, {y})"))),
        _ => Err(invalid("give ref, or x and y")),
    }
}

/// Moves the mouse to a point and clicks there.
pub async fn press_at(
    ctx: &Ctx,
    x: f64,
    y: f64,
    button: &str,
    double: bool,
) -> Result<(), ToolOutput> {
    mouse(ctx, "mouseMoved", x, y, "none", 0).await?;
    for count in 1..=if double { 2 } else { 1 } {
        mouse(ctx, "mousePressed", x, y, button, count).await?;
        mouse(ctx, "mouseReleased", x, y, button, count).await?;
    }
    Ok(())
}

/// Selects a field's text (or picks a `<select>` option): `{kind}`.
const FILL: &str = r#"function(value) {
  if (this.tagName === 'SELECT') {
    const options = [...this.options];
    const option = options.find(o => o.value === value)
      || options.find(o => o.label.trim() === value || o.text.trim() === value);
    if (!option) return {kind: 'nooption', options: options.slice(0, 50).map(o => o.label || o.value)};
    this.value = option.value;
    this.dispatchEvent(new Event('input', {bubbles: true}));
    this.dispatchEvent(new Event('change', {bubbles: true}));
    return {kind: 'select'};
  }
  const field = this.isContentEditable || this.tagName === 'INPUT' || this.tagName === 'TEXTAREA'
    ? this : this.querySelector('input, textarea, [contenteditable=""], [contenteditable="true"]');
  const noText = ['checkbox', 'radio', 'button', 'submit', 'reset', 'file', 'image', 'color', 'range'];
  if (!field || (field.tagName === 'INPUT' && noText.includes(field.type))) return {kind: 'other'};
  field.focus();
  if (field.isContentEditable) {
    const range = document.createRange();
    range.selectNodeContents(field);
    const selection = getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  } else {
    try { field.select(); } catch (e) {}
  }
  return {kind: 'text'};
}"#;

pub async fn fill(ctx: &Ctx, args: &Value) -> Reply {
    let Some(r) = ref_arg(args) else {
        return Err(invalid("ref is required"));
    };
    let value = match args.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => return Err(invalid("value is required")),
    };
    let entry = resolve(ctx, &r).await?;
    let before = effects::before(ctx).await?;
    let session = entry.session.as_deref();
    let node = json!({"backendNodeId": entry.backend});
    let _ = ctx
        .call_in(session, "DOM.scrollIntoViewIfNeeded", node.clone())
        .await;
    let object = ctx
        .call_in(
            session,
            "DOM.resolveNode",
            json!({"backendNodeId": entry.backend, "objectGroup": "monocode"}),
        )
        .await?;
    let object = object["object"]["objectId"].as_str().unwrap_or_default();
    let prepared = ctx
        .call_in(
            session,
            "Runtime.callFunctionOn",
            json!({
                "objectId": object,
                "functionDeclaration": FILL,
                "arguments": [{"value": value}],
                "returnByValue": true,
            }),
        )
        .await?;
    let result = &prepared["result"]["value"];
    let said = match result["kind"].as_str() {
        Some("select") => format!("Picked {} in {}.", quote(&value, 100), label(&r, &entry)),
        Some("nooption") => {
            let options: Vec<String> = result["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|o| quote(o, 60))
                .collect();
            return Err(invalid(format!(
                "{} has no option {}; it has {}",
                label(&r, &entry),
                quote(&value, 100),
                options.join(", ")
            )));
        }
        Some("text") => {
            if value.is_empty() {
                key(ctx, &keys::parse("Delete").expect("a known key")).await?;
                format!("Cleared {}.", label(&r, &entry))
            } else {
                ctx.act("Input.insertText", json!({"text": value})).await?;
                format!("Filled {} with {}.", label(&r, &entry), quote(&value, 100))
            }
        }
        _ => {
            return Err(invalid(format!(
                "{} is not a text field; use browser_click or browser_type",
                label(&r, &entry)
            )));
        }
    };
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(said, true))
}

/// Presses and releases one key.
async fn key(ctx: &Ctx, press: &keys::KeyPress) -> Result<(), ToolOutput> {
    let mut down = json!({
        "type": if press.text.is_some() { "keyDown" } else { "rawKeyDown" },
        "modifiers": press.modifiers,
        "key": press.key,
        "code": press.code,
        "windowsVirtualKeyCode": press.key_code,
        "nativeVirtualKeyCode": press.key_code,
        "location": press.location,
    });
    if let Some(text) = &press.text {
        down["text"] = text.as_str().into();
        down["unmodifiedText"] = text.as_str().into();
    }
    if !press.commands.is_empty() {
        down["commands"] = json!(press.commands);
    }
    ctx.act("Input.dispatchKeyEvent", down).await?;
    ctx.act(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "modifiers": press.modifiers,
            "key": press.key,
            "code": press.code,
            "windowsVirtualKeyCode": press.key_code,
            "nativeVirtualKeyCode": press.key_code,
            "location": press.location,
        }),
    )
    .await
    .map(drop)
}

pub async fn type_text(ctx: &Ctx, args: &Value) -> Reply {
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return Err(invalid("text is required"));
    };
    let count = text.chars().count();
    if count > MAX_TYPED {
        return Err(invalid(format!(
            "text has {count} characters, more than {MAX_TYPED}; use browser_fill for long text"
        )));
    }
    let entry = match ref_arg(args) {
        Some(r) => Some((resolve(ctx, &r).await?, r)),
        None => None,
    };
    let before = effects::before(ctx).await?;
    let mut into = String::new();
    if let Some((entry, r)) = entry {
        let session = entry.session.as_deref();
        let node = json!({"backendNodeId": entry.backend});
        let _ = ctx
            .call_in(session, "DOM.scrollIntoViewIfNeeded", node.clone())
            .await;
        ctx.call_in(session, "DOM.focus", node).await?;
        into = format!(" into {}", label(&r, &entry));
    }
    let mut pending = String::new();
    for ch in text.replace("\r\n", "\n").chars() {
        match keys::for_char(ch) {
            Some(press) => {
                if !pending.is_empty() {
                    let run = std::mem::take(&mut pending);
                    ctx.act("Input.insertText", json!({"text": run})).await?;
                }
                key(ctx, &press).await?;
            }
            None => pending.push(ch),
        }
    }
    if !pending.is_empty() {
        ctx.act("Input.insertText", json!({"text": pending}))
            .await?;
    }
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(format!("Typed {count} characters{into}."), true))
}

pub async fn press(ctx: &Ctx, args: &Value) -> Reply {
    let Some(name) = args.get("key").and_then(Value::as_str) else {
        return Err(invalid("key is required"));
    };
    let Some(press) = keys::parse(name.trim()) else {
        return Err(invalid(format!(
            "{name:?} is not a key; use names like Enter, Tab, Escape, ArrowDown, Backspace, F5, a, with modifiers like Shift+Tab or Meta+A"
        )));
    };
    let before = effects::before(ctx).await?;
    key(ctx, &press).await?;
    let effects = effects::settle(ctx, before).await?;
    Ok(effects.with(format!("Pressed {name}."), true))
}

/// The scroll position of the element's nearest scroll container (itself
/// included): `[x, y, height, shown, itself]`, or `null` when only the page
/// scrolls it.
const SCROLLER: &str = r#"function() {
  const scrolls = v => v === 'auto' || v === 'scroll' || v === 'overlay';
  let el = this.nodeType === 1 ? this : this.parentElement;
  for (; el && el !== document.body && el !== document.documentElement; el = el.parentElement) {
    const style = getComputedStyle(el);
    if ((scrolls(style.overflowY) && el.scrollHeight > el.clientHeight)
      || (scrolls(style.overflowX) && el.scrollWidth > el.clientWidth))
      return [Math.round(el.scrollLeft), Math.round(el.scrollTop), el.scrollHeight, el.clientHeight, el === this];
  }
  return null;
}"#;

pub async fn scroll(ctx: &Ctx, args: &Value) -> Reply {
    let dx = args.get("dx").and_then(Value::as_f64).unwrap_or(0.0);
    let dy = args.get("dy").and_then(Value::as_f64).unwrap_or(0.0);
    let by = dx != 0.0 || dy != 0.0;
    let entry = match ref_arg(args) {
        Some(r) => Some((resolve(ctx, &r).await?, r)),
        None if by => None,
        None => return Err(invalid("give ref, or dx and dy")),
    };
    let before = effects::before(ctx).await?;
    let (at, mut said) = match &entry {
        Some((entry, r)) => (
            Some(point(ctx, r, entry).await?),
            format!("Scrolled {} into view", label(r, entry)),
        ),
        None => (None, String::from("Scrolled")),
    };
    if by {
        let (x, y) = match at {
            Some(point) => point,
            None => {
                let metrics = ctx.call("Page.getLayoutMetrics", json!({})).await?;
                let view = &metrics["cssVisualViewport"];
                (
                    view["clientWidth"].as_f64().unwrap_or(800.0) / 2.0,
                    view["clientHeight"].as_f64().unwrap_or(600.0) / 2.0,
                )
            }
        };
        ctx.act(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "button": "none"}),
        )
        .await?;
        said.push_str(&format!(" by ({dx}, {dy})"));
    }
    let effects = effects::settle(ctx, before).await?;
    if !effects.navigated {
        let container = match &entry {
            Some((entry, _)) => scroller(ctx, entry).await?,
            None => None,
        };
        if let Some(p) = container {
            let n = |i: usize| p.get(i).and_then(Value::as_i64).unwrap_or(0);
            let what = if p.get(4) == Some(&Value::Bool(true)) {
                "it"
            } else {
                "its scroll container"
            };
            said.push_str(&format!(
                "; {what} is scrolled to x={}, y={} of {} px ({} px are shown)",
                n(0),
                n(1),
                n(2),
                n(3)
            ));
        } else {
            let position = nav::probe(
                ctx,
                "[Math.round(scrollX), Math.round(scrollY), document.documentElement.scrollHeight, innerHeight]",
            )
            .await?;
            if let Ok(Value::Array(p)) = position {
                let n = |i: usize| p.get(i).and_then(Value::as_i64).unwrap_or(0);
                said.push_str(&format!(
                    "; the page is scrolled to x={}, y={} of {} px (the viewport is {} px high)",
                    n(0),
                    n(1),
                    n(2),
                    n(3)
                ));
            }
        }
    }
    said.push('.');
    Ok(effects.with(said, false))
}

/// [`SCROLLER`] of a ref's element; `None` when the page scrolls it.
async fn scroller(ctx: &Ctx, entry: &RefEntry) -> Result<Option<Vec<Value>>, ToolOutput> {
    let session = entry.session.as_deref();
    let resolved = ctx
        .cdp
        .call(
            "DOM.resolveNode",
            json!({"backendNodeId": entry.backend, "objectGroup": "monocode"}),
            session,
        )
        .await;
    let object = match resolved {
        Ok(resolved) => resolved["object"]["objectId"].as_str().map(str::to_owned),
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(_) => None,
    };
    let Some(object) = object else {
        return Ok(None);
    };
    let answer = ctx
        .cdp
        .call(
            "Runtime.callFunctionOn",
            json!({"objectId": object, "functionDeclaration": SCROLLER, "returnByValue": true}),
            session,
        )
        .await;
    Ok(match answer {
        Ok(answer) => answer["result"]["value"].as_array().cloned(),
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(_) => None,
    })
}
