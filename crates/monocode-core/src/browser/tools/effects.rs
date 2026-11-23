//! What an action did (K45), after Orca's `agent-browser-action-effects.ts`.
//!
//! [`before`] notes the page as it is: its document, URL and title, the
//! pages of the scope, the console, and a snapshot baseline (the last one
//! the agent saw, or a new one). [`settle`] then waits: for a navigation
//! the action started within 50 ms to load (at most 5 s), else for 100 ms
//! of DOM quiet (at most 750 ms), counted by a `MutationObserver` in an
//! isolated world the page cannot touch. What changed comes back as text:
//! a navigation, URL and title, new tabs, downloads, console errors, a
//! dialog, and the lines of the snapshot that changed (at most 40).
//! When the DOM did not move, no second snapshot is taken.

use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use serde_json::json;

use super::{
    Ctx, describe_dialog,
    nav::{self, Load},
    observe, quote,
    snapshot::{self, Mode},
    state::Shot,
    timed,
};
use crate::browser::{CdpError, PageId, Subscription, ToolOutput};

/// A navigation counts as the action's when it starts within this.
const NAVIGATION_GRACE_MS: u64 = 50;
/// How long a navigation the action started may take to load.
const NAVIGATION_MS: u64 = 5_000;
/// The DOM is quiet after this long without a change...
const QUIET_MS: u64 = 100;
/// ...or once this much time passed.
const QUIET_BUDGET_MS: u64 = 750;
const POLL_MS: u64 = 50;
/// A delta longer than this is not shown.
pub const MAX_DELTA: usize = 40;
/// Tabs, downloads and errors listed at most.
const LIST_ITEMS: usize = 5;

/// Counts DOM changes and input events from now on, once per document;
/// returns the count.
const INSTALL: &str = r#"(() => {
  let s = globalThis.__monocodeChanges;
  if (!s) {
    s = globalThis.__monocodeChanges = {n: 0, at: 0};
    const bump = () => { s.n++; s.at = performance.now(); };
    new MutationObserver(bump).observe(document,
      {subtree: true, childList: true, attributes: true, characterData: true});
    for (const type of ['input', 'change', 'focusin', 'focusout', 'toggle'])
      document.addEventListener(type, bump, true);
  }
  return s.n;
})()"#;

/// Resolves with the count once nothing changed for `QUIET_MS`, or after
/// `QUIET_BUDGET_MS`; -1 when the counter is not there.
fn quiet_probe() -> String {
    format!(
        r#"new Promise(resolve => {{
  const s = globalThis.__monocodeChanges;
  if (!s) {{ resolve(-1); return; }}
  const start = performance.now();
  const check = () => {{
    const now = performance.now();
    if (now - Math.max(start, s.at) >= {QUIET_MS} || now - start >= {QUIET_BUDGET_MS}) resolve(s.n);
    else setTimeout(check, 20);
  }};
  check();
}})"#
    )
}

/// The page's events during the action.
#[derive(Default)]
struct Watch {
    /// A navigation of the main frame to another document was asked for
    /// or started.
    navigating: bool,
    /// The main frame stopped loading after that.
    stopped: bool,
    /// The page asked for a new tab or window.
    new_tab: bool,
    /// `(file name, url)` of downloads that began.
    downloads: Vec<(String, String)>,
}

/// The page before the action.
pub struct Before {
    loader: Option<String>,
    url: String,
    title: String,
    pages: HashSet<PageId>,
    console: Option<u64>,
    /// The page's last permission note then (K63).
    permissions: u64,
    /// The isolated world and its change count.
    counter: Option<(i64, f64)>,
    watch: Rc<RefCell<Watch>>,
    _events: Subscription,
}

/// What the effects read gets at most, so a page too busy to answer only
/// costs them their facts, not the action.
const READ_MS: u64 = 1_000;
const SNAPSHOT_MS: u64 = 3_000;

