//! Merging a worktree's branch back into the branch it was started from (its base).
//!
//! The base is the local branch named by `branch.<b>.base` (written at creation; a
//! remote-tracking base such as `origin/main` means the local `main`). When the base is
//! checked out somewhere (usually the main checkout), the merge runs there with
//! `git merge`, so that checkout's files follow; otherwise the base ref is moved without a
//! checkout (fast-forward, or a merge commit from `git merge-tree`). A conflict changes
//! nothing: the merge is aborted and the conflicting paths are reported.
use super::{catalog, git, git_ok};
use std::path::{Path, PathBuf};

/// Where a worktree's branch stands against its base.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: String,
    /// The local branch to merge into; `None` when nothing usable was recorded.
    pub base: Option<String>,
    /// Commits on the branch that the base does not have (to be merged).
    pub ahead: usize,
    /// Commits on the base that the branch does not have.
    pub behind: usize,
    /// Tracked files with uncommitted changes in the worktree (not part of a merge).
    pub dirty: usize,
    /// The checkout that has the base checked out, where the merge would run.
    pub target: Option<PathBuf>,
}
impl Status {
    /// Something to merge, and nothing in the way on this side.
    pub fn can_merge(&self) -> bool {
        self.base.is_some() && self.ahead > 0 && self.dirty == 0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    pub branch: String,
    pub base: String,
    /// Commits brought into the base; 0 when it already had them.
    pub commits: usize,
    pub fast_forward: bool,
    /// The checkout the merge ran in, if the base was checked out.
    pub target: Option<PathBuf>,
}

fn count(cwd: &Path, range: &str) -> usize {
    git(cwd, &["rev-list", "--count", range])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn has_ref(cwd: &Path, name: &str) -> bool {
    git_ok(cwd, &["show-ref", "--verify", "--quiet", name])
}

/// The local branch `branch.<branch>.base` names, if it exists: `main`, or `origin/main`
/// read as `main`. A commit or a vanished branch gives `None`.
pub fn base_branch(cwd: &Path, branch: &str) -> Option<String> {
    let recorded = git(cwd, &["config", "--get", &format!("branch.{branch}.base")]).ok()?;
    let recorded = recorded.trim();
    if recorded.is_empty() {
        return None;
    }
    let local = |name: &str| {
        (name != branch && has_ref(cwd, &format!("refs/heads/{name}"))).then(|| name.to_owned())
    };
    if let Some(found) = local(recorded) {
        return Some(found);
    }
    if has_ref(cwd, &format!("refs/remotes/{recorded}")) {
        let remotes = git(cwd, &["remote"]).unwrap_or_default();
        let mut remotes: Vec<&str> = remotes.lines().map(str::trim).collect();
        remotes.sort_by_key(|r| std::cmp::Reverse(r.len()));
        let name = remotes
            .iter()
            .find_map(|r| recorded.strip_prefix(&format!("{r}/")))?;
        return local(name);
    }
    None
}

/// The base branch of the worktree registered at `path` (which may be gone from disk),
/// read through its branch.
pub fn base_of(cwd: &Path, path: &Path) -> Option<String> {
    let key = super::meta::key(path);
    let branch = catalog::registered(cwd)
        .ok()?
        .into_iter()
        .find(|t| super::meta::key(Path::new(&t.path)) == key)?
        .branch?;
    base_branch(cwd, &branch)
}

/// A linked worktree of `cwd`'s repository named by its path or its branch (an agent's
/// `worktrees.merge` / `worktrees.delete`). The main checkout is never one.
pub fn find(cwd: &Path, path: Option<&str>, branch: Option<&str>) -> Result<PathBuf, String> {
    super::ensure_local(cwd)?;
    let trees = catalog::registered(cwd)?;
    let found = match (path, branch) {
        (Some(path), _) => {
            let key = super::meta::key(Path::new(path));
            trees
                .iter()
                .position(|t| super::meta::key(Path::new(&t.path)) == key)
                .ok_or_else(|| format!("{path} is not a worktree of this project"))?
        }
        (None, Some(branch)) => trees
            .iter()
            .position(|t| t.branch.as_deref() == Some(branch))
            .ok_or_else(|| format!("No worktree has {branch} checked out"))?,
        (None, None) => return Err("Name the worktree: give its path or its branch".into()),
    };
    if found == 0 {
        return Err("That is the project's main checkout, not a worktree".into());
    }
    Ok(PathBuf::from(&trees[found].path))
}

/// The checkout (among `cwd`'s repository's worktrees) that has `branch` checked out.
pub fn checkout_of(cwd: &Path, branch: &str) -> Option<PathBuf> {
    catalog::registered(cwd)
        .ok()?
        .into_iter()
        .find(|t| t.branch.as_deref() == Some(branch))
        .map(|t| PathBuf::from(t.path))
}

/// The base and the counts for the worktree at `worktree`.
pub fn status(worktree: &Path) -> Result<Status, String> {
    super::ensure_local(worktree)?;
    let branch = git(worktree, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|b| b.trim().to_owned())
        .map_err(|_| "This worktree is not on a branch".to_owned())?;
    let dirty = git(
        worktree,
        &["status", "--porcelain", "-z", "--untracked-files=no"],
    )?
    .split('\0')
    .filter(|f| !f.is_empty())
    .count();
    let mut status = Status {
        dirty,
        ..Status::default()
    };
    if let Some(base) = base_branch(worktree, &branch) {
        status.ahead = count(worktree, &format!("{base}..{branch}"));
        status.behind = count(worktree, &format!("{branch}..{base}"));
        status.target = checkout_of(worktree, &base);
        status.base = Some(base);
    }
    status.branch = branch;
    Ok(status)
}

fn conflicts(cwd: &Path) -> Vec<String> {
    git(cwd, &["diff", "--name-only", "-z", "--diff-filter=U"])
        .unwrap_or_default()
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn conflict_error(branch: &str, base: &str, paths: &[String]) -> String {
    let mut message = format!("Merging {branch} into {base} conflicts");
    if !paths.is_empty() {
        let shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
        message.push_str(&format!(" in {}", shown.join(", ")));
        if paths.len() > shown.len() {
            message.push_str(&format!(" and {} more", paths.len() - shown.len()));
        }
    }
    message.push_str(&format!(
        ". Nothing was changed. Merge {base} into the worktree and resolve it there, then try again."
    ));
    message
}

/// Merge the worktree's branch into its base. Refuses uncommitted changes in the
/// worktree (they would not be merged); a conflict leaves both branches as they were.
pub fn merge(worktree: &Path) -> Result<Merged, String> {
    let status = status(worktree)?;
    let base = status
        .base
        .clone()
        .ok_or("This worktree has no base branch to merge into")?;
    if status.dirty > 0 {
        return Err(format!(
            "The worktree has {} uncommitted {}. Commit them first; only commits are merged.",
            status.dirty,
            if status.dirty == 1 {
                "change"
            } else {
                "changes"
            }
        ));
    }
    let mut merged = Merged {
        branch: status.branch.clone(),
        base: base.clone(),
        commits: status.ahead,
        fast_forward: false,
        target: status.target.clone(),
    };
    if status.ahead == 0 {
        return Ok(merged);
    }
    let branch = status.branch.as_str();
    let tip = git(
        worktree,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )?
    .trim()
    .to_owned();
    let old = git(
        worktree,
        &["rev-parse", "--verify", &format!("refs/heads/{base}")],
    )?
    .trim()
    .to_owned();
    merged.fast_forward = git_ok(worktree, &["merge-base", "--is-ancestor", &old, &tip]);
    let message = format!("Merge branch '{branch}' into {base}");
    if let Some(target) = &status.target {
        // Merging the commit, not the name, so a branch that moved since is not half-merged.
        let out = git(target, &["merge", "--no-edit", "-m", &message, &tip]);
        if let Err(error) = out {
            if git_ok(target, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]) {
                let paths = conflicts(target);
                let _ = git(target, &["merge", "--abort"]);
                return Err(conflict_error(branch, &base, &paths));
            }
            return Err(format!(
                "Could not merge into {base} at {}: {error}",
                target.display()
            ));
        }
        return Ok(merged);
    }
    // Not checked out anywhere: move the ref, comparing against what we read.
    let new = if merged.fast_forward {
        tip.clone()
    } else {
        let tree = match git(worktree, &["merge-tree", "--write-tree", "-z", &old, &tip]) {
            Ok(out) => out.split('\0').next().unwrap_or_default().trim().to_owned(),
            Err(_) => {
                return Err(conflict_error(branch, &base, &[]));
            }
        };
        git(
            worktree,
            &["commit-tree", &tree, "-p", &old, "-p", &tip, "-m", &message],
        )?
        .trim()
        .to_owned()
    };
    git(
        worktree,
        &[
            "update-ref",
            "-m",
            &message,
            &format!("refs/heads/{base}"),
            &new,
            &old,
        ],
    )?;
    Ok(merged)
}

/// Delete `branch` when `base` has every commit of it (it was merged), even if
/// `git branch -d` refused because the main checkout is on another branch. Returns
/// whether it was deleted.
pub fn delete_merged_branch(cwd: &Path, branch: &str, base: &str) -> bool {
    let merged = git_ok(
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{branch}"),
            &format!("refs/heads/{base}"),
        ],
    );
    merged && super::remove::force_delete_branch(cwd, branch).is_ok()
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, run};
    use super::super::{create, CreateRequest};
    use super::*;

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", file]);
    }

    fn worktree(main: &Path, root: &Path, name: &str, base: &str) -> PathBuf {
        let store = fixture::store();
        create::create(
            &CreateRequest {
                root: Some(root.to_path_buf()),
                base: Some(base.into()),
                ..CreateRequest::new(main, name)
            },
            &store,
            &|_| {},
        )
        .unwrap()
        .path
    }

    /// A commit on `branch` without checking it out.
    fn commit_on(dir: &Path, branch: &str) {
        let tree = run(dir, &["rev-parse", &format!("{branch}^{{tree}}")]);
        let parent = run(dir, &["rev-parse", branch]);
        let made = run(
            dir,
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                parent.trim(),
                "-m",
                "more",
            ],
        );
        run(dir, &["branch", "-f", branch, made.trim()]);
    }

    fn head_branch(dir: &Path) -> String {
        run(dir, &["symbolic-ref", "--short", "HEAD"])
            .trim()
            .to_owned()
    }

    #[test]
    fn worktree_merge_into_a_checked_out_base() {
        let (_t, main, root) = fixture::repo();
        let base = head_branch(&main);
        let wt = worktree(&main, &root, "feature", &base);
        let s = status(&wt).unwrap();
        assert_eq!(s.base.as_deref(), Some(base.as_str()));
        assert_eq!((s.ahead, s.behind, s.dirty), (0, 0, 0));
        assert!(!s.can_merge(), "nothing to merge yet");
        commit(&wt, "a.txt", "a\n");
        // Uncommitted work is refused, not silently left out.
        std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
        assert_eq!(status(&wt).unwrap().dirty, 1);
        assert!(merge(&wt).unwrap_err().contains("uncommitted"));
        run(&wt, &["checkout", "--", "a.txt"]);
        let s = status(&wt).unwrap();
        assert!(s.can_merge());
        assert_eq!(
            s.target.as_deref().map(super::super::canonical),
            Some(super::super::canonical(&main))
        );
        let merged = merge(&wt).unwrap();
        assert_eq!((merged.commits, merged.fast_forward), (1, true));
        assert_eq!(std::fs::read_to_string(main.join("a.txt")).unwrap(), "a\n");
        assert_eq!(status(&wt).unwrap().ahead, 0);
        // Diverged: a merge commit in the main checkout.
        commit(&main, "m.txt", "m\n");
        commit(&wt, "b.txt", "b\n");
        let merged = merge(&wt).unwrap();
        assert!(!merged.fast_forward);
        assert!(main.join("b.txt").exists() && main.join("m.txt").exists());
        let parents = run(&main, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3, "a merge commit");
    }

    #[test]
    fn worktree_merge_conflict_changes_nothing() {
        let (_t, main, root) = fixture::repo();
        let base = head_branch(&main);
        let wt = worktree(&main, &root, "clash", &base);
        commit(&wt, "same.txt", "theirs\n");
        commit(&main, "same.txt", "ours\n");
        let before = run(&main, &["rev-parse", "HEAD"]);
        let error = merge(&wt).unwrap_err();
        assert!(error.contains("conflicts in same.txt"), "{error}");
        assert_eq!(run(&main, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            std::fs::read_to_string(main.join("same.txt")).unwrap(),
            "ours\n"
        );
        assert!(!git_ok(
            &main,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
    }

    #[test]
    fn worktree_merge_into_a_base_checked_out_nowhere() {
        let (_t, main, root) = fixture::repo();
        run(&main, &["branch", "develop"]);
        let wt = worktree(&main, &root, "topic", "develop");
        assert_eq!(status(&wt).unwrap().target, None);
        commit(&wt, "t.txt", "t\n");
        let merged = merge(&wt).unwrap();
        assert!(merged.fast_forward);
        assert_eq!(
            run(&main, &["rev-parse", "develop"]),
            run(&wt, &["rev-parse", "HEAD"])
        );
        // Diverged and not checked out: a merge commit without touching any checkout.
        let other = worktree(&main, &root, "other", "develop");
        commit(&other, "o.txt", "o\n");
        commit(&wt, "t2.txt", "t2\n");
        assert!(merge(&wt).unwrap().fast_forward);
        let merged = merge(&other).unwrap();
        assert!(!merged.fast_forward);
        // Merged into develop, which the main checkout is not on: still deletable.
        run(&main, &["branch", "merged-away", "develop"]);
        assert!(delete_merged_branch(&main, "merged-away", "develop"));
        run(&main, &["branch", "unmerged", "HEAD"]);
        commit_on(&main, "unmerged");
        assert!(!delete_merged_branch(&main, "unmerged", "develop"));
        let tree = run(&main, &["ls-tree", "--name-only", "develop"]);
        assert!(tree.contains("o.txt") && tree.contains("t2.txt"));
        // An origin/<b> base means the local <b>; a commit base has none.
        run(&wt, &["config", "branch.topic.base", "origin/develop"]);
        run(
            &main,
            &["update-ref", "refs/remotes/origin/develop", "develop"],
        );
        run(
            &main,
            &["remote", "add", "origin", "https://example.invalid/r.git"],
        );
        assert_eq!(base_branch(&wt, "topic").as_deref(), Some("develop"));
        let sha = run(&main, &["rev-parse", "HEAD"]).trim().to_owned();
        run(&wt, &["config", "branch.topic.base", &sha]);
        assert_eq!(base_branch(&wt, "topic"), None);
        assert!(merge(&wt).unwrap_err().contains("no base branch"));
        // Found by path or branch; the main checkout is refused.
        let by_branch = find(&main, None, Some("topic")).unwrap();
        assert_eq!(
            super::super::canonical(&by_branch),
            super::super::canonical(&wt)
        );
        let by_path = find(&main, Some(&wt.to_string_lossy()), None).unwrap();
        assert_eq!(
            super::super::canonical(&by_path),
            super::super::canonical(&wt)
        );
        assert!(find(&main, Some(&main.to_string_lossy()), None)
            .unwrap_err()
            .contains("main checkout"));
        assert!(find(&main, None, Some("nope")).is_err());
        assert!(find(&main, None, None)
            .unwrap_err()
            .contains("Name the worktree"));
    }
}
