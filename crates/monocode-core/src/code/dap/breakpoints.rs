//! The breakpoints of the app, independent of any debug session: line
//! breakpoints per absolute file path and the exception filters. The editor
//! edits and draws them, the interface persists them as JSON, and running
//! sessions listen for changes to pass on to their adapters and write back
//! what the adapter made of each breakpoint.
use super::client::lock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What a running debugger said about a breakpoint. Not persisted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BreakpointStatus {
    /// No session has answered for it (none runs, or it is disabled).
    #[default]
    Idle,
    /// Set, at this line: the adapter moves a breakpoint that sits on a
    /// line without code to the next line that has some.
    Verified { line: u32 },
    /// The adapter could not set it.
    Rejected { message: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    /// Stable while the store lives; a new one is given on load.
    #[serde(skip)]
    pub id: u64,
    /// 1-based.
    pub line: u32,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Stop only when this expression is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Stop only on these hits, in the adapter's syntax (`5`, `>= 3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    /// Print this instead of stopping; `{expression}` is interpolated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
    #[serde(skip)]
    pub status: BreakpointStatus,
}

fn enabled_by_default() -> bool {
    true
}

impl Breakpoint {
    pub fn at(line: u32) -> Self {
        Self {
            id: 0,
            line,
            enabled: true,
            condition: None,
            hit_condition: None,
            log_message: None,
            status: BreakpointStatus::Idle,
        }
    }

    pub fn with_condition(mut self, condition: &str) -> Self {
        self.condition = Some(condition.to_string());
        self
    }
}

/// When to stop on an exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionFilters {
    /// Whenever one is raised, caught or not.
    #[serde(default)]
    pub raised: bool,
    /// When one is about to end the program.
    #[serde(default = "enabled_by_default")]
    pub uncaught: bool,
}

