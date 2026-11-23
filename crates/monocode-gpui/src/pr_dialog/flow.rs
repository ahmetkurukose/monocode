//! The New Pull Request dialog's decisions, apart from its views: the title
//! it suggests, when Create is allowed, and the push → create → metadata run
//! (a metadata failure keeps the created pull request, so a retry only sets
//! the metadata again). Git and GitHub sit behind [`PrBackend`] so tests can
//! stand in for them.
use monocode_core::scm::{
    self, ChangedFile, CreatedPr, FileDiff, IncomingCommit, PrBranchCheck, PrCandidates,
    PrMetadata, PrPush, PrRange, PrRepo, PrSetup, PrSubmit, RangeContext,
};
use std::path::{Path, PathBuf};

/// Something loading in the background.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Load<T> {
    Loading,
    Failed(String),
    Ready(T),
}
impl<T> Load<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }
    pub(crate) fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Load::Ready(value),
            Err(error) => Load::Failed(error),
        }
    }
}

/// Everything the dialog asks of git and GitHub. [`Gh`] is the real one.
pub(crate) trait PrBackend: Send + Sync + 'static {
    fn setup(&self, cwd: &Path) -> Result<PrSetup, String>;
    fn fetch(&self, cwd: &Path, remotes: &[String]) -> Result<(), String>;
    fn branches(&self, cwd: &Path, repo: &PrRepo) -> Result<Vec<String>, String>;
    fn local_branches(&self, cwd: &Path) -> Result<Vec<String>, String>;
    fn range(&self, cwd: &Path, base_ref: &str, head_ref: &str) -> Result<PrRange, String>;
    fn commit_files(&self, cwd: &Path, sha: &str) -> Result<Vec<ChangedFile>, String>;
    fn file_diff(&self, cwd: &Path, old: &str, new: &str, path: &str) -> Result<FileDiff, String>;
    fn push_state(
        &self,
        cwd: &Path,
        branch: &str,
        remote: &str,
        remote_branch: &str,
    ) -> Result<PrPush, String>;
    fn check(
        &self,
        cwd: &Path,
        request: &CheckRequest,
        range: &PrRange,
    ) -> Result<PrBranchCheck, String>;
    fn candidates(&self, cwd: &Path, repo: &PrRepo) -> Result<PrCandidates, String>;
    fn push(&self, cwd: &Path, plan: &PushPlan) -> Result<(), String>;
    fn open(&self, cwd: &Path, submit: &PrSubmit) -> Result<CreatedPr, String>;
    fn apply_metadata(
        &self,
        cwd: &Path,
        pr: &CreatedPr,
        metadata: &PrMetadata,
    ) -> Result<(), String>;
}

/// The base and head a branch check is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckRequest {
    pub(crate) base: PrRepo,
    pub(crate) base_branch: String,
    pub(crate) head: PrRepo,
    /// The branch name on the head repository.
    pub(crate) head_branch: String,
}

/// `git` and `gh`, through `monocode_core::scm`.
pub(crate) struct Gh;
impl PrBackend for Gh {
    fn setup(&self, cwd: &Path) -> Result<PrSetup, String> {
        scm::pr_setup(cwd)
    }
    fn fetch(&self, cwd: &Path, remotes: &[String]) -> Result<(), String> {
        scm::pr_fetch(cwd, remotes)
    }
    fn branches(&self, cwd: &Path, repo: &PrRepo) -> Result<Vec<String>, String> {
        scm::pr_branches(cwd, repo)
    }
    fn local_branches(&self, cwd: &Path) -> Result<Vec<String>, String> {
        scm::pr_local_branches(cwd)
    }
    fn range(&self, cwd: &Path, base_ref: &str, head_ref: &str) -> Result<PrRange, String> {
        scm::pr_range(cwd, base_ref, head_ref)
    }
    fn commit_files(&self, cwd: &Path, sha: &str) -> Result<Vec<ChangedFile>, String> {
        scm::commit_files(cwd, sha)
    }
    fn file_diff(&self, cwd: &Path, old: &str, new: &str, path: &str) -> Result<FileDiff, String> {
        scm::pr_file_diff(cwd, old, new, path)
    }
    fn push_state(
        &self,
        cwd: &Path,
        branch: &str,
        remote: &str,
        remote_branch: &str,
    ) -> Result<PrPush, String> {
        scm::pr_push_state(cwd, branch, remote, remote_branch)
    }
    fn check(
        &self,
        cwd: &Path,
        request: &CheckRequest,
        range: &PrRange,
    ) -> Result<PrBranchCheck, String> {
        scm::pr_branch_check(
            cwd,
            &request.base,
            &request.base_branch,
            &request.head,
            &request.head_branch,
            range,
        )
    }
    fn candidates(&self, cwd: &Path, repo: &PrRepo) -> Result<PrCandidates, String> {
        scm::pr_candidates(cwd, repo)
    }
    fn push(&self, cwd: &Path, plan: &PushPlan) -> Result<(), String> {
        scm::pr_push(cwd, &plan.branch, &plan.remote, &plan.remote_branch)
    }
    fn open(&self, cwd: &Path, submit: &PrSubmit) -> Result<CreatedPr, String> {
        scm::pr_open(cwd, submit)
    }
    fn apply_metadata(
        &self,
        cwd: &Path,
        pr: &CreatedPr,
        metadata: &PrMetadata,
    ) -> Result<(), String> {
        scm::pr_apply_metadata(cwd, pr, metadata)
    }
}

