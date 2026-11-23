//! Reviewers, assignees and labels of a pull request: three rows of chips,
//! each with a pencil that opens a searchable multi-select. The New Pull
//! Request dialog uses it; the Inbox reuses it to edit an open pull request.
//!
//! The host owns loading: it hands [`PrMetadataPicker::set_candidates`] the
//! state of `scm::pr_candidates`, listens for [`PrMetadataEvent::Changed`],
//! and reloads on [`PrMetadataEvent::Retry`].
//!
//! [`SearchMenu`] (a menu under a search field) is shared with the dialog's
//! branch pickers.
use crate::{
    input::{self, Input},
    popover::{Anchor, AnchorSlot, Menu, MenuAction, MenuEvent, MenuItem, Popover, menu_popover},
    right_panel::icon_action,
    theme::*,
};
use gpui::{
    App, Context, Entity, EventEmitter, Focusable, FontWeight, SharedString, Subscription, Window,
    div, prelude::*, px, rgb,
};
use monocode_core::{
    inbox::Label,
    scm::{PrCandidates, PrMetadata},
};
use std::rc::Rc;

/// The rows of one [`SearchMenu`] for a search text.
pub(crate) type BuildItems = Rc<dyn Fn(&str) -> Vec<MenuItem>>;

struct SearchHeader {
    input: Entity<Input>,
}
impl Render for SearchHeader {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(36.))
            .mx(px(-4.))
            .mt(px(-4.))
            .mb(px(4.))
            .px(px(12.))
            .border_b_1()
            .border_color(content(0.07))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon("search", 14.).text_color(content(0.4)))
            .child(div().flex_1().min_w_0().child(self.input.clone()))
    }
}

/// A menu under a search field: typing rebuilds the rows, the arrows and
/// Return move through them, and keep-open rows toggle in place.
pub(crate) struct SearchMenu {
    header: Entity<SearchHeader>,
    input: Entity<Input>,
    menu: Entity<Menu>,
    build: BuildItems,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<MenuEvent> for SearchMenu {}

impl SearchMenu {
    pub(crate) fn new(
        label: &str,
        placeholder: &str,
        width: f32,
        build: BuildItems,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input =
            cx.new(|cx| Input::field(label.to_owned(), placeholder.to_owned(), content(0.4), cx));
        let header = cx.new(|_| SearchHeader {
            input: input.clone(),
        });
        let items = Self::items_for(&header, &input, &build, cx);
        let menu = cx.new(|cx| {
            Menu::new(label.to_owned(), items, window, cx)
                .width(width)
                .search_navigation()
        });
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, _: &input::Changed, cx| {
                this.refresh(true, cx)
            }),
            cx.subscribe(&menu, |_, _, event: &MenuEvent, cx| {
                cx.emit(match event {
                    MenuEvent::Picked(id, bounds) => MenuEvent::Picked(id.clone(), *bounds),
                    MenuEvent::Dismissed => MenuEvent::Dismissed,
                })
            }),
        ];
        Self {
            header,
            input,
            menu,
            build,
            _subscriptions: subscriptions,
        }
    }
    fn items_for(
        header: &Entity<SearchHeader>,
        input: &Entity<Input>,
        build: &BuildItems,
        cx: &App,
    ) -> Vec<MenuItem> {
        let query = input.read(cx).content.trim().to_owned();
        let mut items = vec![MenuItem::Custom(
            header.clone().into(),
            input.read(cx).focus_handle(cx),
        )];
        items.extend(build(&query));
        items
    }
    /// New rows (the data changed); the highlight stays where it was.
    pub(crate) fn set_build(&mut self, build: BuildItems, cx: &mut Context<Self>) {
        self.build = build;
        self.refresh(false, cx);
    }
    /// Rebuilds the rows. A new search starts the highlight at the top.
    pub(crate) fn refresh(&mut self, from_top: bool, cx: &mut Context<Self>) {
        let items = Self::items_for(&self.header, &self.input, &self.build, cx);
        self.menu.update(cx, |menu, cx| {
            if from_top {
                menu.set_items(items, cx)
            } else {
                menu.replace_items(items, cx)
            }
        });
        cx.notify();
    }
    #[cfg(test)]
    pub(crate) fn query(&self, cx: &App) -> String {
        self.input.read(cx).content.trim().to_owned()
    }
    #[cfg(test)]
    pub(crate) fn rows(&self, cx: &App) -> Vec<MenuItem> {
        (self.build)(&self.query(cx))
    }
    pub(crate) fn popover(&self, anchor: Anchor, cx: &App) -> Popover {
        menu_popover(&self.menu, anchor, cx)
            .gap(4.)
            .max_height(380.)
    }
}

