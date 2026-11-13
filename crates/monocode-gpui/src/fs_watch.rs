//! Recursive file-system change signals for a checkout, so Git status reads
//! again when something on disk changed instead of on a fixed timer.
//!
//! Every relevant event collapses into one pending signal; build output,
//! dependency folders and Git's object store never wake a reader.
use notify::{RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Folders whose churn never changes what Git status shows.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".next",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "DerivedData",
];
/// Parts of a Git directory that change without changing status.
const IGNORED_GIT_DIRS: &[&str] = &["objects", "logs", "lfs", "hooks"];

pub(crate) struct Watch {
    _watcher: notify::RecommendedWatcher,
}

/// Watches each existing root recursively. The receiver holds at most one
/// pending signal however many files changed. None if the platform watcher
/// can't start, so callers fall back to polling.
pub(crate) fn watch(roots: &[PathBuf]) -> Option<(Watch, async_channel::Receiver<()>)> {
    watch_with(roots, |event| match event {
        Ok(event) => event.paths.iter().any(|p| relevant(p)),
        // A dropped or overflowed event stream: read again to be safe.
        Err(_) => true,
    })
}

/// What changed in a checkout since it was last taken: the paths, or
/// `None` when there were too many to keep (or the event stream dropped
/// some), which counts as a change of source.
#[derive(Clone)]
pub(crate) struct ChangedPaths(Arc<Mutex<Option<HashSet<PathBuf>>>>);

/// More changed paths than this between two reads are not looked at one by one.
const MAX_CHANGED_PATHS: usize = 200;

impl ChangedPaths {
    fn add(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = held.as_mut() {
            set.extend(paths);
            if set.len() > MAX_CHANGED_PATHS {
                *held = None;
            }
        }
    }

    fn overflow(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn take(&self) -> Option<Vec<PathBuf>> {
        let mut held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        held.replace(HashSet::new())
            .map(|set| set.into_iter().collect())
    }
}

/// Watches checkout source files recursively for browser reload-on-save.
/// Ignores all Git internals (including `index` and `HEAD`), build caches, and
/// dependency directories. The paths that changed are kept beside the signal,
/// so that the reader can leave out what Git ignores (see `source_changed`).
pub(crate) fn watch_checkout(
    roots: &[PathBuf],
) -> Option<(Watch, async_channel::Receiver<()>, ChangedPaths)> {
    let changed = ChangedPaths(Arc::new(Mutex::new(Some(HashSet::new()))));
    let seen = changed.clone();
    let (watch, rx) = watch_with(roots, move |event| {
        let Ok(event) = event else {
            // A dropped or overflowed event stream: anything may have changed.
            seen.overflow();
            return true;
        };
        let paths: Vec<PathBuf> = (event.paths.iter())
            .filter(|p| relevant_checkout(p))
            .cloned()
            .collect();
        let any = !paths.is_empty();
        seen.add(paths);
        any
    })?;
    Some((watch, rx, changed))
}

/// Whether any of `paths` is a source file of the checkout at `root`: one
/// Git does not ignore. A development server writes logs, databases and
/// build output into its project on every request; reloading a page for
/// those would load it again without end. Outside a Git checkout, or when
/// Git cannot be asked, every path counts.
pub(crate) fn source_changed(root: &Path, paths: Option<Vec<PathBuf>>) -> bool {
    let Some(paths) = paths else {
        return true;
    };
    if paths.is_empty() {
        // Read already, with the signal before this one.
        return false;
    }
    use std::io::Write;
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return true;
    };
    if let Some(mut stdin) = child.stdin.take() {
        for path in &paths {
            let _ = stdin.write_all(path.as_os_str().as_encoded_bytes());
            let _ = stdin.write_all(b"\0");
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return true;
    };
    match output.status.code() {
        // Some are ignored: the ones Git names, one per NUL.
        Some(0) => output.stdout.iter().filter(|&&b| b == 0).count() < paths.len(),
        // None is ignored.
        Some(1) => true,
        // Not a checkout, or Git failed.
        _ => true,
    }
}

fn watch_with(
    roots: &[PathBuf],
    filter: impl Fn(notify::Result<notify::Event>) -> bool + Send + 'static,
) -> Option<(Watch, async_channel::Receiver<()>)> {
    let (tx, rx) = async_channel::bounded(1);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let access = event.as_ref().is_ok_and(|event| event.kind.is_access());
        if !access && filter(event) {
            let _ = tx.try_send(());
        }
    })
    .ok()?;
    let mut watching = false;
    for root in roots {
        if root.exists() && watcher.watch(root, RecursiveMode::Recursive).is_ok() {
            watching = true;
        }
    }
    watching.then_some((Watch { _watcher: watcher }, rx))
}

