//! What was last fetched, kept on disk: boards open at once without a
//! request, and free plans (200 calls a day) last the day.
use super::{Account, BoardSnapshot, Container, Provider};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// How long a board snapshot counts as fresh; older ones are shown and refetched.
pub const FRESH_MS: u64 = 5 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// The cache under the app's data directory.
    pub fn new(data_dir: &Path, provider: Provider) -> Self {
        Self {
            dir: data_dir.join("integrations").join(provider.id()),
        }
    }

    fn read<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        std::fs::read(self.dir.join(name))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let path = self.dir.join(name);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn account(&self) -> Option<Account> {
        self.read("account.json")
    }

    pub fn save_account(&self, account: &Account) -> Result<(), String> {
        self.write("account.json", account)
    }

    pub fn boards(&self) -> Option<Vec<Container>> {
        self.read("boards.json")
    }

    pub fn save_boards(&self, boards: &[Container]) -> Result<(), String> {
        self.write("boards.json", &boards)
    }

    pub fn board(&self, id: &str) -> Option<BoardSnapshot> {
        self.read(&board_file(id))
    }

    pub fn save_board(&self, snapshot: &BoardSnapshot) -> Result<(), String> {
        self.write(&board_file(&snapshot.schema.id), snapshot)
    }

    /// Forgets everything, as on disconnect: the next account may see other boards.
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn board_file(id: &str) -> String {
    let safe: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    format!("board-{safe}.json")
}

impl BoardSnapshot {
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.fetched_at) < FRESH_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::BoardSchema;

    #[test]
    fn snapshots_and_the_account_are_kept_until_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path(), Provider::Monday);
        assert!(cache.board("1").is_none());
        let snapshot = BoardSnapshot {
            schema: BoardSchema {
                id: "1".into(),
                name: "Sprint".into(),
                ..Default::default()
            },
            fetched_at: 1_000,
            ..Default::default()
        };
        cache.save_board(&snapshot).unwrap();
        cache
            .save_account(&Account {
                id: "7".into(),
                name: "Ayşe".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cache.board("1"), Some(snapshot.clone()));
        assert_eq!(cache.account().unwrap().name, "Ayşe");
        assert!(snapshot.is_fresh(1_000 + FRESH_MS - 1));
        assert!(!snapshot.is_fresh(1_000 + FRESH_MS));
        // A hostile id cannot leave the cache directory.
        assert_eq!(board_file("../../etc"), "board-etc.json");
        cache.clear();
        assert!(cache.account().is_none());
    }
}
