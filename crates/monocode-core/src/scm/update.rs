//! Update Project: fetch, bring the upstream in by merge or rebase, and say
//! what arrived — the commits and the files they changed.
use super::review::{git_blob, text_diff};
use super::*;
use std::{collections::HashSet, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMethod {
    Merge,
    Rebase,
}

/// One commit that came in with the update.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct IncomingCommit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub timestamp: i64,
    pub subject: String,
}

/// What an update did.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateReport {
    pub upstream: String,
    pub rebase: bool,
    /// HEAD before and after; equal when there was nothing to bring in.
    pub old: String,
    pub new: String,
    pub commits: Vec<IncomingCommit>,
    /// Files whose content differs between `old` and `new`.
    pub files: Vec<ChangedFile>,
    /// Files left unmerged; the update is then still in progress.
    pub conflicts: Vec<String>,
}

const MAX_INCOMING: u32 = 300;

fn head(root: &Path) -> Result<String, String> {
    git_stdout(root, &["rev-parse", "--verify", "HEAD"]).ok_or_else(|| "No commits yet".into())
}

fn incoming_commits(root: &Path, spec: &str) -> Vec<IncomingCommit> {
    let limit = format!("--max-count={MAX_INCOMING}");
    let Some(text) = git_run(
        root,
        &[
            "log",
            &limit,
            "--format=%H%x1f%h%x1f%an%x1f%ct%x1f%s%x1e",
            &format!("HEAD..{spec}"),
        ],
    ) else {
        return vec![];
    };
    text.split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\u{1f}');
            Some(IncomingCommit {
                sha: fields.next().filter(|s| !s.is_empty())?.to_string(),
                short_sha: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                timestamp: fields.next()?.parse().ok()?,
                subject: fields.next()?.trim().to_string(),
            })
        })
        .collect()
}

/// Files that differ between two commits, with line counts.
fn range_files_for(root: &Path, old: &str, new: &str) -> Vec<ChangedFile> {
    let mut files: HashMap<String, FileAcc> = HashMap::new();
    let mut statuses: HashMap<String, &'static str> = HashMap::new();
    let diff = |kind: &str| {
        git_run(
            root,
            &[
                "diff",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                kind,
                old,
                new,
            ],
        )
    };
    if let Some(text) = diff("--numstat") {
        add_numstat_map(&text, &mut files);
    }
    if let Some(names) = diff("--name-status") {
        add_name_status(&names, &mut statuses);
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let file = |relative: String, status: &str, additions, deletions| ChangedFile {
        path: path_to_js(&root.join(&relative)),
        relative,
        status: status.to_string(),
        additions,
        deletions,
        staged: false,
        unstaged: false,
    };
    for (relative, acc) in files {
        seen.insert(relative.clone());
        let status = statuses.get(&relative).copied().unwrap_or("modified");
        out.push(file(relative, status, acc.additions, acc.deletions));
    }
    for (relative, status) in statuses {
        if !seen.contains(&relative) {
            out.push(file(relative, status, 0, 0));
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    out
}

fn conflicted(root: &Path) -> Vec<String> {
    git_run(root, &["diff", "--name-only", "--diff-filter=U"])
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Fetch, then merge (or rebase onto) the branch's upstream. A conflict is a
/// report with `conflicts` set, not an error: the update stays in progress.
pub fn update_project(cwd: &Path, method: UpdateMethod) -> Result<UpdateReport, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Update Project isn't available on remote workspaces yet".into());
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let upstream = git_stdout(&root, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .ok_or("The current branch has no upstream to update from")?;
    git_checked(&root, &["fetch"]).map_err(with_signing_hint)?;
    bring_in(&root, upstream, "@{upstream}", method)
}

/// Merge another branch into the current one and report what arrived, as an
/// update does. A conflict leaves the merge in progress.
pub fn merge_branch(cwd: &Path, branch: &str) -> Result<UpdateReport, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Merging isn't available on remote workspaces yet".into());
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') || branch.contains(char::is_whitespace) {
        return Err("Choose a branch to merge".into());
    }
    git_stdout(
        &root,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .ok_or_else(|| format!("Unknown branch {branch}"))?;
    bring_in(&root, branch.to_string(), branch, UpdateMethod::Merge)
}

/// Rebase the current branch onto another one and report it like an update.
pub fn rebase_branch(cwd: &Path, branch: &str) -> Result<UpdateReport, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Rebasing isn't available on remote workspaces yet".into());
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let branch = branch.trim();
    if branch.is_empty() || branch.starts_with('-') || branch.contains(char::is_whitespace) {
        return Err("Choose a branch to rebase onto".into());
    }
    git_stdout(
        &root,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .ok_or_else(|| format!("Unknown branch {branch}"))?;
    bring_in(&root, branch.to_string(), branch, UpdateMethod::Rebase)
}

fn bring_in(
    root: &Path,
    label: String,
    spec: &str,
    method: UpdateMethod,
) -> Result<UpdateReport, String> {
    let old = head(root)?;
    let mut report = UpdateReport {
        upstream: label,
        rebase: method == UpdateMethod::Rebase,
        old: old.clone(),
        new: old,
        commits: incoming_commits(root, spec),
        ..Default::default()
    };
    if report.commits.is_empty() {
        return Ok(report);
    }
    let result = match method {
        UpdateMethod::Merge => git_checked(root, &["merge", "--no-edit", spec]),
        UpdateMethod::Rebase => git_checked(root, &["rebase", spec]),
    };
    if let Err(error) = result {
        let conflicts = conflicted(root);
        if conflicts.is_empty() {
            return Err(with_signing_hint(error));
        }
        report.conflicts = conflicts;
        return Ok(report);
    }
    report.new = head(root)?;
    report.files = range_files_for(root, &report.old, &report.new);
    Ok(report)
}

/// Gives up an update stopped by conflicts, restoring the branch.
pub fn abort_update(cwd: &Path) -> Result<(), String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Update Project isn't available on remote workspaces yet".into());
    }
    let root = expand_home(cwd);
    let dir = git_stdout(&root, &["rev-parse", "--git-dir"]).ok_or("Not a git repository")?;
    let git_dir = if Path::new(&dir).is_absolute() {
        PathBuf::from(&dir)
    } else {
        root.join(&dir)
    };
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        git_checked(&root, &["rebase", "--abort"])
    } else if git_dir.join("MERGE_HEAD").exists() {
        git_checked(&root, &["merge", "--abort"])
    } else {
        Err("No update is in progress".into())
    }
}

/// One file between two commits.
pub fn range_file_diff(
    cwd: &Path,
    old: &str,
    new: &str,
    relative: &str,
) -> Result<FileDiff, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Update Project isn't available on remote workspaces yet".into());
    }
    let root = expand_home(cwd);
    let relative = resolve_repo_path(&root, relative)?;
    let old = peel(&root, old)?;
    let new = peel(&root, new)?;
    let original = git_blob(&root, &format!("{old}:{relative}"));
    let current = git_blob(&root, &format!("{new}:{relative}"));
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
fn peel(root: &Path, spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.len() < 4 || spec.len() > 64 || !spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid commit".into());
    }
    git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{spec}^{{commit}}")],
    )
    .ok_or_else(|| "Unknown commit".into())
}
