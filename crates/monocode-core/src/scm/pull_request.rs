//! New Pull Request: everything the native "Create Pull Request" dialog needs from
//! git and GitHub (github.com and GitHub Enterprise), via `git` and the `gh` CLI.
//!
//! Synchronous like the rest of `scm`: callers run these on a background executor.
//!
//! Shape mirrors JetBrains' GHPRCreateViewModel: the dialog loads a [`PrSetup`] once,
//! recomputes a [`PrRange`] + [`PrBranchCheck`] whenever base or head changes, loads
//! [`PrCandidates`] for the metadata pickers, and submits a [`PrSubmit`]. Creating and
//! applying metadata are separate calls so a metadata failure never re-creates the PR.
#![allow(dead_code)]

use super::*;
use crate::inbox::Label;

/// One GitHub repository reachable through a git remote of the working copy.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrRepo {
    /// Git remote name (`origin`, `upstream`, …). Empty for a fork parent that has no
    /// local remote: it can be a base but never a head.
    pub remote: String,
    /// `github.com` or the GitHub Enterprise host name.
    pub host: String,
    pub owner: String,
    pub name: String,
    /// Default branch as GitHub reports it.
    pub default_branch: String,
    /// `owner/name` of the parent when this repository is a fork.
    pub parent: Option<String>,
    /// The signed-in user may push here (WRITE/MAINTAIN/ADMIN).
    pub can_push: bool,
}

impl PrRepo {
    /// `owner/name`.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A pull request template from the base repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrTemplate {
    /// File name, e.g. `pull_request_template.md` or `PULL_REQUEST_TEMPLATE/bug.md`.
    pub name: String,
    pub body: String,
}

/// What the dialog loads once when it opens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrSetup {
    /// Every GitHub repository known through the remotes, plus fork parents
    /// (with an empty `remote`) — candidates for base and (when `remote` is set) head.
    pub repos: Vec<PrRepo>,
    /// Index into `repos`: the fork parent when the main remote is a fork, else the
    /// repository of the main remote (origin).
    pub base_repo: usize,
    /// Index into `repos`: where the current branch is (or would be) pushed.
    pub head_repo: usize,
    /// Suggested base branch: the branch's recorded base, else the base repo's default.
    pub base_branch: String,
    /// The current local branch.
    pub head_branch: String,
    /// Signed-in GitHub login on the head repo's host.
    pub viewer: String,
    /// Templates of the base repo, the default one (`pull_request_template.md`) first.
    pub templates: Vec<PrTemplate>,
}

/// Commits and files a pull request from `head` into `base` would contain, computed
/// locally (three-dot: against the merge base).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrRange {
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: String,
    /// Newest first.
    pub commits: Vec<IncomingCommit>,
    /// Files changed between `merge_base` and `head_sha`.
    pub files: Vec<ChangedFile>,
}

/// What must happen to the head branch before GitHub can see it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PrPush {
    /// The remote branch exists and matches the local one.
    UpToDate,
    /// No remote branch yet: it will be created as `remote/branch`.
    Create { remote: String, branch: String },
    /// The local branch is ahead of `remote/branch`: a fast-forward push.
    Sync { remote: String, branch: String },
    /// The remote branch has commits the local one lacks: pushing would need force.
    Diverged { remote: String, branch: String },
}

/// Whether the selected base/head can become a pull request (gates the button).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PrBranchCheck {
    Ok,
    /// Head has no commits that base lacks.
    NoChanges,
    /// An open pull request already exists for this head into this base.
    AlreadyExists { pr: Pr },
}

/// One user who can be a reviewer or assignee.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrUser {
    pub login: String,
    pub name: String,
    pub avatar_url: String,
}

/// A team reviewer (organization repositories only).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrTeam {
    /// `org/slug`, as GitHub's reviewer APIs take it.
    pub slug: String,
    pub name: String,
}

/// Candidates for the reviewer/assignee/label pickers of a base repository.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrCandidates {
    /// Assignable users (minus the viewer for reviewers; the UI filters).
    pub users: Vec<PrUser>,
    pub teams: Vec<PrTeam>,
    pub labels: Vec<Label>,
}

/// Reviewers, assignees and labels to set on a pull request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrMetadata {
    pub reviewers: Vec<String>,
    /// `org/slug` team reviewers.
    pub team_reviewers: Vec<String>,
    pub assignees: Vec<String>,
    /// Label names.
    pub labels: Vec<String>,
}

impl PrMetadata {
    pub fn is_empty(&self) -> bool {
        self.reviewers.is_empty()
            && self.team_reviewers.is_empty()
            && self.assignees.is_empty()
            && self.labels.is_empty()
    }
}

/// Everything needed to open a pull request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PrSubmit {
    pub base: PrRepo,
    pub base_branch: String,
    pub head: PrRepo,
    /// Branch name on the head repository (may differ from the local name).
    pub head_branch: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

