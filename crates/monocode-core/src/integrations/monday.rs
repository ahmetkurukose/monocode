//! monday.com's GraphQL API (`api.monday.com/v2`), pinned to one version.
use super::{
    Account, BoardSchema, BoardSnapshot, Column, Comment, Container, Group, ItemDetail, PeopleCell,
    Provider, StatusCell, StatusOption, Tracker, TrackerError, WorkItem,
};
use serde_json::{json, Value};
use std::time::Duration;

const ENDPOINT: &str = "https://api.monday.com/v2";
/// Pinned so a new monday release cannot change answers under us. Move it on
/// deliberately: versions are supported about a year (see api-versioning).
pub const API_VERSION: &str = "2026-10";
/// Items fetched per request, and in all, per board: enough for a sprint
/// board, few enough that a free plan's 200 daily calls last.
const PAGE: usize = 200;
const MAX_ITEMS: usize = 1000;

const ITEM_FIELDS: &str = "id name url updated_at board { id } group { id title color } \
     column_values { id type text \
       ... on StatusValue { label label_style { color } } \
       ... on PeopleValue { persons_and_teams { id kind } } }";

/// Sends one GraphQL request: `(http status, json body)`.
pub trait Transport: Send + Sync {
    fn post(&self, token: &str, body: &Value) -> Result<(u16, Value), String>;
}

struct Http(ureq::Agent);

impl Transport for Http {
    fn post(&self, token: &str, body: &Value) -> Result<(u16, Value), String> {
        let response = match self
            .0
            .post(ENDPOINT)
            .set("Authorization", token)
            .set("API-Version", API_VERSION)
            .set("Content-Type", "application/json")
            .set("User-Agent", "Monocode")
            .send_string(&body.to_string())
        {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(error.to_string()),
        };
        let status = response.status();
        let value = response
            .into_string()
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        Ok((status, value))
    }
}

pub struct Monday {
    token: String,
    transport: Box<dyn Transport>,
}

impl Monday {
    pub fn new(token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(60))
            .build();
        Self::with_transport(token, Box::new(Http(agent)))
    }

    pub fn with_transport(token: String, transport: Box<dyn Transport>) -> Self {
        Self { token, transport }
    }

    fn query(&self, query: &str, variables: Value) -> Result<Value, TrackerError> {
        let body = json!({ "query": query, "variables": variables });
        let (status, value) = self
            .transport
            .post(&self.token, &body)
            .map_err(TrackerError::Network)?;
        if let Some(error) = error_of(status, &value) {
            return Err(error);
        }
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }

    fn schema_and_first_page(&self, id: &str, settings: &str) -> Result<Value, TrackerError> {
        let query = format!(
            "query ($ids: [ID!], $limit: Int!) {{ boards(ids: $ids) {{ id name url \
               columns {{ id title type {settings} }} groups {{ id title color }} \
               items_page(limit: $limit) {{ cursor items {{ {ITEM_FIELDS} }} }} }} }}"
        );
        self.query(&query, json!({ "ids": [id], "limit": PAGE }))
    }
}

/// The error a response carries, if any. monday answers errors in several
/// shapes: HTTP statuses, `errors[].extensions.code`, and a top-level `error_code`.
fn error_of(status: u16, value: &Value) -> Option<TrackerError> {
    let mut codes = Vec::new();
    let mut messages = Vec::new();
    let mut retry = value.get("retry_in_seconds").and_then(Value::as_u64);
    if let Some(code) = value.get("error_code").and_then(Value::as_str) {
        codes.push(code.to_owned());
    }
    for key in ["error_message", "errors"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            messages.push(text.to_owned());
        }
    }
    for error in value
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(message) = error.get("message").and_then(Value::as_str) {
            messages.push(message.to_owned());
        }
        let extensions = error.get("extensions");
        if let Some(code) = extensions
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
        {
            codes.push(code.to_owned());
        }
        retry = retry.or_else(|| {
            extensions
                .and_then(|e| e.get("retry_in_seconds"))
                .and_then(Value::as_u64)
        });
    }
    let has = |needle: &str| codes.iter().any(|c| c.eq_ignore_ascii_case(needle));
    if status == 401 || (status == 403 && has("UserUnauthorizedException")) || has("Unauthorized") {
        return Some(TrackerError::Unauthorized);
    }
    if has("DAILY_LIMIT_EXCEEDED") {
        return Some(TrackerError::DailyLimit);
    }
    let limited = [
        "COMPLEXITY_BUDGET_EXHAUSTED",
        "RATE_LIMIT_EXCEEDED",
        "IP_RATE_LIMIT_EXCEEDED",
        "ComplexityException",
        "maxConcurrencyExceeded",
    ];
    if status == 429 || limited.iter().any(|c| has(c)) {
        return Some(TrackerError::RateLimited {
            retry_in: retry.unwrap_or(30),
        });
    }
    if !messages.is_empty() || !codes.is_empty() {
        let text = if messages.is_empty() {
            codes.join(", ")
        } else {
            messages.join("; ")
        };
        return Some(TrackerError::Api(format!("monday.com: {text}")));
    }
    if status >= 400 {
        return Some(TrackerError::Api(format!(
            "monday.com answered HTTP {status}"
        )));
    }
    None
}

fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    Some(text(value, key)).filter(|s| !s.is_empty())
}

fn group(value: &Value) -> Option<Group> {
    let id = opt_text(value, "id")?;
    Some(Group {
        id,
        title: text(value, "title"),
        color: opt_text(value, "color"),
    })
}

fn parse_item(value: &Value, board: &str) -> WorkItem {
    let mut statuses = Vec::new();
    let mut people = Vec::new();
    for cell in value
        .get("column_values")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let column = text(cell, "id");
        match cell.get("type").and_then(Value::as_str) {
            Some("status") => {
                let label = opt_text(cell, "label").or_else(|| opt_text(cell, "text"));
                if let Some(label) = label {
                    let color = cell.get("label_style").and_then(|s| opt_text(s, "color"));
                    statuses.push(StatusCell {
                        column,
                        label,
                        color,
                    });
                }
            }
            Some("people") => {
                let ids = cell
                    .get("persons_and_teams")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|p| p.get("kind").and_then(Value::as_str) != Some("team"))
                    .map(|p| text(p, "id"))
                    .filter(|id| !id.is_empty())
                    .collect();
                let names = text(cell, "text")
                    .split(", ")
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned)
                    .collect();
                people.push(PeopleCell {
                    column,
                    people: ids,
                    names,
                });
            }
            _ => {}
        }
    }
    let board_id = value
        .get("board")
        .and_then(|b| opt_text(b, "id"))
        .unwrap_or_else(|| board.to_owned());
    WorkItem {
        provider: Provider::Monday,
        id: text(value, "id"),
        board_id,
        title: text(value, "name"),
        url: text(value, "url"),
        group: value.get("group").and_then(group),
        statuses,
        people,
        updated_at: opt_text(value, "updated_at"),
    }
}

