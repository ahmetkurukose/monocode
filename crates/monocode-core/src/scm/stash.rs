//! Stash: shelve the working tree's changes, list what is shelved, look inside
//! an entry, and bring it back (apply keeps it, pop drops it), or branch from it.
//! Entries are addressed by the commit they are, since `stash@{n}` shifts
//! whenever another entry is added or dropped.
use super::review::{git_blob, text_diff};
use super::update::range_files_for;
use super::*;
use std::path::PathBuf;
use std::process::Output;

/// One shelved change.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StashEntry {
    /// Position in `git stash list` right now: 0 is the newest.
    pub index: usize,
    pub sha: String,
    /// The branch it was made on.
    pub branch: String,
    /// What to show: the given message, or "WIP" and the commit it sat on.
    pub message: String,
    pub timestamp: i64,
}

/// What bringing an entry back did.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StashOutcome {
    /// Files left unmerged. The entry is then kept, even when popping.
    pub conflicts: Vec<String>,
    /// The entry is gone: it was popped and applied cleanly.
    pub dropped: bool,
}

fn unavailable() -> String {
    "Stash isn't available on remote workspaces yet".into()
}

fn work_tree(cwd: &Path) -> Result<PathBuf, String> {
    if remote_workspace::is_remote(cwd) {
        return Err(unavailable());
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    Ok(root)
}

fn run(root: &Path, args: &[&str]) -> Result<Output, String> {
    git_cmd()
        .arg("--no-pager")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| e.to_string())
}

fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if text.is_empty() {
        "git stash failed".into()
    } else {
        text.to_string()
    }
}

/// `On main: message` or `WIP on main: abc1234 subject`, split into the branch and the rest.
fn split_subject(subject: &str) -> (String, String) {
    let subject = subject.trim();
    let (wip, rest) = match subject.strip_prefix("WIP on ") {
        Some(rest) => (true, rest),
        None => (false, subject.strip_prefix("On ").unwrap_or(subject)),
    };
    match rest.split_once(": ") {
        Some((branch, message)) if !branch.contains(' ') => (
            branch.to_string(),
            if wip {
                format!("WIP: {message}")
            } else {
                message.to_string()
            },
        ),
        _ => (String::new(), subject.to_string()),
    }
}

pub fn stash_list(cwd: &Path) -> Result<Vec<StashEntry>, String> {
    let root = work_tree(cwd)?;
    let Some(text) = git_run(&root, &["stash", "list", "--format=%H%x1f%ct%x1f%gs%x1e"]) else {
        return Ok(vec![]);
    };
    Ok(text
        .split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\u{1f}');
            let sha = fields.next().filter(|s| !s.is_empty())?.to_string();
            let timestamp = fields.next()?.parse().ok()?;
            let (branch, message) = split_subject(fields.next()?);
            Some((sha, timestamp, branch, message))
        })
        .enumerate()
        .map(|(index, (sha, timestamp, branch, message))| StashEntry {
            index,
            sha,
            branch,
            message,
            timestamp,
        })
        .collect())
}

fn index_of(root: &Path, sha: &str) -> Result<usize, String> {
    let list = git_run(root, &["stash", "list", "--format=%H"]).unwrap_or_default();
    list.lines()
        .position(|line| line == sha)
        .ok_or_else(|| "That stash no longer exists".into())
}

fn stash_ref(root: &Path, sha: &str) -> Result<String, String> {
    if sha.len() < 7 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid stash".into());
    }
    Ok(format!("stash@{{{}}}", index_of(root, sha)?))
}

/// Shelve the changes. Untracked files go too when asked; keeping the index
/// leaves what is staged in the working tree as well.
pub fn stash_push(
    cwd: &Path,
    message: &str,
    include_untracked: bool,
    keep_index: bool,
) -> Result<(), String> {
    let root = work_tree(cwd)?;
    let message = message.trim();
    let mut args = vec!["stash", "push"];
    if include_untracked {
        args.push("--include-untracked");
    }
    if keep_index {
        args.push("--keep-index");
    }
    if !message.is_empty() {
        args.extend(["-m", message]);
    }
    let output = run(&root, &args)?;
    if !output.status.success() {
        return Err(failure(&output));
    }
    // Nothing to shelve is a success to git, with a note on stdout.
    if String::from_utf8_lossy(&output.stdout).contains("No local changes to save") {
        return Err("There are no changes to stash".into());
    }
    Ok(())
}

