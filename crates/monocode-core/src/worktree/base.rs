//! Choosing and qualifying the commit a new worktree starts from (spec §4.1–2).
use super::git_ok;
use std::{path::Path, time::Duration};

/// Default timeout for refreshing a remote-tracking base.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

fn has_ref(cwd: &Path, full: &str) -> bool {
    git_ok(cwd, &["show-ref", "--verify", "--quiet", full])
}

/// `origin/HEAD` → `origin/main` → `origin/master` → `main` → `master`.
pub fn detect_default(cwd: &Path) -> Option<String> {
    if let Ok(head) = super::git(
        cwd,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    ) {
        let head = head.trim();
        if has_ref(cwd, head) {
            if let Some(short) = head.strip_prefix("refs/remotes/") {
                return Some(short.to_owned());
            }
        }
    }
    for (full, short) in [
        ("refs/remotes/origin/main", "origin/main"),
        ("refs/remotes/origin/master", "origin/master"),
        ("refs/heads/main", "main"),
        ("refs/heads/master", "master"),
    ] {
        if has_ref(cwd, full) {
            return Some(short.to_owned());
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// What the user sees and what `branch.<b>.base` records (`origin/main`, `main`, a SHA).
    pub reference: String,
    /// `refs/remotes/…`, `refs/heads/…`, `HEAD`, or the full commit SHA.
    pub qualified: String,
    /// `(remote, branch)` when the base is a remote-tracking ref.
    pub remote: Option<(String, String)>,
    /// The base is a raw commit, not a ref.
    pub commit: bool,
    /// Non-fatal notes, e.g. a stale repo default that was skipped.
    pub warnings: Vec<String>,
}

fn remotes(cwd: &Path) -> Vec<String> {
    super::git(cwd, &["remote"])
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Qualify one name: `X/Y` tries `refs/remotes/X/Y` then `refs/heads/X/Y`; other names try
/// `refs/heads/X`; then any commit-ish.
fn qualify(cwd: &Path, name: &str) -> Option<Resolved> {
    let resolved = |reference: &str, qualified: String, remote, commit| Resolved {
        reference: reference.to_owned(),
        qualified,
        remote,
        commit,
        warnings: Vec::new(),
    };
    if name == "HEAD" {
        return git_ok(cwd, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .then(|| resolved("HEAD", "HEAD".into(), None, false));
    }
    if name.starts_with('-') {
        return None;
    }
    if name.contains('/') {
        let full = format!("refs/remotes/{name}");
        if has_ref(cwd, &full) {
            let mut names = remotes(cwd);
            names.sort_by_key(|r| std::cmp::Reverse(r.len()));
            let remote = names.into_iter().find_map(|r| {
                let branch = name.strip_prefix(&format!("{r}/"))?;
                (!branch.is_empty()).then(|| (r, branch.to_owned()))
            });
            return Some(resolved(name, full, remote, false));
        }
    }
    let full = format!("refs/heads/{name}");
    if has_ref(cwd, &full) {
        return Some(resolved(name, full, None, false));
    }
    let sha = super::git(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ],
    )
    .ok()?;
    let sha = sha.trim().to_owned();
    Some(resolved(name, sha, None, true))
}

/// explicit start-from → repo `default_base` → [`detect_default`] → `HEAD`.
/// An explicit base that does not resolve is an error; a stale repo default is skipped
/// with a warning.
pub fn resolve(
    cwd: &Path,
    explicit: Option<&str>,
    repo_default: Option<&str>,
) -> Result<Resolved, String> {
    super::ensure_local(cwd)?;
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return qualify(cwd, name).ok_or_else(|| format!("Could not find {name} to start from"));
    }
    let mut warnings = Vec::new();
    if let Some(name) = repo_default.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(found) = qualify(cwd, name) {
            return Ok(found);
        }
        warnings.push(format!(
            "The default base {name} no longer exists; using the repository default."
        ));
    }
    let mut found = detect_default(cwd)
        .and_then(|name| qualify(cwd, &name))
        .or_else(|| qualify(cwd, "HEAD"))
        .ok_or("This repository has no commits to start from yet")?;
    found.warnings.extend(warnings);
    Ok(found)
}

/// `git fetch --no-tags <remote> <branch>` with a timeout.
pub fn fetch(cwd: &Path, remote: &str, branch: &str, timeout: Duration) -> Result<(), String> {
    super::git_live(
        cwd,
        &["fetch", "--no-tags", "--end-of-options", remote, branch],
        Some(timeout),
        &mut |_| {},
    )
    .map(|_| ())
}

/// The commit `resolved` points at now.
pub fn commit_of(cwd: &Path, resolved: &Resolved) -> Result<String, String> {
    super::git(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{}^{{commit}}", resolved.qualified),
        ],
    )
    .map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, run};
    use super::*;

    #[test]
    fn worktree_base_detection_order() {
        let (_t, main, _root, _origin) = fixture::with_origin();
        // A clone records origin/HEAD.
        assert_eq!(detect_default(&main).as_deref(), Some("origin/main"));
        // origin/HEAD pointing elsewhere wins.
        run(
            &main,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/feature",
            ],
        );
        assert_eq!(detect_default(&main).as_deref(), Some("origin/feature"));
        run(&main, &["remote", "set-head", "origin", "--delete"]);
        assert_eq!(detect_default(&main).as_deref(), Some("origin/main"));
        // origin/master is next.
        run(&main, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
        run(&main, &["update-ref", "-d", "refs/remotes/origin/main"]);
        assert_eq!(detect_default(&main).as_deref(), Some("origin/master"));
        run(&main, &["update-ref", "-d", "refs/remotes/origin/master"]);
        // Then local main / master.
        assert_eq!(detect_default(&main).as_deref(), Some("main"));
        run(&main, &["branch", "-m", "main", "master"]);
        assert_eq!(detect_default(&main).as_deref(), Some("master"));
        run(&main, &["branch", "-m", "master", "trunk"]);
        assert_eq!(detect_default(&main), None);
        // Nothing found → HEAD.
        let r = resolve(&main, None, None).unwrap();
        assert_eq!(
            (r.reference.as_str(), r.qualified.as_str()),
            ("HEAD", "HEAD")
        );
    }

    #[test]
    fn worktree_base_resolution_and_qualification() {
        let (_t, main, _root, _origin) = fixture::with_origin();
        let r = resolve(&main, None, None).unwrap();
        assert_eq!(r.qualified, "refs/remotes/origin/main");
        assert_eq!(r.remote, Some(("origin".into(), "main".into())));
        // Repo default beats detection; explicit beats repo default.
        let r = resolve(&main, None, Some("origin/feature")).unwrap();
        assert_eq!(r.reference, "origin/feature");
        let r = resolve(&main, Some("main"), Some("origin/feature")).unwrap();
        assert_eq!(
            (r.qualified.as_str(), r.remote.clone()),
            ("refs/heads/main", None)
        );
        // A local branch with a slash that is not a remote ref.
        run(&main, &["branch", "team/x"]);
        assert_eq!(
            resolve(&main, Some("team/x"), None).unwrap().qualified,
            "refs/heads/team/x"
        );
        // A SHA.
        let sha = run(&main, &["rev-parse", "HEAD"]).trim().to_owned();
        let r = resolve(&main, Some(&sha[..10]), None).unwrap();
        assert!(r.commit);
        assert_eq!(r.qualified, sha);
        // Stale repo default warns and falls back; bad explicit errors.
        let r = resolve(&main, None, Some("gone")).unwrap();
        assert_eq!(r.reference, "origin/main");
        assert_eq!(r.warnings.len(), 1);
        assert!(resolve(&main, Some("nope"), None).is_err());
        assert!(resolve(&main, Some("--output=x"), None).is_err());
        assert!(resolve(Path::new("remote://env/x"), None, None)
            .unwrap_err()
            .contains("remote"));
    }

    #[test]
    fn worktree_fetch_failure_is_an_error_with_timeout_support() {
        let (t, main, _root, _origin) = fixture::with_origin();
        assert!(fetch(&main, "origin", "main", FETCH_TIMEOUT).is_ok());
        let gone = t.path().join("missing.git");
        run(
            &main,
            &["remote", "set-url", "origin", &gone.to_string_lossy()],
        );
        assert!(fetch(&main, "origin", "main", FETCH_TIMEOUT).is_err());
    }
}
