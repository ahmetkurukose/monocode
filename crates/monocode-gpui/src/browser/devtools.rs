//! DevTools as SOCode's (docs/native/modes.md §DevTools): Developer Tools
//! (⌥⌘I, F12), Inspect Elements (⌥⌘C), JavaScript Console (⌥⌘J), Network
//! Inspector and View Source (⌥⌘U), docked beside the page (right, by
//! default a third of it), left, below, or in a window of their own.
//!
//! The frontend is Chromium's own, in a view of its own beside the page
//! (context.md K28); the app lays it out. It is told which panel to show,
//! and asked which one it shows, through its own DevTools channel (the
//! frontend's, not the page's, so the agents' session (K38) never sees it).

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gpui::actions;

actions!(
    browser,
    [
        /// ⌥⌘C: DevTools on Elements, picking an element of the page.
        InspectElements,
        /// ⌥⌘J: DevTools on the Console; closed when they show it.
        JavaScriptConsole,
        /// DevTools on Network; closed when they show it.
        NetworkInspector,
        /// ⌥⌘U: the page's source, in a new tab.
        ViewSource,
    ]
);

/// What the user asks of a page's DevTools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DevToolsCommand {
    /// Open on Elements, or close.
    Toggle,
    /// Open on Elements and pick an element; never closes.
    Elements,
    /// Open on the Console, or close when it shows.
    Console,
    /// Open on Network, or close when it shows.
    Network,
}

/// The frontend's main panels the commands name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Panel {
    Elements,
    Console,
    Network,
}

impl Panel {
    /// The panel's id in Chromium's frontend.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Panel::Elements => "elements",
            Panel::Console => "console",
            Panel::Network => "network",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        [Panel::Elements, Panel::Console, Panel::Network]
            .into_iter()
            .find(|panel| panel.id() == id)
    }
}

/// What a command does to the DevTools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Close,
    /// Show the panel, and pick an element when `true`.
    Open(Panel, bool),
}

/// What `command` does to DevTools `open` or not, showing `selected`.
pub(crate) fn operation(
    command: DevToolsCommand,
    open: bool,
    selected: Option<Panel>,
) -> Operation {
    use DevToolsCommand as C;
    match command {
        C::Toggle if open => Operation::Close,
        C::Console if open && selected == Some(Panel::Console) => Operation::Close,
        C::Network if open && selected == Some(Panel::Network) => Operation::Close,
        C::Console => Operation::Open(Panel::Console, false),
        C::Network => Operation::Open(Panel::Network, false),
        C::Elements => Operation::Open(Panel::Elements, true),
        C::Toggle => Operation::Open(Panel::Elements, false),
    }
}

/// What `command` does to DevTools already open, whose panel only their
/// frontend knows: `None` closes them; else the panel to show, whether to
/// pick an element, and whether showing it already closes them instead
/// (as [`operation`] decides with the panel known).
pub(crate) fn on_open(command: DevToolsCommand) -> Option<(Panel, bool, bool)> {
    match command {
        DevToolsCommand::Toggle => None,
        DevToolsCommand::Elements => Some((Panel::Elements, true, false)),
        DevToolsCommand::Console => Some((Panel::Console, false, true)),
        DevToolsCommand::Network => Some((Panel::Network, false, true)),
    }
}

/// Where a page's DevTools are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DockSide {
    #[default]
    Right,
    Left,
    Bottom,
    /// A window of their own (Chromium's).
    Undocked,
}

impl DockSide {
    /// Beside the page, so their size is a width.
    pub(crate) fn beside(self) -> bool {
        matches!(self, DockSide::Right | DockSide::Left)
    }
}

thread_local! {
    /// The next DevTools open where the last were put, for the app's run.
    static LAST_SIDE: Cell<DockSide> = const { Cell::new(DockSide::Right) };
}

pub(crate) fn last_side() -> DockSide {
    LAST_SIDE.with(Cell::get)
}

pub(crate) fn set_last_side(side: DockSide) {
    LAST_SIDE.with(|last| last.set(side));
}

/// DevTools and the page beside them each keep at least this many points.
pub(crate) const DEVTOOLS_MIN: f32 = 120.;
pub(crate) const PAGE_MIN: f32 = 80.;

/// DevTools' size along their side in a body `available` points across:
/// the wanted size (0: a third of the body, the page keeping two), kept
/// clear of both minimums. Unmeasured (0), the wanted size as it is.
pub(crate) fn devtools_extent(wanted: u32, available: f32) -> f32 {
    let wanted = if wanted == 0 {
        (available / 3.).floor().max(DEVTOOLS_MIN)
    } else {
        wanted as f32
    };
    if available <= 0. {
        return wanted;
    }
    let max = (available - PAGE_MIN).max(0.);
    wanted.clamp(DEVTOOLS_MIN.min(max), max)
}

