//! Work trackers (monday.com now; Jira and ClickUp later) behind one model:
//! accounts, boards, items and four statuses Monocode cares about. The UI and
//! agents talk to [`Tracker`]; each provider maps its own shapes onto it.
//!
//! Sign-in is a personal API token kept in the OS credential store. OAuth is
//! a known debt, see `docs/integrations-oauth.md`.
pub mod cache;
pub mod config;
pub mod monday;
pub mod writeback;

use serde::{Deserialize, Serialize};
use std::fmt;

pub use cache::Cache;
pub use config::{MondayBinding, ProjectIntegrations};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Monday,
}

impl Provider {
    pub const ALL: [Provider; 1] = [Provider::Monday];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Monday => "monday",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Monday => "monday.com",
        }
    }

    /// Where a user makes the personal token this provider signs in with.
    pub fn token_help_url(self) -> &'static str {
        match self {
            Provider::Monday => "https://developer.monday.com/api-reference/docs/authentication",
        }
    }

    fn credential_id(self) -> String {
        format!("integration:{}", self.id())
    }
}

/// The statuses Monocode moves an item through. Each board maps them onto
/// its own labels ([`MondayBinding::status_map`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusKey {
    Todo,
    InProgress,
    InReview,
    Done,
}

impl StatusKey {
    pub const ALL: [StatusKey; 4] = [
        StatusKey::Todo,
        StatusKey::InProgress,
        StatusKey::InReview,
        StatusKey::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StatusKey::Todo => "To do",
            StatusKey::InProgress => "In progress",
            StatusKey::InReview => "In review",
            StatusKey::Done => "Done",
        }
    }
}

/// The signed-in user.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
    /// The monday.com account (the team's organisation) the user is in.
    #[serde(default)]
    pub team: String,
    /// The account's subdomain, `<slug>.monday.com`.
    #[serde(default)]
    pub slug: String,
}

/// A board (monday), project (Jira) or list (ClickUp).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub id: String,
    pub title: String,
    /// The provider's column type: `status`, `people`, `long_text`, ...
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOption {
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// A board's columns, groups and the labels of each status column.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardSchema {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Status column id → its labels, in the board's order.
    #[serde(default)]
    pub statuses: Vec<(String, Vec<StatusOption>)>,
}

impl BoardSchema {
    pub fn columns_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Column> + 'a {
        self.columns.iter().filter(move |c| c.kind == kind)
    }

    pub fn status_options(&self, column: &str) -> &[StatusOption] {
        self.statuses
            .iter()
            .find(|(id, _)| id == column)
            .map_or(&[], |(_, options)| options.as_slice())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCell {
    pub column: String,
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeopleCell {
    pub column: String,
    /// The user ids assigned; teams are left out.
    pub people: Vec<String>,
    /// The names shown in the cell, teams included.
    #[serde(default)]
    pub names: Vec<String>,
}

/// One item (monday), issue (Jira) or task (ClickUp).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub provider: Provider,
    pub id: String,
    pub board_id: String,
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub group: Option<Group>,
    #[serde(default)]
    pub statuses: Vec<StatusCell>,
    #[serde(default)]
    pub people: Vec<PeopleCell>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl WorkItem {
    /// The label in `column`, or in the first status column when `None`.
    pub fn status(&self, column: Option<&str>) -> Option<&StatusCell> {
        match column {
            Some(column) => self.statuses.iter().find(|s| s.column == column),
            None => self.statuses.first(),
        }
    }

    /// Whether `user` is among the people in `column` (any people column when `None`).
    pub fn assigned_to(&self, user: &str, column: Option<&str>) -> bool {
        self.people
            .iter()
            .filter(|cell| column.is_none_or(|c| cell.column == c))
            .any(|cell| cell.people.iter().any(|p| p == user))
    }

    /// A branch name for work on this item: `mon-1234-fix-login-redirect`.
    pub fn branch_name(&self) -> String {
        let mut slug = String::new();
        for ch in self.title.to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch);
            } else if ch == '\u{307}' {
                // The dot `İ` keeps when lowercased.
            } else if let Some(ascii) = fold_turkish(ch) {
                slug.push(ascii);
            } else if !slug.ends_with('-') && !slug.is_empty() {
                slug.push('-');
            }
            if slug.len() >= 40 {
                break;
            }
        }
        let slug = slug.trim_matches('-');
        let prefix = match self.provider {
            Provider::Monday => "mon",
        };
        if slug.is_empty() {
            format!("{prefix}-{}", self.id)
        } else {
            format!("{prefix}-{}-{slug}", self.id)
        }
    }
}