/// The files an entry holds: what it changed, plus the untracked ones it saved.
pub fn stash_files(cwd: &Path, sha: &str) -> Result<Vec<ChangedFile>, String> {
    let root = work_tree(cwd)?;
    let sha = stash_commit(&root, sha)?;
    let base = git_stdout(&root, &["rev-parse", "--verify", &format!("{sha}^1")])
        .ok_or("Unknown stash")?;
    let mut files = range_files_for(&root, &base, &sha);
    let names = untracked_commit(&root, &sha)
        .and_then(|third| git_run(&root, &["ls-tree", "-r", "--name-only", "-z", &third]));
    for relative in names
        .iter()
        .flat_map(|names| names.split('\0'))
        .filter(|name| !name.is_empty())
    {
        if files.iter().any(|file| file.relative == relative) {
            continue;
        }
        files.push(ChangedFile {
            path: path_to_js(&root.join(relative)),
            relative: relative.to_string(),
            status: "added".into(),
            additions: 0,
            deletions: 0,
            staged: false,
            unstaged: false,
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(files)
}

/// One file of an entry against the commit it was made on.
pub fn stash_file_diff(cwd: &Path, sha: &str, relative: &str) -> Result<FileDiff, String> {
    let root = work_tree(cwd)?;
    let relative = resolve_repo_path(&root, relative)?;
    let sha = stash_commit(&root, sha)?;
    let base = git_stdout(&root, &["rev-parse", "--verify", &format!("{sha}^1")])
        .ok_or("Unknown stash")?;
    let original = git_blob(&root, &format!("{base}:{relative}"));
    let mut current = git_blob(&root, &format!("{sha}:{relative}"));
    if current.is_none() && original.is_none() {
        if let Some(third) = untracked_commit(&root, &sha) {
            current = git_blob(&root, &format!("{third}:{relative}"));
        }
    }
    let status = match (original.is_some(), current.is_some()) {
        (false, true) => "added",
        (true, false) => "deleted",
        _ => "modified",
    };
    Ok(text_diff(
        path_to_js(&root.join(&relative)),
        relative,
        status,
        original.unwrap_or_default(),
        current.unwrap_or_default(),
    ))
}

fn stash_commit(root: &Path, sha: &str) -> Result<String, String> {
    stash_ref(root, sha)?;
    git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
    )
    .ok_or_else(|| "Unknown stash".into())
}

/// The commit holding the untracked files, when the entry saved any.
fn untracked_commit(root: &Path, sha: &str) -> Option<String> {
    git_stdout(
        root,
        &["rev-parse", "--verify", "--quiet", &format!("{sha}^3")],
    )
}

/// Bring an entry back. A conflict is an outcome, not an error: the files are
/// left unmerged and the entry stays, even when popping.
pub fn stash_apply(
    cwd: &Path,
    sha: &str,
    pop: bool,
    reinstate_index: bool,
) -> Result<StashOutcome, String> {
    let root = work_tree(cwd)?;
    let target = stash_ref(&root, sha)?;
    let mut args = vec!["stash", if pop { "pop" } else { "apply" }];
    if reinstate_index {
        args.push("--index");
    }
    args.push(&target);
    let output = run(&root, &args)?;
    if output.status.success() {
        return Ok(StashOutcome {
            conflicts: vec![],
            dropped: pop,
        });
    }
    let conflicts = git_run(&root, &["diff", "--name-only", "--diff-filter=U"])
        .map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default();
    if conflicts.is_empty() {
        return Err(failure(&output));
    }
    Ok(StashOutcome {
        conflicts,
        dropped: false,
    })
}

pub fn stash_drop(cwd: &Path, sha: &str) -> Result<(), String> {
    let root = work_tree(cwd)?;
    let target = stash_ref(&root, sha)?;
    git_checked(&root, &["stash", "drop", &target])
}

/// Remove every entry.
pub fn stash_clear(cwd: &Path) -> Result<(), String> {
    let root = work_tree(cwd)?;
    git_checked(&root, &["stash", "clear"])
}

/// A new branch from the commit the entry was made on, with the entry applied
/// to it and dropped.
pub fn stash_branch(cwd: &Path, sha: &str, name: &str) -> Result<(), String> {
    let root = work_tree(cwd)?;
    let target = stash_ref(&root, sha)?;
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) {
        return Err("Enter a branch name".into());
    }
    let output = run(&root, &["stash", "branch", name, &target])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failure(&output))
    }
}
