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

/// A session's first-send worktree (composer "New worktree", automations, API
/// `workspaceMode: worktree`). Local: named from the first prompt, branched from
/// `base` or the repository's default base. Remote: the old temporary branch.
pub fn for_session(
    cwd: &Path,
    base_ref: Option<&str>,
    prompt: &str,
    session: i64,
    created_by: CreatedBy,
    store: &Mutex<Store>,
) -> Result<PathBuf, String> {
    if crate::files::remote::is_remote(cwd) {
        return git::create_worktree(cwd, base_ref.unwrap_or("HEAD"), session);
    }
    let request = CreateRequest {
        base: base(base_ref),
        created_by,
        ..CreateRequest::new(cwd, &worktree::naming::from_prompt(prompt))
    };
    worktree::create::create(&request, store, &|_| {}).map(|created| created.path)
}

/// A worktree named by the user (Settings form, switcher, API `worktrees.create`).
/// `name` doubles as the branch when it is a valid branch name, as before.
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
        base: if existing { None } else { base(base_ref) },
        branch,
        existing_branch: existing,
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
        .unwrap();
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
}
