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
    /// A new explicit `branch` may share its name with `origin/<branch>`; it is created
    /// locally without tracking it (the external API's old behaviour). The dialog keeps
    /// refusing such names.
    pub allow_remote_name_clash: bool,
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
            allow_remote_name_clash: false,
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

/// What existed just before `worktree add`, so a rollback undoes only this call's work.
struct Undo {
    path: PathBuf,
    /// Something was already at `path` (never ours to delete).
    path_existed: bool,
    /// Git already had a worktree registered at `path` (never ours to remove).
    registered: bool,
    /// A branch `worktree add -b` creates and the commit it creates it at; `None` when
    /// the branch already existed (it is the user's).
    branch: Option<(String, String)>,
}

/// `Some(registered)`, or `None` when Git could not tell.
fn is_registered(main: &Path, path: &Path) -> Option<bool> {
    let key = meta::key(path);
    super::catalog::registered(main)
        .ok()
        .map(|trees| trees.iter().any(|t| meta::key(Path::new(&t.path)) == key))
}

/// Undo `worktree add` and what followed. Removes only a registration this call's add
/// created, never prunes other worktrees, and deletes a new branch only while it still
/// points at the commit it was created at (`update-ref -d <ref> <old>`).
fn rollback(main: &Path, undo: &Undo, store: &Mutex<Store>) {
    let path = undo.path.as_path();
    let spelled = path.to_string_lossy();
    let ours = !undo.path_existed && !undo.registered;
    if !undo.registered && is_registered(main, path) == Some(true) {
        let remove = ["worktree", "remove", "--force", "--force", "--", &spelled];
        let _ = git(main, &remove);
        if is_registered(main, path) == Some(true) {
            // `remove` refuses a half-made checkout; once the folder is gone it only
            // drops this registration.
            if ours && path.symlink_metadata().is_ok() {
                let _ = std::fs::remove_dir_all(path);
            }
            let _ = git(main, &remove);
        }
    }
    if ours && path.symlink_metadata().is_ok() {
        let _ = std::fs::remove_dir_all(path);
    }
    if let Some((branch, sha)) = &undo.branch {
        let _ = git(
            main,
            &["update-ref", "-d", &format!("refs/heads/{branch}"), sha],
        );
    }
    if ours {
        let s = lock(store);
        let _ = meta::delete(&s, path);
    }
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
                        "Started from the local copy of origin/{branch}; fetch failed ({e})."
                    ));
                }
            }
            (Mode::Remote, format!("origin/{branch}"), StartKind::Remote)
        } else {
            return Err(format!("There is no branch named {branch}"));
        }
    } else {
        // Resolved in the caller's checkout: refs are shared, but `HEAD` is the caller's
        // own (an automation run from a linked worktree starts from that worktree).
        let resolved = base::resolve(&req.cwd, req.base.as_deref(), repo.default_base.as_deref())?;
        warnings.extend(resolved.warnings.iter().cloned());
        if let (Some((remote, branch)), true) = (&resolved.remote, req.fetch) {
            progress(Phase::Fetching);
            if let Err(e) = base::fetch(&main, remote, branch, req.fetch_timeout) {
                warnings.push(format!(
                    "Started from the local copy of {}; fetch failed ({e}).",
                    resolved.reference
                ));
            }
        }
        let sha = base::commit_of(&req.cwd, &resolved)?;
        // Record what `HEAD` meant at creation: its branch, else (detached) the commit.
        let head_branch = (resolved.qualified == "HEAD")
            .then(|| git(&req.cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok())
            .flatten()
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.is_empty());
        let commit = resolved.commit || (resolved.qualified == "HEAD" && head_branch.is_none());
        let kind = if commit {
            StartKind::Commit
        } else {
            StartKind::Base
        };
        let reference = if commit {
            sha.clone()
        } else {
            head_branch.unwrap_or_else(|| resolved.reference.clone())
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
        allow_remote_name_clash: req.allow_remote_name_clash,
    })?;
    let branch = picked.branch.clone();
    let path = picked.path.clone();
    std::fs::create_dir_all(&repo_dir)
        .map_err(|e| format!("Could not create {}: {e}", repo_dir.display()))?;

    // 4. worktree add.
    let spelled = path.to_string_lossy().into_owned();
    // Fully qualified, so a tag or local branch named `origin/<b>` cannot win.
    let remote_ref = format!("refs/remotes/origin/{branch}");
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
    let start = match &mode {
        Mode::New { sha } => Some(sha.clone()),
        Mode::Existing => None,
        Mode::Remote => Some(
            git(
                &main,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{remote_ref}^{{commit}}"),
                ],
            )?
            .trim()
            .to_owned(),
        ),
    };
    let undo = Undo {
        path_existed: path.symlink_metadata().is_ok(),
        registered: is_registered(&main, &path).unwrap_or(true),
        branch: start
            .filter(|_| !naming::local_branch_exists(&main, &branch))
            .map(|sha| (branch.clone(), sha)),
        path: path.clone(),
    };
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
        rollback(&main, &undo, store);
        return Err(e);
    }
    let path = super::canonical(&path);
    let fail = |e: String| {
        rollback(&main, &undo, store);
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
    // Bind first: `fail` locks the store again, so the guard must be gone by then.
    let saved = {
        let s = lock(store);
        meta::save(&s, &record)
    };
    saved.map_err(fail)?;

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
        assert!(created.warnings[0]
            .contains("Started from the local copy of origin/main; fetch failed"));
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

    /// Point `main` at a hooks dir with a `post-checkout` hook running `body`.
    fn hook(t: &tempfile::TempDir, main: &Path, body: &str) -> PathBuf {
        let hooks = t.path().canonicalize().unwrap().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        run(
            main,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        hook
    }

    fn registered_paths(main: &Path) -> Vec<PathBuf> {
        super::super::catalog::registered(main)
            .unwrap()
            .into_iter()
            .map(|t| PathBuf::from(t.path))
            .collect()
    }

    #[test]
    fn worktree_create_meta_save_failure_rolls_back_without_deadlock() {
        let (t, main, root) = fixture::repo();
        let db = t.path().join("store.sqlite");
        let store = Store::open(&db).unwrap();
        // A real SQLite failure for worktree metas only (prefs still read and write).
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_meta BEFORE INSERT ON native_settings
                 WHEN NEW.key LIKE 'worktree:%' BEGIN SELECT RAISE(ABORT, 'meta refused'); END;",
            )
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let req = request(&main, &root, "stuck");
        std::thread::spawn(move || {
            let store = Mutex::new(store);
            let result = create(&req, &store, &|_| {});
            let _ = tx.send(result.map(|c| c.path));
        });
        let result = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("create deadlocked rolling back a failed meta save");
        assert!(result.unwrap_err().contains("meta refused"));
        assert!(!root.join("project/stuck").exists());
        assert!(!has_branch(&main, "stuck"));
        assert_eq!(registered_paths(&main).len(), 1);
    }

    #[test]
    fn worktree_create_skips_a_missing_registered_path_and_rollback_spares_it() {
        let (t, main, root) = fixture::repo();
        let store = fixture::store();
        let first = create(&request(&main, &root, "same"), &store, &|_| {}).unwrap();
        std::fs::remove_dir_all(&first.path).unwrap(); // deleted by hand, not pruned
        let second = create(
            &CreateRequest {
                branch: Some("other".into()),
                ..request(&main, &root, "same")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert_eq!(second.path, root.join("project/same-2"));
        assert!(registered_paths(&main).contains(&first.path));
        // A failing creation must not prune the missing worktree or drop its meta.
        hook(&t, &main, "exit 3");
        assert!(create(&request(&main, &root, "doomed"), &store, &|_| {}).is_err());
        assert!(!root.join("project/doomed").exists());
        assert!(!has_branch(&main, "doomed"));
        let paths = registered_paths(&main);
        assert!(paths.contains(&first.path), "{paths:?}");
        assert!(paths.contains(&second.path), "{paths:?}");
        assert_eq!(paths.len(), 3);
        let s = lock(&store);
        assert!(meta::load(&s, &first.path).unwrap().is_some());
        assert!(meta::load(&s, &second.path).unwrap().is_some());
    }

    #[test]
    fn worktree_create_rollback_keeps_a_branch_that_moved() {
        let (t, main, root) = fixture::repo();
        let store = fixture::store();
        // The hook commits on the new branch, then fails the checkout.
        hook(
            &t,
            &main,
            "git commit -q --allow-empty --no-verify -m 'work in hook' && exit 3",
        );
        assert!(create(&request(&main, &root, "moved"), &store, &|_| {}).is_err());
        assert!(!root.join("project/moved").exists());
        assert!(has_branch(&main, "moved"), "moved branch kept");
        assert_eq!(
            run(&main, &["log", "-1", "--format=%s", "moved"]).trim(),
            "work in hook"
        );
        assert_eq!(registered_paths(&main).len(), 1);
    }

    #[test]
    fn worktree_rollback_leaves_pre_existing_things_alone() {
        let (t, main, _root) = fixture::repo();
        let store = fixture::store();
        let path = t.path().canonicalize().unwrap().join("theirs");
        run(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "theirs",
                "--",
                &path.to_string_lossy(),
            ],
        );
        let record = WorktreeMeta {
            path: path.to_string_lossy().into(),
            ..Default::default()
        };
        meta::save(&lock(&store), &record).unwrap();
        let sha = run(&main, &["rev-parse", "HEAD"]).trim().to_owned();
        run(&main, &["branch", "cas"]);
        fixture::commit(&main, "x.txt", "x\n", "Move main");
        run(&main, &["branch", "-f", "cas", "HEAD"]);
        rollback(
            &main,
            &Undo {
                path: path.clone(),
                path_existed: true,
                registered: true,
                branch: Some(("cas".into(), sha.clone())),
            },
            &store,
        );
        assert!(path.join("README.md").exists());
        assert!(registered_paths(&main).contains(&path));
        assert!(meta::load(&lock(&store), &path).unwrap().is_some());
        assert!(
            has_branch(&main, "cas"),
            "moved since: compare-and-swap refuses"
        );
        let now = run(&main, &["rev-parse", "cas"]).trim().to_owned();
        rollback(
            &main,
            &Undo {
                path: path.clone(),
                path_existed: true,
                registered: true,
                branch: Some(("cas".into(), now)),
            },
            &store,
        );
        assert!(!has_branch(&main, "cas"));
        assert!(has_branch(&main, "theirs"));
    }

    #[test]
    fn worktree_create_remote_branch_ignores_same_named_tags() {
        let (t, main, root, origin) = fixture::with_origin();
        let seed = t.path().canonicalize().unwrap().join("seed");
        let old = run(&seed, &["rev-parse", "HEAD"]).trim().to_owned();
        run(&seed, &["tag", "feature", &old]);
        run(&seed, &["checkout", "-q", "feature"]);
        fixture::commit(&seed, "f.txt", "f\n", "Feature");
        let tip = run(&seed, &["rev-parse", "HEAD"]).trim().to_owned();
        run(
            &seed,
            &[
                "push",
                "-q",
                &origin.to_string_lossy(),
                "refs/heads/feature:refs/heads/feature",
                "refs/tags/feature:refs/tags/feature",
            ],
        );
        // A local tag that `origin/feature` would resolve to first.
        run(&main, &["tag", "origin/feature", &old]);
        let store = fixture::store();
        let r = create(
            &CreateRequest {
                branch: Some("feature".into()),
                existing_branch: true,
                ..request(&main, &root, "")
            },
            &store,
            &|_| {},
        )
        .unwrap();
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.head, tip);
        assert_eq!(r.base, "origin/feature");
        assert_eq!(
            run(&r.path, &["config", "--get", "branch.feature.remote"]).trim(),
            "origin"
        );
        assert_eq!(
            run(&r.path, &["config", "--get", "branch.feature.merge"]).trim(),
            "refs/heads/feature"
        );
    }
}
