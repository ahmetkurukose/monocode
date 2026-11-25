//! The working-tree and commit reads behind the unified review, and partial
//! staging. Ported from fs.rs `git_file_diff_for`, `git_commit_files_for`,
//! `git_commit_file_diff_for` and `git_stage_contents_for`.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::Path,
    process::Stdio,
};

/// fs.rs `MAX_TEXT_FILE_BYTES`: larger sides show as too large to diff.
pub const MAX_TEXT_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub relative: String,
    pub status: String,
    pub original: String,
    pub current: String,
    pub binary: bool,
    pub too_large: bool,
}

/// Changed files and counts, without the branch and upstream reads.
pub fn diff_files(cwd: &Path) -> Result<DiffIndex, String> {
    if remote_workspace::is_remote(cwd) {
        return remote_workspace::command("git_diff_files", json!({"cwd":cwd}));
    }
    Ok(diff_index_with(&expand_home(cwd), false))
}

/// Revalidate the review's file list without computing every file's line
/// counts or querying remotes. Loaded diffs supply their own exact counts.
pub fn review_index(cwd: &Path, previous: Option<&DiffIndex>) -> Result<DiffIndex, String> {
    if remote_workspace::is_remote(cwd) {
        return diff_files(cwd);
    }
    let root = expand_home(cwd);
    let status = git_run(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
            ".",
        ],
    )
    .ok_or_else(|| "Could not read Git status".to_string())?;
    let prefix = git_stdout(&root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let previous: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|i| &i.files)
        .map(|f| (f.relative.as_str(), f))
        .collect();
    let mut files = Vec::new();
    for record in status.split('\0').filter(|r| !r.is_empty()) {
        let bytes = record.as_bytes();
        if bytes.len() < 4 {
            continue;
        }
        let Some(relative) = record[3..].strip_prefix(&prefix) else {
            continue;
        };
        let untracked = &bytes[..2] == b"??";
        let (x, y) = (bytes[0], bytes[1]);
        let old = previous.get(relative);
        files.push(ChangedFile {
            path: path_to_js(&root.join(relative)),
            relative: relative.to_string(),
            status: if untracked {
                "untracked"
            } else if x == b'D' || y == b'D' {
                "deleted"
            } else if x == b'A' {
                "added"
            } else {
                "modified"
            }
            .into(),
            additions: old.map_or(0, |f| f.additions),
            deletions: old.map_or(0, |f| f.deletions),
            staged: !untracked && x != b' ',
            unstaged: untracked || y != b' ',
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(DiffIndex {
        head: git_stdout(&root, &["rev-parse", "HEAD"]),
        files,
        ..Default::default()
    })
}

/// The inputs to one diff, without reading its contents. Index object IDs
/// invalidate only the affected paths; disk metadata also catches edits that
/// leave Git's status and line counts unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiffVersion {
    index: String,
    head: Option<String>,
    disk: Option<DiskVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskVersion {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

pub fn file_diff_versions(
    cwd: &Path,
    snapshot: &DiffIndex,
) -> Result<HashMap<String, FileDiffVersion>, String> {
    if remote_workspace::is_remote(cwd) {
        // The host exposes no index object IDs. Fingerprint the bounded diff
        // contents so equal line counts cannot hide an external host edit.
        use sha2::{Digest, Sha256};
        let mut versions = HashMap::new();
        for file in &snapshot.files {
            for staged in [true, false] {
                if if staged { !file.staged } else { !file.unstaged } {
                    continue;
                }
                let diff = file_diff(cwd, &file.relative, staged)?;
                let bytes = serde_json::to_vec(&diff).map_err(|error| error.to_string())?;
                versions.insert(
                    format!(
                        "{}:{}",
                        if staged { "staged" } else { "unstaged" },
                        file.relative
                    ),
                    FileDiffVersion {
                        index: format!("{:x}", Sha256::digest(bytes)),
                        head: snapshot.head.clone(),
                        disk: None,
                    },
                );
            }
        }
        return Ok(versions);
    }
    let root = expand_home(cwd);
    let listing = git_run(&root, &["ls-files", "--stage", "-z", "--", "."])
        .ok_or_else(|| "Could not read Git index versions".to_string())?;
    let mut index: HashMap<&str, String> = HashMap::new();
    for record in listing.split('\0') {
        if let Some((version, path)) = record.split_once('\t') {
            let entry = index.entry(path).or_default();
            entry.push_str(version);
            entry.push('\n');
        }
    }
    let mut versions = HashMap::new();
    for file in &snapshot.files {
        let indexed = index
            .get(file.relative.as_str())
            .cloned()
            .unwrap_or_default();
        if file.staged {
            versions.insert(
                format!("staged:{}", file.relative),
                FileDiffVersion {
                    index: indexed.clone(),
                    head: snapshot.head.clone(),
                    disk: None,
                },
            );
        }
        if file.unstaged {
            let disk = match std::fs::metadata(root.join(&file.relative)) {
                Ok(meta) => Some(DiskVersion {
                    len: meta.len(),
                    modified: meta.modified().ok(),
                    created: meta.created().ok(),
                    #[cfg(unix)]
                    identity: {
                        use std::os::unix::fs::MetadataExt;
                        (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
                    },
                }),
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            versions.insert(
                format!("unstaged:{}", file.relative),
                FileDiffVersion {
                    index: indexed,
                    head: None,
                    disk,
                },
            );
        }
    }
    Ok(versions)
}

/// One file's index or HEAD side against its other side. `staged` compares
/// HEAD with the index; otherwise the index with the working tree.
pub fn file_diff(cwd: &Path, relative: &str, staged: bool) -> Result<FileDiff, String> {
    if remote_workspace::is_remote(cwd) {
        return remote_workspace::command(
            "git_file_diff",
            json!({"cwd":cwd,"relative":relative,"staged":staged}),
        );
    }
    let root = expand_home(cwd);
    let relative = resolve_repo_path(&root, relative)?;
    let abs = root.join(&relative);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let prefix = git_stdout(&root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let index_spec = format!(":{prefix}{relative}");
    let (original, current) = if staged {
        let head_spec = format!("HEAD:{prefix}{relative}");
        (git_blob(&root, &head_spec), git_blob(&root, &index_spec))
    } else {
        let current = abs
            .is_file()
            .then(|| std::fs::read(&abs).unwrap_or_default());
        (git_blob(&root, &index_spec), current)
    };
    let status = match (original.is_some(), current.is_some()) {
        (false, true) if staged => "added",
        (false, true) => "untracked",
        (true, false) => "deleted",
        _ => "modified",
    };
    Ok(text_diff(
        path_to_js(&abs),
        relative,
        status,
        original.unwrap_or_default(),
        current.unwrap_or_default(),
    ))
}

/// One file's working-tree content against HEAD, whatever is staged: what a
/// commit of that file would record. Remote workspaces fall back to the
/// unstaged side.
pub fn file_diff_head(cwd: &Path, relative: &str) -> Result<FileDiff, String> {
    if remote_workspace::is_remote(cwd) {
        return file_diff(cwd, relative, false);
    }
    let root = expand_home(cwd);
    let relative = resolve_repo_path(&root, relative)?;
    let abs = root.join(&relative);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let prefix = git_stdout(&root, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let original = git_blob(&root, &format!("HEAD:{prefix}{relative}"));
    let current = abs
        .is_file()
        .then(|| std::fs::read(&abs).unwrap_or_default());
    let status = match (original.is_some(), current.is_some()) {
        (false, true) => "added",
        (true, false) => "deleted",
        _ => "modified",
    };
    Ok(text_diff(
        path_to_js(&abs),
        relative,
        status,
        original.unwrap_or_default(),
        current.unwrap_or_default(),
    ))
}

/// The files a commit changed, against its first parent (or nothing).
pub fn commit_files(cwd: &Path, sha: &str) -> Result<Vec<ChangedFile>, String> {
    if remote_workspace::is_remote(cwd) {
        return remote_workspace::command("git_commit_files", json!({"cwd":cwd,"sha":sha}));
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let sha = peel_commit(&root, sha)?;
    let mut files: HashMap<String, FileAcc> = HashMap::new();
    let mut statuses: HashMap<String, &'static str> = HashMap::new();
    // A merge has no changes of its own to list: what it brought in is its
    // difference from the first parent, as the file diffs already take it.
    let merged_from = git_stdout(
        &root,
        &["rev-parse", "--verify", "--quiet", &format!("{sha}^2")],
    )
    .and_then(|_| git_stdout(&root, &["rev-parse", "--verify", &format!("{sha}^1")]));
    let tree = |kind: &str| {
        let mut args = vec![
            "diff-tree",
            "-z",
            "--no-commit-id",
            "-r",
            "--root",
            "--no-renames",
            kind,
        ];
        if let Some(parent) = &merged_from {
            args.push(parent);
        }
        args.push(&sha);
        git_run(&root, &args)
    };
    if let Some(text) = tree("--numstat") {
        add_numstat_map(&text, &mut files);
    }
    if let Some(names) = tree("--name-status") {
        add_name_status(&names, &mut statuses);
    }
    let mut out = Vec::with_capacity(files.len().max(statuses.len()));
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
    Ok(out)
}

/// One file in a commit against its first parent.
pub fn commit_file_diff(cwd: &Path, sha: &str, relative: &str) -> Result<FileDiff, String> {
    if remote_workspace::is_remote(cwd) {
        return remote_workspace::command(
            "git_commit_file_diff",
            json!({"cwd":cwd,"sha":sha,"relative":relative}),
        );
    }
    let root = expand_home(cwd);
    let relative = resolve_repo_path(&root, relative)?;
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let sha = peel_commit(&root, sha)?;
    let parent = git_stdout(&root, &["rev-parse", "--verify", &format!("{sha}^")]);
    let original = parent
        .and_then(|parent| git_blob(&root, &format!("{parent}:{relative}")))
        .unwrap_or_default();
    let current = git_blob(&root, &format!("{sha}:{relative}")).unwrap_or_default();
    let status = if original.is_empty() && !current.is_empty() {
        "added"
    } else if !original.is_empty() && current.is_empty() {
        "deleted"
    } else {
        "modified"
    };
    Ok(text_diff(
        path_to_js(&root.join(&relative)),
        relative,
        status,
        original,
        current,
    ))
}

/// Writes `contents` as the file's index entry, keeping its mode: stages a
/// single hunk without touching the working tree.
pub fn stage_contents(cwd: &Path, relative: &str, contents: &[u8]) -> Result<(), String> {
    if remote_workspace::is_remote(cwd) {
        let contents = std::str::from_utf8(contents).map_err(|_| "File is not valid UTF-8.")?;
        return remote_workspace::command(
            "git_stage_contents",
            json!({"cwd":cwd,"relative":relative,"contents":contents}),
        );
    }
    let root = expand_home(cwd);
    let relative = resolve_repo_path(&root, relative)?;
    if contents.len() as u64 > MAX_TEXT_FILE_BYTES {
        return Err("File too large".into());
    }
    let hash = hash_object(&root, &relative, contents)?;
    let mode = index_mode(&root, &relative).unwrap_or_else(|| "100644".into());
    git_checked(
        &root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &mode,
            &hash,
            &relative,
        ],
    )
}

/// One version of a file: a commit that changed it, or what is on disk
/// and not committed yet.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileVersion {
    /// Empty for the uncommitted version.
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    /// Seconds since the epoch; 0 for the uncommitted version.
    pub time: u64,
    /// The file's path in this version, from the repository's root.
    pub path: String,
    /// Its path in the version before, when this one renamed it.
    pub previous_path: Option<String>,
    pub status: String,
}

/// The versions of one file, newest first, followed through renames: what
/// is on disk when it differs from HEAD, then every commit that changed it.
/// `path` is absolute or relative to `cwd`.
pub fn file_history(cwd: &Path, path: &str, limit: usize) -> Result<Vec<FileVersion>, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("File history is not available on remote projects.".into());
    }
    let cwd = expand_home(cwd);
    if !git_is_work_tree(&cwd) {
        return Err("Not a git repository".into());
    }
    let root = git_stdout(&cwd, &["rev-parse", "--show-toplevel"])
        .map(std::path::PathBuf::from)
        .ok_or("Not a git repository")?;
    let mut versions = Vec::new();
    let dirty = git_run(&cwd, &["status", "--porcelain", "-z", "--", path])
        .is_some_and(|status| !status.trim_matches('\0').is_empty());
    let log = git_run(
        &cwd,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--follow",
            &format!("-n{limit}"),
            "--name-status",
            "--format=%x1e%H%x1f%h%x1f%an%x1f%at%x1f%s",
            "--",
            path,
        ],
    )
    .unwrap_or_default();
    for record in log.split('\x1e').filter(|record| !record.trim().is_empty()) {
        let mut lines = record.lines();
        let mut fields = lines.next().unwrap_or_default().split('\x1f');
        let mut field = || fields.next().unwrap_or_default().to_owned();
        let (sha, short_sha, author, time) = (field(), field(), field(), field());
        let subject = field();
        // With --follow the one line after the header is the file's own.
        let Some(change) = lines.find(|line| !line.trim().is_empty()) else {
            continue;
        };
        let mut parts = change.split('\t');
        let code = parts.next().unwrap_or_default();
        let first = parts.next().unwrap_or_default().to_owned();
        let second = parts.next().map(str::to_owned);
        let (path, previous_path, status) = match (code.chars().next(), second) {
            (Some('R'), Some(to)) => (to, Some(first), "renamed"),
            (Some('C'), Some(to)) => (to, Some(first), "added"),
            (Some('A'), _) => (first, None, "added"),
            (Some('D'), _) => (first, None, "deleted"),
            _ => (first, None, "modified"),
        };
        versions.push(FileVersion {
            sha,
            short_sha,
            subject,
            author,
            time: time.parse().unwrap_or(0),
            path,
            previous_path,
            status: status.into(),
        });
    }
    if dirty {
        // The path as the repository knows it: the newest commit's, or the
        // one asked for when the file was never committed.
        let relative = versions.first().map(|v| v.path.clone()).or_else(|| {
            // Both resolved: the root git reports has its links followed.
            let absolute = cwd.join(path).canonicalize().ok()?;
            let root = root.canonicalize().ok()?;
            Some(path_to_js(absolute.strip_prefix(&root).ok()?))
        });
        if let Some(path) = relative {
            versions.insert(
                0,
                FileVersion {
                    sha: String::new(),
                    short_sha: String::new(),
                    subject: "Uncommitted changes".into(),
                    author: String::new(),
                    time: 0,
                    status: if versions.is_empty() {
                        "added"
                    } else {
                        "modified"
                    }
                    .into(),
                    path,
                    previous_path: None,
                },
            );
        }
    }
    Ok(versions)
}

/// What one version changed in the file: the version before it on the
/// left (under the name it had then), this one on the right.
pub fn file_version_diff(cwd: &Path, version: &FileVersion) -> Result<FileDiff, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("File history is not available on remote projects.".into());
    }
    let cwd = expand_home(cwd);
    let root = git_stdout(&cwd, &["rev-parse", "--show-toplevel"])
        .map(std::path::PathBuf::from)
        .ok_or("Not a git repository")?;
    let relative = resolve_repo_path(&root, &version.path)?;
    let before = match &version.previous_path {
        Some(path) => resolve_repo_path(&root, path)?,
        None => relative.clone(),
    };
    let (original, current) = if version.sha.is_empty() {
        (
            git_blob(&root, &format!("HEAD:{before}")).unwrap_or_default(),
            std::fs::read(root.join(&relative)).unwrap_or_default(),
        )
    } else {
        let sha = peel_commit(&root, &version.sha)?;
        let parent = git_stdout(&root, &["rev-parse", "--verify", &format!("{sha}^")]);
        (
            parent
                .and_then(|parent| git_blob(&root, &format!("{parent}:{before}")))
                .unwrap_or_default(),
            git_blob(&root, &format!("{sha}:{relative}")).unwrap_or_default(),
        )
    };
    let status = if original.is_empty() && !current.is_empty() {
        "added"
    } else if !original.is_empty() && current.is_empty() {
        "deleted"
    } else {
        "modified"
    };
    Ok(text_diff(
        path_to_js(&root.join(&relative)),
        relative,
        status,
        original,
        current,
    ))
}

pub(super) fn text_diff(
    path: String,
    relative: String,
    status: &str,
    original: Vec<u8>,
    current: Vec<u8>,
) -> FileDiff {
    let binary = original.contains(&0) || current.contains(&0);
    let too_large =
        original.len() as u64 > MAX_TEXT_FILE_BYTES || current.len() as u64 > MAX_TEXT_FILE_BYTES;
    let (original, current) = if binary || too_large {
        (String::new(), String::new())
    } else {
        (
            String::from_utf8_lossy(&original).into_owned(),
            String::from_utf8_lossy(&current).into_owned(),
        )
    };
    FileDiff {
        path,
        relative,
        status: status.into(),
        original,
        current,
        binary,
        too_large,
    }
}
pub(super) fn git_blob(root: &Path, spec: &str) -> Option<Vec<u8>> {
    git_output(root, &["cat-file", "-p", spec])
}
pub(super) fn peel_commit(root: &Path, spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.len() < 4 || spec.len() > 40 || !spec.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid commit".into());
    }
    git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{spec}^{{commit}}")],
    )
    .ok_or_else(|| "Unknown commit".into())
}
fn index_mode(root: &Path, relative: &str) -> Option<String> {
    let out = git_run(root, &["ls-files", "--stage", "--", relative])?;
    let mode = out.lines().next()?.split_whitespace().next()?;
    (mode.len() == 6 && mode.bytes().all(|b| b.is_ascii_digit())).then(|| mode.to_string())
}
fn hash_object(root: &Path, relative: &str, contents: &[u8]) -> Result<String, String> {
    let mut command = git_cmd();
    command
        .arg("--no-pager")
        .arg("-C")
        .arg(root)
        .args(["hash-object", "-w", "--path", relative, "--stdin"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::process::spawn(&mut command).map_err(|e| e.to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "hash-object stdin".to_string())?;
    stdin.write_all(contents).map_err(|e| e.to_string())?;
    drop(stdin);
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err([stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|m| !m.is_empty())
            .unwrap_or("git hash-object failed")
            .to_string());
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.len() != 40 && hash.len() != 64 {
        return Err("git hash-object returned an invalid hash".into());
    }
    Ok(hash)
}