impl Default for ExceptionFilters {
    fn default() -> Self {
        Self {
            raised: false,
            uncaught: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BreakpointChange {
    /// The breakpoints of this file were added, removed, edited or moved.
    File(PathBuf),
    /// Only what a debugger reported about this file's breakpoints changed.
    Status(PathBuf),
    Exceptions,
}

type Listener = Box<dyn FnMut(&BreakpointChange) -> bool + Send>;

#[derive(Default)]
struct Inner {
    files: BTreeMap<PathBuf, Vec<Breakpoint>>,
    exceptions: ExceptionFilters,
    next_id: u64,
    listeners: Vec<Listener>,
}

/// A handle to the store; clones share it. Every call is cheap and safe on
/// the interface thread.
#[derive(Clone, Default)]
pub struct BreakpointStore {
    inner: Arc<Mutex<Inner>>,
}

impl Inner {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn tidy(&mut self, path: &Path) {
        if let Some(list) = self.files.get_mut(path) {
            list.sort_by_key(|breakpoint| breakpoint.line);
            list.dedup_by_key(|breakpoint| breakpoint.line);
            if list.is_empty() {
                self.files.remove(path);
            }
        }
    }
}

impl BreakpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes from now on. A receiver that is dropped is forgotten.
    pub fn subscribe(&self) -> async_channel::Receiver<BreakpointChange> {
        let (sender, receiver) = async_channel::unbounded();
        self.listen(move |change| sender.try_send(change.clone()).is_ok());
        receiver
    }

    /// Calls `listener` on every change, on the thread that made it, until
    /// it returns false. It must not call back into the store.
    pub(crate) fn listen(&self, listener: impl FnMut(&BreakpointChange) -> bool + Send + 'static) {
        lock(&self.inner).listeners.push(Box::new(listener));
    }

    fn change<T>(&self, edit: impl FnOnce(&mut Inner) -> (T, Vec<BreakpointChange>)) -> T {
        let mut inner = lock(&self.inner);
        let (result, changes) = edit(&mut inner);
        for change in &changes {
            inner.listeners.retain_mut(|listener| listener(change));
        }
        result
    }

    /// The breakpoints of a file, by line.
    pub fn in_file(&self, path: &Path) -> Vec<Breakpoint> {
        lock(&self.inner)
            .files
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    pub fn files(&self) -> Vec<PathBuf> {
        lock(&self.inner).files.keys().cloned().collect()
    }

    /// Adds a plain breakpoint on the line, or removes the one there.
    /// Returns whether the line has one now.
    pub fn toggle(&self, path: &Path, line: u32) -> bool {
        if self.remove(path, line) {
            return false;
        }
        self.set(path, Breakpoint::at(line));
        true
    }

    /// Puts `breakpoint` on its line, replacing what was there, and returns
    /// its id. Editing a condition or the enabled flag is a `set` as well.
    pub fn set(&self, path: &Path, mut breakpoint: Breakpoint) -> u64 {
        self.change(|inner| {
            let existing = inner
                .files
                .get(path)
                .and_then(|list| list.iter().find(|other| other.line == breakpoint.line))
                .map(|other| other.id);
            breakpoint.id = existing.unwrap_or_else(|| inner.id());
            breakpoint.status = BreakpointStatus::Idle;
            let id = breakpoint.id;
            let list = inner.files.entry(path.to_path_buf()).or_default();
            list.retain(|other| other.line != breakpoint.line);
            list.push(breakpoint);
            inner.tidy(path);
            (id, vec![BreakpointChange::File(path.to_path_buf())])
        })
    }

    pub fn remove(&self, path: &Path, line: u32) -> bool {
        self.change(|inner| {
            let Some(list) = inner.files.get_mut(path) else {
                return (false, Vec::new());
            };
            let before = list.len();
            list.retain(|breakpoint| breakpoint.line != line);
            if list.len() == before {
                return (false, Vec::new());
            }
            inner.tidy(path);
            (true, vec![BreakpointChange::File(path.to_path_buf())])
        })
    }

    /// Removes the breakpoints of one file, or all of them.
    pub fn clear(&self, path: Option<&Path>) {
        self.change(|inner| {
            let removed: Vec<PathBuf> = match path {
                Some(path) => inner
                    .files
                    .remove_entry(path)
                    .map(|(key, _)| key)
                    .into_iter()
                    .collect(),
                None => std::mem::take(&mut inner.files).into_keys().collect(),
            };
            (
                (),
                removed.into_iter().map(BreakpointChange::File).collect(),
            )
        });
    }

    pub fn exceptions(&self) -> ExceptionFilters {
        lock(&self.inner).exceptions
    }

    pub fn set_exceptions(&self, filters: ExceptionFilters) {
        self.change(|inner| {
            if inner.exceptions == filters {
                return ((), Vec::new());
            }
            inner.exceptions = filters;
            ((), vec![BreakpointChange::Exceptions])
        });
    }

    /// The file's text changed: at line `start` (1-based), `removed` lines
    /// went and `inserted` lines came in their place. Breakpoints below move
    /// with their lines. One on a replaced line stays while its line still
    /// exists in the replacement and goes with a line that was deleted.
    pub fn lines_changed(&self, path: &Path, start: u32, removed: u32, inserted: u32) {
        if removed == inserted {
            return;
        }
        self.change(|inner| {
            let Some(list) = inner.files.get_mut(path) else {
                return ((), Vec::new());
            };
            let end = start.saturating_add(removed);
            let before = list.clone();
            list.retain_mut(|breakpoint| {
                if breakpoint.line < start {
                    return true;
                }
                if breakpoint.line >= end {
                    breakpoint.line = breakpoint.line - removed + inserted;
                    return true;
                }
                breakpoint.line - start < inserted
            });
            if *list == before {
                return ((), Vec::new());
            }
            for breakpoint in list.iter_mut() {
                // The adapter's answer was about the old line.
                breakpoint.status = BreakpointStatus::Idle;
            }
            inner.tidy(path);
            ((), vec![BreakpointChange::File(path.to_path_buf())])
        });
    }

    /// The store as JSON for the interface to persist: everything, or only
    /// the files under a project's folder.
    pub fn to_json(&self, under: Option<&Path>) -> Value {
        let inner = lock(&self.inner);
        let files: serde_json::Map<String, Value> = inner
            .files
            .iter()
            .filter(|(path, _)| under.is_none_or(|folder| path.starts_with(folder)))
            .map(|(path, list)| (path.to_string_lossy().into_owned(), json!(list)))
            .collect();
        json!({ "version": 1, "exceptions": inner.exceptions, "files": files })
    }

    /// Takes over what `to_json` wrote: the listed files replace the store's
    /// entries for them, other files are kept. Unreadable parts are skipped.
    pub fn load_json(&self, value: &Value) {
        self.change(|inner| {
            let mut changes = Vec::new();
            if let Some(filters) = value
                .get("exceptions")
                .and_then(|filters| ExceptionFilters::deserialize(filters).ok())
            {
                if inner.exceptions != filters {
                    inner.exceptions = filters;
                    changes.push(BreakpointChange::Exceptions);
                }
            }
            let files = value.get("files").and_then(Value::as_object);
            for (path, list) in files.into_iter().flatten() {
                let path = PathBuf::from(path);
                let Ok(mut list) = Vec::<Breakpoint>::deserialize(list) else {
                    continue;
                };
                for breakpoint in &mut list {
                    breakpoint.id = inner.id();
                }
                inner.files.insert(path.clone(), list);
                inner.tidy(&path);
                changes.push(BreakpointChange::File(path));
            }
            ((), changes)
        });
    }

    /// Records what an adapter answered for breakpoints of `path`, by id.
    pub(crate) fn report(&self, path: &Path, answers: &[(u64, BreakpointStatus)]) {
        self.change(|inner| {
            let mut changed = false;
            for breakpoint in inner.files.get_mut(path).into_iter().flatten() {
                let answer = answers.iter().find(|(id, _)| *id == breakpoint.id);
                if let Some((_, status)) = answer {
                    changed |= breakpoint.status != *status;
                    breakpoint.status = status.clone();
                }
            }
            let changes = changed.then(|| BreakpointChange::Status(path.to_path_buf()));
            ((), changes.into_iter().collect())
        });
    }

    /// Forgets every adapter's answers, when the last session has ended.
    pub(crate) fn reset_status(&self) {
        self.change(|inner| {
            let mut changes = Vec::new();
            for (path, list) in &mut inner.files {
                let mut changed = false;
                for breakpoint in list {
                    changed |= breakpoint.status != BreakpointStatus::Idle;
                    breakpoint.status = BreakpointStatus::Idle;
                }
                if changed {
                    changes.push(BreakpointChange::Status(path.clone()));
                }
            }
            ((), changes)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(store: &BreakpointStore, path: &Path) -> Vec<u32> {
        store.in_file(path).iter().map(|b| b.line).collect()
    }

    fn drain(receiver: &async_channel::Receiver<BreakpointChange>) -> Vec<BreakpointChange> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn edits_are_reported_and_kept_in_line_order() {
        let store = BreakpointStore::new();
        let changes = store.subscribe();
        let path = Path::new("/p/a.py");
        assert!(store.toggle(path, 9));
        assert!(store.toggle(path, 3));
        let id = store.set(path, Breakpoint::at(3).with_condition("x > 1"));
        assert_eq!(lines(&store, path), [3, 9]);
        let first = &store.in_file(path)[0];
        assert_eq!((first.id, first.condition.as_deref()), (id, Some("x > 1")));
        assert!(!store.toggle(path, 9));
        assert!(!store.remove(path, 9));
        assert_eq!(drain(&changes).len(), 4);
        assert_eq!(store.files(), [path.to_path_buf()]);

        store.set_exceptions(ExceptionFilters::default());
        assert!(drain(&changes).is_empty());
        store.set_exceptions(ExceptionFilters {
            raised: true,
            uncaught: true,
        });
        assert_eq!(drain(&changes), [BreakpointChange::Exceptions]);

        store.clear(None);
        assert!(store.files().is_empty());
        assert_eq!(drain(&changes), [BreakpointChange::File(path.into())]);
    }

    #[test]
    fn breakpoints_follow_their_lines() {
        // (start, removed, inserted) applied to breakpoints on 2, 5, 6 and 9.
        let cases: [(u32, u32, u32, &[u32]); 8] = [
            (6, 0, 1, &[2, 5, 7, 10]), // a line inserted below 5
            (5, 0, 2, &[2, 7, 8, 11]), // two lines inserted above 5
            (1, 1, 0, &[1, 4, 5, 8]),  // the first line deleted
            (5, 1, 0, &[2, 5, 8]),     // the line of a breakpoint deleted
            (5, 2, 0, &[2, 7]),        // both deleted
            (5, 2, 1, &[2, 5, 8]),     // two lines became one
            (4, 3, 5, &[2, 5, 6, 11]), // a block replaced by a longer one
            (20, 3, 0, &[2, 5, 6, 9]), // an edit below everything
        ];
        for (start, removed, inserted, expected) in cases {
            let store = BreakpointStore::new();
            let path = Path::new("/p/a.py");
            for line in [2, 5, 6, 9] {
                store.toggle(path, line);
            }
            let changes = store.subscribe();
            store.lines_changed(path, start, removed, inserted);
            assert_eq!(
                lines(&store, path),
                expected,
                "{start} -{removed} +{inserted}"
            );
            assert_eq!(drain(&changes).len(), usize::from(start < 20));
        }
    }

    #[test]
    fn the_store_round_trips_through_json() {
        let store = BreakpointStore::new();
        let inside = Path::new("/p/a.py");
        let outside = Path::new("/q/b.py");
        let mut detailed = Breakpoint::at(4).with_condition("n == 2");
        detailed.hit_condition = Some("3".into());
        detailed.log_message = Some("n is {n}".into());
        detailed.enabled = false;
        store.set(inside, detailed);
        store.toggle(outside, 1);
        store.set_exceptions(ExceptionFilters {
            raised: true,
            uncaught: false,
        });

        let saved = store.to_json(Some(Path::new("/p")));
        assert_eq!(
            saved["files"],
            json!({ "/p/a.py": [{
                "line": 4, "enabled": false, "condition": "n == 2",
                "hitCondition": "3", "logMessage": "n is {n}",
            }] })
        );
        let loaded = BreakpointStore::new();
        loaded.toggle(outside, 7);
        loaded.load_json(&saved);
        assert_eq!(loaded.exceptions(), store.exceptions());
        assert_eq!(lines(&loaded, outside), [7]);
        let restored = loaded.in_file(inside);
        assert_eq!(restored.len(), 1);
        assert!(!restored[0].enabled);
        assert_eq!(restored[0].hit_condition.as_deref(), Some("3"));
        assert_ne!(restored[0].id, 0);

        // Damaged input is skipped, never fatal.
        loaded.load_json(&json!({ "files": { "/p/a.py": "nonsense" }, "exceptions": 4 }));
        assert_eq!(lines(&loaded, inside), [4]);
        assert_eq!(
            store.to_json(None)["files"].as_object().map(|m| m.len()),
            Some(2)
        );
    }

    #[test]
    fn an_adapters_answers_are_status_changes_only() {
        let store = BreakpointStore::new();
        let path = Path::new("/p/a.py");
        let id = store.set(path, Breakpoint::at(4));
        let changes = store.subscribe();
        store.report(path, &[(id, BreakpointStatus::Verified { line: 5 })]);
        store.report(path, &[(id, BreakpointStatus::Verified { line: 5 })]);
        assert_eq!(drain(&changes), [BreakpointChange::Status(path.into())]);
        assert_eq!(
            store.in_file(path)[0].status,
            BreakpointStatus::Verified { line: 5 }
        );
        store.reset_status();
        assert_eq!(store.in_file(path)[0].status, BreakpointStatus::Idle);
        assert_eq!(drain(&changes), [BreakpointChange::Status(path.into())]);
    }
}
