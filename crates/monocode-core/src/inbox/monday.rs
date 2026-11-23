//! monday.com items in the Inbox, over [`crate::integrations::monday`].
//! The Inbox lists the boards a user follows (Settings → Integrations) and
//! the boards projects bind in `.monocode/integrations.json`.
use super::{Comment, Details, Item, Label, OptionRow, Person, Status, Thread};
use crate::integrations::{
    self, cache::Cache, monday::Monday, Account, BoardSnapshot, ItemDetail, ProjectIntegrations,
    Provider, Tracker, TrackerError, WorkItem,
};
use std::path::{Path, PathBuf};

/// How long the board list is kept before it is asked for again.
const BOARDS_FRESH_MS: u64 = 60 * 60 * 1000;

fn cache(app: &Path) -> Cache {
    Cache::new(app, Provider::Monday)
}

fn tracker() -> Result<Box<dyn Tracker>, String> {
    integrations::connect(Provider::Monday).map_err(|e| e.to_string())
}

pub fn monday_status(app: PathBuf) -> Result<Status, String> {
    let connected = integrations::token(Provider::Monday)?.is_some();
    let account = cache(&app).account().filter(|_| connected);
    Ok(status(connected, account.as_ref()))
}

fn status(connected: bool, account: Option<&Account>) -> Status {
    let account = account.cloned().unwrap_or_default();
    Status {
        connected,
        authenticated: connected,
        installed: true,
        site: if account.slug.is_empty() {
            String::new()
        } else {
            format!("{}.monday.com", account.slug)
        },
        url: if account.slug.is_empty() {
            String::new()
        } else {
            format!("https://{}.monday.com", account.slug)
        },
        email: account.email,
        organization: account.team,
    }
}

/// Signs in with `token`, checked against monday first; an empty token signs out.
pub fn monday_set_token(app: PathBuf, token: String) -> Result<Status, String> {
    let token = token.trim().to_owned();
    let cache = cache(&app);
    if token.is_empty() {
        integrations::forget_token(Provider::Monday);
        cache.clear();
        return Ok(status(false, None));
    }
    let account = Monday::new(token.clone()).me().map_err(|e| e.to_string())?;
    integrations::save_token(Provider::Monday, &token)?;
    // Another account's boards must not linger.
    if cache.account().is_some_and(|old| old.id != account.id) {
        cache.clear();
    }
    cache.save_account(&account)?;
    Ok(status(true, Some(&account)))
}

/// The boards the user can see, most recently used first.
pub fn monday_list_boards(app: PathBuf, refresh: bool) -> Result<Vec<OptionRow>, String> {
    let cache = cache(&app);
    let stamp = app.join("integrations/monday/boards.json");
    let fresh = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| (age.as_millis() as u64) < BOARDS_FRESH_MS);
    let boards = match cache.boards() {
        Some(boards) if fresh && !refresh => boards,
        cached => match tracker().and_then(|t| t.boards().map_err(|e| e.to_string())) {
            Ok(boards) => {
                let _ = cache.save_boards(&boards);
                boards
            }
            // Offline or out of calls: the last list beats none.
            Err(error) => cached.ok_or(error)?,
        },
    };
    Ok(boards
        .into_iter()
        .map(|board| OptionRow {
            key: board.workspace.unwrap_or_default(),
            id: board.id,
            name: board.name,
        })
        .collect())
}

/// A board's items, from the cache while it is fresh. `refresh` asks monday
/// regardless; a failed request falls back to what was last fetched.
pub fn monday_board(app: PathBuf, board: &str, refresh: bool) -> Result<BoardSnapshot, String> {
    let cache = cache(&app);
    let cached = cache.board(board);
    if !refresh {
        if let Some(snapshot) = &cached {
            if snapshot.is_fresh(crate::time::now_ms()) {
                return Ok(snapshot.clone());
            }
        }
    }
    match tracker().and_then(|t| t.board(board).map_err(|e| e.to_string())) {
        Ok(snapshot) => {
            let _ = cache.save_board(&snapshot);
            Ok(snapshot)
        }
        Err(error) => cached.ok_or(error),
    }
}