/// The size a divider drag started at `start` (pointer, size) gives at
/// `position`, along the side's axis: away from the DevTools grows them.
pub(crate) fn dragged_extent(side: DockSide, start: (f32, f32), position: f32) -> f32 {
    let (from, size) = start;
    let moved = match side {
        DockSide::Left => position - from,
        _ => from - position,
    };
    (size + moved).max(1.)
}

/// The DevTools frontend Chromium ships, for the page whose target is
/// `target`, through the app's endpoint at `base` (`host:port/token`),
/// opening on `panel`.
///
/// Without `can_dock` the frontend has no "Dock side" menu: Chromium's
/// embedder half (DevToolsUIBindings) runs with its default delegate here,
/// which ignores dock requests, so the menu could not work. The app docks
/// them (the Command Bar's Dock DevTools commands).
pub(crate) fn devtools_frontend_url(base: &str, target: &str, panel: Panel) -> String {
    format!(
        "devtools://devtools/bundled/devtools_app.html?ws={base}/devtools/page/{target}&panel={}",
        panel.id()
    )
}

/// View Source's address for a page at `url`: web, file and data pages only.
pub(crate) fn view_source_url(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    ["http", "https", "file", "data"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
        .then(|| format!("view-source:{url}"))
}

/// What the frontend answers [`command_script`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// It showed the panel (and picks an element when asked).
    Shown,
    /// It showed the panel already: the command closes them.
    Close,
    /// Its interface did not come up in time.
    Unready,
}

impl Answer {
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value {
            Some("shown") => Answer::Shown,
            Some("close") => Answer::Close,
            _ => Answer::Unready,
        }
    }
}

/// The script run in the frontend for a command: it waits for the
/// frontend's interface (its main tabbed pane) to come up, then answers
/// `close` when `closes` and `panel` shows already, or shows `panel`
/// (`InspectorFrontendAPI`, the frontend's embedder interface) and, when
/// `inspect`, starts picking an element.
pub(crate) fn command_script(panel: Panel, inspect: bool, closes: bool) -> String {
    format!(
        r#"(async (panel, inspect, closes) => {{
  const sleep = ms => new Promise(done => setTimeout(done, ms));
  const find = (root, selector) => {{
    const hit = root.querySelector(selector);
    if (hit) return hit;
    for (const node of root.querySelectorAll('*')) {{
      const inner = node.shadowRoot && find(node.shadowRoot, selector);
      if (inner) return inner;
    }}
    return null;
  }};
  let pane = null;
  for (let i = 0; i < 200 && !(pane = find(document, '.main-tabbed-pane')); i++) await sleep(50);
  const api = globalThis.InspectorFrontendAPI;
  if (!pane || !api) return 'unready';
  await sleep(0);
  const tab = pane.shadowRoot && pane.shadowRoot.querySelector('.tabbed-pane-header-tab.selected');
  const selected = tab ? tab.id.replace(/^tab-/, '') : null;
  if (closes && selected === panel) return 'close';
  if (selected !== panel) api.showPanel(panel);
  if (inspect) api.enterInspectElementMode();
  return 'shown';
}})({panel:?}, {inspect}, {closes})"#,
        panel = panel.id(),
    )
}

/// Our message ids on the frontend's channel count from here.
const FIRST_ID: u64 = 1;

/// The frontend's own DevTools channel: scripts run in it, their answer
/// comes back. `A` keeps the channel attached (Chromium's registration).
pub(crate) struct FrontendChannel<A> {
    _attachment: A,
    next: Cell<u64>,
    waiting: Rc<RefCell<HashMap<u64, async_channel::Sender<Option<String>>>>>,
}

impl<A> FrontendChannel<A> {
    /// Attaches through `attach`, which hands the sink to the frontend's
    /// view.
    pub(crate) fn attach(
        attach: impl FnOnce(Box<dyn Fn(Option<&[u8]>)>) -> Option<A>,
    ) -> Option<Self> {
        let waiting: Rc<RefCell<HashMap<u64, async_channel::Sender<Option<String>>>>> =
            Rc::default();
        let answers = waiting.clone();
        let attachment = attach(Box::new(move |message| {
            let Some(message) = message else {
                // Gone: nothing will answer.
                answers.borrow_mut().clear();
                return;
            };
            if let Some((id, value)) = reply(message) {
                let waiting = answers.borrow_mut().remove(&id);
                if let Some(waiting) = waiting {
                    let _ = waiting.try_send(value);
                }
            }
        }))?;
        Some(Self {
            _attachment: attachment,
            next: Cell::new(FIRST_ID),
            waiting,
        })
    }