fn relevant(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name == "index.lock" || name == ".DS_Store")
    {
        return false;
    }
    let mut in_git = false;
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let Some(name) = name.to_str() else {
            continue;
        };
        if in_git && IGNORED_GIT_DIRS.contains(&name) {
            return false;
        }
        if IGNORED_DIRS.contains(&name) {
            return false;
        }
        // A main checkout's `.git`, or a common dir such as `repo.git`.
        in_git = name == ".git" || name.ends_with(".git");
    }
    true
}

fn relevant_checkout(path: &Path) -> bool {
    if path
        .file_name()
        .is_some_and(|name| name == "index.lock" || name == ".DS_Store")
    {
        return false;
    }
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == ".git" || name.ends_with(".git") || IGNORED_DIRS.contains(&name) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{relevant, relevant_checkout};
    use std::path::Path;

    #[test]
    fn source_index_and_refs_wake_readers_but_build_output_does_not() {
        assert!(relevant(Path::new("/repo/src/main.rs")));
        assert!(relevant(Path::new("/repo/.git/index")));
        assert!(relevant(Path::new("/repo/.git/HEAD")));
        assert!(relevant(Path::new("/repo/.git/refs/heads/main")));
        assert!(!relevant(Path::new("/repo/.git/objects/ab/cdef")));
        assert!(!relevant(Path::new("/repo/.git/index.lock")));
        assert!(!relevant(Path::new("/repo/target/debug/app")));
        assert!(!relevant(Path::new("/repo/web/node_modules/x/index.js")));
    }

    #[test]
    fn checkout_watcher_ignores_all_git_and_build_directories() {
        assert!(relevant_checkout(Path::new("/repo/src/App.tsx")));
        assert!(relevant_checkout(Path::new("/repo/public/index.html")));
        assert!(!relevant_checkout(Path::new("/repo/.git/index")));
        assert!(!relevant_checkout(Path::new("/repo/.git/HEAD")));
        assert!(!relevant_checkout(Path::new("/repo/.git/refs/heads/main")));
        assert!(!relevant_checkout(Path::new("/repo/repo.git/HEAD")));
        assert!(!relevant_checkout(Path::new("/repo/target/debug/app")));
        assert!(!relevant_checkout(Path::new("/repo/node_modules/a/b.js")));
        assert!(!relevant_checkout(Path::new("/repo/.DS_Store")));
    }

    #[test]
    fn what_git_ignores_is_not_a_change_of_source() {
        let root = std::env::temp_dir().join(format!("monocode-fs-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("log")).unwrap();
        let root = root.canonicalize().unwrap();
        let log = root.join("log/development.log");
        let db = root.join("db.sqlite3");
        let source = root.join("app.py");
        // Not a checkout yet: every path counts.
        assert!(super::source_changed(&root, Some(vec![log.clone()])));
        let git = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(git.success());
        std::fs::write(root.join(".gitignore"), "log/\n*.sqlite3\n").unwrap();
        assert!(!super::source_changed(
            &root,
            Some(vec![log.clone(), db.clone()])
        ));
        assert!(super::source_changed(
            &root,
            Some(vec![log.clone(), source.clone()])
        ));
        assert!(super::source_changed(&root, Some(vec![source])));
        // Too many paths to keep, or a dropped event stream.
        assert!(super::source_changed(&root, None));
        // Paths read with an earlier signal.
        assert!(!super::source_changed(&root, Some(Vec::new())));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_edit_signals_once_and_build_output_stays_quiet() {
        use std::time::Duration;
        let root = std::env::temp_dir().join(format!("monocode-fs-watch-{}", std::process::id()));
        std::fs::create_dir_all(root.join("target")).unwrap();
        // Canonical, as FSEvents reports /private/var for /var.
        let root = root.canonicalize().unwrap();
        let (_watch, changes) = super::watch(std::slice::from_ref(&root)).unwrap();
        // FSEvents starts delivering shortly after the stream opens.
        std::thread::sleep(Duration::from_millis(300));
        while changes.try_recv().is_ok() {}

        std::fs::write(root.join("target/out.o"), "build").unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert!(
            changes.try_recv().is_err(),
            "build output must not wake readers"
        );

        for n in 0..5 {
            std::fs::write(root.join("main.rs"), format!("fn main() {{ {n} }}")).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while changes.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            changes.len(),
            1,
            "a burst of edits leaves one pending signal"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
