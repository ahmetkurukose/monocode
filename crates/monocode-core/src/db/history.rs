//! The statements run against each data source, newest first, one JSON line
//! each under `history/<source-id>.jsonl`. Old entries are dropped so the
//! file stays small.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

const KEEP: usize = 500;
/// The file is rewritten once it holds this many lines.
const COMPACT_AT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub statement: String,
    pub elapsed_ms: f64,
    /// Rows returned or changed; `None` when the statement failed.
    pub rows: Option<u64>,
    pub error: Option<String>,
}

impl HistoryEntry {
    pub fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            root: dir.join("history"),
        }
    }

    fn file(&self, source: &str) -> PathBuf {
        let safe: String = source
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join(format!("{safe}.jsonl"))
    }

    pub fn record(&self, source: &str, entry: &HistoryEntry) {
        let path = self.file(source);
        if std::fs::create_dir_all(&self.root).is_err() {
            return;
        }
        let Ok(line) = serde_json::to_string(entry) else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{line}");
        }
        if std::fs::read_to_string(&path).is_ok_and(|text| text.lines().count() >= COMPACT_AT) {
            let kept = self.read(source);
            let text: String = kept
                .iter()
                .rev()
                .take(KEEP)
                .rev()
                .filter_map(|e| serde_json::to_string(e).ok())
                .map(|l| l + "\n")
                .collect();
            let _ = std::fs::write(&path, text);
        }
    }

    /// Oldest first, as stored.
    fn read(&self, source: &str) -> Vec<HistoryEntry> {
        std::fs::read_to_string(self.file(source))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Newest first, at most [`KEEP`].
    pub fn list(&self, source: &str) -> Vec<HistoryEntry> {
        let mut entries = self.read(source);
        entries.reverse();
        entries.truncate(KEEP);
        entries
    }

    pub fn clear(&self, source: &str) {
        let _ = std::fs::remove_file(self.file(source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u64) -> HistoryEntry {
        HistoryEntry {
            at: n,
            statement: format!("select {n}"),
            elapsed_ms: 1.,
            rows: Some(1),
            error: None,
        }
    }

    #[test]
    fn lists_newest_first_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        assert!(store.list("s").is_empty());
        for n in 1..=3 {
            store.record("s", &entry(n));
        }
        store.record("other", &entry(9));
        let list = store.list("s");
        assert_eq!(list.iter().map(|e| e.at).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(store.list("other").len(), 1);
        store.clear("s");
        assert!(store.list("s").is_empty());
    }

    #[test]
    fn failed_statements_keep_their_error_and_old_entries_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let mut failed = entry(1);
        failed.rows = None;
        failed.error = Some("boom".into());
        store.record("s", &failed);
        assert_eq!(store.list("s")[0].error.as_deref(), Some("boom"));
        for n in 2..=(COMPACT_AT as u64 + 5) {
            store.record("s", &entry(n));
        }
        let list = store.list("s");
        assert!(list.len() <= KEEP + 5, "{}", list.len());
        assert_eq!(
            list[0].at,
            COMPACT_AT as u64 + 5,
            "the newest survives compaction"
        );
    }
}