/// A pull request GitHub just created.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreatedPr {
    pub number: i64,
    pub url: String,
    /// GraphQL node id, for the metadata mutations.
    pub node_id: String,
    /// `owner/name` of the base repo.
    pub repo: String,
    pub host: String,
}

/// Load remotes, fork parent, default/suggested branches, viewer and templates.
pub fn pr_setup(cwd: &Path) -> Result<PrSetup, String> {
    let _ = cwd;
    Err("pr_setup: not implemented".into())
}

/// Branch names of `repo` as the local remote-tracking refs know them (for the base
/// picker), default branch first. For a repo without a local remote, ask GitHub.
pub fn pr_branches(cwd: &Path, repo: &PrRepo) -> Result<Vec<String>, String> {
    let _ = (cwd, repo);
    Err("pr_branches: not implemented".into())
}

/// Local branches (for the head picker), current first.
pub fn pr_local_branches(cwd: &Path) -> Result<Vec<String>, String> {
    let _ = cwd;
    Err("pr_local_branches: not implemented".into())
}

/// `git fetch` the remotes of base and head quietly so the range is not stale.
pub fn pr_fetch(cwd: &Path, remotes: &[String]) -> Result<(), String> {
    let _ = (cwd, remotes);
    Err("pr_fetch: not implemented".into())
}

/// Commits and files of `base_ref...head_ref` (refs like `origin/main`, `feature/x`).
pub fn pr_range(cwd: &Path, base_ref: &str, head_ref: &str) -> Result<PrRange, String> {
    let _ = (cwd, base_ref, head_ref);
    Err("pr_range: not implemented".into())
}

/// Diff of one file between two SHAs of a [`PrRange`] (merge base → head, or a
/// single commit's parent → commit).
pub fn pr_file_diff(cwd: &Path, old: &str, new: &str, path: &str) -> Result<FileDiff, String> {
    let _ = (cwd, old, new, path);
    Err("pr_file_diff: not implemented".into())
}

/// What pushing the local `branch` to `remote/remote_branch` would take.
pub fn pr_push_state(
    cwd: &Path,
    branch: &str,
    remote: &str,
    remote_branch: &str,
) -> Result<PrPush, String> {
    let _ = (cwd, branch, remote, remote_branch);
    Err("pr_push_state: not implemented".into())
}

/// Push local `branch` to `remote/remote_branch` and set it as upstream. Never forces.
pub fn pr_push(cwd: &Path, branch: &str, remote: &str, remote_branch: &str) -> Result<(), String> {
    let _ = (cwd, branch, remote, remote_branch);
    Err("pr_push: not implemented".into())
}

/// NoChanges / AlreadyExists / Ok for `head:head_branch` into `base:base_branch`.
/// Looks for an open PR by `head_owner:branch` even when the branch is not pushed yet.
pub fn pr_branch_check(
    cwd: &Path,
    base: &PrRepo,
    base_branch: &str,
    head: &PrRepo,
    head_branch: &str,
    range: &PrRange,
) -> Result<PrBranchCheck, String> {
    let _ = (cwd, base, base_branch, head, head_branch, range);
    Err("pr_branch_check: not implemented".into())
}

/// Assignable users, org teams and labels of `repo` (all pages).
pub fn pr_candidates(cwd: &Path, repo: &PrRepo) -> Result<PrCandidates, String> {
    let _ = (cwd, repo);
    Err("pr_candidates: not implemented".into())
}

/// Create the pull request (GraphQL `createPullRequest`). Fork heads use
/// `owner:branch`. Does not set metadata.
pub fn pr_open(cwd: &Path, submit: &PrSubmit) -> Result<CreatedPr, String> {
    let _ = (cwd, submit);
    Err("pr_open: not implemented".into())
}

/// Set reviewers, assignees and labels in one GraphQL request; skips empty lists.
/// Safe to retry after a partial failure.
pub fn pr_apply_metadata(cwd: &Path, pr: &CreatedPr, metadata: &PrMetadata) -> Result<(), String> {
    let _ = (cwd, pr, metadata);
    Err("pr_apply_metadata: not implemented".into())
}

/// Current reviewers/assignees/labels of an existing pull request (Inbox editing).
pub fn pr_metadata(cwd: &Path, host: &str, repo: &str, number: i64) -> Result<PrMetadata, String> {
    let _ = (cwd, host, repo, number);
    Err("pr_metadata: not implemented".into())
}

/// Replace reviewers/assignees/labels of an existing pull request with `metadata`
/// (adds and removes as needed; Inbox editing).
pub fn pr_set_metadata(
    cwd: &Path,
    host: &str,
    repo: &str,
    number: i64,
    metadata: &PrMetadata,
) -> Result<(), String> {
    let _ = (cwd, host, repo, number, metadata);
    Err("pr_set_metadata: not implemented".into())
}
