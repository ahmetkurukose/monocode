//! The app's create entry points on top of [`crate::worktree`]: local projects go
//! through `worktree::create`, `remote://` projects keep the old `git` path. Also
//! listing/reconciling without holding the database lock while Git runs.
use crate::{
    git,
    store::Store,
    worktree::{self, catalog, CreateRequest, CreatedBy, Entry},
};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// The empty-or-whitespace base means "use the default base" (spec §4).
fn base(base: Option<&str>) -> Option<String> {
    base.map(str::trim)
        .filter(|b| !b.is_empty())
        .map(str::to_owned)
}

/// The base a caller without a chosen one starts from: the API, automations and the
/// phone keep their old `HEAD`; interactive callers use the repository's default base.
fn base_for(created_by: CreatedBy, base_ref: Option<&str>) -> Option<String> {
    base(base_ref).or_else(|| {
        matches!(
            created_by,
            CreatedBy::Api | CreatedBy::Automation | CreatedBy::Mobile
        )
        .then(|| "HEAD".to_owned())
    })
}

/// A session's new worktree and the non-fatal problems met making it (a failed fetch,
/// a skipped content path, …), for the session to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionWorktree {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// A session's first-send worktree (composer "New worktree", automations, API
/// `workspaceMode: worktree`). Local: named from the first prompt, branched from
/// `base`, else `HEAD` for the API, automations and the phone, else the repository's
/// default base. Remote: the old temporary branch.
pub fn for_session(
    cwd: &Path,
    base_ref: Option<&str>,
    prompt: &str,
    session: i64,
    created_by: CreatedBy,
    store: &Mutex<Store>,
) -> Result<SessionWorktree, String> {
    if crate::files::remote::is_remote(cwd) {
        return git::create_worktree(cwd, base_ref.unwrap_or("HEAD"), session).map(|path| {
            SessionWorktree {
                path,
                warnings: Vec::new(),
            }
        });
    }
    let request = CreateRequest {
        base: base_for(created_by, base_ref),
        created_by,
        ..CreateRequest::new(cwd, &worktree::naming::from_prompt(prompt))
    };
    worktree::create::create(&request, store, &|_| {}).map(|created| SessionWorktree {
        path: created.path,
        warnings: created.warnings,
    })
}

/// A worktree named by the user (Settings form, switcher, API `worktrees.create`).
/// `name` doubles as the branch when it is a valid branch name, as before. For the
/// API (`CreatedBy::Api`) the name must be a valid branch name (the old error), it
/// starts from `HEAD` when no base is given, and it may name a new local branch like
/// a branch that only exists on origin.
pub fn named(
    cwd: &Path,
    name: &str,
    base_ref: Option<&str>,
    existing: bool,
    created_by: CreatedBy,
    store: &Mutex<Store>,
) -> Result<git::Worktree, String> {
    let name = name.trim();
    if crate::files::remote::is_remote(cwd) {
        return git::create_project_worktree(cwd, name, base_ref, existing);
    }
    if created_by == CreatedBy::Api {
        git::valid_branch(cwd, name)?;
    }
    let request = named_request(cwd, name, base_ref, existing, created_by);
    let created = worktree::create::create(&request, store, &|_| {})?;
    Ok(git::Worktree {
        path: created.path.to_string_lossy().into_owned(),
        branch: Some(created.branch),
        head: created.head,
        main: false,
        missing: false,
    })
}

/// The request [`named`] runs, for callers that report progress themselves.
pub fn named_request(
    cwd: &Path,
    name: &str,
    base_ref: Option<&str>,
    existing: bool,
    created_by: CreatedBy,
) -> CreateRequest {
    let name = name.trim();
    let branch = (existing || git::valid_branch(cwd, name).is_ok()).then(|| name.to_owned());
    CreateRequest {
        base: if existing {
            None
        } else {
            base_for(created_by, base_ref)
        },
        branch,
        existing_branch: existing,
        allow_remote_name_clash: created_by == CreatedBy::Api,
        created_by,
        ..CreateRequest::new(cwd, name)
    }
}

/// [`catalog::list_in`]: Git never runs under the database lock.
pub fn list(cwd: &Path, store: &Mutex<Store>) -> Result<Vec<Entry>, String> {
    catalog::list_in(cwd, store)
}

