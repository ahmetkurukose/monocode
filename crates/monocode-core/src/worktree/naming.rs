//! Folder slugs, branch names, and the collision loop (spec §3).
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

const MAX_SLUG_CHARS: usize = 64;
const MAX_SUFFIX: usize = 100;

/// Folder-safe name: Unicode letters/numbers, `.`, `_`, `-`; everything else becomes `-`.
pub fn slug(name: &str) -> Result<String, String> {
    static INVALID: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[^\p{L}\p{N}._-]+").unwrap());
    static DASHES: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"-{2,}").unwrap());
    static DOTS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\.{2,}").unwrap());
    let out = INVALID.replace_all(name.trim(), "-");
    let out = DASHES.replace_all(&out, "-");
    let out = DOTS.replace_all(&out, ".");
    let out = out.trim_matches(['.', '-']);
    let out: String = out.chars().take(MAX_SLUG_CHARS).collect();
    let out = out.trim_end_matches(['.', '-']);
    if out.is_empty() || out == "." || out == ".." {
        return Err("Enter a name with at least one letter or number".into());
    }
    Ok(out.to_owned())
}

/// Composer "New worktree" without a name: the first ~6 words of the prompt.
pub fn from_prompt(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(6).collect();
    slug(&words.join(" ").to_lowercase()).unwrap_or_else(|_| "worktree".into())
}

/// How new branches are prefixed. Stored as `none` | `git-username` | `custom:<p>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum BranchPrefix {
    #[default]
    None,
    GitUsername,
    Custom(String),
}
impl From<String> for BranchPrefix {
    fn from(value: String) -> Self {
        match value.as_str() {
            "git-username" => Self::GitUsername,
            v => match v.strip_prefix("custom:") {
                Some(p) if !p.trim().is_empty() => Self::Custom(p.trim().into()),
                _ => Self::None,
            },
        }
    }
}
impl From<BranchPrefix> for String {
    fn from(value: BranchPrefix) -> Self {
        match value {
            BranchPrefix::None => "none".into(),
            BranchPrefix::GitUsername => "git-username".into(),
            BranchPrefix::Custom(p) => format!("custom:{p}"),
        }
    }
}

/// The prefix to put before `/<slug>`, or `None`. Invalid prefixes are dropped silently.
pub fn resolve_prefix(prefix: &BranchPrefix, cwd: &Path) -> Option<String> {
    let raw = match prefix {
        BranchPrefix::None => return None,
        BranchPrefix::GitUsername => super::git(cwd, &["config", "--get", "user.name"]).ok()?,
        BranchPrefix::Custom(p) => p.clone(),
    };
    let segments: Vec<String> = raw
        .split('/')
        .filter(|s| !s.trim().is_empty())
        .map(|s| slug(s).map(|s| leaf(&s)))
        .collect::<Result<_, _>>()
        .ok()?;
    let prefix = segments.join("/");
    (!prefix.is_empty() && crate::git::valid_branch(cwd, &format!("{prefix}/x")).is_ok())
        .then_some(prefix)
}

/// A ref component may not end with `.lock`.
fn leaf(name: &str) -> String {
    match name.strip_suffix(".lock") {
        Some(stem) => format!("{stem}-lock"),
        None => name.to_owned(),
    }
}

fn join(prefix: Option<&str>, slug: &str) -> String {
    let slug = leaf(slug);
    match prefix {
        Some(p) => format!("{p}/{slug}"),
        None => slug,
    }
}

/// `<prefix>/<slug>` or `<slug>`.
pub fn branch_for(slug: &str, prefix: &BranchPrefix, cwd: &Path) -> String {
    join(resolve_prefix(prefix, cwd).as_deref(), slug)
}

static CLAIMS: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