/// The page's URL and title; `None` when it cannot say now.
async fn location(ctx: &Ctx) -> Result<Option<(String, String)>, ToolOutput> {
    Ok(timed(&ctx.host, READ_MS, nav::location(ctx))
        .await
        .transpose()?
        .flatten()
        .map(|at| (at.url, at.title)))
}

/// The main frame's id and loader id.
async fn main_frame(ctx: &Ctx) -> Result<(Option<String>, Option<String>), ToolOutput> {
    let tree = ctx.call("Page.getFrameTree", json!({})).await?;
    let frame = &tree["frameTree"]["frame"];
    let of = |key: &str| frame[key].as_str().map(str::to_owned);
    Ok((of("id"), of("loaderId")))
}

/// The snapshot options action effects read with.
fn default_key() -> String {
    observe::shot_key(Mode::Interactive, None, None)
}

/// The isolated world of the page's current document, made once per
/// document; `None` when Chromium would not make one.
async fn world(
    ctx: &Ctx,
    frame: &Option<String>,
    loader: &Option<String>,
    fresh: bool,
) -> Result<Option<i64>, ToolOutput> {
    if !fresh {
        if let Some((of, id)) = ctx.state.world.borrow().clone() {
            if &of == loader {
                return Ok(Some(id));
            }
        }
    }
    let Some(frame) = frame else { return Ok(None) };
    let made = ctx
        .cdp
        .call(
            "Page.createIsolatedWorld",
            json!({"frameId": frame, "worldName": "monocode", "grantUniveralAccess": false}),
            None,
        )
        .await;
    let id = match made {
        Ok(made) => made["executionContextId"].as_i64(),
        Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
        Err(_) => None,
    };
    *ctx.state.world.borrow_mut() = id.map(|id| (loader.clone(), id));
    Ok(id)
}

/// Starts counting the DOM's changes: `(world, count)`.
async fn counter(
    ctx: &Ctx,
    frame: &Option<String>,
    loader: &Option<String>,
) -> Result<Option<(i64, f64)>, ToolOutput> {
    for fresh in [false, true] {
        let Some(id) = world(ctx, frame, loader, fresh).await? else {
            return Ok(None);
        };
        let answer = ctx
            .cdp
            .call(
                "Runtime.evaluate",
                json!({"expression": INSTALL, "contextId": id, "returnByValue": true}),
                None,
            )
            .await;
        match answer {
            Ok(answer) => {
                if let Some(count) = answer["result"]["value"].as_f64() {
                    return Ok(Some((id, count)));
                }
                if answer.get("exceptionDetails").is_some() {
                    return Ok(None);
                }
            }
            Err(CdpError::Gone) => return Err(super::page_gone(ctx.id)),
            // The world went with an earlier document: make a new one.
            Err(_) => {}
        }
    }
    Ok(None)
}

