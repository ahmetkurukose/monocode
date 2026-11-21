//! The accessibility snapshot (context.md K31–K33, K42), a Rust port of
//! Orca's in-house engine (`cdp/snapshots/*`): CDP accessibility trees in,
//! indented text with `@eN` refs out.
//!
//! Pure: [`render`] works on parsed trees, so the tests feed it fixture JSON.
//! The CDP side (fetching trees, the DOM sweep) is in `observe.rs`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Interactive roles: each gets a ref.
const INTERACTIVE: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "option",
    "treeitem",
    "listbox",
];

const LANDMARKS: &[&str] = &[
    "banner",
    "navigation",
    "main",
    "complementary",
    "contentinfo",
    "region",
    "form",
    "search",
    "dialog",
    "alertdialog",
];

/// Roles that say nothing themselves: their children are walked.
const SKIPPED: &[&str] = &["none", "presentation", "generic"];

/// Role text values carry Chromium's internal names for static text.
fn is_text(role: &str) -> bool {
    role == "StaticText" || role == "staticText"
}

/// How much a snapshot shows (K32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Interactive elements, headings and landmarks.
    Interactive,
    /// Also text and images.
    Full,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AxNode {
    pub id: String,
    pub backend: Option<i64>,
    pub role: String,
    pub name: String,
    pub ignored: bool,
    pub value: Option<String>,
    pub properties: Vec<(String, Value)>,
    pub children: Vec<String>,
}

/// One frame's `Accessibility.getFullAXTree` answer.
#[derive(Clone, Debug, Default)]
pub struct AxTree {
    pub nodes: Vec<AxNode>,
    index: HashMap<String, usize>,
}

