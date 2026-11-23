//! monday.com notes: a note that shows one monday board, its items in
//! columns by status or group. The body keeps only which board and how it
//! is shown, as JSON; the items come from [`crate::integrations`].
use crate::integrations::{BoardSnapshot, WorkItem};
use serde::{Deserialize, Serialize};

/// The note type a monday note carries in the note languages table.
pub const MONDAY: &str = "monday";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    /// The labels of the board's first status column.
    #[default]
    Status,
    /// The board's groups.
    Group,
}

impl GroupBy {
    pub fn label(self) -> &'static str {
        match self {
            GroupBy::Status => "Status",
            GroupBy::Group => "Group",
        }
    }
}

/// What a monday note's body holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MondayNote {
    /// The board id; empty until one is chosen.
    #[serde(default)]
    pub board: String,
    #[serde(default)]
    pub board_name: String,
    #[serde(default)]
    pub group_by: GroupBy,
    /// Only the items assigned to the signed-in user.
    #[serde(default)]
    pub mine: bool,
}

impl MondayNote {
    /// The settings a note's body holds; an empty or unreadable body has no board.
    pub fn parse(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn has_board(&self) -> bool {
        !self.board.trim().is_empty()
    }

    /// The list's one-line summary.
    pub fn summary(&self) -> String {
        if !self.has_board() {
            "Choose a monday.com board".into()
        } else if self.board_name.trim().is_empty() {
            format!("monday.com board {}", self.board)
        } else {
            format!("monday.com · {}", self.board_name.trim())
        }
    }
}

/// One column of the note's board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane<'a> {
    pub id: String,
    pub title: String,
    /// monday's colour: `#rrggbb` or a name like `green`.
    pub color: Option<String>,
    pub items: Vec<&'a WorkItem>,
}

/// The status column a board is laid out by: its first.
pub fn status_column(snapshot: &BoardSnapshot) -> Option<&str> {
    snapshot
        .schema
        .statuses
        .first()
        .map(|(id, _)| id.as_str())
        .or_else(|| {
            snapshot
                .schema
                .columns_of("status")
                .next()
                .map(|c| c.id.as_str())
        })
}

/// The board's columns in its own order: a status column's labels or the
/// groups, then labels or groups items carry that the schema does not list,
/// then "No status" or "No group" when an item has none. `mine` keeps only
/// the items assigned to that user.
pub fn lanes<'a>(
    snapshot: &'a BoardSnapshot,
    group_by: GroupBy,
    mine: Option<&str>,
) -> Vec<Lane<'a>> {
    let items = snapshot
        .items
        .iter()
        .filter(|item| mine.is_none_or(|me| item.assigned_to(me, None)));
    let mut lanes: Vec<Lane<'a>> = Vec::new();
    let mut none = Vec::new();
    match group_by {
        GroupBy::Status => {
            let column = status_column(snapshot);
            if let Some(column) = column {
                lanes.extend(
                    snapshot
                        .schema
                        .status_options(column)
                        .iter()
                        .map(|option| Lane {
                            id: format!("status:{}", option.label),
                            title: option.label.clone(),
                            color: option.color.clone(),
                            items: Vec::new(),
                        }),
                );
            }
            for item in items {
                let Some(cell) = column
                    .and_then(|column| item.status(Some(column)))
                    .filter(|cell| !cell.label.trim().is_empty())
                else {
                    none.push(item);
                    continue;
                };
                let id = format!("status:{}", cell.label);
                match lanes.iter_mut().find(|lane| lane.id == id) {
                    Some(lane) => lane.items.push(item),
                    None => lanes.push(Lane {
                        id,
                        title: cell.label.clone(),
                        color: cell.color.clone(),
                        items: vec![item],
                    }),
                }
            }
            if !none.is_empty() {
                lanes.push(Lane {
                    id: "none".into(),
                    title: "No status".into(),
                    color: None,
                    items: none,
                });
            }
        }
        GroupBy::Group => {
            lanes.extend(snapshot.schema.groups.iter().map(|group| Lane {
                id: format!("group:{}", group.id),
                title: group.title.clone(),
                color: group.color.clone(),
                items: Vec::new(),
            }));
            for item in items {
                let Some(group) = &item.group else {
                    none.push(item);
                    continue;
                };
                let id = format!("group:{}", group.id);
                match lanes.iter_mut().find(|lane| lane.id == id) {
                    Some(lane) => lane.items.push(item),
                    None => lanes.push(Lane {
                        id,
                        title: group.title.clone(),
                        color: group.color.clone(),
                        items: vec![item],
                    }),
                }
            }
            if !none.is_empty() {
                lanes.push(Lane {
                    id: "none".into(),
                    title: "No group".into(),
                    color: None,
                    items: none,
                });
            }
        }
    }
    lanes
}

