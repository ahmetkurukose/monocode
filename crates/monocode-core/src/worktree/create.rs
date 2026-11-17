//! The creation pipeline (spec §4).
use super::{
    base, config, content, git, git_live, git_ok, layout, lock,
    meta::{self, CreatedBy, StartKind, WorktreeMeta},
    naming::{self, BranchPrefix},
    setup::{self, SetupPlan},
    Phase,
};
use crate::store::Store;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct CreateRequest {
    /// Any checkout of the repository.
    pub cwd: PathBuf,
    /// The user's name for the worktree; slugified for the folder (and branch).
    /// May be empty when `branch` is given.
    pub name: String,
    /// Shown in the UI; defaults to the trimmed `name`, else the slug.
    pub display_name: Option<String>,
    /// Explicit start-from (`origin/main`, `main`, a SHA). `None` → repo default → detected.
    pub base: Option<String>,
    /// Explicit branch override, used as-is (validated, never suffixed).
    pub branch: Option<String>,
    /// Check out the existing local branch `branch` (or create a tracking branch for
    /// `origin/<branch>` when only the remote has it) instead of creating one.
    pub existing_branch: bool,
    /// `None` → `worktree-prefs.root` → `~/monocode/workspaces`.
    pub root: Option<PathBuf>,
    /// `None` → `worktree-prefs.branch_prefix`.
    pub prefix: Option<BranchPrefix>,
    pub created_by: CreatedBy,
    pub parent: Option<PathBuf>,
    /// Refresh a remote-tracking base before branching from it.
    pub fetch: bool,
    pub fetch_timeout: Duration,
}
impl CreateRequest {
    pub fn new(cwd: &Path, name: &str) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            name: name.to_owned(),
            display_name: None,
            base: None,
            branch: None,
            existing_branch: false,
            root: None,
            prefix: None,
            created_by: CreatedBy::Api,
            parent: None,
            fetch: true,
            fetch_timeout: base::FETCH_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Created {
    pub path: PathBuf,
    pub branch: String,
    /// The base reference recorded in `branch.<b>.base` and the meta.
    pub base: String,
    /// The commit the worktree was created at.
    pub head: String,
    pub meta: WorktreeMeta,
    pub content: content::Report,
    /// Non-fatal problems (stale fetch, skipped content paths, config warnings, …).
    pub warnings: Vec<String>,
    pub setup: Option<SetupPlan>,
}

enum Mode {
    New { sha: String },
    Existing,
    Remote,
}

fn checked_out(main: &Path, branch: &str) -> bool {
    super::catalog::registered(main)
        .map(|trees| trees.iter().any(|t| t.branch.as_deref() == Some(branch)))
        .unwrap_or(false)
}

fn percent(line: &str) -> Option<u8> {
    let rest = line.split("Updating files:").nth(1)?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    rest.trim_start()[digits.len()..]
        .starts_with('%')
        .then(|| digits.parse::<u8>().ok())
        .flatten()
        .map(|p| p.min(100))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Undo everything after `worktree add`. Only touches what this call created: the
/// path did not exist before (collision loop) and `branch` is `Some` only for a branch
/// this call created.
fn rollback(main: &Path, path: &Path, branch: Option<&str>, store: &Mutex<Store>) {
    let spelled = path.to_string_lossy();
    let _ = git(main, &["worktree", "remove", "--force", "--", &spelled]);
    if path.symlink_metadata().is_ok() {
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = git(main, &["worktree", "prune"]);
    if let Some(branch) = branch {
        let _ = git(main, &["branch", "-D", "--", branch]);
    }
    let _ = meta::delete(&lock(store), path);
}

/// Create a worktree under the configured root. See spec §4 for the steps.
pub fn create(
    req: &CreateRequest,
    store: &Mutex<Store>,
    progress: &dyn Fn(Phase),
) -> Result<Created, String> {
    super::ensure_local(&req.cwd)?;
    progress(Phase::Resolving);
    let main = layout::main_checkout(&req.cwd)?;
    let (prefs, repo) = {
        let s = lock(store);
        (meta::prefs(&s)?, meta::repo_prefs(&s, &main)?)
    };
    let root = req.root.clone().unwrap_or_else(|| prefs.root_or_default());
    if !root.is_absolute() {
        return Err("The worktree location must be an absolute path".into());
    }
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("Could not create {}: {e}", root.display()))?;
    let root = super::canonical(&root);
    let mut warnings = Vec::new();
    let explicit = req
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty());

    // 1–2. Base (or existing branch) and fetch.
    let (mode, base_ref, start_kind) = if req.existing_branch {
        let branch = explicit.ok_or("Choose the branch to check out")?;
        crate::git::valid_branch(&main, branch)?;
        if naming::local_branch_exists(&main, branch) {
            if checked_out(&main, branch) {
                return Err(
                    "This branch already has a working copy. Select it from the picker.".into(),
                );
            }
            (Mode::Existing, branch.to_owned(), StartKind::Branch)
        } else if naming::origin_branch_exists(&main, branch) {
            if req.fetch {
                progress(Phase::Fetching);
                if let Err(e) = base::fetch(&main, "origin", branch, req.fetch_timeout) {
                    warnings.push(format!(
                        "Could not refresh origin/{branch} ({e}); using the last fetched copy."
                    ));
                }
            }
            (Mode::Remote, format!("origin/{branch}"), StartKind::Remote)
        } else {
            return Err(format!("There is no branch named {branch}"));
        }
    } else {
        let resolved = base::resolve(&main, req.base.as_deref(), repo.default_base.as_deref())?;
        warnings.extend(resolved.warnings.iter().cloned());
        if let (Some((remote, branch)), true) = (&resolved.remote, req.fetch) {
            progress(Phase::Fetching);
            if let Err(e) = base::fetch(&main, remote, branch, req.fetch_timeout) {
                warnings.push(format!(
                    "Could not refresh {} ({e}); using the last fetched copy.",
                    resolved.reference
                ));
            }
        }
        let sha = base::commit_of(&main, &resolved)?;
        let kind = if resolved.commit {
            StartKind::Commit
        } else {
            StartKind::Base
        };
        let reference = if resolved.commit {
            sha.clone()
        } else {
            resolved.reference.clone()
        };
        (Mode::New { sha }, reference, kind)
    };

    // 3. Names and the path claim (held until we return).
    let slug = match naming::slug(&req.name) {
        Ok(slug) => slug,
        Err(e) => match explicit {
            Some(branch) => naming::slug(branch)?,
            None => return Err(e),
        },
    };
    let prefix = naming::resolve_prefix(req.prefix.as_ref().unwrap_or(&prefs.branch_prefix), &main);
    let repo_dir = layout::repo_dir(&root, &main);
    let picked = naming::pick(&naming::PickRequest {
        cwd: &main,
        repo_dir: &repo_dir,
        slug: &slug,
        prefix: prefix.as_deref(),
        branch: explicit,
        existing: req.existing_branch,
    })?;
    let branch = picked.branch.clone();
    let path = picked.path.clone();
    std::fs::create_dir_all(&repo_dir)
        .map_err(|e| format!("Could not create {}: {e}", repo_dir.display()))?;

    // 4. worktree add.
    let spelled = path.to_string_lossy().into_owned();
    let remote_ref = format!("origin/{branch}");
    let mut args: Vec<&str> = Vec::new();
    if !git_ok(&main, &["config", "--get", "checkout.workers"]) {
        args.extend(["-c", "checkout.workers=4"]);
    }
    args.extend(["worktree", "add"]);
    match &mode {
        Mode::New { sha } => args.extend([
            "--no-track",
            "-b",
            branch.as_str(),
            "--",
            spelled.as_str(),
            sha.as_str(),
        ]),
        Mode::Existing => args.extend(["--", spelled.as_str(), branch.as_str()]),
        Mode::Remote => args.extend([
            "--track",
            "-b",
            branch.as_str(),
            "--",
            spelled.as_str(),
            remote_ref.as_str(),
        ]),
    }
    let owned_branch = (!matches!(mode, Mode::Existing)).then_some(branch.as_str());
    progress(Phase::CheckingOut { percent: None });
    let mut last = None;
    let added = git_live(&main, &args, None, &mut |line| {
        if let Some(p) = percent(line) {
            if last.is_none_or(|l| p > l) {
                last = Some(p);
                progress(Phase::CheckingOut { percent: Some(p) });
            }
        }
    });
    if let Err(e) = added {
        rollback(&main, &path, owned_branch, store);
        return Err(e);
    }
    let path = super::canonical(&path);
    let fail = |e: String| {
        rollback(&main, &path, owned_branch, store);
        e
    };

    // 5. Branch config.
    let head = git(&path, &["rev-parse", "HEAD"])
        .map_err(fail)?
        .trim()
        .to_owned();
    if !matches!(mode, Mode::Existing) {
        git(
            &path,
            &[
                "config",
                "--local",
                &format!("branch.{branch}.base"),
                &base_ref,
            ],
        )
        .map_err(fail)?;
    }
    if !git_ok(&path, &["config", "--get", "push.autoSetupRemote"]) {
        git(
            &path,
            &["config", "--local", "push.autoSetupRemote", "true"],
        )
        .map_err(fail)?;
    }

    // 6. Meta, before content paths, so a crash still leaves an owned row.
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| Some(req.name.trim()).filter(|s| !s.is_empty()))
        .unwrap_or(&picked.slug)
        .to_owned();
    let record = WorktreeMeta {
        path: path.to_string_lossy().into_owned(),
        repo: main.to_string_lossy().into_owned(),
        display_name,
        branch_at_create: branch.clone(),
        base_ref: base_ref.clone(),
        start_kind,
        created_at: now_ms(),
        created_by: req.created_by,
        branch_user_chosen: explicit.is_some(),
        // A branch that existed before this worktree is the user's; never delete it.
        preserve_branch_on_delete: matches!(mode, Mode::Existing),
        pinned: false,
        comment: String::new(),
        parent: req
            .parent
            .as_deref()
            .map(|p| super::canonical(p).to_string_lossy().into_owned()),
    };
    meta::save(&lock(store), &record).map_err(fail)?;

