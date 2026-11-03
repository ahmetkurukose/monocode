//! The Git branches popup, after IntelliJ's: a search field, the Git actions,
//! then Recent, Local and Remote branches as folding trees. A branch's `›`
//! (or a right click) opens its own actions: checkout, branch from it, merge,
//! rebase, rename, delete. The list is virtualized, and rows are rebuilt only
//! when the data, the search or a fold changes, so hundreds of branches stay
//! light.
use crate::{input, popover, theme::*};
use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, MouseButton,
    Role, SharedString, Subscription, Window, div, prelude::*, px, rgb, uniform_list,
};
use monocode_core::git::{self, Branch, BranchInfo, BranchRow};
use std::{collections::HashSet, path::PathBuf};

const ROW: f32 = 28.;
const RESULT_CAP: usize = 300;
const BLUE: u32 = 0x60a5fa;

pub(crate) enum PopupEvent {
    Close,
    /// A branch was created, switched, renamed or deleted.
    Changed(String),
    Merge(String),
    Rebase(String),
    Update,
    Commit,
    Push,
    Fetch,
}

#[derive(Clone, PartialEq, Eq)]
struct Target {
    name: String,
    remote: Option<String>,
}
impl Target {
    fn of(row: &BranchRow) -> Self {
        Self {
            name: row.name.clone(),
            remote: row.remote.clone(),
        }
    }
    /// `origin/feature` for a remote branch, `feature` for a local one.
    fn full(&self) -> String {
        match &self.remote {
            Some(remote) => format!("{remote}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

enum Mode {
    List,
    /// One branch's actions.
    Branch(Target),
    /// The search field is a name field: create from `base`, or rename `old`.
    Create {
        base: Option<String>,
    },
    Rename {
        old: String,
    },
    /// Waiting for a yes: delete this branch, `force`d when unmerged.
    Delete {
        name: String,
        force: bool,
    },
}

#[derive(Clone)]
enum Row {
    Action {
        id: &'static str,
        label: &'static str,
        icon: &'static str,
        keys: &'static str,
    },
    Rule,
    Section {
        key: &'static str,
        title: &'static str,
        open: bool,
    },
    Folder {
        key: String,
        name: String,
        depth: usize,
        open: bool,
    },
    Branch {
        ix: usize,
        depth: usize,
        label: String,
    },
    Note(String),
}

#[derive(Default)]
struct Node {
    dirs: Vec<(String, Node)>,
    leaves: Vec<usize>,
}
impl Node {
    fn insert(&mut self, parts: &[&str], ix: usize) {
        match parts {
            [] => {}
            [_] => self.leaves.push(ix),
            [dir, rest @ ..] => {
                let at = match self.dirs.iter().position(|(name, _)| name == dir) {
                    Some(at) => at,
                    None => {
                        self.dirs.push((dir.to_string(), Node::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[at].1.insert(rest, ix);
            }
        }
    }
    fn flatten(
        &mut self,
        branches: &[BranchRow],
        scope: &str,
        depth: usize,
        expanded: &HashSet<String>,
        out: &mut Vec<Row>,
    ) {
        self.dirs.sort_by_key(|d| d.0.to_lowercase());
        self.leaves
            .sort_by_key(|&ix| branches[ix].name.to_lowercase());
        for (name, dir) in &mut self.dirs {
            let key = format!("{scope}/{name}");
            let open = expanded.contains(&key);
            out.push(Row::Folder {
                key: key.clone(),
                name: name.clone(),
                depth,
                open,
            });
            if open {
                dir.flatten(branches, &key, depth + 1, expanded, out);
            }
        }
        for &ix in &self.leaves {
            let name = &branches[ix].name;
            out.push(Row::Branch {
                ix,
                depth,
                label: name.rsplit('/').next().unwrap_or(name).to_owned(),
            });
        }
    }
}

pub(crate) struct BranchPopup {
    cwd: PathBuf,
    info: Option<BranchInfo>,
    search: Entity<input::Input>,
    expanded: HashSet<String>,
    rows: Vec<Row>,
    mode: Mode,
    active: usize,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<PopupEvent> for BranchPopup {}
impl Focusable for BranchPopup {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

const ACTIONS: [(&str, &str, &str, &str); 4] = [
    ("update", "Update Project", "refresh-cw", "⌥⌘U"),
    ("commit", "Commit…", "git-commit", "⌥⌘K"),
    ("push", "Push…", "cloud-upload", ""),
    ("fetch", "Fetch", "arrow-down-circle", ""),
];

impl BranchPopup {
    pub(crate) fn new(cwd: PathBuf, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            input::Input::field(
                "Search for branches and actions",
                "Search for branches and actions",
                content(0.4),
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, _: &input::Changed, cx| {
                this.active = 0;
                this.rebuild(cx);
            }),
            cx.subscribe(&search, |this, _, _: &input::Submitted, cx| this.submit(cx)),
        ];
        let load = cwd.clone();
        let work = cx
            .background_executor()
            .spawn(async move { git::branch_info(&load) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(info) => this.info = Some(info),
                    Err(error) => this.error = Some(error),
                }
                this.rebuild(cx);
            });
        })
        .detach();
        let mut this = Self {
            cwd,
            info: None,
            search,
            expanded: ["sec:recent", "sec:local"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            rows: vec![],
            mode: Mode::List,
            active: 0,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.rebuild(cx);
        this
    }
    pub(crate) fn focus(&self, cx: &gpui::App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    fn query(&self, cx: &gpui::App) -> String {
        self.search.read(cx).content.trim().to_lowercase()
    }
    fn current(&self) -> Option<&str> {
        self.info.as_ref().and_then(|i| i.current.as_deref())
    }

    /// Rows for the list mode: actions, then the trees — or, while searching,
    /// matching actions and branches flat.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::List) {
            cx.notify();
            return;
        }
        let query = self.query(cx);
        let mut rows = Vec::new();
        let actions = ACTIONS
            .iter()
            .filter(|(_, label, _, _)| query.is_empty() || label.to_lowercase().contains(&query));
        for &(id, label, icon, keys) in actions {
            rows.push(Row::Action {
                id,
                label,
                icon,
                keys,
            });
        }
        if query.is_empty() {
            rows.push(Row::Action {
                id: "new",
                label: "New Branch…",
                icon: "plus",
                keys: "",
            });
        }
        let Some(info) = &self.info else {
            rows.push(Row::Note(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Loading branches…".into()),
            ));
            self.rows = rows;
            cx.notify();
            return;
        };
        if !rows.is_empty() {
            rows.push(Row::Rule);
        }
        if query.is_empty() {
            let open = self.expanded.contains("sec:recent");
            let recent: Vec<usize> = info
                .recent
                .iter()
                .filter_map(|name| {
                    info.branches
                        .iter()
                        .position(|b| b.remote.is_none() && &b.name == name)
                })
                .collect();
            if !recent.is_empty() {
                rows.push(Row::Section {
                    key: "sec:recent",
                    title: "Recent",
                    open,
                });
                if open {
                    rows.extend(recent.into_iter().map(|ix| Row::Branch {
                        ix,
                        depth: 0,
                        label: info.branches[ix].name.clone(),
                    }));
                }
            }
            let open = self.expanded.contains("sec:local");
            rows.push(Row::Section {
                key: "sec:local",
                title: "Local",
                open,
            });
            if open {
                let mut tree = Node::default();
                for (ix, branch) in info.branches.iter().enumerate() {
                    if branch.remote.is_none() {
                        tree.insert(&branch.name.split('/').collect::<Vec<_>>(), ix);
                    }
                }
                tree.flatten(&info.branches, "local", 0, &self.expanded, &mut rows);
            }
            let open = self.expanded.contains("sec:remote");
            rows.push(Row::Section {
                key: "sec:remote",
                title: "Remote",
                open,
            });
            if open {
                let mut remotes: Vec<&str> = info
                    .branches
                    .iter()
                    .filter_map(|b| b.remote.as_deref())
                    .collect();
                remotes.sort_unstable();
                remotes.dedup();
                for remote in remotes {
                    let key = format!("remote/{remote}");
                    let open = self.expanded.contains(&key);
                    rows.push(Row::Folder {
                        key: key.clone(),
                        name: remote.to_owned(),
                        depth: 0,
                        open,
                    });
                    if open {
                        let mut tree = Node::default();
                        for (ix, branch) in info.branches.iter().enumerate() {
                            if branch.remote.as_deref() == Some(remote) {
                                tree.insert(&branch.name.split('/').collect::<Vec<_>>(), ix);
                            }
                        }
                        tree.flatten(&info.branches, &key, 1, &self.expanded, &mut rows);
                    }
                }
            }
        } else {
            let mut shown = 0;
            for local in [true, false] {
                for (ix, branch) in info.branches.iter().enumerate() {
                    if branch.remote.is_none() != local || shown == RESULT_CAP {
                        continue;
                    }
                    let full = Target::of(branch).full().to_lowercase();
                    if full.contains(&query) {
                        rows.push(Row::Branch {
                            ix,
                            depth: 0,
                            label: Target::of(branch).full(),
                        });
                        shown += 1;
                    }
                }
            }
            if shown == 0 && rows.is_empty() {
                rows.push(Row::Note("No matches".into()));
            }
        }
        self.rows = rows;
        self.active = self.active.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_owned());
        }
        self.rebuild(cx);
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible_len();
        self.active = self
            .active
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        cx.notify();
    }
    fn visible_len(&self) -> usize {
        match &self.mode {
            Mode::List => self.rows.len(),
            Mode::Branch(target) => self.branch_actions(target).len(),
            _ => 0,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        match &self.mode {
            Mode::List => self.activate_row(self.active, cx),
            Mode::Branch(target) => {
                let actions = self.branch_actions(target);
                if let Some(&(id, _, true)) = actions
                    .get(self.active)
                    .map(|(id, _, on)| (id, (), on))
                    .as_ref()
                {
                    self.run_branch_action(id, cx);
                }
            }
            Mode::Create { .. } | Mode::Rename { .. } => self.submit_name(cx),
            Mode::Delete { .. } => self.confirm_delete(cx),
        }
    }
    fn activate_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        match row {
            Row::Action { id, .. } => match id {
                "update" => cx.emit(PopupEvent::Update),
                "commit" => cx.emit(PopupEvent::Commit),
                "push" => cx.emit(PopupEvent::Push),
                "fetch" => cx.emit(PopupEvent::Fetch),
                "new" => self.begin_name(Mode::Create { base: None }, "", cx),
                _ => {}
            },
            Row::Section { key, .. } => self.toggle(key, cx),
            Row::Folder { key, .. } => self.toggle(&key, cx),
            Row::Branch { ix, .. } => {
                if let Some(target) = self.info.as_ref().and_then(|i| i.branches.get(ix)) {
                    self.checkout(Target::of(target), cx);
                }
            }
            Row::Rule | Row::Note(_) => {}
        }
    }

    fn begin_name(&mut self, mode: Mode, text: &str, cx: &mut Context<Self>) {
        let (label, placeholder) = match &mode {
            Mode::Create { base: Some(base) } => (
                "New branch name".to_owned(),
                format!("New branch from ‘{base}’ — name, then Enter"),
            ),
            Mode::Create { base: None } => (
                "New branch name".to_owned(),
                "New branch name, then Enter".to_owned(),
            ),
            Mode::Rename { old } => ("Branch name".to_owned(), format!("Rename ‘{old}’ to…")),
            _ => unreachable!(),
        };
        self.mode = mode;
        self.error = None;
        self.search.update(cx, |input, cx| {
            input.set_text(text.to_owned(), cx);
            input.set_placeholder(placeholder.clone(), cx);
        });
        let _ = label;
        cx.notify();
    }
    fn back_to_list(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::List;
        self.error = None;
        self.active = 0;
        self.search.update(cx, |input, cx| {
            input.set_text(String::new(), cx);
            input.set_placeholder("Search for branches and actions".to_owned(), cx);
        });
        self.rebuild(cx);
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::List) {
            cx.emit(PopupEvent::Close);
        } else {
            self.back_to_list(cx);
        }
    }

    fn run(
        &mut self,
        done: &'static str,
        work: impl FnOnce(PathBuf) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let cwd = self.cwd.clone();
        let task = cx.background_executor().spawn(async move { work(cwd) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(PopupEvent::Changed(done.to_owned())),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn checkout(&mut self, target: Target, cx: &mut Context<Self>) {
        if target.remote.is_none() && self.current() == Some(target.name.as_str()) {
            cx.emit(PopupEvent::Close);
            return;
        }
        let branch = Branch {
            name: target.name,
            remote: target.remote,
        };
        self.run(
            "Switched branch",
            move |cwd| git::checkout(&cwd, &branch),
            cx,
        );
    }
    fn submit_name(&mut self, cx: &mut Context<Self>) {
        let name = self.search.read(cx).content.trim().to_owned();
        if name.is_empty() {
            return;
        }
        match &self.mode {
            Mode::Create { base } => {
                let base = base.clone();
                self.run(
                    "Created branch",
                    move |cwd| git::create_branch_from(&cwd, &name, base.as_deref()),
                    cx,
                );
            }
            Mode::Rename { old } => {
                let old = old.clone();
                self.run(
                    "Renamed branch",
                    move |cwd| git::rename_branch(&cwd, &old, &name),
                    cx,
                );
            }
            _ => {}
        }
    }

    /// (id, label, enabled) for one branch's actions, after IntelliJ's menu.
    fn branch_actions(&self, target: &Target) -> Vec<(&'static str, String, bool)> {
        let full = target.full();
        let current = self.current().map(str::to_owned);
        let is_current = target.remote.is_none() && current.as_deref() == Some(&target.name);
        let mut out = vec![
            ("checkout", "Checkout".to_owned(), !is_current),
            ("branch", format!("New Branch from ‘{full}’…"), true),
        ];
        if let Some(current) = current.filter(|_| !is_current) {
            out.push(("rebase", format!("Rebase ‘{current}’ onto ‘{full}’"), true));
            out.push(("merge", format!("Merge ‘{full}’ into ‘{current}’"), true));
        }
        if is_current {
            out.push(("update", "Update Project".to_owned(), true));
            out.push(("push", "Push…".to_owned(), true));
        }
        if target.remote.is_none() {
            out.push(("rename", "Rename…".to_owned(), true));
            out.push(("delete", "Delete".to_owned(), !is_current));
        }
        out
    }
    fn run_branch_action(&mut self, id: &str, cx: &mut Context<Self>) {
        let Mode::Branch(target) = &self.mode else {
            return;
        };
        let target = target.clone();
        let full = target.full();
        match id {
            "checkout" => self.checkout(target, cx),
            "branch" => self.begin_name(Mode::Create { base: Some(full) }, "", cx),
            "rebase" => cx.emit(PopupEvent::Rebase(full)),
            "merge" => cx.emit(PopupEvent::Merge(full)),
            "update" => cx.emit(PopupEvent::Update),
            "push" => cx.emit(PopupEvent::Push),
            "rename" => {
                let old = target.name.clone();
                self.begin_name(Mode::Rename { old: old.clone() }, &old, cx)
            }
            "delete" => self.delete(target.name, cx),
            _ => {}
        }
    }
    fn delete(&mut self, name: String, cx: &mut Context<Self>) {
        self.mode = Mode::Delete { name, force: false };
        self.error = None;
        cx.notify();
    }
    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Mode::Delete { name, force } = &self.mode else {
            return;
        };
        let (name, force) = (name.clone(), *force);
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let cwd = self.cwd.clone();
        let target = name.clone();
        let task = cx
            .background_executor()
            .spawn(async move { git::delete_branch(&cwd, &target, force) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(PopupEvent::Changed("Deleted branch".into())),
                    Err(error) if !force && error.contains("not fully merged") => {
                        this.mode = Mode::Delete { name, force: true };
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_branch(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.info.as_ref().and_then(|i| i.branches.get(ix)) else {
            return;
        };
        self.mode = Mode::Branch(Target::of(row));
        self.error = None;
        self.active = 0;
        cx.notify();
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().h(px(ROW)).into_any_element();
        };
        let active = ix == self.active;
        let base = div()
            .id(("branch-row", ix))
            .h(px(ROW))
            .mx(px(6.))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(13.))
            .cursor_pointer()
            .text_color(content(0.9))
            .when(active, |d| d.bg(content(0.09)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.active != ix {
                    this.active = ix;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.activate_row(ix, cx)));
        match row {
            Row::Action {
                label,
                icon: name,
                keys,
                ..
            } => base
                .child(icon(name, 14.).text_color(content(0.6)))
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(content(0.35))
                        .child(keys),
                )
                .into_any_element(),
            Row::Rule => div()
                .h(px(9.))
                .flex()
                .items_center()
                .child(div().mx(px(14.)).h(px(1.)).w_full().bg(content(0.08)))
                .into_any_element(),
            Row::Section { title, open, .. } => base
                .text_color(content(0.75))
                .font_weight(FontWeight::MEDIUM)
                .child(
                    icon(
                        if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        12.,
                    )
                    .text_color(content(0.5)),
                )
                .child(title)
                .into_any_element(),
            Row::Folder {
                name, depth, open, ..
            } => base
                .pl(px(8. + 14. + depth as f32 * 16.))
                .child(
                    icon(
                        if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        12.,
                    )
                    .text_color(content(0.5)),
                )
                .child(crate::file_icons::file_type_icon(&name, true, open, 14.))
                .child(div().min_w_0().truncate().child(name))
                .into_any_element(),
            Row::Note(text) => div()
                .px(px(14.))
                .h(px(ROW))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(content(0.45))
                .child(text)
                .into_any_element(),
            Row::Branch {
                ix: bix,
                depth,
                label,
            } => {
                let Some(info) = &self.info else {
                    return base.into_any_element();
                };
                let Some(branch) = info.branches.get(bix) else {
                    return base.into_any_element();
                };
                let is_current = branch.remote.is_none()
                    && info.current.as_deref() == Some(branch.name.as_str());
                let is_default = branch.remote.is_none()
                    && info.default_branch.as_deref() == Some(branch.name.as_str());
                let upstream = branch.upstream.clone();
                let behind = branch.behind;
                let ahead = branch.ahead;
                base.pl(px(8. + 14. + depth as f32 * 16.))
                    .child(if is_default {
                        icon("star-filled", 14.).text_color(rgb(0xfbbf24))
                    } else {
                        icon("git-branch", 14.).text_color(content(0.55))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(is_current, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(label),
                    )
                    .when(behind > 0, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(rgb(BLUE))
                                .child(format!("↙{behind}")),
                        )
                    })
                    .when(ahead > 0, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(rgb(0x4ade80))
                                .child(format!("↗{ahead}")),
                        )
                    })
                    .child(div().flex_1())
                    .when_some(upstream, |d, upstream| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(content(0.4))
                                .child(upstream),
                        )
                    })
                    .when(is_current, |d| d.child(icon("check-2", 14.)))
                    .child(
                        div()
                            .id(("branch-more", ix))
                            .role(Role::Button)
                            .aria_label("Branch actions")
                            .size(px(20.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|s| s.bg(content(0.12)))
                            .child(icon("chevron-right", 12.).text_color(content(0.5)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.open_branch(bix, cx);
                            })),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.open_branch(bix, cx);
                        }),
                    )
                    .into_any_element()
            }
        }
    }

    fn branch_menu(&self, target: &Target, cx: &mut Context<Self>) -> AnyElement {
        let actions = self.branch_actions(target);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("branch-menu-back")
                    .role(Role::Button)
                    .h(px(34.))
                    .mx(px(6.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .text_color(content(0.7))
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.07)))
                    .child(icon("chevron-left", 12.).text_color(content(0.5)))
                    .child(icon("git-branch", 14.).text_color(content(0.55)))
                    .child(div().min_w_0().truncate().child(target.full()))
                    .on_click(cx.listener(|this, _, _, cx| this.back_to_list(cx))),
            )
            .child(div().mx(px(14.)).h(px(1.)).bg(content(0.08)))
            .children(
                actions
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (id, label, enabled))| {
                        let destructive = id == "delete";
                        div()
                            .id(("branch-action", ix))
                            .h(px(ROW))
                            .mx(px(6.))
                            .mt(px(if ix == 0 { 4. } else { 0. }))
                            .px(px(10.))
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .text_size(px(13.))
                            .when(self.active == ix && enabled, |d| d.bg(content(0.09)))
                            .text_color(if !enabled {
                                content(0.3)
                            } else if destructive {
                                rgb(0xf87171).into()
                            } else {
                                content(0.9)
                            })
                            .when(enabled, |d| {
                                d.cursor_pointer()
                                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                        if *hovered && this.active != ix {
                                            this.active = ix;
                                            cx.notify();
                                        }
                                    }))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.run_branch_action(id, cx)
                                    }))
                            })
                            .child(label)
                    }),
            )
            .into_any_element()
    }
}