    /// Runs `script` through `send`; its string answer (`None`: none, or it
    /// failed) comes on the receiver.
    pub(crate) fn evaluate(
        &self,
        script: &str,
        send: impl FnOnce(&[u8]) -> bool,
    ) -> async_channel::Receiver<Option<String>> {
        let (tx, rx) = async_channel::bounded(1);
        let id = self.next.get();
        self.next.set(id + 1);
        let message = serde_json::json!({
            "id": id,
            "method": "Runtime.evaluate",
            "params": {
                "expression": script,
                "awaitPromise": true,
                "returnByValue": true,
            },
        });
        self.waiting.borrow_mut().insert(id, tx.clone());
        if !send(message.to_string().as_bytes()) {
            self.waiting.borrow_mut().remove(&id);
            let _ = tx.try_send(None);
        }
        rx
    }
}

/// A result on the frontend's channel: its id and string value.
fn reply(message: &[u8]) -> Option<(u64, Option<String>)> {
    let value: serde_json::Value = serde_json::from_slice(message).ok()?;
    let id = value.get("id")?.as_u64()?;
    let answer = value["result"]["result"]["value"]
        .as_str()
        .map(str::to_owned);
    Some((id, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shown_panel_decides_what_a_command_does_and_inspecting_never_closes() {
        use DevToolsCommand as C;
        for selected in [
            None,
            Some(Panel::Elements),
            Some(Panel::Console),
            Some(Panel::Network),
        ] {
            assert_eq!(
                operation(C::Toggle, false, selected),
                Operation::Open(Panel::Elements, false)
            );
            assert_eq!(operation(C::Toggle, true, selected), Operation::Close);
            assert_eq!(
                operation(C::Elements, true, selected),
                Operation::Open(Panel::Elements, true)
            );
            assert_eq!(
                operation(C::Elements, false, selected),
                Operation::Open(Panel::Elements, true)
            );
            for (command, panel) in [(C::Console, Panel::Console), (C::Network, Panel::Network)] {
                assert_eq!(
                    operation(command, false, selected),
                    Operation::Open(panel, false)
                );
                assert_eq!(
                    operation(command, true, selected),
                    if selected == Some(panel) {
                        Operation::Close
                    } else {
                        Operation::Open(panel, false)
                    }
                );
            }
        }
    }

    #[test]
    fn open_devtools_ask_their_frontend_what_operation_would_decide() {
        use DevToolsCommand as C;
        for command in [C::Toggle, C::Elements, C::Console, C::Network] {
            for selected in [Panel::Elements, Panel::Console, Panel::Network] {
                let decided = operation(command, true, Some(selected));
                let asked = match on_open(command) {
                    None => Operation::Close,
                    Some((panel, _, true)) if panel == selected => Operation::Close,
                    Some((panel, inspect, _)) => Operation::Open(panel, inspect),
                };
                assert_eq!(asked, decided, "{command:?} on {selected:?}");
            }
        }
    }

    #[test]
    fn devtools_take_a_third_and_keep_clear_of_both_minimums() {
        // Unset: a third of the body, the page keeping two.
        assert_eq!(devtools_extent(0, 1200.), 400.);
        assert_eq!(devtools_extent(0, 250.), DEVTOOLS_MIN);
        // Dragged: as wanted, within both minimums.
        assert_eq!(devtools_extent(300, 1000.), 300.);
        assert_eq!(devtools_extent(10, 1000.), DEVTOOLS_MIN);
        assert_eq!(devtools_extent(990, 1000.), 1000. - PAGE_MIN);
        // Too small for both: the page keeps its minimum.
        assert_eq!(devtools_extent(300, 150.), 150. - PAGE_MIN);
        assert_eq!(devtools_extent(300, 50.), 0.);
        // Not measured yet.
        assert_eq!(devtools_extent(300, 0.), 300.);
        assert_eq!(devtools_extent(0, 0.), DEVTOOLS_MIN);
    }

    #[test]
    fn dragging_the_divider_away_from_the_devtools_grows_them() {
        // Right: the divider goes left.
        assert_eq!(dragged_extent(DockSide::Right, (800., 300.), 700.), 400.);
        // Left: it goes right.
        assert_eq!(dragged_extent(DockSide::Left, (300., 300.), 400.), 400.);
        // Bottom: it goes up.
        assert_eq!(dragged_extent(DockSide::Bottom, (500., 200.), 450.), 250.);
        assert_eq!(dragged_extent(DockSide::Bottom, (500., 200.), 900.), 1.);
        assert!(DockSide::Right.beside() && DockSide::Left.beside());
        assert!(!DockSide::Bottom.beside() && !DockSide::Undocked.beside());
    }

    #[test]
    fn the_next_devtools_open_where_the_last_were_put() {
        assert_eq!(last_side(), DockSide::Right);
        set_last_side(DockSide::Bottom);
        assert_eq!(last_side(), DockSide::Bottom);
        set_last_side(DockSide::Right);
    }

    #[test]
    fn the_frontend_loads_on_the_apps_endpoint_at_the_panel_asked() {
        assert_eq!(
            devtools_frontend_url("127.0.0.1:5000/abc", "F00D", Panel::Console),
            "devtools://devtools/bundled/devtools_app.html?ws=127.0.0.1:5000/abc/devtools/page/F00D&panel=console"
        );
        assert_eq!(Panel::from_id("network"), Some(Panel::Network));
        assert_eq!(Panel::from_id("sources"), None);
    }

    #[test]
    fn view_source_is_for_web_file_and_data_pages() {
        assert_eq!(
            view_source_url("https://a.test/x?y").as_deref(),
            Some("view-source:https://a.test/x?y")
        );
        assert_eq!(
            view_source_url("HTTP://a.test").as_deref(),
            Some("view-source:HTTP://a.test")
        );
        assert!(view_source_url("file:///tmp/a.html").is_some());
        assert!(view_source_url("data:text/html,hi").is_some());
        for url in [
            "",
            "about:blank",
            "view-source:https://a.test",
            "chrome://version",
            "devtools://x",
        ] {
            assert_eq!(view_source_url(url), None, "{url}");
        }
    }

    #[test]
    fn the_command_script_names_its_panel_and_what_it_may_do() {
        let script = command_script(Panel::Console, false, true);
        assert!(
            script.ends_with(r#"})("console", false, true)"#),
            "{script}"
        );
        assert!(script.contains("api.showPanel(panel)"));
        assert!(script.contains("api.enterInspectElementMode()"));
        let inspect = command_script(Panel::Elements, true, false);
        assert!(inspect.ends_with(r#"})("elements", true, false)"#));
        assert_eq!(Answer::parse(Some("close")), Answer::Close);
        assert_eq!(Answer::parse(Some("shown")), Answer::Shown);
        assert_eq!(Answer::parse(Some("unready")), Answer::Unready);
        assert_eq!(Answer::parse(None), Answer::Unready);
    }

    #[test]
    fn the_frontend_channel_hands_each_answer_to_its_script() {
        type Sink = Box<dyn Fn(Option<&[u8]>)>;
        let sink: Rc<RefCell<Option<Sink>>> = Rc::default();
        let kept = sink.clone();
        let channel = FrontendChannel::attach(|s| {
            *kept.borrow_mut() = Some(s);
            Some(())
        })
        .unwrap();
        let sent: RefCell<Vec<serde_json::Value>> = RefCell::default();
        let send = |bytes: &[u8]| {
            sent.borrow_mut()
                .push(serde_json::from_slice(bytes).unwrap());
            true
        };
        let first = channel.evaluate("1", send);
        let second = channel.evaluate("2", |bytes: &[u8]| send(bytes));
        let sent = sent.into_inner();
        assert_eq!(sent[0]["id"], 1);
        assert_eq!(sent[0]["method"], "Runtime.evaluate");
        assert_eq!(sent[0]["params"]["expression"], "1");
        assert_eq!(sent[0]["params"]["awaitPromise"], true);
        assert_eq!(sent[1]["id"], 2);
        let deliver = |text: &str| (sink.borrow().as_ref().unwrap())(Some(text.as_bytes()));
        // Events and others' results are not ours.
        deliver(r#"{"method":"Runtime.consoleAPICalled","params":{}}"#);
        deliver(r#"{"id":9,"result":{}}"#);
        deliver(r#"{"id":2,"result":{"result":{"type":"string","value":"close"}}}"#);
        assert_eq!(second.try_recv(), Ok(Some("close".into())));
        assert!(first.try_recv().is_err());
        deliver(r#"{"id":1,"error":{"message":"no"}}"#);
        assert_eq!(first.try_recv(), Ok(None));
        // Unsent: answered at once.
        let refused = channel.evaluate("3", |_| false);
        assert_eq!(refused.try_recv(), Ok(None));
        // Detached: the waiting go unanswered.
        let pending = channel.evaluate("4", |_| true);
        (sink.borrow().as_ref().unwrap())(None);
        assert!(pending.try_recv().is_err());
        assert!(pending.is_closed());
    }
}