    // 7. Content paths; never fatal.
    progress(Phase::CopyingFiles);
    let config = config::load(&main);
    warnings.extend(config.warnings.iter().cloned());
    let report = content::materialize(&main, &path, &config);
    warnings.extend(report.warnings.iter().cloned());

    // 8. Setup plan.
    let setup = setup::plan(
        &main,
        &path,
        &picked.slug,
        repo.setup_approved_hash.as_deref(),
    );
    drop(picked);
    progress(Phase::Done);
    Ok(Created {
        path,
        branch,
        base: base_ref,
        head,
        meta: record,
        content: report,
        warnings,
        setup,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{self, has_branch, run};
    use super::*;
    use std::cell::RefCell;

    fn request(main: &Path, root: &Path, name: &str) -> CreateRequest {
        CreateRequest {
            root: Some(root.to_path_buf()),
            ..CreateRequest::new(main, name)
        }
    }

    #[test]
    fn worktree_progress_percent_parsing() {
        assert_eq!(percent("Updating files:  45% (9/20)"), Some(45));
        assert_eq!(percent("Updating files: 100% (20/20), done."), Some(100));
        assert_eq!(percent("Preparing worktree (new branch 'x')"), None);
        assert_eq!(percent("Updating files: x%"), None);
    }

    #[test]
    fn worktree_create_new_branch_from_fetched_default_base() {
        let (t, main, root, origin) = fixture::with_origin();
        let base_dir = t.path().canonicalize().unwrap();
        // origin moves on after the clone; create must fetch it.
        let seed = base_dir.join("seed");
        fixture::commit(&seed, "new.txt", "new\n", "Upstream");
        run(&seed, &["push", "-q", &origin.to_string_lossy(), "main"]);
        let upstream = run(&seed, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(main.join(".gitignore"), "node_modules/\n.env\n").unwrap();
        std::fs::create_dir(main.join("node_modules")).unwrap();
        std::fs::write(main.join(".env"), "A=1").unwrap();
        std::fs::write(main.join(".worktreeinclude"), ".env\n").unwrap();
        std::fs::write(
            main.join("monocode.yaml"),
            "worktree:\n  sharedDirectories: [node_modules]\nscripts:\n  setup: npm ci\n",
        )
        .unwrap();

        let store = fixture::store();
        let phases = RefCell::new(Vec::new());
        let created = create(
            &CreateRequest {
                created_by: CreatedBy::Dialog,
                ..request(&main, &root, "My Feature")
            },
            &store,
            &|p| phases.borrow_mut().push(p),
        )
        .unwrap();
        assert_eq!(created.path, root.join("project/My-Feature"));
        assert_eq!(created.branch, "My-Feature");
        assert_eq!(created.base, "origin/main");
        assert_eq!(created.head, upstream);
        assert!(created.warnings.is_empty(), "{:?}", created.warnings);
        assert_eq!(
            run(
                &created.path,
                &["config", "--get", "branch.My-Feature.base"]
            )
            .trim(),
            "origin/main"
        );
        assert_eq!(
            run(&created.path, &["config", "--get", "push.autoSetupRemote"]).trim(),
            "true"
        );
        assert!(
            super::super::git(&created.path, &["rev-parse", "--abbrev-ref", "@{u}"]).is_err(),
            "--no-track"
        );
        let phases = phases.into_inner();
        assert_eq!(phases.first(), Some(&Phase::Resolving));
        assert_eq!(phases.get(1), Some(&Phase::Fetching));
        assert!(phases.contains(&Phase::CheckingOut { percent: None }));
        assert_eq!(
            &phases[phases.len() - 2..],
            [Phase::CopyingFiles, Phase::Done]
        );
        // Content paths and setup.
        assert_eq!(created.content.shared, ["node_modules"]);
        assert_eq!(created.content.copied, [".env"]);
        let plan = created.setup.clone().unwrap();
        assert_eq!((plan.script.as_str(), plan.approved), ("npm ci", false));
        // Meta.
        let m = meta::load(&lock(&store), &created.path).unwrap().unwrap();
        assert_eq!(m, created.meta);
        assert_eq!(m.start_kind, StartKind::Base);
        assert_eq!(m.created_by, CreatedBy::Dialog);
        assert_eq!(m.display_name, "My Feature");
        assert_eq!(m.repo, main.to_string_lossy());
        assert!(!m.branch_user_chosen && !m.preserve_branch_on_delete);

        // Same name again → suffixed; approved setup recognised; repo default base honoured.
        {
            let s = lock(&store);
            meta::save_repo_prefs(
                &s,
                &main,
                &meta::RepoPrefs {
                    default_base: Some("origin/feature".into()),
                    setup_approved_hash: Some(setup::hash("npm ci")),
                    show_external: false,
                },
            )
            .unwrap();
            meta::save_prefs(
                &s,
                &meta::Prefs {
                    root: Some(root.clone()),
                    branch_prefix: BranchPrefix::Custom("me".into()),
                },
            )
            .unwrap();
        }
        let again = create(
            &CreateRequest::new(&created.path, "My Feature"),
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(again.path, root.join("project/My-Feature-2"));
        assert_eq!(again.branch, "me/My-Feature-2");
        assert_eq!(again.base, "origin/feature");
        assert!(again.setup.unwrap().approved);
    }

    #[test]
    fn worktree_create_fetch_failure_falls_back_with_warning() {
        let (t, main, root, _origin) = fixture::with_origin();
        let gone = t.path().join("gone.git");
        run(
            &main,
            &["remote", "set-url", "origin", &gone.to_string_lossy()],
        );
        let store = fixture::store();
        let created = create(&request(&main, &root, "offline"), &store, &|_| {}).unwrap();
        assert_eq!(created.base, "origin/main");
        assert_eq!(created.warnings.len(), 1, "{:?}", created.warnings);
        assert!(created.warnings[0].contains("Could not refresh origin/main"));
    }

    #[test]
    fn worktree_create_existing_remote_and_commit_starts() {
        let (_t, main, root, _origin) = fixture::with_origin();
        let store = fixture::store();
        let first = run(&main, &["rev-parse", "HEAD"]).trim().to_owned();
        fixture::commit(&main, "two.txt", "2\n", "Two");
        // Existing local branch.
        run(&main, &["branch", "topic"]);
        let c = create(
            &CreateRequest {
                branch: Some("topic".into()),
                existing_branch: true,
                ..request(&main, &root, "")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(c.path, root.join("project/topic"));
        assert_eq!(c.branch, "topic");
        assert_eq!(c.meta.start_kind, StartKind::Branch);
        assert!(c.meta.branch_user_chosen && c.meta.preserve_branch_on_delete);
        assert!(super::super::git(&main, &["config", "--get", "branch.topic.base"]).is_err());
        // Already checked out → refused.
        assert!(create(
            &CreateRequest {
                branch: Some("topic".into()),
                existing_branch: true,
                ..request(&main, &root, "again")
            },
            &store,
            &|_| {},
        )
        .unwrap_err()
        .contains("already has a working copy"));
        // Remote-only branch → tracking branch.
        let r = create(
            &CreateRequest {
                branch: Some("feature".into()),
                existing_branch: true,
                ..request(&main, &root, "Feature work")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(r.path, root.join("project/Feature-work"));
        assert_eq!(r.meta.start_kind, StartKind::Remote);
        assert_eq!(r.base, "origin/feature");
        assert_eq!(
            run(&r.path, &["rev-parse", "--abbrev-ref", "@{u}"]).trim(),
            "origin/feature"
        );
        assert_eq!(
            run(&main, &["config", "--get", "branch.feature.base"]).trim(),
            "origin/feature"
        );
        // Explicit SHA.
        let s = create(
            &CreateRequest {
                base: Some(first[..12].into()),
                ..request(&main, &root, "from sha")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(s.head, first);
        assert_eq!(s.base, first);
        assert_eq!(s.meta.start_kind, StartKind::Commit);
        // Explicit local base.
        let l = create(
            &CreateRequest {
                base: Some("main".into()),
                branch: Some("custom/branch".into()),
                ..request(&main, &root, "local")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(l.branch, "custom/branch");
        assert_eq!(l.head, run(&main, &["rev-parse", "main"]).trim());
        // Missing branch / base.
        assert!(create(
            &CreateRequest {
                branch: Some("nope".into()),
                existing_branch: true,
                ..request(&main, &root, "x")
            },
            &store,
            &|_| {},
        )
        .is_err());
        assert!(create(
            &CreateRequest {
                base: Some("does-not-exist".into()),
                ..request(&main, &root, "x")
            },
            &store,
            &|_| {},
        )
        .is_err());
        assert!(!root.join("project/x").exists());
        assert!(create(&request(&main, &root, "🚀"), &store, &|_| {}).is_err());
        assert!(create(
            &request(Path::new("remote://env/repo"), &root, "x"),
            &store,
            &|_| {}
        )
        .unwrap_err()
        .contains("remote"));
    }

    #[test]
    fn worktree_create_rollback_leaves_nothing_behind() {
        let (t, main, root) = fixture::repo();
        let hooks = t.path().canonicalize().unwrap().join("hooks");
        std::fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        std::fs::write(&hook, "#!/bin/sh\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        run(
            &main,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        run(&main, &["branch", "keep"]);
        let store = fixture::store();
        let err = create(&request(&main, &root, "doomed"), &store, &|_| {});
        assert!(err.is_err());
        let path = root.join("project/doomed");
        assert!(!path.exists(), "directory removed");
        assert!(!has_branch(&main, "doomed"), "new branch deleted");
        assert!(has_branch(&main, "keep"), "other branches untouched");
        assert!(meta::all(&lock(&store)).unwrap().is_empty(), "no meta");
        assert_eq!(super::super::catalog::registered(&main).unwrap().len(), 1);
        // An existing branch is never deleted by rollback.
        let err = create(
            &CreateRequest {
                branch: Some("keep".into()),
                existing_branch: true,
                ..request(&main, &root, "keep")
            },
            &store,
            &|_| {},
        );
        assert!(err.is_err());
        assert!(has_branch(&main, "keep"));
        // The claim was released: the same name works once the hook is gone.
        std::fs::remove_file(&hook).unwrap();
        let ok = create(&request(&main, &root, "doomed"), &store, &|_| {}).unwrap();
        assert_eq!(ok.path, path);
    }
}