/// A status column's labels from its settings: the typed `settings`
/// (`{"labels": [{label, color, is_done, index}]}`) or the older `settings_str`
/// (`{"labels": {"1": "Done"}, "labels_colors": {"1": {"color": ..}}, "done_colors": [1]}`).
fn status_options(column: &Value) -> Vec<StatusOption> {
    let settings = match column
        .get("settings")
        .or_else(|| column.get("settings_str"))
    {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    match settings.get("labels") {
        Some(Value::Array(labels)) => {
            let mut options: Vec<(i64, StatusOption)> = labels
                .iter()
                .filter(|l| l.get("is_deactivated").and_then(Value::as_bool) != Some(true))
                .filter_map(|l| {
                    let label = opt_text(l, "label")?;
                    let order = l.get("index").and_then(Value::as_i64).unwrap_or(i64::MAX);
                    Some((
                        order,
                        StatusOption {
                            label,
                            color: opt_text(l, "hex").or_else(|| opt_text(l, "color")),
                            done: l.get("is_done").and_then(Value::as_bool).unwrap_or(false),
                        },
                    ))
                })
                .collect();
            options.sort_by_key(|(order, _)| *order);
            options.into_iter().map(|(_, o)| o).collect()
        }
        Some(Value::Object(labels)) => {
            let done: Vec<String> = settings
                .get("done_colors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|v| v.to_string())
                .collect();
            let mut options: Vec<(i64, StatusOption)> = labels
                .iter()
                .filter_map(|(index, label)| {
                    let label = label.as_str().filter(|l| !l.is_empty())?;
                    let color = settings
                        .get("labels_colors")
                        .and_then(|c| c.get(index))
                        .and_then(|c| opt_text(c, "color"));
                    Some((
                        index.parse().unwrap_or(i64::MAX),
                        StatusOption {
                            label: label.to_owned(),
                            color,
                            done: done.contains(index),
                        },
                    ))
                })
                .collect();
            options.sort_by_key(|(order, _)| *order);
            options.into_iter().map(|(_, o)| o).collect()
        }
        _ => Vec::new(),
    }
}

fn parse_schema(board: &Value) -> BoardSchema {
    let mut schema = BoardSchema {
        id: text(board, "id"),
        name: text(board, "name"),
        url: opt_text(board, "url"),
        ..Default::default()
    };
    for column in board
        .get("columns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = text(column, "type");
        let id = text(column, "id");
        if kind == "status" {
            schema.statuses.push((id.clone(), status_options(column)));
        }
        schema.columns.push(Column {
            id,
            title: text(column, "title"),
            kind,
        });
    }
    schema.groups = board
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(group)
        .collect();
    schema
}

fn items_and_cursor(page: &Value, board: &str) -> (Vec<WorkItem>, Option<String>) {
    let items = page
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| parse_item(item, board))
        .collect();
    (items, opt_text(page, "cursor"))
}

impl Tracker for Monday {
    fn provider(&self) -> Provider {
        Provider::Monday
    }

    fn me(&self) -> Result<Account, TrackerError> {
        let data = self.query(
            "query { me { id name email account { id name slug } } }",
            json!({}),
        )?;
        let me = data
            .get("me")
            .ok_or_else(|| TrackerError::Api("monday.com sent no user".into()))?;
        let account = me.get("account").cloned().unwrap_or(Value::Null);
        Ok(Account {
            id: text(me, "id"),
            name: text(me, "name"),
            email: text(me, "email"),
            team: text(&account, "name"),
            slug: text(&account, "slug"),
        })
    }

    fn boards(&self) -> Result<Vec<Container>, TrackerError> {
        let mut boards = Vec::new();
        // Most recently used first; ten pages is a thousand boards.
        for page in 1..=10 {
            let data = self.query(
                "query ($page: Int!) { boards(limit: 100, page: $page, state: active, order_by: used_at) \
                   { id name type workspace { name } } }",
                json!({ "page": page }),
            )?;
            let list = data
                .get("boards")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let count = list.len();
            boards.extend(
                list.iter()
                    // Subitem boards and docs are not boards anyone picks.
                    .filter(|b| {
                        matches!(b.get("type").and_then(Value::as_str), None | Some("board"))
                    })
                    .map(|b| Container {
                        id: text(b, "id"),
                        name: text(b, "name"),
                        workspace: b.get("workspace").and_then(|w| opt_text(w, "name")),
                    }),
            );
            if count < 100 {
                break;
            }
        }
        Ok(boards)
    }

    fn board(&self, id: &str) -> Result<BoardSnapshot, TrackerError> {
        // `settings_str` gave way to the typed `settings` in 2025-10; an
        // account still answering the old shape gets the old field.
        let data = match self.schema_and_first_page(id, "settings") {
            Err(TrackerError::Api(message)) if message.contains("settings") => {
                self.schema_and_first_page(id, "settings_str")?
            }
            other => other?,
        };
        let board = data
            .get("boards")
            .and_then(Value::as_array)
            .and_then(|b| b.first())
            .ok_or_else(|| {
                TrackerError::Api("That board is gone, or you have no access to it.".into())
            })?;
        let schema = parse_schema(board);
        let (mut items, mut cursor) = board
            .get("items_page")
            .map(|page| items_and_cursor(page, id))
            .unwrap_or_default();
        while let Some(next) = cursor.take() {
            if items.len() >= MAX_ITEMS {
                cursor = Some(next);
                break;
            }
            let data = self.query(
                &format!(
                    "query ($cursor: String!, $limit: Int!) {{ next_items_page(cursor: $cursor, limit: $limit) \
                       {{ cursor items {{ {ITEM_FIELDS} }} }} }}"
                ),
                json!({ "cursor": next, "limit": PAGE }),
            )?;
            let (more, after) = data
                .get("next_items_page")
                .map(|page| items_and_cursor(page, id))
                .unwrap_or_default();
            items.extend(more);
            cursor = after;
        }
        Ok(BoardSnapshot {
            schema,
            items,
            fetched_at: crate::time::now_ms(),
            truncated: cursor.is_some(),
        })
    }

    fn item(&self, id: &str) -> Result<ItemDetail, TrackerError> {
        let data = self.query(
            &format!(
                "query ($ids: [ID!]) {{ items(ids: $ids) {{ {ITEM_FIELDS} \
                   long: column_values(types: [long_text]) {{ text }} \
                   updates(limit: 10) {{ text_body created_at creator {{ name }} }} }} }}"
            ),
            json!({ "ids": [id] }),
        )?;
        let value = data
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .ok_or_else(|| {
                TrackerError::Api("That item is gone, or you have no access to it.".into())
            })?;
        let description = value
            .get("long")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|cell| text(cell, "text"))
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let comments = value
            .get("updates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|update| Comment {
                author: update
                    .get("creator")
                    .map(|c| text(c, "name"))
                    .unwrap_or_default(),
                body: text(update, "text_body"),
                created_at: opt_text(update, "created_at"),
            })
            .collect();
        Ok(ItemDetail {
            item: parse_item(value, ""),
            description,
            comments,
        })
    }

    fn comment(&self, item: &str, body: &str) -> Result<(), TrackerError> {
        self.query(
            "mutation ($item: ID!, $body: String!) { create_update(item_id: $item, body: $body) { id } }",
            json!({ "item": item, "body": body }),
        )
        .map(drop)
    }

    fn set_status(
        &self,
        board: &str,
        item: &str,
        column: &str,
        label: &str,
    ) -> Result<(), TrackerError> {
        self.query(
            "mutation ($board: ID!, $item: ID!, $column: String!, $value: String) { \
               change_simple_column_value(board_id: $board, item_id: $item, column_id: $column, value: $value) { id } }",
            json!({ "board": board, "item": item, "column": column, "value": label }),
        )
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Answers requests in turn and keeps what was sent.
    #[derive(Clone, Default)]
    struct Script {
        answers: Arc<Mutex<Vec<(u16, Value)>>>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl Transport for Script {
        fn post(&self, token: &str, body: &Value) -> Result<(u16, Value), String> {
            assert_eq!(token, "tok");
            self.sent.lock().unwrap().push(body.clone());
            let mut answers = self.answers.lock().unwrap();
            Ok(if answers.is_empty() {
                (500, Value::Null)
            } else {
                answers.remove(0)
            })
        }
    }

    fn client(answers: Vec<(u16, Value)>) -> (Monday, Script) {
        let script = Script {
            answers: Arc::new(Mutex::new(answers)),
            ..Default::default()
        };
        (
            Monday::with_transport("tok".into(), Box::new(script.clone())),
            script,
        )
    }

    fn item_json(id: &str, status: &str, person: &str) -> Value {
        json!({
            "id": id, "name": format!("Item {id}"), "url": format!("https://t.monday.com/boards/9/pulses/{id}"),
            "board": { "id": "9" },
            "group": { "id": "topics", "title": "Sprint 4", "color": "#579bfc" },
            "column_values": [
                { "id": "status", "type": "status", "text": status, "label": status, "label_style": { "color": "#00c875" } },
                { "id": "person", "type": "people", "text": "Ayşe",
                  "persons_and_teams": [ { "id": person, "kind": "person" }, { "id": "55", "kind": "team" } ] },
                { "id": "text", "type": "text", "text": "x" }
            ]
        })
    }

    #[test]
    fn me_reads_the_user_and_their_team() {
        let (monday, _) = client(vec![(
            200,
            json!({ "data": { "me": {
            "id": 7, "name": "Ayşe", "email": "a@x.com", "account": { "id": 1, "name": "Acme", "slug": "acme" } } } }),
        )]);
        let me = monday.me().unwrap();
        assert_eq!(
            (me.id.as_str(), me.team.as_str(), me.slug.as_str()),
            ("7", "Acme", "acme")
        );
    }

    #[test]
    fn a_board_reads_its_schema_and_follows_the_cursor() {
        let first = json!({ "data": { "boards": [ {
            "id": "9", "name": "Sprint", "url": "https://t.monday.com/boards/9",
            "columns": [
                { "id": "status", "title": "Status", "type": "status", "settings": { "labels": [
                    { "id": 1, "label": "Done", "color": "green", "index": 2, "is_done": true },
                    { "id": 0, "label": "Working on it", "color": "orange", "index": 0 },
                    { "id": 5, "label": "Old", "index": 1, "is_deactivated": true } ] } },
                { "id": "person", "title": "Owner", "type": "people", "settings": {} }
            ],
            "groups": [ { "id": "topics", "title": "Sprint 4", "color": "#579bfc" } ],
            "items_page": { "cursor": "c1", "items": [ item_json("1", "Working on it", "7") ] }
        } ] } });
        let second = json!({ "data": { "next_items_page": { "cursor": null, "items": [ item_json("2", "Done", "8") ] } } });
        let (monday, script) = client(vec![(200, first), (200, second)]);
        let snapshot = monday.board("9").unwrap();
        assert_eq!(snapshot.schema.name, "Sprint");
        let labels: Vec<_> = snapshot
            .schema
            .status_options("status")
            .iter()
            .map(|o| o.label.as_str())
            .collect();
        assert_eq!(labels, ["Working on it", "Done"]);
        assert!(snapshot.schema.status_options("status")[1].done);
        assert_eq!(snapshot.items.len(), 2);
        assert!(!snapshot.truncated);
        let one = &snapshot.items[0];
        assert_eq!(one.status(Some("status")).unwrap().label, "Working on it");
        assert!(one.assigned_to("7", Some("person")));
        assert!(!one.assigned_to("55", None), "teams are not people");
        assert_eq!(one.group.as_ref().unwrap().title, "Sprint 4");
        let sent = script.sent.lock().unwrap();
        assert_eq!(sent[1]["variables"]["cursor"], "c1");
    }

    #[test]
    fn the_old_settings_string_is_read_too() {
        let column = json!({ "type": "status", "settings_str":
            "{\"labels\":{\"1\":\"Done\",\"0\":\"Working on it\",\"5\":\"\"},\"labels_colors\":{\"1\":{\"color\":\"#00c875\"}},\"done_colors\":[1]}" });
        let options = status_options(&column);
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "Working on it");
        assert_eq!((options[1].label.as_str(), options[1].done), ("Done", true));
        assert_eq!(options[1].color.as_deref(), Some("#00c875"));
    }

    #[test]
    fn a_rejected_settings_field_falls_back_to_settings_str() {
        let refused = json!({ "errors": [ { "message": "Cannot query field \"settings\" on type \"Column\"." } ] });
        let ok = json!({ "data": { "boards": [ { "id": "9", "name": "B", "columns": [], "groups": [],
            "items_page": { "cursor": null, "items": [] } } ] } });
        let (monday, script) = client(vec![(200, refused), (200, ok)]);
        assert_eq!(monday.board("9").unwrap().schema.name, "B");
        let sent = script.sent.lock().unwrap();
        assert!(sent[1]["query"].as_str().unwrap().contains("settings_str"));
    }

    #[test]
    fn errors_become_the_kind_the_ui_explains() {
        assert_eq!(
            error_of(
                401,
                &json!({ "errors": [{ "message": "Not Authenticated" }] })
            ),
            Some(TrackerError::Unauthorized)
        );
        assert_eq!(
            error_of(
                200,
                &json!({ "errors": [{ "message": "Budget", "extensions": { "code": "COMPLEXITY_BUDGET_EXHAUSTED", "retry_in_seconds": 12 } }] })
            ),
            Some(TrackerError::RateLimited { retry_in: 12 })
        );
        assert_eq!(
            error_of(429, &json!({})),
            Some(TrackerError::RateLimited { retry_in: 30 })
        );
        assert_eq!(
            error_of(
                200,
                &json!({ "errors": [{ "message": "Daily", "extensions": { "code": "DAILY_LIMIT_EXCEEDED" } }] })
            ),
            Some(TrackerError::DailyLimit)
        );
        assert_eq!(
            error_of(
                200,
                &json!({ "error_code": "InvalidBoardIdException", "error_message": "Board not found" })
            ),
            Some(TrackerError::Api("monday.com: Board not found".into()))
        );
        assert_eq!(error_of(200, &json!({ "data": {} })), None);
    }

    #[test]
    fn an_item_brings_its_description_and_updates() {
        let mut value = item_json("1", "Stuck", "7");
        value["long"] = json!([{ "text": "Steps: open /login" }, { "text": "" }]);
        value["updates"] = json!([{ "text_body": "Safari only", "created_at": "2026-11-01", "creator": { "name": "Ayşe" } }]);
        let (monday, _) = client(vec![(200, json!({ "data": { "items": [value] } }))]);
        let detail = monday.item("1").unwrap();
        assert_eq!(detail.item.board_id, "9");
        assert_eq!(detail.description, "Steps: open /login");
        assert_eq!(detail.comments[0].author, "Ayşe");
    }

    #[test]
    fn writes_send_the_item_and_label() {
        let (monday, script) = client(vec![
            (200, json!({ "data": {} })),
            (200, json!({ "data": {} })),
        ]);
        monday
            .comment("1", "PR: https://github.com/x/y/pull/2")
            .unwrap();
        monday.set_status("9", "1", "status", "Review").unwrap();
        let sent = script.sent.lock().unwrap();
        assert_eq!(
            sent[0]["variables"]["body"],
            "PR: https://github.com/x/y/pull/2"
        );
        assert_eq!(
            sent[1]["variables"],
            json!({ "board": "9", "item": "1", "column": "status", "value": "Review" })
        );
    }
}