/// Notes the page before an action.
pub async fn before(ctx: &Ctx) -> Result<Before, ToolOutput> {
    let (frame, loader) = main_frame(ctx).await?;
    let (url, title) = match location(ctx).await? {
        Some(at) => at,
        None => {
            let info = ctx.page.info();
            (info.url, info.title)
        }
    };
    let pages = ctx
        .host
        .pages(&ctx.scope)
        .iter()
        .map(|p| p.info().id)
        .collect();
    let console = ctx.page.log().map(|log| log.console_mark());
    let permissions = permission_mark(&*ctx.page);
    let watch: Rc<RefCell<Watch>> = Rc::default();
    let seen = watch.clone();
    let main = frame.clone();
    let events = ctx.cdp.subscribe(move |event| {
        if event.session_id.is_some() {
            return;
        }
        let p = &event.params;
        let ours = main.as_deref().is_none_or(|m| p["frameId"] == m);
        let mut watch = seen.borrow_mut();
        match event.method.as_str() {
            "Page.frameRequestedNavigation" => match p["disposition"].as_str() {
                None | Some("currentTab") if ours => watch.navigating = true,
                Some("newTab" | "newWindow") => watch.new_tab = true,
                _ => {}
            },
            "Page.frameStartedNavigating" if ours => {
                let kind = p["navigationType"].as_str().unwrap_or("");
                if !kind.to_ascii_lowercase().contains("samedocument") {
                    watch.navigating = true;
                }
            }
            "Page.frameStartedLoading" if ours => {
                watch.navigating = true;
                watch.stopped = false;
            }
            "Page.frameStoppedLoading" if ours => watch.stopped = true,
            "Page.windowOpen" => watch.new_tab = true,
            "Page.downloadWillBegin" => watch.downloads.push((
                p["suggestedFilename"].as_str().unwrap_or("").to_owned(),
                p["url"].as_str().unwrap_or("").to_owned(),
            )),
            _ => {}
        }
    });
    let counter = timed(&ctx.host, READ_MS, counter(ctx, &frame, &loader))
        .await
        .transpose()?
        .flatten();
    // The baseline is what the agent saw last; without one of this
    // document, it is read now.
    let known = ctx
        .state
        .seen
        .borrow()
        .as_ref()
        .is_some_and(|shot| shot.loader == loader && shot.key == default_key());
    if !known {
        let read = observe::read(ctx, Mode::Interactive, None, None);
        if let Some(reading) = timed(&ctx.host, SNAPSHOT_MS, read).await.transpose()? {
            *ctx.state.seen.borrow_mut() = Some(Shot {
                loader: reading.loader,
                key: default_key(),
                lines: reading.lines,
            });
        }
    }
    Ok(Before {
        loader,
        url,
        title,
        pages,
        console,
        permissions,
        counter,
        watch,
        _events: events,
    })
}

/// What the action did.
pub struct Effects {
    /// The page moved to another document.
    pub navigated: bool,
    /// Sentences about it, each starting with a space.
    facts: String,
    /// The changed lines, or why they are not shown.
    delta: Delta,
    /// The change counter saw the DOM move.
    moved: bool,
}

enum Delta {
    /// Not read: the DOM did not move, or the page is another document.
    None,
    Lines(Vec<String>),
    TooLong(usize),
}

impl Effects {
    /// `said` followed by the effects; `quiet_note` says so when nothing
    /// happened.
    pub fn with(self, said: impl Into<String>, quiet_note: bool) -> ToolOutput {
        let mut out = said.into();
        out.push_str(&self.facts);
        match self.delta {
            Delta::Lines(lines) if !lines.is_empty() => {
                out.push_str("\nThe page changed (+ added, - removed, ~ changed):\n");
                out.push_str(&lines.join("\n"));
            }
            Delta::TooLong(n) => out.push_str(&format!(
                " The page changed a lot ({n} lines); take a browser_snapshot to see it."
            )),
            // Text or styles moved, none of the outline's lines did.
            Delta::Lines(_) if self.moved && quiet_note => out.push_str(
                " The page changed, but none of its controls did (browser_snapshot with mode full shows its text).",
            ),
            _ if self.facts.is_empty() && quiet_note => {
                out.push_str(" Nothing on the page changed.");
            }
            _ => {}
        }
        ToolOutput::text(out)
    }
}