/// How far loading the pickers' choices got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CandidateState {
    Loading,
    Failed(String),
    Ready(PrCandidates),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Reviewers,
    Assignees,
    Labels,
}
impl Field {
    pub(crate) const ALL: [Field; 3] = [Field::Reviewers, Field::Assignees, Field::Labels];
    pub(crate) fn title(self) -> &'static str {
        match self {
            Field::Reviewers => "Reviewers",
            Field::Assignees => "Assignees",
            Field::Labels => "Labels",
        }
    }
    fn empty(self) -> &'static str {
        match self {
            Field::Reviewers => "No reviewers",
            Field::Assignees => "Unassigned",
            Field::Labels => "No labels",
        }
    }
    fn search(self) -> &'static str {
        match self {
            Field::Reviewers => "Search users and teams…",
            Field::Assignees => "Search users…",
            Field::Labels => "Search labels…",
        }
    }
    fn index(self) -> usize {
        self as usize
    }
}

/// One row of a picker: `key` is `user:login`, `team:org/slug` or
/// `label:name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickOption {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) detail: Option<String>,
    pub(crate) checked: bool,
    pub(crate) color: Option<u32>,
}

/// `#rrggbb` (or `rrggbb`) as a colour.
pub(crate) fn label_color(color: &str) -> Option<u32> {
    let hex = color.trim().trim_start_matches('#');
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

fn matches(query: &str, texts: &[&str]) -> bool {
    let needle = query.trim().to_lowercase();
    needle.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&needle))
}

/// The choices of `field` for `query`, chosen ones first. Reviewers leave
/// out the viewer (GitHub refuses a self-review) and list teams as
/// `org/slug`. Chosen values the candidates do not know stay listed so they
/// can be removed.
pub(crate) fn options(
    field: Field,
    candidates: &PrCandidates,
    viewer: &str,
    metadata: &PrMetadata,
    query: &str,
) -> Vec<PickOption> {
    let mut out: Vec<PickOption> = Vec::new();
    let user = |login: &str, name: &str, chosen: &[String]| PickOption {
        key: format!("user:{login}"),
        label: login.to_owned(),
        detail: (!name.is_empty() && name != login).then(|| name.to_owned()),
        checked: chosen.iter().any(|c| c == login),
        color: None,
    };
    match field {
        Field::Reviewers | Field::Assignees => {
            let chosen = if field == Field::Reviewers {
                &metadata.reviewers
            } else {
                &metadata.assignees
            };
            for candidate in &candidates.users {
                if field == Field::Reviewers && candidate.login.eq_ignore_ascii_case(viewer) {
                    continue;
                }
                if matches(query, &[&candidate.login, &candidate.name]) {
                    out.push(user(&candidate.login, &candidate.name, chosen));
                }
            }
            for login in chosen {
                if !candidates.users.iter().any(|u| &u.login == login) && matches(query, &[login]) {
                    out.push(user(login, "", chosen));
                }
            }
            if field == Field::Reviewers {
                for team in &candidates.teams {
                    if matches(query, &[&team.slug, &team.name]) {
                        out.push(PickOption {
                            key: format!("team:{}", team.slug),
                            label: team.slug.clone(),
                            detail: (!team.name.is_empty()).then(|| team.name.clone()),
                            checked: metadata.team_reviewers.contains(&team.slug),
                            color: None,
                        });
                    }
                }
                for slug in &metadata.team_reviewers {
                    if !candidates.teams.iter().any(|t| &t.slug == slug) && matches(query, &[slug])
                    {
                        out.push(PickOption {
                            key: format!("team:{slug}"),
                            label: slug.clone(),
                            detail: None,
                            checked: true,
                            color: None,
                        });
                    }
                }
            }
        }
        Field::Labels => {
            for label in &candidates.labels {
                if matches(query, &[&label.name]) {
                    out.push(PickOption {
                        key: format!("label:{}", label.name),
                        label: label.name.clone(),
                        detail: None,
                        checked: metadata.labels.contains(&label.name),
                        color: label_color(&label.color),
                    });
                }
            }
            for name in &metadata.labels {
                if !candidates.labels.iter().any(|l| &l.name == name) && matches(query, &[name]) {
                    out.push(PickOption {
                        key: format!("label:{name}"),
                        label: name.clone(),
                        detail: None,
                        checked: true,
                        color: None,
                    });
                }
            }
        }
    }
    // Chosen first, each half in the candidates' order.
    out.sort_by_key(|o| !o.checked);
    out
}