/// `feature/foo-bar` → "Foo bar": the last path part, dashes and
/// underscores as spaces, the first letter capitalised.
pub(crate) fn humanize_branch(branch: &str) -> String {
    let last = branch
        .trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let words = last
        .split(['-', '_', '.'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The title the dialog suggests: a lone commit's subject, else the head
/// branch in words.
pub(crate) fn suggested_title(commits: Option<&[IncomingCommit]>, head_branch: &str) -> String {
    match commits {
        Some([only]) if !only.subject.trim().is_empty() => only.subject.trim().to_owned(),
        _ => humanize_branch(head_branch),
    }
}

/// The title Create sends: what was typed, else the head branch in words.
pub(crate) fn effective_title(typed: &str, head_branch: &str) -> String {
    let typed = typed.trim();
    if typed.is_empty() {
        humanize_branch(head_branch)
    } else {
        typed.to_owned()
    }
}

/// Whether Create Pull Request can run.
pub(crate) fn can_create(
    check: &Load<PrBranchCheck>,
    push: &Load<PrPush>,
    busy: bool,
    title: &str,
) -> bool {
    !busy
        && matches!(check, Load::Ready(PrBranchCheck::Ok))
        && matches!(push, Load::Ready(p) if !matches!(p, PrPush::Diverged { .. }))
        && !title.trim().is_empty()
}

/// Pushing the local `branch` to `remote/remote_branch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PushPlan {
    pub(crate) branch: String,
    pub(crate) remote: String,
    pub(crate) remote_branch: String,
}

/// What a push state asks for before the pull request can be opened.
pub(crate) fn push_plan(local: &str, push: &PrPush) -> Option<PushPlan> {
    match push {
        PrPush::Create { remote, branch } | PrPush::Sync { remote, branch } => Some(PushPlan {
            branch: local.to_owned(),
            remote: remote.clone(),
            remote_branch: branch.clone(),
        }),
        PrPush::UpToDate | PrPush::Diverged { .. } => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Pushing,
    Creating,
    Metadata,
}
impl Stage {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Stage::Pushing => "Pushing…",
            Stage::Creating => "Creating pull request…",
            Stage::Metadata => "Updating reviewers, assignees and labels…",
        }
    }
}

/// One Create press: what to push, open and set, and what got created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Submission {
    pub(crate) cwd: PathBuf,
    pub(crate) push: Option<PushPlan>,
    pub(crate) submit: PrSubmit,
    pub(crate) metadata: PrMetadata,
    /// Set once GitHub created the pull request; later runs never create again.
    pub(crate) created: Option<CreatedPr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Created(CreatedPr),
    /// The pull request exists, but setting its metadata failed.
    MetadataFailed(CreatedPr, String),
    Failed(String),
}

impl Submission {
    /// The steps still to run.
    pub(crate) fn stages(&self) -> Vec<Stage> {
        let mut stages = Vec::new();
        if self.created.is_none() {
            if self.push.is_some() {
                stages.push(Stage::Pushing);
            }
            stages.push(Stage::Creating);
        }
        if !self.metadata.is_empty() {
            stages.push(Stage::Metadata);
        }
        stages
    }
    /// Runs one step. Blocking.
    pub(crate) fn run(&mut self, stage: Stage, backend: &dyn PrBackend) -> Result<(), String> {
        match stage {
            Stage::Pushing => match &self.push {
                Some(plan) => backend.push(&self.cwd, plan),
                None => Ok(()),
            },
            Stage::Creating => {
                if self.created.is_none() {
                    self.created = Some(backend.open(&self.cwd, &self.submit)?);
                }
                Ok(())
            }
            Stage::Metadata => match &self.created {
                Some(pr) => backend.apply_metadata(&self.cwd, pr, &self.metadata),
                None => Err("The pull request was not created".into()),
            },
        }
    }
    /// How the run ended, given the step that failed, if one did.
    pub(crate) fn outcome(&self, failed: Option<(Stage, String)>) -> Outcome {
        match (failed, &self.created) {
            (None, Some(pr)) => Outcome::Created(pr.clone()),
            (None, None) => Outcome::Failed("The pull request was not created".into()),
            (Some((Stage::Metadata, error)), Some(pr)) => {
                Outcome::MetadataFailed(pr.clone(), error)
            }
            (Some((_, error)), _) => Outcome::Failed(error),
        }
    }
    /// Every remaining step in order, stopping at the first failure. Blocking.
    #[cfg(test)]
    pub(crate) fn run_all(&mut self, backend: &dyn PrBackend) -> Outcome {
        for stage in self.stages() {
            if let Err(error) = self.run(stage, backend) {
                return self.outcome(Some((stage, error)));
            }
        }
        self.outcome(None)
    }
}

/// What the model is told about the chosen commits: their subjects, the
/// files, and `git diff old new`. Blocking.
pub(crate) fn range_context(
    cwd: &Path,
    base: &str,
    head: &str,
    commits: &[IncomingCommit],
    files: &[ChangedFile],
    old: &str,
    new: &str,
) -> Result<RangeContext, String> {
    let commit_summary = commits
        .iter()
        .map(|c| c.subject.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let diff_summary = files
        .iter()
        .map(|f| format!("{} | +{} -{}", f.relative, f.additions, f.deletions))
        .collect::<Vec<_>>()
        .join("\n");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["diff", "--no-ext-diff", "--no-color", old, new])
        .output()
        .map_err(|e| format!("Couldn’t run git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(RangeContext {
        base: base.to_owned(),
        head: head.to_owned(),
        commit_summary,
        diff_summary,
        diff_patch: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}
