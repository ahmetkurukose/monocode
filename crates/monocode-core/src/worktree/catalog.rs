//! `git worktree list` joined with stored metadata (spec §1 ownership).
use super::meta::{self, WorktreeMeta};
use crate::store::Store;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// The primary checkout.
    Main,
    /// Has a [`WorktreeMeta`].
    Managed,
    /// No meta, under the old sibling root `<main>-worktrees/`.
    Legacy,
    /// Anything else (`.claude/worktrees/*`, the user's own `git worktree add`, …).
    External,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Registered {
    pub path: String,
    pub head: String,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
}

pub(crate) fn registered(cwd: &Path) -> Result<Vec<Registered>, String> {
    let out = super::git(cwd, &["worktree", "list", "--porcelain", "-z"])?;
    let mut trees = Vec::new();
    let mut tree = Registered::default();
    for field in out.split('\0') {
        if field.is_empty() {
            if !tree.path.is_empty() {
                trees.push(std::mem::take(&mut tree));
            }
        } else if let Some(path) = field.strip_prefix("worktree ") {
            tree.path = path.into();
        } else if let Some(head) = field.strip_prefix("HEAD ") {
            tree.head = head.into();
        } else if let Some(branch) = field.strip_prefix("branch ") {
            tree.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).into());
        } else if field == "bare" {
            tree.bare = true;
        } else if field == "detached" {
            tree.detached = true;
        } else if field == "locked" || field.starts_with("locked ") {
            tree.locked = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            tree.prunable = true;
        }
    }
    if !tree.path.is_empty() {
        trees.push(tree);
    }
    Ok(trees)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// As Git reports it.
    pub path: PathBuf,
    /// `meta::key(path)`.
    pub key: String,
    pub branch: Option<String>,
    pub head: String,
    pub ownership: Ownership,
    pub meta: Option<WorktreeMeta>,
    pub locked: bool,
    pub prunable: bool,
    /// The directory no longer exists.
    pub missing: bool,
}

pub(crate) fn inside(path: &Path, dir: &Path) -> bool {
    let path = super::canonical(path);
    let dir = super::canonical(dir);
    path != dir && path.starts_with(&dir)
}

/// The repository's worktrees with ownership, from already-loaded metas.
pub fn list_with(cwd: &Path, metas: &[WorktreeMeta]) -> Result<Vec<Entry>, String> {
    super::ensure_local(cwd)?;
    let trees = registered(cwd)?;
    let Some(main) = trees.first().map(|t| PathBuf::from(&t.path)) else {
        return Ok(Vec::new());
    };
    let legacy = super::layout::legacy_root(&super::canonical(&main));
    Ok(trees
        .into_iter()
        .enumerate()
        .map(|(ix, t)| {
            let path = PathBuf::from(&t.path);
            let key = meta::key(&path);
            let found = metas
                .iter()
                .find(|m| meta::key(Path::new(&m.path)) == key)
                .cloned();
            let ownership = if ix == 0 {
                Ownership::Main
            } else if found.is_some() {
                Ownership::Managed
            } else if inside(&path, &legacy) {
                Ownership::Legacy
            } else {
                Ownership::External
            };
            Entry {
                missing: !path.is_dir(),
                key,
                path,
                branch: t.branch,
                head: t.head,
                ownership,
                meta: found.filter(|_| ix != 0),
                locked: t.locked,
                prunable: t.prunable,
            }
        })
        .collect())
}

fn metas(store: &Store) -> Result<Vec<WorktreeMeta>, String> {
    Ok(meta::all(store)?.into_iter().map(|(_, m)| m).collect())
}

/// [`list_with`] using every stored meta. The caller's borrow of `store` (and so any lock
/// it came from) lasts through the `git worktree list` call; prefer [`list_in`] off the
/// main thread.
pub fn list(cwd: &Path, store: &Store) -> Result<Vec<Entry>, String> {
    list_with(cwd, &metas(store)?)
}

/// [`list`] that locks `store` only while reading the metas, never while Git runs.
pub fn list_in(cwd: &Path, store: &Mutex<Store>) -> Result<Vec<Entry>, String> {
    let metas = {
        let s = super::lock(store);
        metas(&s)
    };
    list_with(cwd, &metas?)
}

/// `(main checkout key, registered worktree keys)`, or `None` without any worktree.
fn live_keys(cwd: &Path) -> Result<Option<(String, HashSet<String>)>, String> {
    super::ensure_local(cwd)?;
    let trees = registered(cwd)?;
    let Some(main) = trees.first() else {
        return Ok(None);
    };
    let main_key = meta::key(Path::new(&main.path));
    let live = trees
        .iter()
        .map(|t| meta::key(Path::new(&t.path)))
        .collect();
    Ok(Some((main_key, live)))
}

/// This repository's metas (from `all`) whose path is gone and no longer registered.
fn stale(
    main_key: &str,
    live: &HashSet<String>,
    all: Vec<(String, WorktreeMeta)>,
) -> Vec<(String, WorktreeMeta)> {
    all.into_iter()
        .filter(|(_, m)| meta::key(Path::new(&m.repo)) == main_key)
        .filter(|(_, m)| {
            let path = Path::new(&m.path);
            !live.contains(&meta::key(path)) && path.symlink_metadata().is_err()
        })
        .collect()
}

/// Drop metas of this repository whose path is gone and no longer registered.
/// Returns the removed meta keys. The caller's borrow of `store` lasts through
/// `git worktree list`; prefer [`reconcile_in`] off the main thread.
pub fn reconcile(cwd: &Path, store: &Store) -> Result<Vec<String>, String> {
    let Some((main_key, live)) = live_keys(cwd)? else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for (key, _) in stale(&main_key, &live, meta::all(store)?) {
        meta::delete_key(store, &key)?;
        removed.push(key);
    }
    Ok(removed)
}