fn flip(list: &mut Vec<String>, value: &str) {
    if let Some(at) = list.iter().position(|v| v == value) {
        list.remove(at);
    } else {
        list.push(value.to_owned());
    }
}

/// Adds or removes the value behind an option `key` of `field`. Returns
/// whether the key meant anything there.
pub(crate) fn toggle(metadata: &mut PrMetadata, field: Field, key: &str) -> bool {
    match (field, key.split_once(':')) {
        (Field::Reviewers, Some(("user", login))) => flip(&mut metadata.reviewers, login),
        (Field::Reviewers, Some(("team", slug))) => flip(&mut metadata.team_reviewers, slug),
        (Field::Assignees, Some(("user", login))) => flip(&mut metadata.assignees, login),
        (Field::Labels, Some(("label", name))) => flip(&mut metadata.labels, name),
        _ => return false,
    }
    true
}

pub(crate) enum PrMetadataEvent {
    /// The user added or removed a value; the new set.
    #[allow(
        dead_code,
        reason = "the Inbox saves it; the dialog reads the picker on Create"
    )]
    Changed(PrMetadata),
    /// The choices failed to load and the user asked to try again.
    Retry,
}

struct OpenPicker {
    field: Field,
    menu: Entity<SearchMenu>,
    _events: Subscription,
}

/// The three rows, and the picker of whichever is being edited.
pub(crate) struct PrMetadataPicker {
    metadata: PrMetadata,
    candidates: CandidateState,
    viewer: String,
    /// The Inbox provider id behind the avatars (`github`).
    provider: &'static str,
    enabled: bool,
    anchors: [AnchorSlot; 3],
    open: Option<OpenPicker>,
}
impl EventEmitter<PrMetadataEvent> for PrMetadataPicker {}