/// The board as Markdown lists, for a chat or another reader.
pub fn to_markdown(
    title: &str,
    note: &MondayNote,
    snapshot: &BoardSnapshot,
    mine: Option<&str>,
) -> String {
    let mut out = format!("# {}\n\n", title.trim());
    let name = if snapshot.schema.name.is_empty() {
        &note.board_name
    } else {
        &snapshot.schema.name
    };
    match snapshot.schema.url.as_deref().filter(|url| !url.is_empty()) {
        Some(url) => out.push_str(&format!("monday.com board [{name}]({url})\n")),
        None => out.push_str(&format!("monday.com board {name}\n")),
    }
    for lane in lanes(snapshot, note.group_by, mine) {
        if lane.items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {}\n\n", lane.title));
        for item in lane.items {
            if item.url.is_empty() {
                out.push_str(&format!("- {}", item.title));
            } else {
                out.push_str(&format!("- [{}]({})", item.title, item.url));
            }
            let names = assignees(item);
            if !names.is_empty() {
                out.push_str(&format!(" ({})", names.join(", ")));
            }
            out.push('\n');
        }
    }
    out
}

/// The names in an item's people columns, each once.
pub fn assignees(item: &WorkItem) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in item.people.iter().flat_map(|cell| cell.names.iter()) {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{BoardSchema, Group, PeopleCell, Provider, StatusCell, StatusOption};

    fn option(label: &str) -> StatusOption {
        StatusOption {
            label: label.into(),
            color: Some("#00c875".into()),
            done: label == "Done",
        }
    }

    fn item(id: &str, status: Option<&str>, group: Option<&str>, person: &str) -> WorkItem {
        WorkItem {
            provider: Provider::Monday,
            id: id.into(),
            board_id: "9".into(),
            title: format!("Item {id}"),
            url: format!("https://t.monday.com/boards/9/pulses/{id}"),
            group: group.map(|g| Group {
                id: g.into(),
                title: g.to_uppercase(),
                color: None,
            }),
            statuses: status
                .map(|label| StatusCell {
                    column: "status".into(),
                    label: label.into(),
                    color: None,
                })
                .into_iter()
                .collect(),
            people: vec![PeopleCell {
                column: "person".into(),
                people: vec![person.into()],
                names: vec![format!("User {person}")],
            }],
            updated_at: None,
        }
    }

    fn snapshot() -> BoardSnapshot {
        BoardSnapshot {
            schema: BoardSchema {
                id: "9".into(),
                name: "Sprint".into(),
                url: Some("https://t.monday.com/boards/9".into()),
                groups: vec![Group {
                    id: "a".into(),
                    title: "A".into(),
                    color: None,
                }],
                statuses: vec![(
                    "status".into(),
                    vec![option("Working on it"), option("Stuck"), option("Done")],
                )],
                ..Default::default()
            },
            items: vec![
                item("1", Some("Done"), Some("a"), "7"),
                item("2", Some("Working on it"), Some("b"), "8"),
                item("3", None, None, "7"),
                item("4", Some("Waiting"), Some("a"), "7"),
            ],
            fetched_at: 1,
            truncated: false,
        }
    }

    fn shape(lanes: &[Lane]) -> Vec<(String, Vec<String>)> {
        lanes
            .iter()
            .map(|lane| {
                (
                    lane.title.clone(),
                    lane.items.iter().map(|item| item.id.clone()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_note_body_round_trips_and_an_empty_one_asks_for_a_board() {
        let note = MondayNote {
            board: "9".into(),
            board_name: "Sprint".into(),
            group_by: GroupBy::Group,
            mine: true,
        };
        let json = note.to_json();
        assert!(json.contains("\"boardName\": \"Sprint\""), "{json}");
        assert!(json.contains("\"groupBy\": \"group\""), "{json}");
        assert_eq!(MondayNote::parse(&json), note);
        let empty = MondayNote::parse("");
        assert!(!empty.has_board());
        assert_eq!(empty.group_by, GroupBy::Status);
        assert_eq!(MondayNote::parse(r#"{"board":"5"}"#).board, "5");
        assert_eq!(note.summary(), "monday.com · Sprint");
        assert_eq!(empty.summary(), "Choose a monday.com board");
        assert_eq!(crate::notes::note_type(MONDAY), MONDAY);
        assert_eq!(crate::notes::matching_board_types("mond")[0].0, MONDAY);
    }

    #[test]
    fn lanes_follow_the_status_labels_then_strays_then_none() {
        let snapshot = snapshot();
        let lanes = lanes(&snapshot, GroupBy::Status, None);
        let expected: Vec<(String, Vec<String>)> = vec![
            ("Working on it".into(), vec!["2".into()]),
            ("Stuck".into(), vec![]),
            ("Done".into(), vec!["1".into()]),
            ("Waiting".into(), vec!["4".into()]),
            ("No status".into(), vec!["3".into()]),
        ];
        assert_eq!(shape(&lanes), expected);
    }

    #[test]
    fn lanes_by_group_and_only_mine() {
        let snapshot = snapshot();
        let lanes = lanes(&snapshot, GroupBy::Group, Some("7"));
        let expected: Vec<(String, Vec<String>)> = vec![
            ("A".into(), vec!["1".into(), "4".into()]),
            ("No group".into(), vec!["3".into()]),
        ];
        assert_eq!(shape(&lanes), expected);
        let markdown = to_markdown("Plan", &MondayNote::default(), &snapshot, None);
        assert!(markdown.contains("monday.com board [Sprint](https://t.monday.com/boards/9)"));
        assert!(markdown
            .contains("## Done\n\n- [Item 1](https://t.monday.com/boards/9/pulses/1) (User 7)"));
        assert!(!markdown.contains("## Stuck"));
    }
}
