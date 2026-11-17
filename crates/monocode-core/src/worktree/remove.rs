//! Removal preflight and removal (spec §6).
use super::{catalog, content, git, lock, meta};
use crate::store::Store;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Ignored directories that are cheap to regenerate and never reported as at risk.
pub const REGENERABLE: [&str; 11] = [
    "node_modules",
    "dist",
    "build",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".turbo",
    ".cache",
    ".git",
];
pub const AT_RISK_LIMIT: usize = 5000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preflight {
    /// As Git reports it.
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
    /// `status --porcelain` paths, minus our shared symlinks.
    pub dirty: Vec<String>,
    /// Ignored local files that would be lost; `.env*` first. At most [`AT_RISK_LIMIT`].
    pub at_risk: Vec<String>,
    pub at_risk_truncated: bool,
    /// Commits on this branch that are on no other branch and no remote.
    pub unpushed: usize,
    /// Our shared-directory symlinks inside the worktree (relative).
    pub shared_links: Vec<String>,
    /// The directory is already gone (removal only prunes).
    pub missing: bool,
}
impl Preflight {
    /// Removing would discard changes or local files; needs the user's confirmation.
    pub fn needs_force(&self) -> bool {
        !self.dirty.is_empty() || !self.at_risk.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoveOptions {
    /// The user confirmed discarding dirty/at-risk files (`git worktree remove --force`).
    pub force: bool,
    /// Delete a managed worktree's branch when Git considers it merged (`branch -d`).
    pub delete_branch: bool,
}
impl Default for RemoveOptions {
    fn default() -> Self {
        Self {
            force: false,
            delete_branch: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// The branch survived because `git branch -d` refused (unmerged); the UI may offer
    /// [`force_delete_branch`].
    pub preserved_branch: Option<String>,
    pub branch_deleted: bool,
    pub warnings: Vec<String>,
}

fn find(cwd: &Path, path: &Path) -> Result<(Vec<catalog::Registered>, usize), String> {
    let trees = catalog::registered(cwd)?;
    let key = meta::key(path);
    let ix = trees
        .iter()
        .position(|t| meta::key(Path::new(&t.path)) == key)
        .ok_or("This path is not a registered worktree of this repository")?;
    Ok((trees, ix))
}

fn status_paths(out: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = out.split('\0');
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let (xy, path) = field.split_at(3);
        paths.push(path.to_owned());
        if xy.contains(['R', 'C']) {
            fields.next(); // rename/copy source
        }
    }
    paths
}

fn regenerable(rel: &str) -> bool {
    rel.split('/').any(|s| REGENERABLE.contains(&s))
}

fn env_first(a: &String, b: &String) -> std::cmp::Ordering {
    let env = |p: &str| !p.rsplit('/').next().unwrap_or(p).starts_with(".env");
    env(a).cmp(&env(b)).then_with(|| a.cmp(b))
}

/// Refuse main, unregistered, locked, detached, and nesting worktrees; report what
/// removal would lose.
pub fn preflight(cwd: &Path, path: &Path) -> Result<Preflight, String> {
    super::ensure_local(cwd)?;
    let (trees, ix) = find(cwd, path)?;
    let tree = &trees[ix];
    if ix == 0 {
        return Err("The main working copy cannot be deleted".into());
    }
    if tree.locked {
        return Err("This worktree is locked. Unlock it in Git before deleting it.".into());
    }
    let Some(branch) = tree.branch.clone() else {
        return Err("Create a branch for this detached worktree before deleting it.".into());
    };
    let target = PathBuf::from(&tree.path);
    if let Some(nested) = trees
        .iter()
        .enumerate()
        .find(|(i, t)| *i != ix && catalog::inside(Path::new(&t.path), &target))
    {
        return Err(format!(
            "This worktree contains another worktree ({}). Delete that one first.",
            nested.1.path
        ));
    }
    let main = PathBuf::from(&trees[0].path);
    let mut pre = Preflight {
        path: target.clone(),
        branch: branch.clone(),
        head: tree.head.clone(),
        missing: !target.is_dir(),
        ..Preflight::default()
    };
    if pre.missing {
        return Ok(pre);
    }
    pre.shared_links = content::shared_links(&main, &target)
        .iter()
        .filter_map(|p| p.strip_prefix(&target).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let ours = |rel: &str| {
        pre.shared_links.iter().any(|l| {
            rel == l
                || rel
                    .strip_prefix(l.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
    };
    let status = git(
        &target,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let dirty: Vec<String> = status_paths(&status)
        .into_iter()
        .filter(|p| !ours(p.trim_end_matches('/')))
        .collect();
    // Per-file listing (not `--directory`): a collapsed `sub/` would hide `sub/.env`, and
    // `pkg/` holding only `pkg/node_modules` would look at risk. Regenerable directories are
    // excluded by pathspec so Git does not descend into them.
    let excludes: Vec<String> = REGENERABLE
        .iter()
        .flat_map(|d| {
            [
                format!(":(exclude,glob)**/{d}"),
                format!(":(exclude,glob)**/{d}/**"),
            ]
        })
        .collect();
    let mut args = vec![
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--",
        ".",
    ];
    args.extend(excludes.iter().map(String::as_str));
    let ignored = git(&target, &args)?;
    let mut at_risk: Vec<String> = ignored
        .split('\0')
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty() && !regenerable(p) && !ours(p))
        .map(str::to_owned)
        .collect();
    at_risk.sort_by(env_first);
    pre.at_risk_truncated = at_risk.len() > AT_RISK_LIMIT;
    at_risk.truncate(AT_RISK_LIMIT);
    pre.dirty = dirty;
    pre.at_risk = at_risk;
    pre.unpushed = git(
        &target,
        &[
            "rev-list",
            "--count",
            "HEAD",
            "--not",
            &format!("--exclude={branch}"),
            "--branches",
            "--remotes",
        ],
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0);
    Ok(pre)
}

fn unlink(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if path.is_dir() {
        return std::fs::remove_dir(path);
    }
    std::fs::remove_file(path)
}

fn relink(main: &Path, worktree: &Path, rel: &str) {
    let at = worktree.join(rel);
    if at.symlink_metadata().is_ok() {
        return;
    }
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(main.join(rel), at);
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_dir(main.join(rel), at);
}

/// Remove a worktree: discard parked providers, unlink our shared symlinks,
/// `git worktree remove [--force]`, delete a managed worktree's merged branch, prune,
/// and delete the meta. Legacy/external worktrees never lose their branch.
pub fn remove(
    cwd: &Path,
    path: &Path,
    opts: &RemoveOptions,
    store: &Mutex<Store>,
) -> Result<Removed, String> {
    let pre = preflight(cwd, path)?;
    if pre.needs_force() && !opts.force {
        return Err(format!(
            "This worktree has {} uncommitted change(s) and {} local file(s) that would be lost. Confirm to delete it anyway.",
            pre.dirty.len(),
            pre.at_risk.len()
        ));
    }
    let main = super::layout::main_checkout(cwd)?;
    let record = meta::load(&lock(store), &pre.path)?;
    let mut removed = Removed::default();
    crate::provider::discard_worktree(&pre.path);
    let spelled = pre.path.to_string_lossy().into_owned();
    if pre.missing {
        git(&main, &["worktree", "prune"])?;
    } else {
        let mut unlinked = Vec::new();
        for rel in &pre.shared_links {
            match unlink(&pre.path.join(rel)) {
                Ok(()) => unlinked.push(rel.clone()),
                Err(e) => {
                    for rel in &unlinked {
                        relink(&main, &pre.path, rel);
                    }
                    return Err(format!("Could not unlink shared directory {rel}: {e}"));
                }
            }
        }
        let mut args = vec!["worktree", "remove"];
        if opts.force {
            args.push("--force");
        }
        args.extend(["--", spelled.as_str()]);
        if let Err(e) = git(&main, &args) {
            for rel in &unlinked {
                relink(&main, &pre.path, rel);
            }
            return Err(e);
        }
    }
    let may_delete = opts.delete_branch
        && record
            .as_ref()
            .is_some_and(|m| !m.preserve_branch_on_delete);
    let elsewhere = catalog::registered(&main)
        .map(|t| t.iter().any(|t| t.branch.as_deref() == Some(&pre.branch)))
        .unwrap_or(true);
    if may_delete && !elsewhere {
        match git(&main, &["branch", "-d", "--", &pre.branch]) {
            Ok(_) => removed.branch_deleted = true,
            Err(_) => removed.preserved_branch = Some(pre.branch.clone()),
        }
    }
    if let Err(e) = git(&main, &["worktree", "prune"]) {
        removed
            .warnings
            .push(format!("Could not prune worktrees: {e}"));
    }
    if let Err(e) = meta::delete(&lock(store), &pre.path) {
        removed
            .warnings
            .push(format!("Could not forget the worktree: {e}"));
    }
    Ok(removed)
}

/// "Delete anyway" for a preserved branch (`git branch -D`).
pub fn force_delete_branch(cwd: &Path, branch: &str) -> Result<(), String> {
    super::ensure_local(cwd)?;
    crate::git::valid_branch(cwd, branch)?;
    if catalog::registered(cwd)?
        .iter()
        .any(|t| t.branch.as_deref() == Some(branch))
    {
        return Err(format!("{branch} is checked out in a working copy"));
    }
    git(cwd, &["branch", "-D", "--", branch]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, has_branch, run};
    use super::super::{create, CreateRequest, Created};
    use super::*;

    fn make(main: &Path, root: &Path, store: &Mutex<Store>, name: &str) -> Created {
        create::create(
            &CreateRequest {
                root: Some(root.to_path_buf()),
                ..CreateRequest::new(main, name)
            },
            store,
            &|_| {},
        )
        .unwrap()
    }

    #[test]
    fn worktree_preflight_refusals() {
        let (t, main, root) = fixture::repo();
        let store = fixture::store();
        assert!(preflight(&main, &main).unwrap_err().contains("main"));
        assert!(preflight(&main, &t.path().join("nowhere"))
            .unwrap_err()
            .contains("not a registered"));
        let wt = make(&main, &root, &store, "locked");
        run(
            &main,
            &["worktree", "lock", "--", &wt.path.to_string_lossy()],
        );
        assert!(preflight(&main, &wt.path).unwrap_err().contains("locked"));
        run(
            &main,
            &["worktree", "unlock", "--", &wt.path.to_string_lossy()],
        );
        assert!(preflight(&main, &wt.path).is_ok());
        let detached = t.path().canonicalize().unwrap().join("detached");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                "--",
                &detached.to_string_lossy(),
            ],
        );
        assert!(preflight(&main, &detached)
            .unwrap_err()
            .contains("detached"));
        let outer = make(&main, &root, &store, "outer");
        let inner = outer.path.join("inner");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "inner",
                "--",
                &inner.to_string_lossy(),
            ],
        );
        assert!(preflight(&main, &outer.path)
            .unwrap_err()
            .contains("contains another worktree"));
        assert!(remove(
            &main,
            &outer.path,
            &RemoveOptions {
                force: true,
                ..Default::default()
            },
            &store
        )
        .is_err());
        assert!(outer.path.exists());
        assert!(preflight(&main, &inner).is_ok());
        assert!(preflight(Path::new("remote://e/x"), &inner)
            .unwrap_err()
            .contains("remote"));
    }

    #[test]
    fn worktree_preflight_reports_dirty_at_risk_unpushed() {
        let (_t, main, root) = fixture::repo();
        std::fs::write(
            main.join(".gitignore"),
            ".env*\n*.pem\nnode_modules/\nbuild/\ncache/\nnotes.txt\n",
        )
        .unwrap();
        run(&main, &["add", ".gitignore"]);
        run(&main, &["commit", "-q", "-m", "Ignore"]);
        std::fs::create_dir_all(main.join("cache")).unwrap();
        std::fs::write(main.join("cache/blob"), "b").unwrap();
        std::fs::write(
            main.join("monocode.yaml"),
            "worktree:\n  sharedDirectories: [cache]\n",
        )
        .unwrap();
        let store = fixture::store();
        let wt = make(&main, &root, &store, "report");
        let p = &wt.path;
        assert_eq!(wt.content.shared, ["cache"]);
        let clean = preflight(&main, p).unwrap();
        assert!(!clean.needs_force(), "{clean:?}");
        assert_eq!(clean.shared_links, ["cache"]);
        assert_eq!(clean.unpushed, 0);

        std::fs::write(p.join("README.md"), "changed\n").unwrap();
        std::fs::write(p.join("new.txt"), "n").unwrap();
        for f in [
            "a.pem",
            ".env",
            "sub/.env.local",
            "notes.txt",
            "node_modules/x/y.js",
            "pkg/node_modules/z",
            "build/out",
        ] {
            let f = p.join(f);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, "x").unwrap();
        }
        let pre = preflight(&main, p).unwrap();
        assert_eq!(pre.dirty, ["README.md", "new.txt"]);
        assert_eq!(
            pre.at_risk,
            [".env", "sub/.env.local", "a.pem", "notes.txt"]
        );
        assert!(!pre.at_risk_truncated);
        assert!(pre.needs_force());

        run(p, &["add", "README.md"]);
        run(p, &["commit", "-q", "-m", "Work"]);
        assert_eq!(preflight(&main, p).unwrap().unpushed, 1);
        // Removing without force is refused; nothing changes.
        assert!(remove(&main, p, &RemoveOptions::default(), &store)
            .unwrap_err()
            .contains("Confirm"));
        assert!(p.join("new.txt").exists());
    }

    #[test]
    fn worktree_remove_branch_policy_and_shared_dirs_survive() {
        let (_t, main, root) = fixture::repo();
        std::fs::write(main.join(".gitignore"), "node_modules/\n").unwrap();
        run(&main, &["add", ".gitignore"]);
        run(&main, &["commit", "-q", "-m", "Ignore"]);
        std::fs::create_dir_all(main.join("node_modules/pkg")).unwrap();
        std::fs::write(main.join("node_modules/pkg/index.js"), "keep me").unwrap();
        std::fs::write(
            main.join("monocode.yaml"),
            "worktree:\n  sharedDirectories: [node_modules]\n",
        )
        .unwrap();
        let store = fixture::store();

        // Merged (no new commits): worktree, branch, and meta go; main's shared dir stays.
        let merged = make(&main, &root, &store, "merged");
        assert!(merged.path.join("node_modules/pkg/index.js").exists());
        // Remove from inside the worktree itself, with force (exercises the symlink path).
        let out = remove(
            &merged.path,
            &merged.path,
            &RemoveOptions {
                force: true,
                ..Default::default()
            },
            &store,
        )
        .unwrap();
        assert_eq!(
            out,
            Removed {
                preserved_branch: None,
                branch_deleted: true,
                warnings: vec![]
            }
        );
        assert!(!merged.path.exists());
        assert!(!has_branch(&main, "merged"));
        assert_eq!(
            std::fs::read_to_string(main.join("node_modules/pkg/index.js")).unwrap(),
            "keep me"
        );
        assert!(meta::load(&lock(&store), &merged.path).unwrap().is_none());

        // Unmerged: branch preserved, then force-deleted.
        let unmerged = make(&main, &root, &store, "unmerged");
        fixture::commit(&unmerged.path, "work.txt", "w\n", "Work");
        let out = remove(&main, &unmerged.path, &RemoveOptions::default(), &store).unwrap();
        assert_eq!(out.preserved_branch.as_deref(), Some("unmerged"));
        assert!(!out.branch_deleted);
        assert!(!unmerged.path.exists());
        assert!(has_branch(&main, "unmerged"));
        assert!(force_delete_branch(&main, "main")
            .unwrap_err()
            .contains("checked out"));
        force_delete_branch(&main, "unmerged").unwrap();
        assert!(!has_branch(&main, "unmerged"));

        // delete_branch: false keeps a merged branch.
        let keep = make(&main, &root, &store, "keep");
        let out = remove(
            &main,
            &keep.path,
            &RemoveOptions {
                delete_branch: false,
                ..Default::default()
            },
            &store,
        )
        .unwrap();
        assert_eq!(out.preserved_branch, None);
        assert!(has_branch(&main, "keep"));

        // preserve_branch_on_delete (existing branch checked out) is respected.
        let existing = create::create(
            &CreateRequest {
                root: Some(root.clone()),
                branch: Some("keep".into()),
                existing_branch: true,
                ..CreateRequest::new(&main, "existing")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert!(existing.meta.preserve_branch_on_delete);
        let out = remove(&main, &existing.path, &RemoveOptions::default(), &store).unwrap();
        assert!(!out.branch_deleted && out.preserved_branch.is_none());
        assert!(has_branch(&main, "keep"));

        // Legacy (no meta): branch kept like today.
        let legacy = crate::git::create_named_worktree(&main, "legacy-copy").unwrap();
        let out = remove(
            &main,
            Path::new(&legacy.path),
            &RemoveOptions::default(),
            &store,
        )
        .unwrap();
        assert!(!out.branch_deleted && out.preserved_branch.is_none());
        assert!(has_branch(&main, "legacy-copy"));
        assert!(!Path::new(&legacy.path).exists());
        let _ = std::fs::remove_dir(crate::worktree_management::default_root(&main));

        // A worktree whose directory vanished is pruned and forgotten.
        let vanished = make(&main, &root, &store, "vanished");
        std::fs::remove_dir_all(&vanished.path).unwrap();
        let out = remove(&main, &vanished.path, &RemoveOptions::default(), &store).unwrap();
        assert!(out.branch_deleted);
        assert_eq!(catalog::registered(&main).unwrap().len(), 1);
        assert!(meta::all(&lock(&store)).unwrap().is_empty());
        assert!(main.join("node_modules/pkg/index.js").exists());
    }

    #[test]
    fn worktree_status_parsing() {
        assert_eq!(
            status_paths(" M a.rs\0R  new.rs\0old.rs\0?? dir/x\0"),
            ["a.rs", "new.rs", "dir/x"]
        );
        let mut v = vec![
            "b".to_owned(),
            "x/.env".to_owned(),
            "a".to_owned(),
            ".env.local".to_owned(),
        ];
        v.sort_by(env_first);
        assert_eq!(v, [".env.local", "x/.env", "a", "b"]);
        assert!(regenerable("pkg/node_modules/x") && !regenerable("src/main.rs"));
    }
}