/// Waits for what the action started to settle and says what it did.
pub async fn settle(ctx: &Ctx, before: Before) -> Result<Effects, ToolOutput> {
    let mut facts = String::new();
    ctx.sleep(NAVIGATION_GRACE_MS).await;
    let mut waited = NAVIGATION_GRACE_MS;
    // A navigation: wait for its document.
    let mut navigated = false;
    loop {
        if let Some(dialog) = ctx.page.dialog() {
            return Ok(dialog_effects(ctx, &before, facts, &dialog));
        }
        let (_, loader) = main_frame(ctx).await?;
        if loader != before.loader {
            navigated = true;
            break;
        }
        let pending = {
            let watch = before.watch.borrow();
            watch.navigating && !watch.stopped
        };
        if !pending || waited >= NAVIGATION_MS {
            break;
        }
        ctx.sleep(POLL_MS).await;
        waited += POLL_MS;
    }
    if navigated {
        let budget = NAVIGATION_MS.saturating_sub(waited).max(POLL_MS);
        let done = nav::settle(ctx, before.loader.clone(), None, Load::Loaded, budget).await?;
        facts.push_str(" The page navigated: ");
        facts.push_str(&nav::report(ctx, done).await?);
        facts.push_str(" Refs of earlier snapshots no longer work.");
    }
    // Then the DOM: quiet, or the budget spent. `None`: not known.
    let mut moved = None;
    if !navigated {
        match before.counter {
            Some((world, start)) => match quiet(ctx, world).await? {
                Ok(Some(count)) => moved = Some(count != start),
                Ok(None) => {}
                Err(dialog) => return Ok(dialog_effects(ctx, &before, facts, &dialog)),
            },
            None => ctx.sleep(QUIET_MS).await,
        }
    }
    // A tab the page asked for may take a moment to show up.
    let mut waited = 0;
    while before.watch.borrow().new_tab
        && new_pages(ctx, &before).is_empty()
        && waited < QUIET_BUDGET_MS
    {
        ctx.sleep(POLL_MS).await;
        waited += POLL_MS;
    }
    if !navigated {
        if let Some((url, title)) = location(ctx).await? {
            if url != before.url {
                facts.push_str(&format!(" The URL is now {url}."));
            }
            if title != before.title {
                facts.push_str(&format!(" The title is now {}.", quote(&title, 200)));
            }
        }
    }
    others(ctx, &before, &mut facts);
    if let Some(dialog) = ctx.page.dialog() {
        return Ok(dialog_effects(ctx, &before, facts, &dialog));
    }
    let delta = if navigated || moved == Some(false) {
        Delta::None
    } else {
        timed(&ctx.host, SNAPSHOT_MS, read_delta(ctx, &before))
            .await
            .transpose()?
            .unwrap_or(Delta::None)
    };
    Ok(Effects {
        navigated,
        facts,
        delta,
        moved: moved == Some(true),
    })
}

/// The change count once the DOM is quiet; `Ok(None)` when it cannot be
/// read, `Err` with the dialog that blocks the page.
async fn quiet(
    ctx: &Ctx,
    world: i64,
) -> Result<Result<Option<f64>, crate::browser::DialogInfo>, ToolOutput> {
    let probe = ctx.act_raw(
        "Runtime.evaluate",
        json!({
            "expression": quiet_probe(),
            "contextId": world,
            "awaitPromise": true,
            "returnByValue": true,
        }),
    );
    let limit = ctx.host.sleep(Duration::from_millis(QUIET_BUDGET_MS + 250));
    let answer = match futures_util::future::select(std::pin::pin!(probe), limit).await {
        futures_util::future::Either::Left((answer, _)) => answer,
        futures_util::future::Either::Right(_) => return Ok(Ok(None)),
    };
    match answer {
        Ok(Ok(value)) => Ok(Ok(value["result"]["value"].as_f64().filter(|n| *n >= 0.0))),
        Ok(Err(CdpError::Gone)) => Err(super::page_gone(ctx.id)),
        Ok(Err(_)) => Ok(Ok(None)),
        Err(_) => match ctx.page.dialog() {
            Some(dialog) => Ok(Err(dialog)),
            None => Ok(Ok(None)),
        },
    }
}

fn new_pages(ctx: &Ctx, before: &Before) -> Vec<(PageId, String)> {
    ctx.host
        .pages(&ctx.scope)
        .iter()
        .map(|p| p.info())
        .filter(|info| !before.pages.contains(&info.id))
        .map(|info| (info.id, info.url))
        .collect()
}

/// The seq of the page's last permission or passkey note; 0 without one.
pub(crate) fn permission_mark(page: &dyn crate::browser::Page) -> u64 {
    let permission = page.permission_notes().last().map_or(0, |n| n.seq);
    let passkey = page.passkey_notes().last().map_or(0, |n| n.seq);
    permission.max(passkey)
}

