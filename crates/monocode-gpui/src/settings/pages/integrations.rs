//! Settings → Integrations: the work trackers and code hosts the Inbox reads.
//! For monday.com also the boards the Inbox follows, and the board a project
//! works from (`.monocode/integrations.json`, committed so the team shares it).
use crate::{
    settings::{
        Section, SettingsEvent, SettingsView,
        widgets::{RED, note, primary_button, secondary_button},
    },
    theme::*,
};
use gpui::{AnyElement, Context, SharedString, Task, div, prelude::*, px, rgb};
use monocode_core::{
    inbox::{OptionRow, Status, monday},
    integrations::{BoardSchema, MondayBinding, ProjectIntegrations, Provider, StatusKey},
};
use std::path::{Path, PathBuf};

/// The status map's selects, one per Monocode status.
const STATUS_SELECTS: [(StatusKey, &str); 4] = [
    (StatusKey::Todo, "monday-map-todo"),
    (StatusKey::InProgress, "monday-map-in-progress"),
    (StatusKey::InReview, "monday-map-in-review"),
    (StatusKey::Done, "monday-map-done"),
];

/// What the page knows about monday.com beyond the Inbox's connection list.
#[derive(Default)]
pub(in crate::settings) struct IntegrationsState {
    /// The sign-in as read here; `None` until read, or without an app database.
    pub(in crate::settings) monday: Option<Status>,
    /// Every board the user can see.
    pub(in crate::settings) boards: Vec<OptionRow>,
    pub(in crate::settings) boards_loading: bool,
    pub(in crate::settings) boards_error: Option<String>,
    /// The project `saved` and `draft` belong to.
    pub(in crate::settings) project: Option<String>,
    pub(in crate::settings) saved: Option<MondayBinding>,
    pub(in crate::settings) draft: MondayBinding,
    /// The draft board's columns and labels.
    pub(in crate::settings) schema: Option<BoardSchema>,
    pub(in crate::settings) schema_loading: bool,
    pub(in crate::settings) schema_error: Option<String>,
    /// What Save or Remove last said.
    pub(in crate::settings) notice: Option<Result<String, String>>,
    load: Option<Task<()>>,
    schema_task: Option<Task<()>>,
}

/// Who a token signed in as: email, team and site.
fn account_label(status: &Status) -> String {
    let parts = [&status.email, &status.organization, &status.site]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "Connected".into()
    } else {
        parts.join(" · ")
    }
}

fn board_label(board: &OptionRow) -> SharedString {
    if board.key.is_empty() {
        board.name.clone().into()
    } else {
        format!("{} · {}", board.name, board.key).into()
    }
}

fn error_line(text: String) -> AnyElement {
    div()
        .px_4()
        .pb(px(14.))
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(rgb(RED))
        .child(text)
        .into_any_element()
}

impl SettingsView {
    fn app_data_dir(&self) -> Option<PathBuf> {
        let store = self.ctx.store.clone()?;
        crate::runtime::locked(&store.0)
            .ok()?
            .data_dir()
            .map(Path::to_path_buf)
    }

    pub(in crate::settings) fn monday_connected(&self) -> bool {
        match &self.integrations.monday {
            Some(status) => status.connected,
            None => (self.ctx.inbox_connections.get("monday")).is_some_and(|(on, _)| *on),
        }
    }

    /// Entering Integrations reads monday's sign-in and boards once.
    pub(in crate::settings) fn watch_integrations(&mut self, cx: &mut Context<Self>) {
        if self.section == Section::Integrations
            && self.integrations.monday.is_none()
            && !self.integrations.boards_loading
        {
            self.load_monday(false, cx);
        }
    }