impl PrMetadataPicker {
    /// `viewer` is the signed-in login: never offered as a reviewer.
    pub(crate) fn new(viewer: String, metadata: PrMetadata) -> Self {
        Self {
            metadata,
            candidates: CandidateState::Loading,
            viewer,
            provider: "github",
            enabled: true,
            anchors: Default::default(),
            open: None,
        }
    }
    pub(crate) fn metadata(&self) -> &PrMetadata {
        &self.metadata
    }
    #[allow(dead_code, reason = "for the Inbox's metadata editor")]
    pub(crate) fn candidates(&self) -> &CandidateState {
        &self.candidates
    }
    pub(crate) fn set_viewer(&mut self, viewer: String, cx: &mut Context<Self>) {
        self.viewer = viewer;
        self.refresh_open(cx);
    }
    /// Replaces the chosen values without emitting `Changed`.
    #[allow(dead_code, reason = "for the Inbox's metadata editor")]
    pub(crate) fn set_metadata(&mut self, metadata: PrMetadata, cx: &mut Context<Self>) {
        self.metadata = metadata;
        self.refresh_open(cx);
        cx.notify();
    }
    pub(crate) fn set_candidates(&mut self, candidates: CandidateState, cx: &mut Context<Self>) {
        self.candidates = candidates;
        self.refresh_open(cx);
        cx.notify();
    }
    /// Off while the host is busy: the pencils grey out and an open picker closes.
    pub(crate) fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled {
                self.open = None;
            }
            cx.notify();
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn build(&self, field: Field) -> BuildItems {
        let candidates = self.candidates.clone();
        let viewer = self.viewer.clone();
        let metadata = self.metadata.clone();
        Rc::new(move |query: &str| match &candidates {
            CandidateState::Loading => vec![MenuItem::Note("Loading…".into())],
            CandidateState::Failed(error) => vec![
                MenuItem::Note(
                    format!("Couldn’t load {}: {error}", field.title().to_lowercase()).into(),
                ),
                MenuAction::new("retry", "Retry")
                    .icon("refresh-cw")
                    .keep_open()
                    .into(),
            ],
            CandidateState::Ready(candidates) => {
                let options = options(field, candidates, &viewer, &metadata, query);
                if options.is_empty() {
                    return vec![MenuItem::Note(
                        if query.is_empty() {
                            "Nothing to choose from"
                        } else {
                            "No matches"
                        }
                        .into(),
                    )];
                }
                options
                    .into_iter()
                    .map(|option| {
                        let action = MenuAction::new(option.key.clone(), option.label)
                            .checked(option.checked)
                            .keep_open();
                        let action = match (option.color, option.key.starts_with("team:")) {
                            (Some(color), _) => action.icon("dot").icon_color(rgb(color).into()),
                            (None, true) => action.icon("message-multiple"),
                            (None, false) if field == Field::Labels => action.icon("dot"),
                            (None, false) => action.icon("account-user"),
                        };
                        match option.detail {
                            Some(detail) => action.value(detail),
                            None => action,
                        }
                        .into()
                    })
                    .collect()
            }
        })
    }
    fn refresh_open(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = &self.open {
            let build = self.build(open.field);
            open.menu.update(cx, |menu, cx| menu.set_build(build, cx));
        }
    }

    pub(crate) fn toggle_picker(
        &mut self,
        field: Field,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open.take().is_some_and(|open| open.field == field) || !self.enabled {
            cx.notify();
            return;
        }
        let build = self.build(field);
        let menu =
            cx.new(|cx| SearchMenu::new(field.title(), field.search(), 280., build, window, cx));
        let events = cx.subscribe(&menu, move |this, _, event: &MenuEvent, cx| match event {
            MenuEvent::Picked(id, _) if id.as_ref() == "retry" => {
                this.candidates = CandidateState::Loading;
                this.refresh_open(cx);
                cx.emit(PrMetadataEvent::Retry);
            }
            MenuEvent::Picked(id, _) => {
                if toggle(&mut this.metadata, field, id) {
                    this.refresh_open(cx);
                    cx.emit(PrMetadataEvent::Changed(this.metadata.clone()));
                    cx.notify();
                }
            }
            MenuEvent::Dismissed => {
                this.open = None;
                cx.notify();
            }
        });
        self.open = Some(OpenPicker {
            field,
            menu,
            _events: events,
        });
        cx.notify();
    }