/// Holds a destination path for one in-process creation; released on drop.
#[derive(Debug)]
pub struct Claim(PathBuf);
impl Claim {
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Claim {
    fn drop(&mut self) {
        CLAIMS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Claim `path`, or `None` when another in-process creation holds it.
pub fn claim(path: &Path) -> Option<Claim> {
    let path = path.to_path_buf();
    let inserted = CLAIMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.clone());
    // The guard is released above: constructing a Claim that is then dropped must not
    // re-lock (or release someone else's claim).
    inserted.then(|| Claim(path))
}

pub struct PickRequest<'a> {
    /// Any checkout of the repository (Git commands run here).
    pub cwd: &'a Path,
    /// `layout::repo_dir(root, main)`.
    pub repo_dir: &'a Path,
    pub slug: &'a str,
    /// Already resolved (`resolve_prefix`).
    pub prefix: Option<&'a str>,
    /// Explicit branch override: used as-is, never suffixed.
    pub branch: Option<&'a str>,
    /// The explicit branch is an existing one to check out.
    pub existing: bool,
}

#[derive(Debug)]
pub struct Picked {
    pub slug: String,
    pub path: PathBuf,
    pub branch: String,
    pub claim: Claim,
}

pub(crate) fn local_branch_exists(cwd: &Path, branch: &str) -> bool {
    super::git_ok(
        cwd,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}
pub(crate) fn origin_branch_exists(cwd: &Path, branch: &str) -> bool {
    super::git_ok(
        cwd,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
}

/// Try `slug`, `slug-2` … `slug-100`; see spec §3. A candidate is taken when its path
/// exists, is registered with Git (even as a missing/prunable worktree), is claimed by
/// another in-process creation, or (without an explicit branch) its branch exists.
pub fn pick(req: &PickRequest) -> Result<Picked, String> {
    if let Some(branch) = req.branch {
        crate::git::valid_branch(req.cwd, branch)?;
        if !req.existing && local_branch_exists(req.cwd, branch) {
            return Err(format!(
                "A branch named {branch} already exists. Use the existing branch or choose another name."
            ));
        }
        if !req.existing && origin_branch_exists(req.cwd, branch) {
            return Err(format!(
                "origin/{branch} already exists. Use the existing branch or choose another name."
            ));
        }
    }
    // `git worktree add` refuses a missing-but-registered path, and rolling that failure
    // back must never touch the other worktree's registration or meta.
    let registered: HashSet<String> = super::catalog::registered(req.cwd)
        .map(|trees| {
            trees
                .iter()
                .map(|t| super::meta::key(Path::new(&t.path)))
                .collect()
        })
        .unwrap_or_default();
    for n in 1..=MAX_SUFFIX {
        let slug = if n == 1 {
            req.slug.to_owned()
        } else {
            format!("{}-{n}", req.slug)
        };
        let path = req.repo_dir.join(&slug);
        if path.symlink_metadata().is_ok() || registered.contains(&super::meta::key(&path)) {
            continue;
        }
        let branch = match req.branch {
            Some(b) => b.to_owned(),
            None => {
                let branch = join(req.prefix, &slug);
                if local_branch_exists(req.cwd, &branch) || origin_branch_exists(req.cwd, &branch) {
                    continue;
                }
                branch
            }
        };
        let Some(claim) = claim(&path) else {
            continue;
        };
        if req.branch.is_none() {
            crate::git::valid_branch(req.cwd, &branch)?;
        }
        return Ok(Picked {
            slug,
            path,
            branch,
            claim,
        });
    }
    Err(format!(
        "Could not find a free name for {} (tried {MAX_SUFFIX}). Choose another name.",
        req.slug
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, run};
    use super::*;

    #[test]
    fn worktree_slug_rules() {
        let ok = |s: &str| slug(s).unwrap();
        assert_eq!(ok("Fix login bug"), "Fix-login-bug");
        assert_eq!(ok("  feature/new thing  "), "feature-new-thing");
        assert_eq!(ok("a--b___c"), "a-b___c");
        assert_eq!(ok("a...b..c"), "a.b.c");
        assert_eq!(ok("--.x.--"), "x");
        assert_eq!(ok("../../etc/passwd"), "etc-passwd");
        assert_eq!(ok("Ünïcödé 名前"), "Ünïcödé-名前");
        assert_eq!(ok("café_ü-2"), "café_ü-2");
        assert_eq!(ok("🚀 launch rocket"), "launch-rocket");
        assert_eq!(ok("x🚀y"), "x-y");
        assert_eq!(ok("v1.2.lock"), "v1.2.lock");
        assert_eq!(ok("@{upstream}"), "upstream");
        assert_eq!(ok("١٢٣"), "١٢٣");
        let long = "a".repeat(80);
        assert_eq!(ok(&long).chars().count(), 64);
        // Char-boundary safe truncation and trailing separators trimmed.
        let multi = format!("{}-{}", "é".repeat(63), "zz");
        assert_eq!(ok(&multi), "é".repeat(63));
        for bad in ["", "   ", ".", "..", "...", "-", "🚀🚀", "///", "!!!", "@"] {
            assert!(slug(bad).is_err(), "{bad:?} must be rejected");
        }
        assert_eq!(
            from_prompt("Fix the Login page when user clicks twice"),
            "fix-the-login-page-when-user"
        );
        assert_eq!(from_prompt("   "), "worktree");
        assert_eq!(from_prompt("🚀"), "worktree");
    }

    #[test]
    fn worktree_branch_prefix_modes() {
        let (_t, main, _root) = fixture::repo();
        assert_eq!(branch_for("x", &BranchPrefix::None, &main), "x");
        assert_eq!(
            branch_for("x", &BranchPrefix::Custom("team/me".into()), &main),
            "team/me/x"
        );
        assert_eq!(
            branch_for("x", &BranchPrefix::Custom("bad name.lock".into()), &main),
            "bad-name-lock/x"
        );
        run(&main, &["config", "user.name", "Jane Doe"]);
        assert_eq!(
            branch_for("x", &BranchPrefix::GitUsername, &main),
            "Jane-Doe/x"
        );
        // An unusable user name is dropped silently.
        run(&main, &["config", "user.name", "🚀"]);
        assert_eq!(branch_for("x", &BranchPrefix::GitUsername, &main), "x");
        assert_eq!(branch_for("v1.lock", &BranchPrefix::None, &main), "v1-lock");
        // Serde round trip.
        for (p, s) in [
            (BranchPrefix::None, "\"none\""),
            (BranchPrefix::GitUsername, "\"git-username\""),
            (BranchPrefix::Custom("me".into()), "\"custom:me\""),
        ] {
            assert_eq!(serde_json::to_string(&p).unwrap(), s);
            assert_eq!(serde_json::from_str::<BranchPrefix>(s).unwrap(), p);
        }
        assert_eq!(
            serde_json::from_str::<BranchPrefix>("\"future-mode\"").unwrap(),
            BranchPrefix::None
        );
    }

    #[test]
    fn worktree_collision_loop() {
        let (_t, main, root, _origin) = fixture::with_origin();
        let repo_dir = root.join("project");
        let req = |slug: &'static str| PickRequest {
            cwd: &main,
            repo_dir: &repo_dir,
            slug,
            prefix: None,
            branch: None,
            existing: false,
        };
        // Local branch exists.
        run(&main, &["branch", "taken"]);
        let p = pick(&req("taken")).unwrap();
        assert_eq!((p.slug.as_str(), p.branch.as_str()), ("taken-2", "taken-2"));
        drop(p);
        // origin/<branch> exists (feature is only a remote branch here).
        assert!(!local_branch_exists(&main, "feature"));
        assert_eq!(pick(&req("feature")).unwrap().branch, "feature-2");
        // Path exists.
        std::fs::create_dir_all(repo_dir.join("dir")).unwrap();
        std::fs::create_dir_all(repo_dir.join("dir-2")).unwrap();
        assert_eq!(pick(&req("dir")).unwrap().slug, "dir-3");
        // Concurrent in-process claim.
        let held = claim(&repo_dir.join("busy")).unwrap();
        assert!(claim(&repo_dir.join("busy")).is_none());
        let p = pick(&req("busy")).unwrap();
        assert_eq!(p.slug, "busy-2");
        drop(held);
        drop(p);
        assert_eq!(pick(&req("busy")).unwrap().slug, "busy");
        // Prefix applies per candidate.
        run(&main, &["branch", "me/pre"]);
        let p = pick(&PickRequest {
            prefix: Some("me"),
            ..req("pre")
        })
        .unwrap();
        assert_eq!(p.branch, "me/pre-2");
        // Explicit branch: never suffixed, error on collision; folder may be suffixed.
        assert!(pick(&PickRequest {
            branch: Some("taken"),
            ..req("x")
        })
        .unwrap_err()
        .contains("already exists"));
        assert!(pick(&PickRequest {
            branch: Some("feature"),
            ..req("x")
        })
        .unwrap_err()
        .contains("origin/feature"));
        let p = pick(&PickRequest {
            branch: Some("brand/new"),
            ..req("dir")
        })
        .unwrap();
        assert_eq!((p.slug.as_str(), p.branch.as_str()), ("dir-3", "brand/new"));
        assert!(pick(&PickRequest {
            branch: Some("-bad"),
            ..req("x")
        })
        .is_err());
        let p = pick(&PickRequest {
            branch: Some("taken"),
            existing: true,
            ..req("taken")
        })
        .unwrap();
        assert_eq!(p.branch, "taken");
        drop(p);
        // A registered worktree whose folder is gone (not pruned) still takes its path.
        let gone = repo_dir.join("gone");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "gone-branch",
                "--",
                &gone.to_string_lossy(),
            ],
        );
        std::fs::remove_dir_all(&gone).unwrap();
        let p = pick(&PickRequest {
            branch: Some("fresh"),
            ..req("gone")
        })
        .unwrap();
        assert_eq!(p.slug, "gone-2");
    }
}
