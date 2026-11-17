//! Overlay metadata in the native settings table (spec §1). Every struct is
//! `#[serde(default)]` so older/newer builds read each other's rows.
use super::naming::BranchPrefix;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const META_PREFIX: &str = "worktree:";
pub const REPO_PREFIX: &str = "worktree-repo:";
pub const PREFS_KEY: &str = "worktree-prefs";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartKind {
    Branch,
    Commit,
    Remote,
    /// Also what unknown future kinds read as.
    #[default]
    #[serde(other)]
    Base,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreatedBy {
    Dialog,
    Composer,
    Automation,
    Switcher,
    /// A session started from the phone.
    Mobile,
    /// A session started from the quick composer.
    Quick,
    /// Also what unknown future values read as.
    #[default]
    #[serde(other)]
    Api,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreeMeta {
    pub path: String,
    /// The main checkout's path.
    pub repo: String,
    pub display_name: String,
    pub branch_at_create: String,
    pub base_ref: String,
    pub start_kind: StartKind,
    /// Unix milliseconds.
    pub created_at: u64,
    pub created_by: CreatedBy,
    pub branch_user_chosen: bool,
    pub preserve_branch_on_delete: bool,
    pub pinned: bool,
    pub comment: String,
    pub parent: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoPrefs {
    pub default_base: Option<String>,
    pub setup_approved_hash: Option<String>,
    pub show_external: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// `None` = [`super::layout::default_root`].
    pub root: Option<PathBuf>,
    pub branch_prefix: BranchPrefix,
}
impl Prefs {
    pub fn root_or_default(&self) -> PathBuf {
        self.root
            .clone()
            .filter(|r| r.is_absolute())
            .unwrap_or_else(super::layout::default_root)
    }
}

/// `files::path_key` of the canonical path (the path itself when it no longer exists).
pub fn key(path: &Path) -> String {
    crate::files::path_key(&super::canonical(path).to_string_lossy())
}

fn read<T: serde::de::DeserializeOwned + Default>(store: &Store, key: &str) -> Result<T, String> {
    Ok(store
        .setting(key)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}
fn write<T: Serialize>(store: &Store, key: &str, value: &T) -> Result<(), String> {
    store.save_setting(
        key,
        &serde_json::to_string(value).map_err(|e| e.to_string())?,
    )
}

pub fn load(store: &Store, path: &Path) -> Result<Option<WorktreeMeta>, String> {
    Ok(store
        .setting(&format!("{META_PREFIX}{}", key(path)))?
        .and_then(|v| serde_json::from_str(&v).ok()))
}
pub fn save(store: &Store, meta: &WorktreeMeta) -> Result<(), String> {
    write(
        store,
        &format!("{META_PREFIX}{}", key(Path::new(&meta.path))),
        meta,
    )
}
pub fn delete(store: &Store, path: &Path) -> Result<(), String> {
    store.delete_setting(&format!("{META_PREFIX}{}", key(path)))?;
    // The path may be gone (no canonical form); also drop the literal spelling.
    store.delete_setting(&format!(
        "{META_PREFIX}{}",
        crate::files::path_key(&path.to_string_lossy())
    ))
}
/// Every stored worktree meta, as `(settings key suffix, meta)`.
pub fn all(store: &Store) -> Result<Vec<(String, WorktreeMeta)>, String> {
    Ok(store
        .settings_by_prefix(META_PREFIX)?
        .into_iter()
        .filter_map(|(k, v)| {
            Some((
                k.strip_prefix(META_PREFIX)?.to_owned(),
                serde_json::from_str(&v).ok()?,
            ))
        })
        .collect())
}
pub(crate) fn delete_key(store: &Store, key: &str) -> Result<(), String> {
    store.delete_setting(&format!("{META_PREFIX}{key}"))
}

pub fn repo_prefs(store: &Store, main: &Path) -> Result<RepoPrefs, String> {
    read(store, &format!("{REPO_PREFIX}{}", key(main)))
}
pub fn save_repo_prefs(store: &Store, main: &Path, prefs: &RepoPrefs) -> Result<(), String> {
    write(store, &format!("{REPO_PREFIX}{}", key(main)), prefs)
}
pub fn prefs(store: &Store) -> Result<Prefs, String> {
    read(store, PREFS_KEY)
}
pub fn save_prefs(store: &Store, prefs: &Prefs) -> Result<(), String> {
    write(store, PREFS_KEY, prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_meta_round_trip_and_forward_compat() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wt");
        std::fs::create_dir(&path).unwrap();
        let meta = WorktreeMeta {
            path: super::super::canonical(&path).to_string_lossy().into(),
            repo: "/repo".into(),
            display_name: "wt".into(),
            start_kind: StartKind::Remote,
            created_by: CreatedBy::Composer,
            ..Default::default()
        };
        save(&store, &meta).unwrap();
        // Lookup through a non-canonical spelling still finds it.
        assert_eq!(load(&store, &path).unwrap(), Some(meta.clone()));
        assert_eq!(all(&store).unwrap().len(), 1);
        // Unknown fields/variants are tolerated.
        store
            .save_setting(
                "worktree:/x",
                r#"{"path":"/x","start_kind":"future","created_by":"robot","new_field":1}"#,
            )
            .unwrap();
        let x = load(&store, Path::new("/x")).unwrap().unwrap();
        assert_eq!(
            (x.start_kind, x.created_by),
            (StartKind::Base, CreatedBy::Api)
        );
        assert_eq!(all(&store).unwrap().len(), 2);
        for (by, text) in [
            (CreatedBy::Automation, "\"automation\""),
            (CreatedBy::Mobile, "\"mobile\""),
            (CreatedBy::Quick, "\"quick\""),
        ] {
            assert_eq!(serde_json::to_string(&by).unwrap(), text);
            assert_eq!(serde_json::from_str::<CreatedBy>(text).unwrap(), by);
        }
        // Other prefixes are not listed.
        save_prefs(&store, &Prefs::default()).unwrap();
        save_repo_prefs(&store, Path::new("/repo"), &RepoPrefs::default()).unwrap();
        assert_eq!(all(&store).unwrap().len(), 2);
        delete(&store, &path).unwrap();
        assert_eq!(load(&store, &path).unwrap(), None);
        assert_eq!(all(&store).unwrap().len(), 1);

        let p = Prefs {
            root: Some("/w".into()),
            branch_prefix: BranchPrefix::Custom("me".into()),
        };
        save_prefs(&store, &p).unwrap();
        assert_eq!(prefs(&store).unwrap(), p);
        assert_eq!(
            Prefs::default().root_or_default(),
            super::super::layout::default_root()
        );
        let r = RepoPrefs {
            default_base: Some("origin/dev".into()),
            setup_approved_hash: Some("abc".into()),
            show_external: true,
        };
        save_repo_prefs(&store, Path::new("/repo"), &r).unwrap();
        assert_eq!(repo_prefs(&store, Path::new("/repo")).unwrap(), r);
        store.save_setting("worktree-prefs", "not json").unwrap();
        assert_eq!(prefs(&store).unwrap(), Prefs::default());
    }
}
