//! A monday.com note: one monday board, its items in columns by status or
//! group. Cards drag onto a kanban note in the list, or go there from their
//! menu. The note's body keeps only the board and how it is shown.
use crate::{
    input::{self, Input},
    popover::{Anchor, popover},
    theme::*,
};
use gpui::{
    AnyElement, ClickEvent, Context, CursorStyle, Entity, EventEmitter, Focusable, FontWeight,
    Hsla, Pixels, Point, Role, SharedString, Subscription, Task, Window, div, prelude::*, px, rgb,
};
use monocode_core::{
    inbox::{OptionRow, monday as service},
    integrations::{BoardSnapshot, Cache, Provider, WorkItem},
    monday_note::{self, GroupBy, MondayNote},
};
use std::path::PathBuf;

// One event at a time, read and dropped at once: boxing the large
// variant would cost more than its size does.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Event {
    /// The note's settings changed and want saving.
    Changed,
    /// Put the item on the kanban note with this id.
    AddToKanban { kanban: String, item: WorkItem },
    /// Open Settings → Integrations to connect monday.com.
    OpenIntegrations,
}

/// What follows the pointer while a monday card is dragged.
#[derive(Clone)]
pub(crate) struct DraggedWorkItem {
    pub(crate) item: WorkItem,
}

impl gpui::Render for DraggedWorkItem {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(240.))
            .px_3()
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.16))
            .bg(pane_fill())
            .shadow_lg()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .text_color(content(1.))
            .child(icon("inbox-monday", 12.).text_color(content(0.7)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.item.title.clone()),
            )
    }
}

/// Whether a monday token is saved. Tests never reach the credential store.
fn connected() -> Result<bool, String> {
    #[cfg(test)]
    {
        Ok(false)
    }
    #[cfg(not(test))]
    {
        monocode_core::integrations::token(Provider::Monday).map(|token| token.is_some())
    }
}

/// What a background check brought back.
struct Fetched {
    connected: Result<bool, String>,
    board: Option<Result<BoardSnapshot, String>>,
    boards: Option<Result<Vec<OptionRow>, String>>,
}