    /// The sign-in, then the boards; `refresh` asks monday instead of the cache.
    fn load_monday(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(dir) = self.app_data_dir() else {
            return;
        };
        self.integrations.boards_loading = true;
        self.integrations.boards_error = None;
        let work = cx.background_executor().spawn(async move {
            let status = monday::monday_status(dir.clone())?;
            let boards = status
                .connected
                .then(|| monday::monday_list_boards(dir, refresh));
            Ok::<_, String>((status, boards))
        });
        self.integrations.load = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.integrations;
                state.boards_loading = false;
                match result {
                    Ok((status, boards)) => {
                        state.monday = Some(status);
                        match boards {
                            Some(Ok(boards)) => state.boards = boards,
                            Some(Err(error)) => state.boards_error = Some(error),
                            None => state.boards.clear(),
                        }
                    }
                    Err(error) => state.boards_error = Some(error),
                }
                this.load_monday_schema(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The draft board's columns and labels, unless already here.
    fn load_monday_schema(&mut self, cx: &mut Context<Self>) {
        let board = self.integrations.draft.board.clone();
        if board.is_empty()
            || !self.monday_connected()
            || (self.integrations.schema.as_ref()).is_some_and(|s| s.id == board)
        {
            return;
        }
        let Some(dir) = self.app_data_dir() else {
            return;
        };
        self.integrations.schema_loading = true;
        self.integrations.schema_error = None;
        let id = board.clone();
        let work = cx
            .background_executor()
            .spawn(async move { monday::monday_board(dir, &id, false).map(|s| s.schema) });
        self.integrations.schema_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.integrations;
                if state.draft.board != board {
                    return;
                }
                state.schema_loading = false;
                match result {
                    Ok(schema) => state.schema = Some(schema),
                    Err(error) => state.schema_error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The project picker moved: show that project's binding.
    pub(in crate::settings) fn sync_monday_project(&mut self, cx: &mut Context<Self>) {
        if self.integrations.project.as_deref() == Some(self.selected_project.as_str()) {
            return;
        }
        self.integrations.project = Some(self.selected_project.clone());
        let saved = if self.selected_project.is_empty() {
            None
        } else {
            ProjectIntegrations::load(Path::new(&self.selected_project)).monday
        };
        let state = &mut self.integrations;
        state.draft = saved.clone().unwrap_or_default();
        state.saved = saved;
        state.schema = None;
        state.schema_loading = false;
        state.schema_error = None;
        state.schema_task = None;
        state.notice = None;
        self.load_monday_schema(cx);
    }

    fn toggle_monday_board(&mut self, board: String, cx: &mut Context<Self>) {
        let boards = &mut self.ctx.monday_boards;
        match boards.iter().position(|b| *b == board) {
            Some(ix) => {
                boards.remove(ix);
            }
            None => boards.push(board),
        }
        cx.emit(SettingsEvent::MondayBoards(self.ctx.monday_boards.clone()));
        cx.notify();
    }

    pub(in crate::settings) fn choose_monday_board(
        &mut self,
        board: String,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.integrations;
        if state.draft.board == board {
            return;
        }
        let board_name = (state.boards.iter())
            .find(|b| b.id == board)
            .map(|b| b.name.clone())
            .unwrap_or_default();
        // Columns and labels belong to the board they came from.
        state.draft = MondayBinding {
            board,
            board_name,
            write_back: state.draft.write_back,
            ..Default::default()
        };
        state.schema = None;
        state.schema_error = None;
        state.schema_loading = false;
        state.notice = None;
        self.load_monday_schema(cx);
        cx.notify();
    }

    pub(in crate::settings) fn edit_monday_draft(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut MondayBinding),
    ) {
        change(&mut self.integrations.draft);
        self.integrations.notice = None;
        cx.notify();
    }

    /// Writes the draft (or, with `remove`, no board) to the project's file.
    pub(in crate::settings) fn save_monday_binding(
        &mut self,
        remove: bool,
        cx: &mut Context<Self>,
    ) {
        if self.selected_project.is_empty() {
            return;
        }
        let project = PathBuf::from(&self.selected_project);
        let mut config = ProjectIntegrations::load(&project);
        let binding = (!remove && !self.integrations.draft.board.is_empty())
            .then(|| self.integrations.draft.clone());
        config.monday = binding.clone();
        let state = &mut self.integrations;
        match config.save(&project) {
            Ok(()) => {
                if binding.is_none() {
                    state.draft = MondayBinding::default();
                    state.schema = None;
                }
                state.saved = binding;
                state.notice = Some(Ok(if remove {
                    "Removed from .monocode/integrations.json.".into()
                } else {
                    "Saved to .monocode/integrations.json.".into()
                }));
                cx.emit(SettingsEvent::IntegrationsChanged);
            }
            Err(error) => state.notice = Some(Err(error)),
        }
        cx.notify();
    }

    pub(super) fn integrations_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut page = vec![self.monday_group(cx)];
        if self.monday_connected() {
            self.sync_monday_project(cx);
            page.push(self.monday_project_group(cx));
        }
        for (id, title, description, label, hint) in [
            (
                "jira",
                "Jira",
                "Jira Cloud issues from the projects you pick.",
                "Connection",
                "Connect your Jira Cloud site with your email and an API token.",
            ),
            (
                "linear",
                "Linear",
                "Issues assigned to you, from the teams you pick.",
                "API key",
                "Create a personal API key in Linear → Settings → Security & Access. Disconnect deletes it.",
            ),
            (
                "github",
                "GitHub",
                "Pull requests, reviews, and issues, read through the GitHub CLI.",
                "Connection",
                "Install GitHub CLI from cli.github.com, run gh auth login in a terminal, then check again.",
            ),
            (
                "gitlab",
                "GitLab",
                "Merge requests from GitLab.com or a self-managed instance.",
                "Connection",
                "Connect GitLab.com or a self-managed GitLab instance. Use a personal access token with API access; the token is stored locally and Disconnect deletes it.",
            ),
            (
                "azuredevops",
                "ADO",
                "Pull requests and Boards work items from your ADO organization.",
                "Connection",
                "Connect your ADO organization with a personal access token (Boards + Repos read & write for comments). The token is stored locally and Disconnect deletes it.",
            ),
        ] {
            let (connected, connection_label) = self
                .ctx
                .inbox_connections
                .get(id)
                .cloned()
                .unwrap_or_else(|| (false, "Checking connection…".into()));
            let mut controls = vec![connection_status(connected, connection_label)];
            if id == "github" {
                controls.push(
                    secondary_button(
                        "github-installation-guide",
                        "Installation guide",
                        false,
                        false,
                    )
                    .child("Installation guide")
                    .on_click(|_, _, cx| cx.open_url("https://cli.github.com/"))
                    .into_any_element(),
                );
                controls.push(
                    secondary_button("github-check-again", "Check again", false, false)
                        .child("Check again")
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(SettingsEvent::ManageInboxProvider("github".into()))
                        }))
                        .into_any_element(),
                );
            } else {
                controls.push(manage_button(id, connected, cx));
            }
            let row = self.row(None, true, label, Some(hint.into()), controls);
            page.push(
                self.group(
                    Some(id),
                    false,
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(
                            match id {
                                "linear" => "inbox-linear",
                                "jira" => "inbox-jira",
                                "gitlab" => "inbox-gitlab",
                                "azuredevops" => "inbox-azuredevops",
                                _ => "inbox-github",
                            },
                            16.,
                        ))
                        .child(title),
                    Some(description.into()),
                    None,
                    vec![row],
                ),
            );
        }
        page
    }

    fn monday_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.monday_connected();
        let label = match &self.integrations.monday {
            Some(status) if status.connected => account_label(status),
            Some(_) => "Not connected".into(),
            None => (self.ctx.inbox_connections.get("monday"))
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| "Checking connection…".into()),
        };
        let controls = vec![
            connection_status(connected, label),
            secondary_button("monday-token-guide", "Token guide", false, false)
                .child("Token guide")
                .on_click(|_, _, cx| cx.open_url(Provider::Monday.token_help_url()))
                .into_any_element(),
            manage_button("monday", connected, cx),
        ];
        let mut rows = vec![self.row(
            None,
            true,
            "API token",
            Some(
                "In monday.com, open your avatar → Developers → API token, or Administration → Connections → Personal API token. Disconnect deletes it."
                    .into(),
            ),
            controls,
        )];
        if connected {
            rows.extend(self.monday_boards_rows(cx));
        }
        self.group(
            Some("monday"),
            true,
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    gpui::img("inbox-monday-color.svg")
                        .size(px(16.))
                        .flex_shrink_0(),
                )
                .child("monday.com"),
            Some(
                "Boards and items from monday.com; each teammate connects with their own personal API token."
                    .into(),
            ),
            None,
            rows,
        )
    }

    fn monday_boards_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let followed = self.ctx.monday_boards.clone();
        let boards = &self.integrations.boards;
        let mut options: Vec<(String, SharedString, bool)> = boards
            .iter()
            .map(|b| (b.id.clone(), board_label(b), followed.contains(&b.id)))
            .collect();
        // A followed board the list lacks (deleted, or out of reach) can still be dropped.
        for id in &followed {
            if !options.iter().any(|(option, _, _)| option == id) {
                options.push((id.clone(), format!("Board {id}").into(), true));
            }
        }
        let summary: SharedString = match followed.as_slice() {
            [] => "None".into(),
            [id] => (boards.iter().find(|b| &b.id == id))
                .map_or_else(|| format!("Board {id}"), |b| b.name.clone())
                .into(),
            many => format!("{} boards", many.len()).into(),
        };
        let loading = self.integrations.boards_loading;
        let picker = self.multi_select(
            "monday-boards",
            "Boards in Inbox",
            summary,
            options,
            None,
            cx,
            |this, id, cx| this.toggle_monday_board(id, cx),
        );
        let refresh = secondary_button("monday-refresh-boards", "Refresh boards", false, loading)
            .child(icon("refresh-cw", 14.))
            .child(if loading {
                "Loading…"
            } else {
                "Refresh boards"
            })
            .when(!loading, |d| {
                d.on_click(cx.listener(|this, _, _, cx| this.load_monday(true, cx)))
            })
            .into_any_element();
        let mut rows = vec![self.row(
            Some("monday-boards"),
            false,
            "Boards in Inbox",
            Some(
                "The boards the Inbox lists, also chosen from its filter menu. A board a project works from is always listed."
                    .into(),
            ),
            vec![picker, refresh],
        )];
        if let Some(error) = self.integrations.boards_error.clone() {
            rows.push(error_line(error));
        }
        rows
    }

    fn monday_project_group(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let picker = (!self.ctx.projects.is_empty()).then(|| self.project_picker(cx));
        let project = (self.ctx.projects.iter())
            .find(|(path, _)| *path == self.selected_project)
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| self.ctx.project_label.clone());
        if self.selected_project.is_empty() {
            return self.group(
                Some("monday-project"),
                false,
                "Project board",
                Some("The monday.com board a project works from.".into()),
                picker,
                vec![note("Open a project to choose the board it works from.")],
            );
        }
        let draft = self.integrations.draft.clone();
        let mut board_options = vec![(String::new(), "Choose a board".into(), None)];
        board_options.extend(
            (self.integrations.boards.iter()).map(|b| (b.id.clone(), board_label(b), None)),
        );
        if !draft.board.is_empty() && !board_options.iter().any(|(id, _, _)| *id == draft.board) {
            let name = if draft.board_name.is_empty() {
                format!("Board {}", draft.board)
            } else {
                draft.board_name.clone()
            };
            board_options.push((draft.board.clone(), name.into(), None));
        }
        let board = self.select(
            "monday-board",
            "Board",
            &draft.board,
            board_options,
            None,
            cx,
            |this, board, cx| this.choose_monday_board(board, cx),
        );
        let mut rows = vec![self.row(
            None,
            true,
            "Board",
            Some("Items, statuses and assignees come from this board.".into()),
            vec![board],
        )];
        if self.integrations.schema_loading {
            rows.push(note("Reading the board…"));
        }
        if let Some(error) = self.integrations.schema_error.clone() {
            rows.push(error_line(error));
        }
        let schema = (self.integrations.schema.clone()).filter(|s| s.id == draft.board);
        if let Some(schema) = schema {
            rows.extend(self.monday_schema_rows(&schema, &draft, cx));
        }
        rows.push(self.monday_save_row(cx));
        self.group(
            Some("monday-project"),
            false,
            "Project board",
            Some(
                format!("The monday.com board {project} works from, and what its statuses mean.")
                    .into(),
            ),
            picker,
            rows,
        )
    }

    fn monday_schema_rows(
        &mut self,
        schema: &BoardSchema,
        draft: &MondayBinding,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        let status_columns = schema
            .columns_of("status")
            .map(|c| (c.id.clone(), SharedString::from(c.title.clone()), None))
            .collect::<Vec<_>>();
        let status_column = (draft.status_column.clone())
            .or_else(|| status_columns.first().map(|(id, _, _)| id.clone()))
            .unwrap_or_default();
        if status_columns.is_empty() {
            rows.push(note(
                "This board has no status column, so Monocode cannot read or move its items' statuses.",
            ));
        } else {
            let select = self.select(
                "monday-status-column",
                "Status column",
                &status_column,
                status_columns,
                None,
                cx,
                |this, column, cx| {
                    if this.integrations.draft.status_column.as_deref() != Some(column.as_str()) {
                        this.edit_monday_draft(cx, |draft| {
                            draft.status_column = Some(column);
                            // Labels belong to the column they came from.
                            draft.status_map.clear();
                        });
                    }
                },
            );
            rows.push(self.row(
                None,
                false,
                "Status column",
                Some("The column Monocode reads and moves.".into()),
                vec![select],
            ));
        }
        let mut people = vec![(String::new(), SharedString::from("Any people column"), None)];
        people.extend(
            schema
                .columns_of("people")
                .map(|c| (c.id.clone(), SharedString::from(c.title.clone()), None)),
        );
        let people_column = draft.people_column.clone().unwrap_or_default();
        let select = self.select(
            "monday-people-column",
            "People column",
            &people_column,
            people,
            None,
            cx,
            |this, column, cx| {
                this.edit_monday_draft(cx, |draft| {
                    draft.people_column = (!column.is_empty()).then_some(column)
                })
            },
        );
        rows.push(self.row(
            None,
            false,
            "People column",
            Some("Where \"Assigned to me\" looks for you.".into()),
            vec![select],
        ));
        let labels = schema.status_options(&status_column).to_vec();
        if !labels.is_empty() {
            for (ix, (key, id)) in STATUS_SELECTS.into_iter().enumerate() {
                let mut options = vec![(String::new(), SharedString::from("Not mapped"), None)];
                options.extend(labels.iter().map(|option| {
                    (
                        option.label.clone(),
                        SharedString::from(option.label.clone()),
                        None,
                    )
                }));
                let value = draft.status_map.get(&key).cloned().unwrap_or_default();
                let select = self.select(
                    id,
                    key.label(),
                    &value,
                    options,
                    None,
                    cx,
                    move |this, label, cx| {
                        this.edit_monday_draft(cx, |draft| {
                            if label.is_empty() {
                                draft.status_map.remove(&key);
                            } else {
                                draft.status_map.insert(key, label);
                            }
                        })
                    },
                );
                rows.push(self.row(
                    None,
                    false,
                    key.label(),
                    (ix == 0).then(|| {
                        "The board label each Monocode status stands for. Unmapped statuses are never written."
                            .into()
                    }),
                    vec![select],
                ));
            }
        }
        let write_back = match draft.write_back {
            None => "ask",
            Some(true) => "on",
            Some(false) => "off",
        };
        let segmented = self.segmented(
            "monday-write-back",
            "Update items on PR",
            write_back,
            &[("ask", "Ask on first PR"), ("on", "On"), ("off", "Off")],
            cx,
            |this, choice, cx| {
                this.edit_monday_draft(cx, |draft| {
                    draft.write_back = match choice {
                        "on" => Some(true),
                        "off" => Some(false),
                        _ => None,
                    }
                })
            },
        );
        rows.push(self.row(
            None,
            false,
            "Update items on PR",
            Some("Move an item to In review when its pull request opens, and to Done when it merges.".into()),
            vec![segmented],
        ));
        rows
    }

    fn monday_save_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.integrations;
        let unchanged = state.saved.as_ref() == Some(&state.draft)
            || (state.saved.is_none() && state.draft.board.is_empty());
        let can_save = !unchanged && !state.draft.board.is_empty();
        let mut controls = Vec::new();
        if let Some(notice) = state.notice.clone() {
            let (text, color) = match notice {
                Ok(text) => (text, content(0.6)),
                Err(error) => (error, rgb(RED).into()),
            };
            controls.push(
                div()
                    .text_size(px(12.))
                    .text_color(color)
                    .child(text)
                    .into_any_element(),
            );
        }
        if state.saved.is_some() {
            controls.push(
                secondary_button("monday-binding-remove", "Remove", true, false)
                    .child("Remove")
                    .on_click(cx.listener(|this, _, _, cx| this.save_monday_binding(true, cx)))
                    .into_any_element(),
            );
        }
        controls.push(
            primary_button("monday-binding-save", "Save", !can_save)
                .debug_selector(|| "settings-monday-binding-save".into())
                .child("Save")
                .when(can_save, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.save_monday_binding(false, cx)))
                })
                .into_any_element(),
        );
        self.row(
            None,
            false,
            "Shared with your team",
            Some(
                "Saved in the project as .monocode/integrations.json. Commit it so everyone works from the same board and statuses; tokens never go there."
                    .into(),
            ),
            controls,
        )
    }
}

fn connection_status(connected: bool, label: String) -> AnyElement {
    div()
        .text_size(px(12.))
        .text_color(content(if connected { 0.7 } else { 0.5 }))
        .child(label)
        .into_any_element()
}

fn manage_button(id: &'static str, connected: bool, cx: &mut Context<SettingsView>) -> AnyElement {
    secondary_button(
        SharedString::from(format!("connect-{id}")),
        if connected { "Manage" } else { "Connect" },
        false,
        false,
    )
    .debug_selector(move || format!("settings-connect-{id}"))
    .child(if connected { "Manage" } else { "Connect" })
    .on_click(
        cx.listener(move |_, _, _, cx| cx.emit(SettingsEvent::ManageInboxProvider(id.into()))),
    )
    .into_any_element()
}