impl gpui::Render for BranchPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match &self.mode {
            Mode::List => {
                let entity = cx.entity();
                uniform_list("branch-rows", self.rows.len(), move |range, _, cx| {
                    entity.update(cx, |this, cx| {
                        range.map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
                    })
                })
                .py(px(4.))
                .flex_1()
                .min_h_0()
                .into_any_element()
            }
            Mode::Branch(target) => {
                let target = target.clone();
                self.branch_menu(&target, cx)
            }
            Mode::Delete { name, force } => {
                let message = if *force {
                    format!("\"{name}\" is not fully merged. Delete it anyway?")
                } else {
                    format!("Delete branch \"{name}\"?")
                };
                div()
                    .flex_1()
                    .p(px(14.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(div().text_size(px(13.)).child(message))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                crate::modal::danger_button("branch-delete-yes", "Delete")
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.confirm_delete(cx)),
                                    ),
                            )
                            .child(
                                crate::modal::ghost_button("branch-delete-no", "Cancel")
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.back_to_list(cx))),
                            ),
                    )
                    .into_any_element()
            }
            Mode::Create { .. } | Mode::Rename { .. } => div()
                .flex_1()
                .p(px(14.))
                .text_size(px(12.))
                .text_color(content(0.5))
                .child("Type a name and press Enter. Esc goes back.")
                .into_any_element(),
        };
        div()
            .id("branch-popup")
            .role(Role::Dialog)
            .aria_label("Git branches")
            .key_context("GitPicker")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_action(cx.listener(|this, _: &input::Up, _, cx| {
                this.step(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Down, _, cx| {
                this.step(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &input::Submit, _, cx| {
                this.submit(cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &popover::Cancel, _, cx| {
                this.cancel(cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(44.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(content(0.07))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon("search", 14.).text_color(content(0.4)))
                    .child(div().flex_1().min_w_0().child(self.search.clone())),
            )
            .child(body)
            .when(self.busy, |d| {
                d.child(
                    div()
                        .px(px(14.))
                        .py(px(6.))
                        .text_size(px(11.))
                        .text_color(content(0.5))
                        .child("Working…"),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .id("branch-popup-error")
                        .max_h(px(96.))
                        .overflow_y_scroll()
                        .px(px(14.))
                        .py(px(8.))
                        .text_size(px(11.))
                        .text_color(rgb(0xf87171))
                        .child(SharedString::from(error)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, remote: Option<&str>) -> BranchRow {
        BranchRow {
            name: name.into(),
            remote: remote.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn local_branches_fold_into_folders_with_folders_first() {
        let branches = vec![
            row("master", None),
            row("AK/FIX/gated", None),
            row("AK/FIX/other", None),
            row("AK/top", None),
            row("Chatbot", None),
        ];
        let mut tree = Node::default();
        for (ix, branch) in branches.iter().enumerate() {
            tree.insert(&branch.name.split('/').collect::<Vec<_>>(), ix);
        }
        let expanded: HashSet<String> = ["local/AK".to_string()].into();
        let mut rows = vec![];
        tree.flatten(&branches, "local", 0, &expanded, &mut rows);
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                Row::Folder {
                    name, depth, open, ..
                } => format!("dir {depth} {name} {open}"),
                Row::Branch { depth, label, .. } => format!("branch {depth} {label}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "dir 0 AK true",
                "dir 1 FIX false",
                "branch 1 top",
                "branch 0 Chatbot",
                "branch 0 master"
            ]
        );
    }

    #[test]
    fn a_remote_target_names_itself_with_its_remote() {
        let target = Target::of(&row("AK/x", Some("origin")));
        assert_eq!(target.full(), "origin/AK/x");
        assert_eq!(Target::of(&row("main", None)).full(), "main");
    }
}