/// [`reconcile`] that runs Git without the lock and locks `store` only to read and
/// delete metas. A row that changed (or whose path reappeared) in between is kept.
pub fn reconcile_in(cwd: &Path, store: &Mutex<Store>) -> Result<Vec<String>, String> {
    let Some((main_key, live)) = live_keys(cwd)? else {
        return Ok(Vec::new());
    };
    let all = {
        let s = super::lock(store);
        meta::all(&s)
    };
    let candidates = stale(&main_key, &live, all?);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let s = super::lock(store);
    let current = meta::all(&s)?;
    let mut removed = Vec::new();
    for (key, m) in candidates {
        let unchanged = current.iter().any(|(k, now)| *k == key && *now == m);
        if unchanged && Path::new(&m.path).symlink_metadata().is_err() {
            meta::delete_key(&s, &key)?;
            removed.push(key);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, run};
    use super::*;

    #[test]
    fn worktree_catalog_ownership_and_reconcile() {
        let (t, main, root) = fixture::repo();
        let base = t.path().canonicalize().unwrap();
        let store = fixture::store();
        let req = super::super::CreateRequest {
            root: Some(root.clone()),
            ..super::super::CreateRequest::new(&main, "managed")
        };
        let created = super::super::create::create(&req, &store, &|_| {}).unwrap();
        let legacy = base.join("project-worktrees/mc-native-1-2");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mc/native-1-2",
                "--",
                &legacy.to_string_lossy(),
            ],
        );
        let external = main.join(".claude/worktrees/ext");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "ext",
                "--",
                &external.to_string_lossy(),
            ],
        );
        let locked = base.join("locked");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                "--",
                &locked.to_string_lossy(),
            ],
        );
        run(
            &main,
            &["worktree", "lock", "--", &locked.to_string_lossy()],
        );

        let entries = list(&main, &super::super::lock(&store)).unwrap();
        let own = |p: &Path| {
            entries
                .iter()
                .find(|e| super::super::canonical(&e.path) == super::super::canonical(p))
                .unwrap()
                .clone()
        };
        assert_eq!(own(&main).ownership, Ownership::Main);
        let managed = own(&created.path);
        assert_eq!(managed.ownership, Ownership::Managed);
        assert_eq!(managed.meta.unwrap().display_name, "managed");
        assert_eq!(managed.branch.as_deref(), Some("managed"));
        assert_eq!(own(&legacy).ownership, Ownership::Legacy);
        assert_eq!(own(&external).ownership, Ownership::External);
        let l = own(&locked);
        assert!(l.locked && l.branch.is_none());
        // Listing from a linked worktree gives the same view.
        assert_eq!(
            list(&created.path, &super::super::lock(&store))
                .unwrap()
                .len(),
            5
        );

        // Reconcile keeps live and registered-but-missing rows, drops vanished ones.
        let gone = super::super::WorktreeMeta {
            path: base.join("vanished").to_string_lossy().into(),
            repo: main.to_string_lossy().into(),
            ..Default::default()
        };
        let other_repo = super::super::WorktreeMeta {
            path: base.join("vanished-elsewhere").to_string_lossy().into(),
            repo: "/some/other/repo".into(),
            ..Default::default()
        };
        {
            let s = super::super::lock(&store);
            meta::save(&s, &gone).unwrap();
            meta::save(&s, &other_repo).unwrap();
        }
        std::fs::remove_dir_all(&created.path).unwrap(); // still registered (prunable)
        let removed = reconcile(&main, &super::super::lock(&store)).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("/vanished"));
        let s = super::super::lock(&store);
        assert!(meta::load(&s, &created.path).unwrap().is_some());
        assert!(meta::load(&s, Path::new(&other_repo.path))
            .unwrap()
            .is_some());
        let entries = list(&main, &s).unwrap();
        let managed = entries
            .iter()
            .find(|e| e.ownership == Ownership::Managed)
            .unwrap();
        assert!(managed.missing && managed.prunable);
        drop(s);
        run(&main, &["worktree", "prune"]);
        let removed = reconcile(&main, &super::super::lock(&store)).unwrap();
        assert_eq!(removed.len(), 1);
        run(
            &main,
            &["worktree", "unlock", "--", &locked.to_string_lossy()],
        );
    }

    #[test]
    fn worktree_catalog_mutex_variants_match() {
        let (t, main, root) = fixture::repo();
        let base = t.path().canonicalize().unwrap();
        let store = fixture::store();
        let req = super::super::CreateRequest {
            root: Some(root.clone()),
            ..super::super::CreateRequest::new(&main, "managed")
        };
        let created = super::super::create::create(&req, &store, &|_| {}).unwrap();
        let gone = super::super::WorktreeMeta {
            path: base.join("vanished").to_string_lossy().into(),
            repo: main.to_string_lossy().into(),
            ..Default::default()
        };
        meta::save(&super::super::lock(&store), &gone).unwrap();
        let entries = list_in(&main, &store).unwrap();
        assert_eq!(entries, list(&main, &super::super::lock(&store)).unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].ownership, Ownership::Managed);
        std::fs::remove_dir_all(&created.path).unwrap(); // registered, so kept
        let removed = reconcile_in(&main, &store).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("/vanished"));
        assert!(reconcile_in(&main, &store).unwrap().is_empty());
        let s = store.try_lock().expect("not left locked");
        assert!(meta::load(&s, &created.path).unwrap().is_some());
        drop(s);
        assert!(list_in(Path::new("remote://env/x"), &store).is_err());
        assert!(reconcile_in(Path::new("remote://env/x"), &store).is_err());
    }
}