/// The page's permission and passkey requests since `mark` (K58, K63), as
/// sentences each starting with a space, in the order they happened.
pub(crate) fn permission_facts(page: &dyn crate::browser::Page, mark: u64) -> String {
    let mut notes: Vec<(u64, String)> = (page.permission_notes().into_iter())
        .filter(|n| n.seq > mark)
        .map(|n| (n.seq, n.describe()))
        .chain(
            (page.passkey_notes().into_iter())
                .filter(|n| n.seq > mark)
                .map(|n| (n.seq, n.describe())),
        )
        .collect();
    notes.sort_by_key(|(seq, _)| *seq);
    let mut facts = String::new();
    for (_, line) in notes.iter().rev().take(LIST_ITEMS).rev() {
        facts.push(' ');
        facts.push_str(line);
    }
    facts
}

/// New tabs, downloads, console errors and permission requests.
fn others(ctx: &Ctx, before: &Before, facts: &mut String) {
    facts.push_str(&permission_facts(&*ctx.page, before.permissions));
    let pages = new_pages(ctx, before);
    for (id, url) in pages.iter().take(LIST_ITEMS) {
        facts.push_str(&format!(" It opened page {id} ({}).", short(url)));
    }
    if pages.len() > LIST_ITEMS {
        facts.push_str(&format!(" And {} more pages.", pages.len() - LIST_ITEMS));
    }
    for (name, url) in before.watch.borrow().downloads.iter().take(LIST_ITEMS) {
        facts.push_str(&format!(
            " It started a download of {} ({}).",
            quote(name, 100),
            short(url)
        ));
    }
    if let (Some(log), Some(mark)) = (ctx.page.log(), before.console) {
        let errors: Vec<String> = log
            .console_since(mark)
            .into_iter()
            .filter(|e| e.level == crate::browser::log::Level::Error)
            .map(|e| e.text)
            .collect();
        for error in errors.iter().take(LIST_ITEMS) {
            facts.push_str(&format!(" Console error: {}.", quote(error, 300)));
        }
        if errors.len() > LIST_ITEMS {
            facts.push_str(&format!(
                " And {} more console errors (browser_console).",
                errors.len() - LIST_ITEMS
            ));
        }
    }
}

fn short(url: &str) -> String {
    if url.chars().count() <= 200 {
        return url.to_owned();
    }
    let cut: String = url.chars().take(200).collect();
    format!("{cut}…")
}

/// The effects when a dialog blocks the page: nothing more can be read.
fn dialog_effects(
    ctx: &Ctx,
    before: &Before,
    mut facts: String,
    dialog: &crate::browser::DialogInfo,
) -> Effects {
    others(ctx, before, &mut facts);
    facts.push_str(&format!(
        " The page opened {} and waits until you answer it with browser_dialog.",
        describe_dialog(dialog)
    ));
    Effects {
        navigated: false,
        facts,
        delta: Delta::None,
        moved: false,
    }
}

/// The snapshot lines that changed since the baseline. The agent sees a
/// short delta, which becomes the next baseline; after a long one, it has
/// seen nothing and the next action reads a new baseline.
async fn read_delta(ctx: &Ctx, before: &Before) -> Result<Delta, ToolOutput> {
    let baseline = ctx.state.seen.borrow().clone();
    let Some(baseline) = baseline.filter(|s| s.loader == before.loader && s.key == default_key())
    else {
        return Ok(Delta::None);
    };
    let reading = observe::read(ctx, Mode::Interactive, None, None).await?;
    if reading.loader != before.loader {
        return Ok(Delta::None);
    }
    let lines = snapshot::delta(&baseline.lines, &reading.lines);
    if lines.len() > MAX_DELTA {
        *ctx.state.seen.borrow_mut() = None;
        return Ok(Delta::TooLong(lines.len()));
    }
    *ctx.state.seen.borrow_mut() = Some(Shot {
        loader: reading.loader,
        key: default_key(),
        lines: reading.lines,
    });
    Ok(Delta::Lines(lines))
}
