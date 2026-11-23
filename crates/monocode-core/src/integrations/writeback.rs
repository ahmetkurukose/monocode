//! Telling the tracker about pull requests: when a branch named for an item
//! (`mon-1234-…`, see [`super::WorkItem::branch_name`]) opens a PR, its item
//! gets a comment with the link and moves to "In review"; when it merges, to
//! "Done". Each step is written once per PR, recorded in a ledger on disk.
use super::{
    cache::Cache, config::ProjectIntegrations, MondayBinding, Provider, StatusKey, Tracker,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrEvent {
    Opened,
    Merged,
}

impl PrEvent {
    /// From `gh`'s PR state; closed-unmerged PRs say nothing.
    pub fn from_state(state: &str) -> Option<Self> {
        match state.to_ascii_lowercase().as_str() {
            "open" => Some(PrEvent::Opened),
            "merged" => Some(PrEvent::Merged),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            PrEvent::Opened => "opened",
            PrEvent::Merged => "merged",
        }
    }

    fn status(self) -> StatusKey {
        match self {
            PrEvent::Opened => StatusKey::InReview,
            PrEvent::Merged => StatusKey::Done,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The branch is not named for an item, or the project binds no board.
    NotLinked,
    /// The team has not decided whether Monocode may write; ask, then
    /// [`decide`] and call again.
    NeedsDecision { item: String },
    /// The project turned writing off.
    Off,
    /// This step was written for this PR before.
    AlreadyWritten,
    Written {
        item: String,
        status: Option<String>,
    },
}

/// The monday item a branch was named for: `mon-1234-fix-login` → `1234`.
pub fn linked_item(branch: &str) -> Option<String> {
    let rest = branch.rsplit('/').next()?.strip_prefix("mon-")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let boundary = rest[id.len()..].chars().next();
    (!id.is_empty() && matches!(boundary, None | Some('-'))).then_some(id)
}

/// The settings file for `cwd`: its own, an enclosing folder's, or the main
/// checkout's when `cwd` is a worktree made before the file was committed.
pub fn find_config(cwd: &Path) -> Option<(PathBuf, ProjectIntegrations)> {
    for dir in cwd.ancestors() {
        if ProjectIntegrations::path(dir).is_file() {
            return Some((dir.to_path_buf(), ProjectIntegrations::load(dir)));
        }
    }
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let main = common.parent()?;
    ProjectIntegrations::path(main)
        .is_file()
        .then(|| (main.to_path_buf(), ProjectIntegrations::load(main)))
}

/// Records the team's answer in the project file.
pub fn decide(project: &Path, write: bool) -> Result<(), String> {
    let mut config = ProjectIntegrations::load(project);
    let binding = config
        .monday
        .as_mut()
        .ok_or("This project has no monday.com board")?;
    binding.write_back = Some(write);
    config.save(project)
}

struct Ledger {
    path: PathBuf,
    done: BTreeSet<String>,
}

impl Ledger {
    fn open(data_dir: &Path) -> Self {
        let path = data_dir
            .join("integrations")
            .join(Provider::Monday.id())
            .join("writeback.json");
        let done = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, done }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_vec(&self.done).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())
    }
}

/// Writes what `event` on the PR at `pr_url` means for the item `branch`
/// is named for. Cheap when there is nothing to do: no request is made
/// unless a linked, allowed and unwritten step is found.
pub fn on_pr(
    data_dir: &Path,
    cwd: &Path,
    branch: &str,
    pr_url: &str,
    event: PrEvent,
    connect: impl FnOnce() -> Result<Box<dyn Tracker>, String>,
) -> Result<Outcome, String> {
    let Some(item) = linked_item(branch) else {
        return Ok(Outcome::NotLinked);
    };
    let Some(binding) = find_config(cwd).and_then(|(_, config)| config.monday) else {
        return Ok(Outcome::NotLinked);
    };
    let key = format!("{item}:{}:{pr_url}", event.id());
    let mut ledger = Ledger::open(data_dir);
    if ledger.done.contains(&key) {
        return Ok(Outcome::AlreadyWritten);
    }
    match binding.write_back {
        None => return Ok(Outcome::NeedsDecision { item }),
        Some(false) => return Ok(Outcome::Off),
        Some(true) => {}
    }
    let tracker = connect()?;
    let status = write(tracker.as_ref(), data_dir, &binding, &item, pr_url, event)?;
    ledger.done.insert(key);
    ledger.save()?;
    Ok(Outcome::Written { item, status })
}

fn write(
    tracker: &dyn Tracker,
    data_dir: &Path,
    binding: &MondayBinding,
    item: &str,
    pr_url: &str,
    event: PrEvent,
) -> Result<Option<String>, String> {
    let note = match event {
        PrEvent::Opened => format!("Pull request opened: {pr_url}"),
        PrEvent::Merged => format!("Pull request merged: {pr_url}"),
    };
    tracker.comment(item, &note).map_err(|e| e.to_string())?;
    let Some(label) = binding.label_for(event.status()) else {
        return Ok(None);
    };
    let column = match &binding.status_column {
        Some(column) => column.clone(),
        None => Cache::new(data_dir, Provider::Monday)
            .board(&binding.board)
            .and_then(|b| b.schema.statuses.first().map(|(id, _)| id.clone()))
            .unwrap_or_else(|| "status".into()),
    };
    tracker
        .set_status(&binding.board, item, &column, label)
        .map_err(|e| e.to_string())?;
    Ok(Some(label.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{Account, BoardSnapshot, Container, ItemDetail, TrackerError};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Tracker for Fake {
        fn provider(&self) -> Provider {
            Provider::Monday
        }
        fn me(&self) -> Result<Account, TrackerError> {
            unreachable!()
        }
        fn boards(&self) -> Result<Vec<Container>, TrackerError> {
            unreachable!()
        }
        fn board(&self, _: &str) -> Result<BoardSnapshot, TrackerError> {
            unreachable!()
        }
        fn item(&self, _: &str) -> Result<ItemDetail, TrackerError> {
            unreachable!()
        }
        fn comment(&self, item: &str, body: &str) -> Result<(), TrackerError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("comment {item} {body}"));
            Ok(())
        }
        fn set_status(
            &self,
            board: &str,
            item: &str,
            column: &str,
            label: &str,
        ) -> Result<(), TrackerError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("status {board} {item} {column} {label}"));
            Ok(())
        }
    }

    fn project(write_back: Option<bool>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut binding = MondayBinding {
            board: "9".into(),
            status_column: Some("status".into()),
            write_back,
            ..Default::default()
        };
        binding
            .status_map
            .insert(StatusKey::InReview, "Review".into());
        binding.status_map.insert(StatusKey::Done, "Done".into());
        ProjectIntegrations {
            monday: Some(binding),
        }
        .save(dir.path())
        .unwrap();
        dir
    }

    #[test]
    fn branches_named_for_items_link_to_them() {
        assert_eq!(linked_item("mon-1234-fix-login").as_deref(), Some("1234"));
        assert_eq!(linked_item("mon-1234").as_deref(), Some("1234"));
        assert_eq!(linked_item("cagdas/mon-77-x").as_deref(), Some("77"));
        assert_eq!(linked_item("mon-12ab"), None);
        assert_eq!(linked_item("monday-12"), None);
        assert_eq!(linked_item("main"), None);
    }

    #[test]
    fn an_opened_pr_comments_and_moves_to_review_once() {
        let data = tempfile::tempdir().unwrap();
        let project = project(Some(true));
        let fake = Fake::default();
        let run = |fake: &Fake| {
            let fake = fake.clone();
            on_pr(
                data.path(),
                project.path(),
                "mon-12-x",
                "https://gh/pr/3",
                PrEvent::Opened,
                move || Ok(Box::new(fake)),
            )
        };
        assert_eq!(
            run(&fake).unwrap(),
            Outcome::Written {
                item: "12".into(),
                status: Some("Review".into())
            }
        );
        assert_eq!(run(&fake).unwrap(), Outcome::AlreadyWritten);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "comment 12 Pull request opened: https://gh/pr/3",
                "status 9 12 status Review"
            ]
        );
        let merged = on_pr(
            data.path(),
            project.path(),
            "mon-12-x",
            "https://gh/pr/3",
            PrEvent::Merged,
            {
                let fake = fake.clone();
                move || Ok(Box::new(fake))
            },
        );
        assert_eq!(
            merged.unwrap(),
            Outcome::Written {
                item: "12".into(),
                status: Some("Done".into())
            }
        );
    }

    #[test]
    fn nothing_is_written_until_the_team_decides() {
        let data = tempfile::tempdir().unwrap();
        let project = project(None);
        let never =
            || -> Result<Box<dyn Tracker>, String> { panic!("no request before a decision") };
        assert_eq!(
            on_pr(
                data.path(),
                project.path(),
                "mon-12-x",
                "u",
                PrEvent::Opened,
                never
            )
            .unwrap(),
            Outcome::NeedsDecision { item: "12".into() }
        );
        decide(project.path(), false).unwrap();
        assert_eq!(
            on_pr(
                data.path(),
                project.path(),
                "mon-12-x",
                "u",
                PrEvent::Opened,
                never
            )
            .unwrap(),
            Outcome::Off
        );
        assert_eq!(
            on_pr(
                data.path(),
                project.path(),
                "feature/x",
                "u",
                PrEvent::Opened,
                never
            )
            .unwrap(),
            Outcome::NotLinked
        );
    }

    #[test]
    fn a_subfolder_finds_its_projects_file() {
        let project = project(Some(true));
        let sub = project.path().join("crates/x");
        std::fs::create_dir_all(&sub).unwrap();
        let (root, config) = find_config(&sub).unwrap();
        assert_eq!(root, project.path());
        assert_eq!(config.monday.unwrap().board, "9");
    }
}