/// [`catalog::reconcile_in`]: drops metas of this repository whose path is gone and no
/// longer registered, without holding the lock while Git runs. Returns the keys.
pub fn reconcile(cwd: &Path, store: &Mutex<Store>) -> Result<Vec<String>, String> {
    catalog::reconcile_in(cwd, store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::meta;

    fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
        store.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&main).unwrap();
        let run = |args: &[&str]| git::git(&main, args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "Fixture"]);
        run(&["config", "user.email", "fixture@example.invalid"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(main.join("README.md"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "Initial"]);
        (temp, main)
    }

    fn store(root: &Path) -> Mutex<Store> {
        let store = Store::open(Path::new(":memory:")).unwrap();
        meta::save_prefs(
            &store,
            &worktree::Prefs {
                root: Some(root.to_path_buf()),
                ..Default::default()
            },
        )
        .unwrap();
        Mutex::new(store)
    }

    #[test]
    fn callers_create_managed_worktrees_and_reconcile_without_the_lock() {
        let (temp, main) = repo();
        let root = temp.path().canonicalize().unwrap().join("workspaces");
        let store = store(&root);
        let path = for_session(
            &main,
            None,
            "Fix the login page redirect loop please now",
            7,
            CreatedBy::Composer,
            &store,
        )
        .unwrap()
        .path;
        assert!(path.starts_with(&root));
        assert!(path.ends_with("project/fix-the-login-page-redirect-loop"));
        let m = meta::load(&lock(&store), &path).unwrap().unwrap();
        assert_eq!(m.created_by, CreatedBy::Composer);
        assert!(!m.branch_user_chosen);

        let tree = named(&main, "feature/x", Some(""), false, CreatedBy::Api, &store).unwrap();
        assert_eq!(tree.branch.as_deref(), Some("feature/x"));
        assert!(Path::new(&tree.path).ends_with("project/feature-x"));
        let m = meta::load(&lock(&store), Path::new(&tree.path))
            .unwrap()
            .unwrap();
        assert!(m.branch_user_chosen);
        assert!(named(&main, "feature/x", None, false, CreatedBy::Api, &store).is_err());

        assert_eq!(list(&main, &store).unwrap().len(), 3);
        std::fs::remove_dir_all(&path).unwrap();
        git::git(&main, &["worktree", "prune"]).unwrap();
        assert_eq!(reconcile(&main, &store).unwrap().len(), 1);
        assert!(meta::load(&lock(&store), &path).unwrap().is_none());
        assert_eq!(reconcile(&main, &store).unwrap().len(), 0);
    }

    fn head(cwd: &Path) -> String {
        git::git(cwd, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn api_worktrees_start_from_head_validate_branches_and_may_shadow_origin_names() {
        let (temp, main) = repo();
        let root = temp.path().canonicalize().unwrap().join("workspaces");
        let store = store(&root);
        // A saved default base elsewhere: the API still starts from HEAD.
        git::git(&main, &["branch", "develop"]).unwrap();
        git::git(&main, &["commit", "-q", "--allow-empty", "-m", "Ahead"]).unwrap();
        meta::save_repo_prefs(
            &lock(&store),
            &main,
            &meta::RepoPrefs {
                default_base: Some("develop".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let tree = named(&main, "api/one", None, false, CreatedBy::Api, &store).unwrap();
        assert_eq!(tree.head, head(&main));
        let m = meta::load(&lock(&store), Path::new(&tree.path))
            .unwrap()
            .unwrap();
        let current = git::git(&main, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        assert_eq!(m.base_ref, current.trim(), "HEAD is recorded as its branch");
        let session = for_session(&main, None, "api session", 3, CreatedBy::Api, &store).unwrap();
        assert_eq!(head(&session.path), head(&main));
        for by in [CreatedBy::Automation, CreatedBy::Mobile] {
            let made = for_session(&main, None, "from elsewhere", 4, by, &store).unwrap();
            assert_eq!(head(&made.path), head(&main), "{by:?} starts from HEAD");
        }
        // HEAD is the caller's own: from a linked worktree, that worktree's commit.
        let linked = Path::new(&tree.path);
        git::git(linked, &["commit", "-q", "--allow-empty", "-m", "Linked"]).unwrap();
        let child = for_session(
            linked,
            None,
            "from linked",
            5,
            CreatedBy::Automation,
            &store,
        )
        .unwrap();
        assert_eq!(head(&child.path), head(linked));
        assert_ne!(head(&child.path), head(&main));
        let m = meta::load(&lock(&store), &child.path).unwrap().unwrap();
        assert_eq!(m.base_ref, "api/one");
        // The dialog's callers keep the repository's default base.
        let dialog = named(&main, "from-dialog", None, false, CreatedBy::Dialog, &store).unwrap();
        assert_ne!(dialog.head, head(&main));

        // An invalid branch fails with the old message instead of being slugged.
        assert_eq!(
            named(&main, "-bad", None, false, CreatedBy::Api, &store).unwrap_err(),
            "Enter a valid branch name"
        );
        assert!(named(&main, "bad..branch", None, false, CreatedBy::Api, &store).is_err());
        assert!(named(&main, "bad..branch", None, false, CreatedBy::Dialog, &store).is_ok());

        // A branch that only exists as origin/<b> becomes a new, untracked local branch.
        let sha = head(&main);
        git::git(&main, &["update-ref", "refs/remotes/origin/shared", &sha]).unwrap();
        let tree = named(&main, "shared", None, false, CreatedBy::Api, &store).unwrap();
        assert_eq!(tree.branch.as_deref(), Some("shared"));
        assert!(git::git(&main, &["config", "--get", "branch.shared.remote"]).is_err());
        git::git(&main, &["update-ref", "refs/remotes/origin/other", &sha]).unwrap();
        assert!(named(&main, "other", None, false, CreatedBy::Dialog, &store).is_err());
    }

    #[test]
    fn session_worktrees_return_their_creation_warnings() {
        let (temp, main) = repo();
        let root = temp.path().canonicalize().unwrap().join("workspaces");
        let store = store(&root);
        let missing = temp.path().join("no-such-remote");
        git::git(
            &main,
            &["remote", "add", "origin", &missing.to_string_lossy()],
        )
        .unwrap();
        let sha = head(&main);
        git::git(&main, &["update-ref", "refs/remotes/origin/main", &sha]).unwrap();
        let made = for_session(&main, None, "warn me", 9, CreatedBy::Composer, &store).unwrap();
        assert!(made.path.is_dir());
        assert!(
            made.warnings
                .iter()
                .any(|w| w.starts_with("Started from the local copy of origin/main; fetch failed")),
            "{:?}",
            made.warnings
        );
    }
}