/// The boards the Inbox lists: those followed and those `projects` bind.
pub fn inbox_boards(followed: &[String], projects: &[PathBuf]) -> Vec<String> {
    let mut boards = followed.to_vec();
    for project in projects {
        if let Some(binding) = ProjectIntegrations::load(project).monday {
            boards.push(binding.board);
        }
    }
    let mut seen = std::collections::HashSet::new();
    boards.retain(|b| !b.is_empty() && seen.insert(b.clone()));
    boards
}

pub fn monday_list_items(
    app: PathBuf,
    boards: &[String],
    assigned_to_me: bool,
    state: &str,
    limit: usize,
) -> Result<Vec<Item>, String> {
    let me = cache(&app).account().map(|a| a.id);
    let mut items = Vec::new();
    let mut first_error = None;
    for board in boards {
        let snapshot = match monday_board(app.clone(), board, false) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                first_error.get_or_insert(error);
                continue;
            }
        };
        items.extend(
            snapshot
                .items
                .iter()
                .filter(|item| {
                    !assigned_to_me || me.as_deref().is_some_and(|me| item.assigned_to(me, None))
                })
                .map(|item| inbox_item(item, &snapshot))
                .filter(|item| match state {
                    "open" => item.state_type != "completed",
                    "closed" => item.state_type == "completed",
                    _ => true,
                }),
        );
    }
    if items.is_empty() {
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    items.sort_by(|a, b| b.updated_ms().cmp(&a.updated_ms()));
    items.truncate(limit);
    Ok(items)
}

/// An item as an Inbox row: its board is the project, its group the team.
pub fn inbox_item(item: &WorkItem, snapshot: &BoardSnapshot) -> Item {
    let status = item.status(None);
    let done = status.is_some_and(|cell| {
        snapshot
            .schema
            .status_options(&cell.column)
            .iter()
            .any(|o| o.done && o.label == cell.label)
    });
    let group = item.group.clone();
    Item {
        provider: "monday".into(),
        kind: "issue".into(),
        id: item.id.clone(),
        identifier: String::new(),
        number: item.id.parse().unwrap_or(0),
        title: item.title.clone(),
        url: item.url.clone(),
        state: status.map(|s| s.label.clone()).unwrap_or_default(),
        state_type: if done { "completed" } else { "started" }.into(),
        updated_at: item.updated_at.clone().unwrap_or_default(),
        created_at: item.updated_at.clone().unwrap_or_default(),
        labels: group
            .iter()
            .map(|g| Label {
                name: g.title.clone(),
                color: g.color.clone().unwrap_or_default(),
            })
            .collect(),
        assignees: item
            .people
            .iter()
            .flat_map(|cell| cell.names.iter())
            .map(|name| Person {
                login: name.clone(),
                avatar_url: String::new(),
            })
            .collect(),
        team_id: group.as_ref().map(|g| g.id.clone()).unwrap_or_default(),
        team_name: group.map(|g| g.title).unwrap_or_default(),
        project_id: snapshot.schema.id.clone(),
        project_name: snapshot.schema.name.clone(),
        ..Default::default()
    }
}

fn detail(id: &str) -> Result<ItemDetail, String> {
    tracker()?.item(id).map_err(|e: TrackerError| e.to_string())
}

pub fn monday_item_details(id: String) -> Result<Details, String> {
    let detail = detail(&id)?;
    Ok(Details {
        body: detail.description,
        ..Default::default()
    })
}