pub(crate) struct MondayBoard {
    note: MondayNote,
    data_dir: Option<PathBuf>,
    snapshot: Option<BoardSnapshot>,
    /// The signed-in user's monday id, for "Assigned to me".
    account: Option<String>,
    connected: Option<bool>,
    loading: bool,
    error: Option<String>,
    boards: Option<Vec<OptionRow>>,
    boards_error: Option<String>,
    query: Entity<Input>,
    menu: Option<(String, Point<Pixels>)>,
    /// The kanban notes a card can go to: id and title.
    kanbans: Vec<(String, String)>,
    generation: u64,
    fetch: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for MondayBoard {}

impl MondayBoard {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut input = Input::field("Find board", "Find board…", content(0.4), cx);
            input.set_font_size(12.);
            input
        });
        let subscriptions = vec![cx.subscribe(&query, |_, _, _: &input::Changed, cx| cx.notify())];
        Self {
            note: MondayNote::default(),
            data_dir: None,
            snapshot: None,
            account: None,
            connected: None,
            loading: false,
            error: None,
            boards: None,
            boards_error: None,
            query,
            menu: None,
            kanbans: Vec::new(),
            generation: 0,
            fetch: None,
            _subscriptions: subscriptions,
        }
    }

    fn cache(&self) -> Option<Cache> {
        (self.data_dir.as_deref()).map(|dir| Cache::new(dir, Provider::Monday))
    }

    /// Shows the board a note names: what was last fetched at once, then
    /// whatever monday says when a token is saved.
    pub(crate) fn load(&mut self, body: &str, data_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        self.note = MondayNote::parse(body);
        self.data_dir = data_dir;
        self.menu = None;
        self.error = None;
        self.boards_error = None;
        self.connected = None;
        self.account = self.cache().and_then(|cache| cache.account()).map(|a| a.id);
        self.snapshot = self.cached_board();
        self.start(false, cx);
    }

    fn cached_board(&self) -> Option<BoardSnapshot> {
        let cache = self.cache()?;
        self.note
            .has_board()
            .then(|| cache.board(&self.note.board))
            .flatten()
    }

    /// The board search, which takes the keys while no board is chosen.
    pub(crate) fn search_focus(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.query.focus_handle(cx)
    }

    pub(crate) fn note(&self) -> &MondayNote {
        &self.note
    }

    pub(crate) fn set_kanbans(&mut self, kanbans: Vec<(String, String)>) {
        self.kanbans = kanbans;
    }

    /// The board as Markdown, once there is one to show.
    pub(crate) fn to_markdown(&self, title: &str) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        Some(monday_note::to_markdown(
            title,
            &self.note,
            snapshot,
            self.mine(),
        ))
    }

    /// Closes the card menu; false when it was not open.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.menu.take().is_some();
        cx.notify();
        open
    }

    /// The user "Assigned to me" keeps, when it is on. An unknown account
    /// matches nothing.
    fn mine(&self) -> Option<&str> {
        self.note
            .mine
            .then(|| self.account.as_deref().unwrap_or("\u{0}"))
    }

    /// Checks the token, then fetches the board, or the list of boards to
    /// pick from. `refresh` asks monday even when the cache is fresh.
    fn start(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(dir) = self.data_dir.clone() else {
            self.loading = false;
            cx.notify();
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let board = self.note.has_board().then(|| self.note.board.clone());
        self.loading = true;
        let work = cx.background_executor().spawn(async move {
            let connected = connected();
            if !matches!(connected, Ok(true)) {
                return Fetched {
                    connected,
                    board: None,
                    boards: None,
                };
            }
            match board {
                Some(board) => Fetched {
                    connected,
                    board: Some(service::monday_board(dir, &board, refresh)),
                    boards: None,
                },
                None => Fetched {
                    connected,
                    board: None,
                    boards: Some(service::monday_list_boards(dir, refresh)),
                },
            }
        });
        self.fetch = Some(cx.spawn(async move |this, cx| {
            let fetched = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match fetched.connected {
                    Ok(connected) => this.connected = Some(connected),
                    Err(error) => {
                        this.connected = Some(false);
                        this.error = Some(format!("Could not read the saved token: {error}"));
                    }
                }
                if let Some(result) = fetched.board {
                    match result {
                        Ok(snapshot) => {
                            this.error = None;
                            if this.note.board_name != snapshot.schema.name
                                && !snapshot.schema.name.is_empty()
                            {
                                this.note.board_name = snapshot.schema.name.clone();
                                cx.emit(Event::Changed);
                            }
                            this.snapshot = Some(snapshot);
                        }
                        Err(error) => this.error = Some(error),
                    }
                    // A fetch may have saved the account for the first time.
                    if let Some(account) = this.cache().and_then(|c| c.account()) {
                        this.account = Some(account.id);
                    }
                }
                if let Some(result) = fetched.boards {
                    match result {
                        Ok(boards) => {
                            this.boards = Some(boards);
                            this.boards_error = None;
                        }
                        Err(error) => this.boards_error = Some(error),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn choose_board(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        self.note.board = id;
        self.note.board_name = name;
        self.error = None;
        self.snapshot = self.cached_board();
        cx.emit(Event::Changed);
        self.start(false, cx);
    }

    fn change_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note.board.clear();
        self.note.board_name.clear();
        self.snapshot = None;
        self.error = None;
        self.query
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        window.focus(&self.query.focus_handle(cx), cx);
        cx.emit(Event::Changed);
        self.start(false, cx);
    }

    fn set_group_by(&mut self, group_by: GroupBy, cx: &mut Context<Self>) {
        if self.note.group_by != group_by {
            self.note.group_by = group_by;
            cx.emit(Event::Changed);
            cx.notify();
        }
    }

    fn toggle_mine(&mut self, cx: &mut Context<Self>) {
        self.note.mine = !self.note.mine;
        cx.emit(Event::Changed);
        cx.notify();
    }

    fn item(&self, id: &str) -> Option<&WorkItem> {
        self.snapshot.as_ref()?.items.iter().find(|i| i.id == id)
    }

    fn button(name: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(name)
            .debug_selector(move || name.into())
            .role(Role::Button)
            .aria_label(label)
            .h(px(28.))
            .px_3()
            .rounded_md()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
    }

    fn primary(name: &'static str, label: &'static str) -> gpui::Stateful<gpui::Div> {
        let mut ink = shell_fill();
        ink.a = 1.;
        Self::button(name, label)
            .bg(content(1.))
            .text_color(ink)
            .hover(move |s| s.bg(content(0.85)).text_color(ink))
            .child(label)
    }

    /// A centred message with an action, for states without a board to show.
    fn panel(&self, title: &str, body: &str, action: Option<AnyElement>) -> AnyElement {
        div()
            .debug_selector(|| "monday-panel".into())
            .flex_1()
            .min_h(px(240.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .text_center()
            .child(icon("inbox-monday", 28.).text_color(content(0.4)))
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_size(px(12.))
                    .text_color(content(0.55))
                    .child(body.to_owned()),
            )
            .children(action.map(|action| div().mt_2().child(action)))
            .into_any_element()
    }

    fn connect_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Self::primary("monday-connect", "Open Settings → Integrations")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::OpenIntegrations)))
            .into_any_element()
    }

    fn retry_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Self::button("monday-retry", "Retry")
            .text_color(content(0.8))
            .border_1()
            .border_color(content(0.15))
            .hover(|s| s.bg(content(0.08)))
            .child(icon("refresh-cw", 12.))
            .child("Retry")
            .on_click(cx.listener(|this, _, _, cx| this.start(true, cx)))
            .into_any_element()
    }

    fn not_connected(&self, cx: &mut Context<Self>) -> AnyElement {
        self.panel(
            "monday.com is not connected",
            "Add your monday.com API token in Settings → Integrations to show a board here.",
            Some(self.connect_button(cx)),
        )
    }

    fn picker(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.data_dir.is_none() {
            return self.panel(
                "monday.com boards are unavailable",
                "Monocode has no data folder to keep monday.com boards in.",
                None,
            );
        }
        if self.connected == Some(false) {
            return self.not_connected(cx);
        }
        let query = self.query.read(cx).content.to_lowercase();
        let boards: Vec<OptionRow> = (self.boards.iter().flatten())
            .filter(|board| {
                query.trim().is_empty()
                    || board.name.to_lowercase().contains(query.trim())
                    || board.key.to_lowercase().contains(query.trim())
            })
            .cloned()
            .collect();
        let status = if self.boards.is_none() && self.loading {
            Some("Loading boards…".to_owned())
        } else if let Some(error) = &self.boards_error {
            Some(error.clone())
        } else if self.boards.is_some() && boards.is_empty() {
            Some(if query.trim().is_empty() {
                "No boards in this monday.com account".to_owned()
            } else {
                "No matching boards".to_owned()
            })
        } else {
            None
        };
        div()
            .debug_selector(|| "monday-board-picker".into())
            .w_full()
            .max_w(px(560.))
            .mx_auto()
            .mt_4()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose a monday.com board"),
            )
            .child(
                div()
                    .h(px(34.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .border_1()
                    .border_color(content(0.12))
                    .bg(content(0.04))
                    .child(icon("search", 12.))
                    .child(div().flex_1().min_w_0().child(self.query.clone()))
                    .child(
                        icon_button("monday-boards-refresh", "Refresh boards", "refresh-cw")
                            .on_click(cx.listener(|this, _, _, cx| this.start(true, cx))),
                    ),
            )
            .when_some(status, |d, status| {
                d.child(
                    div()
                        .px_1()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .child(status)
                        .when(self.boards_error.is_some(), |d| {
                            d.child(self.retry_button(cx))
                        }),
                )
            })
            .child(
                div()
                    .id("monday-board-results")
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(boards.into_iter().map(|board| {
                        let id = board.id.clone();
                        let name = board.name.clone();
                        div()
                            .id(SharedString::from(format!("monday-board-{id}")))
                            .debug_selector({
                                let id = id.clone();
                                move || format!("monday-board-{id}")
                            })
                            .role(Role::Button)
                            .aria_label(board.name.clone())
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(content(0.08)))
                            .child(icon("kanban", 14.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .child(board.name.clone()),
                            )
                            .when(!board.key.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(11.))
                                        .text_color(content(0.4))
                                        .child(board.key.clone()),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_board(id.clone(), name.clone(), cx)
                            }))
                    })),
            )
            .into_any_element()
    }

    fn chip(text: String, color: Option<&str>) -> AnyElement {
        div()
            .max_w(px(140.))
            .h(px(18.))
            .px_1p5()
            .rounded_sm()
            .flex()
            .items_center()
            .gap_1()
            .bg(content(0.06))
            .text_size(px(10.))
            .text_color(content(0.65))
            .child(div().size(px(6.)).rounded_full().bg(swatch(color)))
            .child(div().truncate().child(text))
            .into_any_element()
    }

    fn card(&self, item: &WorkItem, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let names = monday_note::assignees(item);
        let chip = match self.note.group_by {
            GroupBy::Status => item
                .group
                .as_ref()
                .map(|g| Self::chip(g.title.clone(), g.color.as_deref())),
            GroupBy::Group => self
                .snapshot
                .as_ref()
                .and_then(monday_note::status_column)
                .and_then(|column| item.status(Some(column)))
                .filter(|cell| !cell.label.is_empty())
                .map(|cell| Self::chip(cell.label.clone(), cell.color.as_deref())),
        };
        div()
            .id(SharedString::from(format!("monday-card-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("monday-card-{id}")
            })
            .role(Role::ListItem)
            .aria_label(item.title.clone())
            .flex_shrink_0()
            .px_3()
            .py(px(10.))
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.08))
            .bg(content(0.05))
            .hover(|s| s.bg(content(0.08)))
            .cursor(CursorStyle::OpenHand)
            .flex()
            .flex_col()
            .gap_1p5()
            .on_drag(
                DraggedWorkItem { item: item.clone() },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.menu = Some((id.clone(), event.position()));
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .child(item.title.clone()),
            )
            .when(chip.is_some() || !names.is_empty(), |d| {
                d.child(div().flex().items_center().gap_1p5().children(chip).when(
                    !names.is_empty(),
                    |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(11.))
                                .text_color(content(0.5))
                                .child(icon("account-user", 11.))
                                .child(div().truncate().child(names.join(", "))),
                        )
                    },
                ))
            })
            .into_any_element()
    }

    fn lanes(&self, snapshot: &BoardSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let lanes = monday_note::lanes(snapshot, self.note.group_by, self.mine());
        let count = lanes.len().max(1) as f32;
        let columns: Vec<AnyElement> = lanes
            .iter()
            .enumerate()
            .map(|(ix, lane)| {
                let cards: Vec<AnyElement> =
                    lane.items.iter().map(|item| self.card(item, cx)).collect();
                let lane_id = lane.id.clone();
                div()
                    .id(("monday-lane", ix))
                    .debug_selector(move || format!("monday-lane-{lane_id}"))
                    .role(Role::List)
                    .aria_label(lane.title.clone())
                    .w(px(260.))
                    .flex_shrink_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(content(0.08))
                    .bg(content(0.03))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_h(px(24.))
                            .child(
                                div()
                                    .size(px(8.))
                                    .rounded_full()
                                    .bg(swatch(lane.color.as_deref())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(lane.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(content(0.45))
                                    .child(lane.items.len().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .id(("monday-lane-cards", ix))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(cards)
                            .when(lane.items.is_empty(), |d| {
                                d.child(
                                    div()
                                        .py(px(20.))
                                        .text_center()
                                        .text_size(px(12.))
                                        .text_color(content(0.35))
                                        .child("No items"),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        let empty = columns.is_empty();
        div()
            .id("monday-lanes")
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            .child(
                div()
                    .h_full()
                    .min_w(px(count * 260. + (count - 1.) * 14.))
                    .flex()
                    .gap(px(14.))
                    .children(columns)
                    .when(empty, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .py(px(28.))
                                .text_center()
                                .text_size(px(13.))
                                .text_color(content(0.45))
                                .child(if self.note.mine {
                                    "Nothing on this board is assigned to you"
                                } else {
                                    "This board has no items"
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn toolbar(&self, snapshot: Option<&BoardSnapshot>, cx: &mut Context<Self>) -> AnyElement {
        let name = snapshot
            .map(|s| s.schema.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.note.board_name.clone());
        let url = snapshot
            .and_then(|s| s.schema.url.clone())
            .filter(|u| !u.is_empty());
        let segment = |group_by: GroupBy, cx: &mut Context<Self>| {
            let selected = self.note.group_by == group_by;
            div()
                .id(SharedString::from(format!(
                    "monday-group-by-{}",
                    group_by.label().to_lowercase()
                )))
                .debug_selector(move || {
                    format!("monday-group-by-{}", group_by.label().to_lowercase())
                })
                .role(Role::Tab)
                .aria_label(group_by.label())
                .aria_selected(selected)
                .h(px(24.))
                .px_2()
                .rounded_sm()
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(content(if selected { 1. } else { 0.55 }))
                .when(selected, |d| d.bg(content(0.1)))
                .hover(|s| s.text_color(content(1.)))
                .cursor_pointer()
                .child(group_by.label())
                .on_click(cx.listener(move |this, _, _, cx| this.set_group_by(group_by, cx)))
        };
        let mine = self.note.mine;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(icon("inbox-monday", 16.).text_color(content(0.7)))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(320.))
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(if name.is_empty() {
                        format!("Board {}", self.note.board)
                    } else {
                        name
                    }),
            )
            .child(
                div()
                    .id("monday-group-by")
                    .role(Role::TabList)
                    .aria_label("Columns by")
                    .ml_2()
                    .p(px(2.))
                    .rounded_md()
                    .bg(content(0.05))
                    .flex()
                    .gap(px(2.))
                    .child(segment(GroupBy::Status, cx))
                    .child(segment(GroupBy::Group, cx)),
            )
            .child(
                div()
                    .id("monday-mine")
                    .debug_selector(|| "monday-mine".into())
                    .role(Role::Switch)
                    .aria_label("Assigned to me")
                    .aria_selected(mine)
                    .h(px(24.))
                    .px_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(12.))
                    .text_color(content(if mine { 1. } else { 0.55 }))
                    .when(mine, |d| d.bg(content(0.1)))
                    .hover(|s| s.bg(content(0.08)))
                    .cursor_pointer()
                    .child(icon("account-user", 12.))
                    .child("Assigned to me")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mine(cx))),
            )
            .child(div().flex_1())
            .when_some(snapshot, |d, snapshot| {
                d.child(
                    div()
                        .debug_selector(|| "monday-fetched".into())
                        .text_size(px(11.))
                        .text_color(content(0.45))
                        .child(if self.loading {
                            "Refreshing…".to_owned()
                        } else {
                            format!("Fetched {}", ago(snapshot.fetched_at))
                        }),
                )
            })
            .child(
                icon_button("monday-refresh", "Refresh from monday.com", "refresh-cw")
                    .debug_selector(|| "monday-refresh".into())
                    .on_click(cx.listener(|this, _, _, cx| this.start(true, cx))),
            )
            .when_some(url, |d, url| {
                d.child(
                    icon_button("monday-open-board", "Open in monday.com", "external-link")
                        .debug_selector(|| "monday-open-board".into())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            })
            .child(
                Self::button("monday-change-board", "Change board")
                    .text_color(content(0.6))
                    .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                    .child("Change board")
                    .on_click(cx.listener(|this, _, window, cx| this.change_board(window, cx))),
            )
            .into_any_element()
    }

    /// A one-line note above the columns: stale copy, error, truncation.
    fn notice(
        name: &'static str,
        text: String,
        warn: bool,
        action: Option<AnyElement>,
    ) -> AnyElement {
        div()
            .debug_selector(move || name.into())
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded_md()
            .bg(content(0.04))
            .text_size(px(12.))
            .text_color(if warn {
                rgb(0xf59e0b).into()
            } else {
                content(0.6)
            })
            .child(div().flex_1().min_w_0().child(text))
            .children(action)
            .into_any_element()
    }

    fn menu(&self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.weak_entity();
        let item = self.item(id).cloned();
        let url = item.as_ref().map(|i| i.url.clone()).unwrap_or_default();
        let row = |name: SharedString, label: String, glyph: &'static str| {
            div()
                .id(name.clone())
                .debug_selector(move || name.to_string())
                .role(Role::MenuItem)
                .aria_label(label.clone())
                .h(px(28.))
                .px_2()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(content(0.9))
                .hover(|s| s.bg(content(0.08)))
                .cursor_pointer()
                .child(icon(glyph, 14.))
                .child(div().flex_1().min_w_0().truncate().child(label))
        };
        popover("monday-card-menu", Anchor::Point(at))
            .width(240.)
            .max_height(360.)
            .on_dismiss(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            })
            .child(
                div()
                    .id("monday-card-menu-body")
                    .p_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .when(!url.is_empty(), |d| {
                        d.child(
                            row(
                                "monday-menu-open".into(),
                                "Open in monday.com".into(),
                                "external-link",
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.menu = None;
                                    cx.open_url(&url);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(div().my_1().h(px(1.)).bg(content(0.08)))
                    })
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_size(px(11.))
                            .text_color(content(0.45))
                            .child("Add to kanban"),
                    )
                    .when(self.kanbans.is_empty(), |d| {
                        d.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_size(px(12.))
                                .text_color(content(0.4))
                                .child("No kanban notes yet"),
                        )
                    })
                    .children(
                        self.kanbans
                            .iter()
                            .enumerate()
                            .map(|(ix, (kanban, title))| {
                                let kanban = kanban.clone();
                                let item = item.clone();
                                row(
                                    format!("monday-menu-kanban-{ix}").into(),
                                    title.clone(),
                                    "kanban",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.menu = None;
                                        if let Some(item) = item.clone() {
                                            cx.emit(Event::AddToKanban {
                                                kanban: kanban.clone(),
                                                item,
                                            });
                                        }
                                        cx.notify();
                                    },
                                ))
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl gpui::Render for MondayBoard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("monday-board")
            .debug_selector(|| "monday-board".into())
            .size_full()
            .flex()
            .flex_col()
            .gap_3();
        if !self.note.has_board() {
            return root.child(self.picker(cx)).into_any_element();
        }
        let snapshot = self.snapshot.clone();
        let mut notices = Vec::new();
        if let Some(snapshot) = &snapshot {
            if self.connected == Some(false) {
                notices.push(Self::notice(
                    "monday-offline",
                    format!(
                        "Showing the copy fetched {}. Connect monday.com to refresh.",
                        ago(snapshot.fetched_at)
                    ),
                    false,
                    Some(self.connect_button(cx)),
                ));
            } else if let Some(error) = &self.error {
                notices.push(Self::notice(
                    "monday-error",
                    format!("{error} Showing the last fetched copy."),
                    true,
                    Some(self.retry_button(cx)),
                ));
            }
            if self.note.mine && self.account.is_none() {
                notices.push(Self::notice(
                    "monday-no-account",
                    "Your monday.com account is not known yet; refresh once connected to see your items."
                        .into(),
                    false,
                    None,
                ));
            }
            if snapshot.truncated {
                notices.push(Self::notice(
                    "monday-truncated",
                    format!(
                        "Showing the first {} items; the board has more.",
                        snapshot.items.len()
                    ),
                    false,
                    None,
                ));
            }
        }
        let body = match &snapshot {
            Some(snapshot) => self.lanes(snapshot, cx),
            None if self.connected == Some(false) => self.not_connected(cx),
            None if self.loading || self.connected.is_none() => {
                self.panel("Loading board…", "Asking monday.com for its items.", None)
            }
            None => self.panel(
                "Could not load the board",
                self.error
                    .as_deref()
                    .unwrap_or("monday.com sent nothing back."),
                Some(self.retry_button(cx)),
            ),
        };
        root.child(self.toolbar(snapshot.as_ref(), cx))
            .children(notices)
            .child(body)
            .when_some(self.menu.clone(), |d, (id, at)| {
                d.child(self.menu(&id, at, cx))
            })
            .into_any_element()
    }
}

/// "just now", "4 min ago", "3 h ago", "2 d ago".
fn ago(at_ms: u64) -> String {
    let minutes = monocode_core::time::now_ms().saturating_sub(at_ms) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

/// monday's colour, given as `#rrggbb` or as a name.
pub(crate) fn swatch(color: Option<&str>) -> Hsla {
    let Some(color) = color.map(str::trim).filter(|c| !c.is_empty()) else {
        return content(0.3);
    };
    if let Some(hex) = color.strip_prefix('#')
        && hex.len() == 6
        && let Ok(value) = u32::from_str_radix(hex, 16)
    {
        return rgb(value).into();
    }
    let value = match color.to_lowercase().replace(['_', ' '], "-").as_str() {
        "green" | "done-green" | "grass-green" => 0x00c875,
        "bright-green" | "lime-green" | "saladish" => 0x9cd326,
        "orange" | "working-orange" => 0xfdab3d,
        "red" | "stuck-red" | "dark-red" => 0xe2445c,
        "blue" | "bright-blue" | "river" => 0x579bfc,
        "dark-blue" | "navy" => 0x0086c0,
        "purple" | "dark-purple" | "lavender" => 0xa25ddc,
        "pink" | "sofia-pink" | "lipstick" => 0xff5ac4,
        "yellow" | "egg-yolk" => 0xffcb00,
        "aquamarine" | "teal" | "australia" => 0x4eccc6,
        "brown" => 0x7f5347,
        "peach" => 0xffadad,
        "grey" | "gray" | "american-gray" | "explosive" => 0xc4c4c4,
        _ => return content(0.3),
    };
    rgb(value).into()
}
