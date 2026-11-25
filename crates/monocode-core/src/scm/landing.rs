//! One commit in full, and how it reached the repository's main branch: by
//! itself, or carried in by a merge. A file's history shows what a commit
//! did to that file; this is the way from there to the whole change and to
//! when it landed.
use super::review::peel_commit;
use super::*;

/// A commit as a list names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitRef {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    /// Seconds since the epoch.
    pub time: u64,
    /// The author's date in local time, `2026-11-25 14:03`.
    pub date: String,
}

/// How a commit reached the main branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// It is one of the main branch's own commits.
    Direct { branch: String },
    /// A merge brought it in; the merge's changes are the whole branch's.
    Merged { branch: String, merge: CommitRef },
    /// The main branch does not have it.
    NotMerged { branch: String },
    /// No main branch was found, or Git gave no answer in time.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub commit: CommitRef,
    /// The message after its subject, trimmed.
    pub body: String,
    /// Two or more for a merge.
    pub parents: usize,
    pub landing: Landing,
}

const FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%ad%x1f%P%x1f%s%x1f%b";

fn describe(root: &Path, sha: &str) -> Option<(CommitRef, String, usize)> {
    let out = git_run(
        root,
        &[
            "show",
            "-s",
            "--date=format-local:%Y-%m-%d %H:%M",
            FORMAT,
            sha,
        ],
    )?;
    let mut fields = out.splitn(8, '\x1f');
    let mut next = || fields.next().unwrap_or_default().trim().to_owned();
    let (sha, short_sha, author, time, date, parents, subject, body) = (
        next(),
        next(),
        next(),
        next(),
        next(),
        next(),
        next(),
        next(),
    );
    if sha.is_empty() {
        return None;
    }
    let commit = CommitRef {
        sha,
        short_sha,
        subject,
        author,
        time: time.parse().unwrap_or(0),
        date,
    };
    Some((commit, body, parents.split_whitespace().count()))
}

/// The branch everything is merged into, as a ref Git resolves: what the
/// remote calls its HEAD, else the usual names, the remote's copy first
/// since a fetch moves it and nothing moves a local one left behind.
fn main_branch(root: &Path) -> Option<String> {
    if let Some(head) = git_stdout(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        return Some(head);
    }
    ["origin/master", "origin/main", "master", "main"]
        .into_iter()
        .find(|name| {
            git_stdout(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{name}^{{commit}}"),
                ],
            )
            .is_some()
        })
        .map(str::to_owned)
}

fn landing(root: &Path, sha: &str) -> Landing {
    let Some(branch) = main_branch(root) else {
        return Landing::Unknown;
    };
    let name = branch.strip_prefix("origin/").unwrap_or(&branch).to_owned();
    let Some(tip) = git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    ) else {
        return Landing::Unknown;
    };
    if tip == sha {
        return Landing::Direct { branch: name };
    }
    let base = git_stdout_within(root, &["merge-base", sha, &tip], HISTORY_WALK_TIMEOUT);
    if base.as_deref() != Some(sha) {
        return Landing::NotMerged { branch: name };
    }
    // The main branch's own commits, newest first, and everything that has
    // this commit behind it: the oldest in both is where it came in. (Git
    // does not combine the two walks in one.)
    let range = format!("{sha}..{tip}");
    let walk =
        |kind: &str| git_stdout_within(root, &["rev-list", kind, &range], HISTORY_WALK_TIMEOUT);
    let (Some(mainline), Some(after)) = (walk("--first-parent"), walk("--ancestry-path")) else {
        return Landing::Unknown;
    };
    let after: std::collections::HashSet<&str> = after.lines().map(str::trim).collect();
    let Some(entry) = (mainline.lines().map(str::trim)).rfind(|line| after.contains(line)) else {
        return Landing::Unknown;
    };
    // Its first parent is this commit: the commit is on the main line itself.
    let first_parent = git_stdout(root, &["rev-parse", "--verify", &format!("{entry}^1")]);
    if first_parent.as_deref() == Some(sha) {
        return Landing::Direct { branch: name };
    }
    match describe(root, entry) {
        Some((merge, ..)) => Landing::Merged {
            branch: name,
            merge,
        },
        None => Landing::Unknown,
    }
}

/// A commit's message and author, and how it reached the main branch.
pub fn commit_info(cwd: &Path, sha: &str) -> Result<CommitInfo, String> {
    if remote_workspace::is_remote(cwd) {
        return Err("Commit details are not available on remote projects.".into());
    }
    let root = expand_home(cwd);
    if !git_is_work_tree(&root) {
        return Err("Not a git repository".into());
    }
    let sha = peel_commit(&root, sha)?;
    let (commit, body, parents) = describe(&root, &sha).ok_or("Unknown commit")?;
    let landing = landing(&root, &sha);
    Ok(CommitInfo {
        commit,
        body,
        parents,
        landing,
    })
}