pub fn monday_item_thread(id: String) -> Result<Thread, String> {
    let detail = detail(&id)?;
    Ok(Thread {
        comments: detail
            .comments
            .into_iter()
            // monday sends the newest first; the Inbox reads top to bottom.
            .rev()
            .enumerate()
            .map(|(at, comment)| Comment {
                id: format!("{id}:{at}"),
                kind: "comment".into(),
                author: comment.author,
                body: comment.body,
                created_at: comment.created_at.unwrap_or_default(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    })
}

pub fn monday_item_comment(id: String, body: String) -> Result<String, String> {
    tracker()?.comment(&id, &body).map_err(|e| e.to_string())?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{
        BoardSchema, Group, MondayBinding, PeopleCell, StatusCell, StatusOption,
    };

    fn snapshot() -> BoardSnapshot {
        BoardSnapshot {
            schema: BoardSchema {
                id: "9".into(),
                name: "Sprint".into(),
                statuses: vec![(
                    "status".into(),
                    vec![
                        StatusOption {
                            label: "Working on it".into(),
                            color: None,
                            done: false,
                        },
                        StatusOption {
                            label: "Done".into(),
                            color: None,
                            done: true,
                        },
                    ],
                )],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn work(id: &str, status: &str) -> WorkItem {
        WorkItem {
            provider: Provider::Monday,
            id: id.into(),
            board_id: "9".into(),
            title: format!("Item {id}"),
            url: format!("https://t.monday.com/boards/9/pulses/{id}"),
            group: Some(Group {
                id: "g".into(),
                title: "Sprint 4".into(),
                color: Some("#579bfc".into()),
            }),
            statuses: vec![StatusCell {
                column: "status".into(),
                label: status.into(),
                color: None,
            }],
            people: vec![PeopleCell {
                column: "person".into(),
                people: vec!["7".into()],
                names: vec!["Ayşe".into()],
            }],
            updated_at: Some("2026-11-20T10:00:00Z".into()),
        }
    }

    #[test]
    fn an_item_becomes_an_inbox_row_on_its_board() {
        let row = inbox_item(&work("12", "Working on it"), &snapshot());
        assert_eq!(
            (row.provider.as_str(), row.number, row.state.as_str()),
            ("monday", 12, "Working on it")
        );
        assert_eq!(row.state_type, "started");
        assert_eq!(
            (row.project_name.as_str(), row.team_name.as_str()),
            ("Sprint", "Sprint 4")
        );
        assert_eq!(row.assignees[0].login, "Ayşe");
        assert_eq!(row.status(), "Open");
        let done = inbox_item(&work("13", "Done"), &snapshot());
        assert_eq!(done.state_type, "completed");
        assert_eq!(done.status(), "Closed");
        assert_ne!(row.key(), done.key());
    }

    #[test]
    fn the_inbox_follows_chosen_boards_and_those_projects_bind() {
        let project = tempfile::tempdir().unwrap();
        ProjectIntegrations {
            monday: Some(MondayBinding {
                board: "9".into(),
                ..Default::default()
            }),
        }
        .save(project.path())
        .unwrap();
        let other = tempfile::tempdir().unwrap();
        let boards = inbox_boards(
            &["5".into(), "9".into()],
            &[project.path().into(), other.path().into()],
        );
        assert_eq!(boards, ["5", "9"]);
    }

    #[test]
    fn cached_boards_list_without_a_request() {
        let app = tempfile::tempdir().unwrap();
        let mut fresh = snapshot();
        fresh.items = vec![work("1", "Working on it"), work("2", "Done")];
        fresh.fetched_at = crate::time::now_ms();
        cache(app.path()).save_board(&fresh).unwrap();
        cache(app.path())
            .save_account(&Account {
                id: "7".into(),
                ..Default::default()
            })
            .unwrap();
        let open = monday_list_items(app.path().into(), &["9".into()], true, "open", 40).unwrap();
        assert_eq!(
            open.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["1"]
        );
        let all = monday_list_items(app.path().into(), &["9".into()], false, "all", 40).unwrap();
        assert_eq!(all.len(), 2);
    }
}