fn text_of(value: &Value) -> String {
    match value.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

impl AxTree {
    /// Parses `{"nodes": [...]}`.
    pub fn parse(result: &Value) -> AxTree {
        let nodes: Vec<AxNode> = result
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .map(|n| AxNode {
                        id: match &n["nodeId"] {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        },
                        backend: n.get("backendDOMNodeId").and_then(Value::as_i64),
                        role: n.get("role").map(text_of).unwrap_or_default(),
                        name: n.get("name").map(text_of).unwrap_or_default(),
                        ignored: n.get("ignored").and_then(Value::as_bool).unwrap_or(false),
                        value: n.get("value").map(text_of).filter(|v| !v.is_empty()),
                        properties: n
                            .get("properties")
                            .and_then(Value::as_array)
                            .map(|ps| {
                                ps.iter()
                                    .filter_map(|p| {
                                        Some((
                                            p.get("name")?.as_str()?.to_owned(),
                                            p.get("value")
                                                .and_then(|v| v.get("value"))
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        children: n
                            .get("childIds")
                            .and_then(Value::as_array)
                            .map(|ids| {
                                ids.iter()
                                    .map(|id| match id {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let index = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect();
        AxTree { nodes, index }
    }

    pub fn get(&self, id: &str) -> Option<&AxNode> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    /// The node no other node lists as a child (the document).
    pub fn root(&self) -> Option<&AxNode> {
        let children: HashSet<&str> = self
            .nodes
            .iter()
            .flat_map(|n| n.children.iter().map(String::as_str))
            .collect();
        self.nodes
            .iter()
            .find(|n| !children.contains(n.id.as_str()))
            .or(self.nodes.first())
    }

    pub fn by_backend(&self, backend: i64) -> Option<&AxNode> {
        self.nodes.iter().find(|n| n.backend == Some(backend))
    }

    /// One-based place of `backend` among the nodes with this role and name,
    /// in the tree's order: what stale-ref recovery counts (K33).
    pub fn nth(&self, role: &str, name: &str, backend: i64) -> usize {
        self.same(role, name)
            .position(|b| b == backend)
            .map_or(1, |i| i + 1)
    }

    /// Backend ids of the nodes with this role and name, the `nth` first.
    pub fn candidates(&self, role: &str, name: &str, nth: usize) -> Vec<i64> {
        let mut all: Vec<i64> = self.same(role, name).collect();
        if nth >= 1 && nth <= all.len() {
            let first = all.remove(nth - 1);
            all.insert(0, first);
        }
        all
    }

    fn same<'a>(&'a self, role: &'a str, name: &'a str) -> impl Iterator<Item = i64> + 'a {
        self.nodes
            .iter()
            .filter(move |n| !n.ignored && n.role == role && n.name == name)
            .filter_map(|n| n.backend)
    }
}

/// A frame of the page and its tree (K42).
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub tree: AxTree,
    /// The CDP session of an out-of-process iframe; `None` for the page's own.
    pub session: Option<String>,
    /// The frame's id; `None` for the main frame.
    pub frame_id: Option<String>,
    /// The backend id of the `<iframe>` element in the page's session.
    pub owner: Option<i64>,
    pub url: String,
}

/// What a ref points at.
#[derive(Clone, Debug, PartialEq)]
pub struct RefEntry {
    pub backend: i64,
    pub session: Option<String>,
    pub frame_id: Option<String>,
    /// For an out-of-process frame: its `<iframe>` element, to offset points.
    pub owner: Option<i64>,
    /// The raw accessibility role and name, for recovery.
    pub role: String,
    pub name: String,
    pub nth: usize,
}

/// More refs than this and the table starts over (numbers keep growing).
const MAX_REFS: usize = 10_000;

/// A page's refs, bound to one document (K33). The same element keeps its
/// ref across snapshots of that document.
#[derive(Debug, Default)]
pub struct Refs {
    /// The main frame's loader id the refs belong to.
    pub loader: Option<String>,
    next: u64,
    entries: HashMap<String, RefEntry>,
    by_node: HashMap<(Option<String>, i64), String>,
}

impl Refs {
    /// Forgets every ref: a new document.
    pub fn reset(&mut self, loader: Option<String>) {
        self.loader = loader;
        self.next = 0;
        self.entries.clear();
        self.by_node.clear();
    }

    /// Takes `@e3` or `e3`.
    pub fn get(&self, r: &str) -> Option<&RefEntry> {
        self.key_of(r).and_then(|key| self.entries.get(&key))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Points `r` at a node found again after a re-render.
    pub fn moved(&mut self, r: &str, backend: i64) {
        let Some(key) = self.key_of(r) else { return };
        let entry = self.entries.get_mut(&key).expect("key_of found it");
        self.by_node.remove(&(entry.session.clone(), entry.backend));
        entry.backend = backend;
        self.by_node.insert((entry.session.clone(), backend), key);
    }

    fn key_of(&self, r: &str) -> Option<String> {
        let key = r.trim();
        let key = key.strip_prefix('@').unwrap_or(key);
        let key = format!("@{key}");
        self.entries.contains_key(&key).then_some(key)
    }

    fn assign(&mut self, entry: RefEntry) -> String {
        let node = (entry.session.clone(), entry.backend);
        if let Some(r) = self.by_node.get(&node) {
            let r = r.clone();
            self.entries.insert(r.clone(), entry);
            return r;
        }
        if self.entries.len() >= MAX_REFS {
            self.entries.clear();
            self.by_node.clear();
        }
        self.next += 1;
        let r = format!("@e{}", self.next);
        self.entries.insert(r.clone(), entry);
        self.by_node.insert(node, r.clone());
        r
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub mode: Mode,
    /// Walk only this element of the main frame (a `selector`).
    pub root: Option<i64>,
    /// Lines deeper than this are left out.
    pub max_depth: Option<usize>,
}

/// Nesting deeper than this is not walked (very deep trees).
const MAX_NESTING: usize = 400;
const MAX_NAME: usize = 200;
const MAX_TEXT: usize = 500;

/// The snapshot's lines. `clickables` are elements of the main frame the
/// DOM sweep found (backend id → text); refs go into `refs`. `None` when
/// `options.root` is not in the tree.
pub fn render(
    frames: &[Frame],
    clickables: &[(i64, String)],
    options: &Options,
    refs: &mut Refs,
) -> Option<Vec<String>> {
    let mut walker = Walker {
        frames,
        clickables: clickables.iter().cloned().collect(),
        options,
        refs,
        lines: Vec::new(),
        owners: frames
            .iter()
            .enumerate()
            .skip(1)
            .filter_map(|(i, f)| f.owner.map(|o| (o, i)))
            .collect(),
        walked_frames: HashSet::new(),
        emitted: HashSet::new(),
    };
    let main = frames.first()?;
    let start = match options.root {
        Some(backend) => main.tree.by_backend(backend)?,
        None => match main.tree.root() {
            Some(root) => root,
            None => return Some(Vec::new()),
        },
    };
    walker.walked_frames.insert(0);
    walker.walk(0, start, 0, 0);
    if options.root.is_none() {
        // Sweep finds the tree does not have, then frames whose element was
        // not reached.
        let main_backends: HashSet<i64> =
            main.tree.nodes.iter().filter_map(|n| n.backend).collect();
        for (backend, text) in clickables {
            if !main_backends.contains(backend) && !walker.emitted.contains(&(0, *backend)) {
                walker.clickable(0, *backend, text, 0);
            }
        }
        for i in 1..frames.len() {
            if !walker.walked_frames.contains(&i) {
                walker.frame(i, 0);
            }
        }
    }
    Some(walker.lines)
}

struct Walker<'a> {
    frames: &'a [Frame],
    clickables: HashMap<i64, String>,
    options: &'a Options,
    refs: &'a mut Refs,
    lines: Vec<String>,
    owners: HashMap<i64, usize>,
    walked_frames: HashSet<usize>,
    emitted: HashSet<(usize, i64)>,
}

impl Walker<'_> {
    fn shows(&self, depth: usize) -> bool {
        self.options.max_depth.is_none_or(|max| depth <= max)
    }

    fn line(&mut self, depth: usize, text: String) {
        if self.shows(depth) {
            self.lines.push(format!("{}{text}", "  ".repeat(depth)));
        }
    }

    fn children(&mut self, f: usize, node: &AxNode, depth: usize, nesting: usize) {
        let tree = &self.frames[f].tree;
        for id in &node.children {
            if let Some(child) = tree.get(id) {
                self.walk(f, child, depth, nesting + 1);
            }
        }
    }

    fn walk(&mut self, f: usize, node: &AxNode, depth: usize, nesting: usize) {
        if nesting > MAX_NESTING || !self.shows(depth) {
            return;
        }
        // An `<iframe>` whose document we fetched on its own.
        if self.frames[f].session.is_none() {
            if let Some(&child) = node.backend.and_then(|b| self.owners.get(&b)) {
                if self.walked_frames.insert(child) {
                    self.frame(child, depth);
                    return;
                }
            }
        }
        if node.ignored {
            return self.children(f, node, depth, nesting);
        }
        let role = node.role.as_str();
        let interactive = INTERACTIVE.contains(&role);
        if f == 0 && !interactive {
            if let Some(text) = node.backend.and_then(|b| self.clickables.get(&b)).cloned() {
                self.clickable(f, node.backend.unwrap_or_default(), &text, depth);
                return;
            }
        }
        if SKIPPED.contains(&role) {
            return self.children(f, node, depth, nesting);
        }
        if interactive {
            if let Some(backend) = node.backend {
                if focusable(node) || role == "option" || role == "treeitem" {
                    self.interactive(f, node, backend, depth);
                    return;
                }
            }
            return self.children(f, node, depth, nesting);
        }
        if LANDMARKS.contains(&role) {
            // An unnamed region or form is not a landmark (ARIA).
            if (role == "region" || role == "form") && node.name.is_empty() {
                return self.children(f, node, depth, nesting);
            }
            self.line(depth, landmark(role, &node.name));
            return self.children(f, node, depth + 1, nesting);
        }
        if role == "heading" && !node.name.is_empty() {
            let level = property(node, "level")
                .and_then(Value::as_i64)
                .map(|l| format!(" level={l}"))
                .unwrap_or_default();
            self.line(
                depth,
                format!("heading {}{level}", quoted(&node.name, MAX_NAME)),
            );
            return;
        }
        if self.options.mode == Mode::Full {
            if is_text(role) {
                let text = node.name.trim();
                if !text.is_empty() {
                    self.line(depth, format!("text {}", quoted(text, MAX_TEXT)));
                }
                return;
            }
            if (role == "image" || role == "img") && !node.name.is_empty() {
                self.line(depth, format!("img {}", quoted(&node.name, MAX_NAME)));
                return;
            }
        }
        self.children(f, node, depth, nesting)
    }

    fn frame(&mut self, f: usize, depth: usize) {
        self.walked_frames.insert(f);
        let frame = &self.frames[f];
        self.line(depth, format!("[iframe {}]", quoted(&frame.url, MAX_NAME)));
        if let Some(root) = frame.tree.root() {
            self.walk(f, root, depth + 1, 0);
        }
    }

    fn interactive(&mut self, f: usize, node: &AxNode, backend: i64, depth: usize) {
        if !self.shows(depth) || !self.emitted.insert((f, backend)) {
            return;
        }
        let frame = &self.frames[f];
        let r = self.refs.assign(RefEntry {
            backend,
            session: frame.session.clone(),
            frame_id: frame.frame_id.clone(),
            owner: frame.session.as_ref().and(frame.owner),
            role: node.role.clone(),
            name: node.name.clone(),
            nth: frame.tree.nth(&node.role, &node.name, backend),
        });
        let mut text = format!("[{r}] {}", node.role);
        if !node.name.is_empty() {
            text.push(' ');
            text.push_str(&quoted(&node.name, MAX_NAME));
        }
        text.push_str(&states(node));
        self.line(depth, text);
    }

    fn clickable(&mut self, f: usize, backend: i64, text: &str, depth: usize) {
        if !self.shows(depth) || !self.emitted.insert((f, backend)) {
            return;
        }
        let r = self.refs.assign(RefEntry {
            backend,
            session: None,
            frame_id: None,
            owner: None,
            role: "clickable".into(),
            name: text.to_owned(),
            nth: 1,
        });
        self.line(depth, format!("[{r}] clickable {}", quoted(text, MAX_NAME)));
    }
}

fn property<'a>(node: &'a AxNode, name: &str) -> Option<&'a Value> {
    node.properties
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
}

fn truthy(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
        || matches!(value, Some(Value::String(s)) if s == "true")
}

fn focusable(node: &AxNode) -> bool {
    !matches!(property(node, "focusable"), Some(Value::Bool(false)))
}

/// The state suffix: ` checked focused value="…"` (K31).
fn states(node: &AxNode) -> String {
    let mut out = String::new();
    match property(node, "checked") {
        Some(Value::String(s)) if s == "mixed" => out.push_str(" mixed"),
        v if truthy(v) => out.push_str(" checked"),
        _ => {}
    }
    match property(node, "pressed") {
        Some(Value::String(s)) if s == "mixed" => out.push_str(" mixed"),
        v if truthy(v) => out.push_str(" pressed"),
        _ => {}
    }
    if truthy(property(node, "selected")) {
        out.push_str(" selected");
    }
    match property(node, "expanded") {
        Some(Value::Bool(true)) => out.push_str(" expanded"),
        Some(Value::Bool(false)) => out.push_str(" collapsed"),
        _ => {}
    }
    if truthy(property(node, "disabled")) {
        out.push_str(" disabled");
    }
    if truthy(property(node, "focused")) {
        out.push_str(" focused");
    }
    if let Some(value) = &node.value {
        if value != &node.name {
            out.push_str(&format!(" value={}", quoted(value, MAX_NAME)));
        }
    }
    out
}

fn landmark(role: &str, name: &str) -> String {
    if !name.is_empty() {
        let kind = match role {
            "dialog" | "alertdialog" => "Dialog: ",
            _ => "",
        };
        return format!("[{kind}{}]", clean(name, MAX_NAME));
    }
    match role {
        "banner" => "[Header]",
        "navigation" => "[Navigation]",
        "main" => "[Main Content]",
        "complementary" => "[Sidebar]",
        "contentinfo" => "[Footer]",
        "search" => "[Search]",
        "dialog" | "alertdialog" => "[Dialog]",
        other => return format!("[{other}]"),
    }
    .to_owned()
}

/// One line, at most `max` characters.
fn clean(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max).collect();
    cut.push('…');
    cut
}

fn quoted(text: &str, max: usize) -> String {
    format!(
        "\"{}\"",
        clean(text, max).replace('\\', "\\\\").replace('"', "\\\"")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, role: &str, name: &str, backend: i64, children: &[&str]) -> Value {
        json!({
            "nodeId": id, "ignored": false, "backendDOMNodeId": backend,
            "role": {"type": "role", "value": role},
            "name": {"type": "computedString", "value": name},
            "childIds": children,
        })
    }

    /// A small realistic page: header with nav links, a main form, a
    /// generic wrapper, an ignored hidden button, static text.
    fn page() -> Value {
        let mut search = node("8", "textbox", "Search", 108, &[]);
        search["properties"] = json!([
            {"name": "focusable", "value": {"type": "booleanOrUndefined", "value": true}},
            {"name": "focused", "value": {"type": "booleanOrUndefined", "value": true}},
        ]);
        search["value"] = json!({"type": "string", "value": "rust"});
        let mut remember = node("10", "checkbox", "Remember me", 110, &[]);
        remember["properties"] = json!([
            {"name": "checked", "value": {"type": "tristate", "value": "true"}},
        ]);
        let mut save = node("11", "button", "Save", 111, &["12"]);
        save["properties"] = json!([
            {"name": "disabled", "value": {"type": "boolean", "value": true}},
        ]);
        let mut hidden = node("13", "button", "Hidden", 113, &[]);
        hidden["ignored"] = json!(true);
        let mut title = node("5", "heading", "Settings", 105, &["6"]);
        title["properties"] = json!([{"name": "level", "value": {"type": "integer", "value": 1}}]);
        json!({"nodes": [
            node("1", "RootWebArea", "Settings - Example", 1, &["2", "4"]),
            node("2", "banner", "", 102, &["3", "14"]),
            node("3", "link", "Home", 103, &[]),
            node("14", "link", "Home", 114, &[]),
            node("4", "main", "", 104, &["5", "7", "15"]),
            title,
            node("6", "StaticText", "Settings", 106, &[]),
            node("7", "generic", "", 107, &["8", "9", "10", "11", "13"]),
            search,
            node("9", "StaticText", "Pick   a\nname", 109, &[]),
            remember,
            save,
            node("12", "StaticText", "Save", 112, &[]),
            hidden,
            node("15", "generic", "", 115, &[]),
        ]})
    }

    fn opts(mode: Mode) -> Options {
        Options {
            mode,
            root: None,
            max_depth: None,
        }
    }

    fn main_frame(tree: Value) -> Vec<Frame> {
        vec![Frame {
            tree: AxTree::parse(&tree),
            ..Frame::default()
        }]
    }

    #[test]
    fn interactive_mode_lists_refs_landmarks_and_headings() {
        let mut refs = Refs::default();
        let lines = render(
            &main_frame(page()),
            &[],
            &opts(Mode::Interactive),
            &mut refs,
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![
                "[Header]",
                "  [@e1] link \"Home\"",
                "  [@e2] link \"Home\"",
                "[Main Content]",
                "  heading \"Settings\" level=1",
                "  [@e3] textbox \"Search\" focused value=\"rust\"",
                "  [@e4] checkbox \"Remember me\" checked",
                "  [@e5] button \"Save\" disabled",
            ]
        );
        let home2 = refs.get("@e2").unwrap();
        assert_eq!((home2.backend, home2.nth), (114, 2));
        assert_eq!(refs.get("e3").unwrap().role, "textbox");
        assert!(refs.get("@e9").is_none());
    }

    #[test]
    fn full_mode_adds_text_and_clickables_keep_their_place() {
        let mut refs = Refs::default();
        let clickables = vec![(115, "Open menu".to_owned()), (999, "Floating".to_owned())];
        let lines = render(
            &main_frame(page()),
            &clickables,
            &opts(Mode::Full),
            &mut refs,
        )
        .unwrap();
        assert!(lines.contains(&"  text \"Pick a name\"".to_owned()));
        let menu = lines
            .iter()
            .position(|l| l.contains("clickable \"Open menu\""))
            .unwrap();
        let save = lines.iter().position(|l| l.contains("\"Save\"")).unwrap();
        assert!(menu > save, "the sweep's find sits where it is in the tree");
        assert_eq!(lines.last().unwrap(), "[@e7] clickable \"Floating\"");
        // The button's own text is not repeated.
        assert_eq!(lines.iter().filter(|l| l.contains("Save")).count(), 1);
    }

    #[test]
    fn refs_stay_for_the_same_element_and_reset_with_the_document() {
        let mut refs = Refs::default();
        refs.reset(Some("L1".into()));
        render(
            &main_frame(page()),
            &[],
            &opts(Mode::Interactive),
            &mut refs,
        );
        let again = render(
            &main_frame(page()),
            &[],
            &opts(Mode::Interactive),
            &mut refs,
        )
        .unwrap();
        assert!(again.contains(&"  [@e5] button \"Save\" disabled".to_owned()));
        refs.reset(Some("L2".into()));
        assert!(refs.is_empty());
    }

    #[test]
    fn selector_root_and_depth_narrow_it() {
        let mut refs = Refs::default();
        let scoped = Options {
            root: Some(107),
            ..opts(Mode::Interactive)
        };
        let lines = render(&main_frame(page()), &[], &scoped, &mut refs).unwrap();
        assert_eq!(lines[0], "[@e1] textbox \"Search\" focused value=\"rust\"");
        assert_eq!(lines.len(), 3);
        let missing = Options {
            root: Some(4242),
            ..opts(Mode::Interactive)
        };
        assert!(render(&main_frame(page()), &[], &missing, &mut refs).is_none());

        let shallow = Options {
            max_depth: Some(0),
            ..opts(Mode::Interactive)
        };
        let lines = render(&main_frame(page()), &[], &shallow, &mut Refs::default()).unwrap();
        assert_eq!(lines, vec!["[Header]", "[Main Content]"]);
    }

    #[test]
    fn iframes_render_where_their_element_is() {
        let main = json!({"nodes": [
            node("1", "RootWebArea", "Shop", 1, &["2", "3"]),
            node("2", "Iframe", "", 50, &[]),
            node("3", "button", "Buy", 51, &[]),
        ]});
        let pay = json!({"nodes": [
            node("a", "RootWebArea", "Pay", 1, &["b"]),
            node("b", "textbox", "Card number", 7, &[]),
        ]});
        let frames = vec![
            Frame {
                tree: AxTree::parse(&main),
                ..Frame::default()
            },
            Frame {
                tree: AxTree::parse(&pay),
                session: Some("S1".into()),
                frame_id: Some("F2".into()),
                owner: Some(50),
                url: "https://pay.example/frame".into(),
            },
        ];
        let mut refs = Refs::default();
        let lines = render(&frames, &[], &opts(Mode::Interactive), &mut refs).unwrap();
        assert_eq!(
            lines,
            vec![
                "[iframe \"https://pay.example/frame\"]",
                "  [@e1] textbox \"Card number\"",
                "[@e2] button \"Buy\"",
            ]
        );
        let card = refs.get("@e1").unwrap();
        assert_eq!(card.session.as_deref(), Some("S1"));
        assert_eq!(card.owner, Some(50));
        // Same backend id in another session is another element.
        assert_eq!(refs.get("@e2").unwrap().session, None);
    }

    #[test]
    fn recovery_candidates_put_the_nth_first() {
        let tree = AxTree::parse(&page());
        assert_eq!(tree.candidates("link", "Home", 2), vec![114, 103]);
        assert_eq!(tree.candidates("link", "Home", 5), vec![103, 114]);
        assert!(tree.candidates("link", "Away", 1).is_empty());
    }

    #[test]
    fn names_are_one_line_and_escaped() {
        assert_eq!(quoted("say \"hi\"\n now", 50), "\"say \\\"hi\\\" now\"");
        assert_eq!(clean(&"x".repeat(10), 4), "xxxx…");
    }
}