fn fold_turkish(ch: char) -> Option<char> {
    Some(match ch {
        'ç' => 'c',
        'ğ' => 'g',
        'ı' => 'i',
        'ö' => 'o',
        'ş' => 's',
        'ü' => 'u',
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// An item with what an agent needs to work on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetail {
    pub item: WorkItem,
    /// The item's long-text columns, joined.
    #[serde(default)]
    pub description: String,
    /// The newest updates first.
    #[serde(default)]
    pub comments: Vec<Comment>,
}

impl ItemDetail {
    /// The item as context for an agent's first prompt.
    pub fn to_prompt(&self) -> String {
        let mut out = format!(
            "# {} ({} item {})\n{}\n",
            self.item.title,
            self.item.provider.label(),
            self.item.id,
            self.item.url
        );
        if let Some(status) = self.item.status(None) {
            out.push_str(&format!("Status: {}\n", status.label));
        }
        if !self.description.trim().is_empty() {
            out.push_str(&format!("\n## Description\n{}\n", self.description.trim()));
        }
        if !self.comments.is_empty() {
            out.push_str("\n## Recent updates\n");
            for comment in self.comments.iter().take(5) {
                out.push_str(&format!("- {}: {}\n", comment.author, comment.body.trim()));
            }
        }
        out
    }
}

/// A board's items as last fetched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardSnapshot {
    pub schema: BoardSchema,
    pub items: Vec<WorkItem>,
    /// Unix milliseconds.
    pub fetched_at: u64,
    /// More items exist than were fetched.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackerError {
    /// No token is saved.
    NotConnected,
    /// The token was refused: revoked, regenerated or mistyped.
    Unauthorized,
    /// A budget ran out; try again after this many seconds.
    RateLimited {
        retry_in: u64,
    },
    /// The account's daily call limit is spent until midnight UTC.
    DailyLimit,
    Network(String),
    Api(String),
}

impl fmt::Display for TrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackerError::NotConnected => {
                write!(f, "Not connected. Add a token in Settings → Integrations.")
            }
            TrackerError::Unauthorized => write!(
                f,
                "The token was refused. Paste a new one in Settings → Integrations."
            ),
            TrackerError::RateLimited { retry_in } => {
                write!(f, "Too many requests. Try again in {retry_in}s.")
            }
            TrackerError::DailyLimit => write!(
                f,
                "The account's daily API limit is used up; it resets at midnight UTC."
            ),
            TrackerError::Network(e) => write!(f, "Could not reach the service: {e}"),
            TrackerError::Api(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TrackerError {}

/// What every provider offers.
pub trait Tracker: Send + Sync {
    fn provider(&self) -> Provider;
    fn me(&self) -> Result<Account, TrackerError>;
    fn boards(&self) -> Result<Vec<Container>, TrackerError>;
    fn board(&self, id: &str) -> Result<BoardSnapshot, TrackerError>;
    fn item(&self, id: &str) -> Result<ItemDetail, TrackerError>;
    fn comment(&self, item: &str, body: &str) -> Result<(), TrackerError>;
    /// Sets `column` of `item` on `board` to the label `label`.
    fn set_status(
        &self,
        board: &str,
        item: &str,
        column: &str,
        label: &str,
    ) -> Result<(), TrackerError>;
}

/// The saved token. `Err` means the credential store could not be read.
pub fn token(provider: Provider) -> Result<Option<String>, String> {
    // One credential store item holds every Monocode secret, so macOS asks
    // for access once, not once per kind of secret.
    crate::db::source::saved_password_checked(&provider.credential_id())
}

pub fn save_token(provider: Provider, token: &str) -> Result<(), String> {
    crate::db::source::save_password(&provider.credential_id(), token.trim())
}

pub fn forget_token(provider: Provider) {
    crate::db::source::forget_password(&provider.credential_id());
}

/// A client for `provider` signed in with its saved token.
pub fn connect(provider: Provider) -> Result<Box<dyn Tracker>, TrackerError> {
    let token = token(provider)
        .map_err(TrackerError::Api)?
        .ok_or(TrackerError::NotConnected)?;
    Ok(match provider {
        Provider::Monday => Box::new(monday::Monday::new(token)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> WorkItem {
        WorkItem {
            provider: Provider::Monday,
            id: "1234".into(),
            board_id: "9".into(),
            title: title.into(),
            url: String::new(),
            group: None,
            statuses: vec![],
            people: vec![],
            updated_at: None,
        }
    }

    #[test]
    fn branch_names_are_short_ascii_slugs_with_the_item_id() {
        assert_eq!(
            item("Fix login redirect!").branch_name(),
            "mon-1234-fix-login-redirect"
        );
        assert_eq!(
            item("Ödeme ekranı çöküyor").branch_name(),
            "mon-1234-odeme-ekrani-cokuyor"
        );
        assert_eq!(item("  ").branch_name(), "mon-1234");
        assert!(item(&"x".repeat(200)).branch_name().len() <= 50);
    }

    #[test]
    fn assignment_is_checked_in_the_chosen_people_column() {
        let mut it = item("a");
        it.people = vec![
            PeopleCell {
                column: "owner".into(),
                people: vec!["7".into()],
                names: vec![],
            },
            PeopleCell {
                column: "reviewer".into(),
                people: vec!["8".into()],
                names: vec![],
            },
        ];
        assert!(it.assigned_to("7", None));
        assert!(it.assigned_to("8", None));
        assert!(it.assigned_to("7", Some("owner")));
        assert!(!it.assigned_to("8", Some("owner")));
    }

    #[test]
    fn the_prompt_carries_title_link_description_and_recent_updates() {
        let mut it = item("Fix login");
        it.url = "https://x.monday.com/boards/9/pulses/1234".into();
        it.statuses = vec![StatusCell {
            column: "status".into(),
            label: "Working on it".into(),
            color: None,
        }];
        let detail = ItemDetail {
            item: it,
            description: "Users loop back to /login.".into(),
            comments: vec![Comment {
                author: "Ayşe".into(),
                body: "Only on Safari".into(),
                created_at: None,
            }],
        };
        let prompt = detail.to_prompt();
        assert!(prompt.starts_with("# Fix login (monday.com item 1234)\nhttps://x.monday.com"));
        assert!(prompt.contains("Status: Working on it"));
        assert!(prompt.contains("Users loop back"));
        assert!(prompt.contains("- Ayşe: Only on Safari"));
    }
}