    fn avatar_url(&self, login: &str) -> String {
        match &self.candidates {
            CandidateState::Ready(c) => c
                .users
                .iter()
                .find(|u| u.login == login)
                .map(|u| u.avatar_url.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
    fn label(&self, name: &str) -> Label {
        match &self.candidates {
            CandidateState::Ready(c) => c.labels.iter().find(|l| l.name == name).cloned(),
            _ => None,
        }
        .unwrap_or_else(|| Label {
            name: name.to_owned(),
            color: String::new(),
        })
    }

    fn person_chip(&self, login: &str, window: &mut Window, cx: &mut App) -> gpui::AnyElement {
        let url = self.avatar_url(login);
        div()
            .flex_shrink_0()
            .h(px(22.))
            .pl(px(3.))
            .pr(px(8.))
            .rounded_full()
            .bg(content(0.08))
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(12.))
            .text_color(content(0.85))
            .child(crate::inbox::avatar_image(
                self.provider,
                login,
                &url,
                16.,
                window,
                cx,
            ))
            .child(login.to_owned())
            .into_any_element()
    }
    fn team_chip(slug: &str) -> gpui::AnyElement {
        div()
            .flex_shrink_0()
            .h(px(22.))
            .px(px(8.))
            .rounded_full()
            .bg(content(0.08))
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(12.))
            .text_color(content(0.85))
            .child(icon("message-multiple", 12.).text_color(content(0.5)))
            .child(slug.to_owned())
            .into_any_element()
    }

    fn row(&self, field: Field, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let chips: Vec<gpui::AnyElement> = match field {
            Field::Reviewers => {
                let mut chips: Vec<_> = self
                    .metadata
                    .reviewers
                    .iter()
                    .map(|login| self.person_chip(login, window, cx))
                    .collect();
                chips.extend(
                    self.metadata
                        .team_reviewers
                        .iter()
                        .map(|s| Self::team_chip(s)),
                );
                chips
            }
            Field::Assignees => self
                .metadata
                .assignees
                .iter()
                .map(|login| self.person_chip(login, window, cx))
                .collect(),
            Field::Labels => self
                .metadata
                .labels
                .iter()
                .map(|name| {
                    crate::inbox::label_badge(&self.label(name), false)
                        .flex_shrink_0()
                        .into_any_element()
                })
                .collect(),
        };
        let open = self.open.as_ref().is_some_and(|o| o.field == field);
        let enabled = self.enabled;
        let id: SharedString = format!("pr-meta-{}", field.title().to_lowercase()).into();
        div()
            .debug_selector({
                let id = id.clone();
                move || id.to_string()
            })
            .min_h(px(28.))
            .flex()
            .items_start()
            .gap(px(8.))
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(content(0.55))
                    .child(field.title()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .when(chips.is_empty(), |d| {
                        d.child(
                            div()
                                .h(px(22.))
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(content(0.4))
                                .child(field.empty()),
                        )
                    })
                    .children(chips),
            )
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(self.anchors[field.index()].probe())
                    .child(
                        icon_action(
                            SharedString::from(format!("{id}-edit")),
                            format!("Edit {}", field.title().to_lowercase()),
                            "pencil",
                            enabled,
                        )
                        .debug_selector({
                            let id = id.clone();
                            move || format!("{id}-edit")
                        })
                        .when(open, |d| d.bg(content(0.1)))
                        .when(enabled, |d| {
                            d.cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.toggle_picker(field, window, cx)
                                }))
                        }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for PrMetadataPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = Field::ALL
            .into_iter()
            .map(|field| self.row(field, window, cx))
            .collect();
        let popover = self.open.as_ref().map(|open| {
            open.menu
                .read(cx)
                .popover(Anchor::Slot(self.anchors[open.field.index()].clone()), cx)
                .align(crate::popover::Align::End)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .children(rows)
            .children(popover)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::scm::{PrTeam, PrUser};

    fn candidates() -> PrCandidates {
        PrCandidates {
            users: vec![
                PrUser {
                    login: "me".into(),
                    name: "Me".into(),
                    avatar_url: String::new(),
                },
                PrUser {
                    login: "ada".into(),
                    name: "Ada Lovelace".into(),
                    avatar_url: String::new(),
                },
                PrUser {
                    login: "bob".into(),
                    name: String::new(),
                    avatar_url: String::new(),
                },
            ],
            teams: vec![PrTeam {
                slug: "acme/core".into(),
                name: "Core".into(),
            }],
            labels: vec![
                Label {
                    name: "bug".into(),
                    color: "d73a4a".into(),
                },
                Label {
                    name: "docs".into(),
                    color: "#0075ca".into(),
                },
            ],
        }
    }

    #[test]
    fn reviewers_leave_out_the_viewer_and_list_teams_by_slug() {
        let keys: Vec<_> = options(
            Field::Reviewers,
            &candidates(),
            "me",
            &PrMetadata::default(),
            "",
        )
        .into_iter()
        .map(|o| o.key)
        .collect();
        assert_eq!(keys, ["user:ada", "user:bob", "team:acme/core"]);
        // Assignees may be the viewer.
        let assignees = options(
            Field::Assignees,
            &candidates(),
            "me",
            &PrMetadata::default(),
            "",
        );
        assert_eq!(assignees.len(), 3);
    }

    #[test]
    fn search_matches_login_or_name_and_chosen_values_come_first() {
        let metadata = PrMetadata {
            reviewers: vec!["bob".into()],
            ..Default::default()
        };
        let found = options(Field::Reviewers, &candidates(), "me", &metadata, "love");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "ada");
        assert_eq!(found[0].detail.as_deref(), Some("Ada Lovelace"));
        let all = options(Field::Reviewers, &candidates(), "me", &metadata, "");
        assert_eq!(all[0].key, "user:bob");
        assert!(all[0].checked);
        // A chosen value the candidates lost stays, so it can be removed.
        let metadata = PrMetadata {
            labels: vec!["gone".into()],
            ..Default::default()
        };
        let labels = options(Field::Labels, &candidates(), "me", &metadata, "");
        assert_eq!(labels[0].key, "label:gone");
        assert_eq!(labels[1].color, Some(0xd73a4a));
        assert_eq!(labels[2].color, Some(0x0075ca));
    }

    #[test]
    fn toggling_adds_then_removes_in_the_right_list() {
        let mut metadata = PrMetadata::default();
        assert!(toggle(&mut metadata, Field::Reviewers, "user:ada"));
        assert!(toggle(&mut metadata, Field::Reviewers, "team:acme/core"));
        assert!(toggle(&mut metadata, Field::Assignees, "user:ada"));
        assert!(toggle(&mut metadata, Field::Labels, "label:bug"));
        assert_eq!(metadata.reviewers, ["ada"]);
        assert_eq!(metadata.team_reviewers, ["acme/core"]);
        assert_eq!(metadata.assignees, ["ada"]);
        assert_eq!(metadata.labels, ["bug"]);
        assert!(toggle(&mut metadata, Field::Reviewers, "user:ada"));
        assert!(metadata.reviewers.is_empty());
        assert!(!toggle(&mut metadata, Field::Labels, "user:ada"));
    }

    #[gpui::test]
    fn picking_toggles_and_reports_and_a_failed_load_offers_retry(cx: &mut gpui::TestAppContext) {
        cx.update(input::bind);
        cx.update(crate::popover::bind);
        let window =
            cx.add_window(|_, _| PrMetadataPicker::new("me".into(), PrMetadata::default()));
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let retries = std::rc::Rc::new(std::cell::Cell::new(0));
        let picker = window.root(cx).unwrap();
        cx.update(|cx| {
            let (changes, retries) = (changes.clone(), retries.clone());
            cx.subscribe(&picker, move |_, event: &PrMetadataEvent, _| match event {
                PrMetadataEvent::Changed(m) => changes.borrow_mut().push(m.clone()),
                PrMetadataEvent::Retry => retries.set(retries.get() + 1),
            })
            .detach();
        });
        window
            .update(cx, |picker, window, cx| {
                picker.set_candidates(CandidateState::Failed("offline".into()), cx);
                picker.toggle_picker(Field::Labels, window, cx);
                let menu = picker.open.as_ref().unwrap().menu.clone();
                let rows = menu.read(cx).rows(cx);
                assert!(matches!(&rows[1], MenuItem::Action(a) if a.id.as_ref() == "retry"));
                menu.update(cx, |_, cx| cx.emit(MenuEvent::Picked("retry".into(), None)));
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(retries.get(), 1);
        window
            .update(cx, |picker, _, cx| {
                assert_eq!(picker.candidates, CandidateState::Loading);
                picker.set_candidates(CandidateState::Ready(candidates()), cx);
                let menu = picker.open.as_ref().unwrap().menu.clone();
                menu.update(cx, |_, cx| {
                    cx.emit(MenuEvent::Picked("label:bug".into(), None))
                });
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(changes.borrow().len(), 1);
        assert_eq!(changes.borrow()[0].labels, ["bug"]);
        window
            .update(cx, |picker, _, cx| {
                // The open picker shows the new state at once.
                let menu = picker.open.as_ref().unwrap().menu.clone();
                let rows = menu.read(cx).rows(cx);
                assert!(matches!(&rows[0], MenuItem::Action(a) if a.checked));
            })
            .unwrap();
    }
}
