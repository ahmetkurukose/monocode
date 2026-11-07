//! Console texts, one `.sql` file each under `consoles/<source-id>/`, so what
//! was typed survives a restart (like IntelliJ's console files).

use std::path::{Path, PathBuf};

pub struct ConsoleStore {
    root: PathBuf,
}

impl ConsoleStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            root: dir.join("consoles"),
        }
    }

    fn folder(&self, source: &str) -> PathBuf {
        // Source ids are uuids, but never let one walk out of the folder.
        let safe: String = source
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join(safe)
    }

    fn file(&self, source: &str, id: u32) -> PathBuf {
        self.folder(source).join(format!("{id}.sql"))
    }

    /// The console numbers saved for `source`, ascending.
    pub fn list(&self, source: &str) -> Vec<u32> {
        let mut ids: Vec<u32> = std::fs::read_dir(self.folder(source))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".sql")?.parse().ok()
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn load(&self, source: &str, id: u32) -> String {
        std::fs::read_to_string(self.file(source, id)).unwrap_or_default()
    }

    pub fn save(&self, source: &str, id: u32, text: &str) -> Result<(), String> {
        let path = self.file(source, id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("sql.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub fn delete(&self, source: &str, id: u32) {
        let _ = std::fs::remove_file(self.file(source, id));
    }

    /// Removes every console of a deleted data source.
    pub fn delete_source(&self, source: &str) {
        let _ = std::fs::remove_dir_all(self.folder(source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consoles_round_trip_list_in_order_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConsoleStore::new(dir.path());
        assert!(store.list("src").is_empty());
        store.save("src", 10, "select 10").unwrap();
        store.save("src", 2, "select 2").unwrap();
        store.save("other", 1, "x").unwrap();
        assert_eq!(store.list("src"), [2, 10]);
        assert_eq!(store.load("src", 10), "select 10");
        store.delete("src", 2);
        assert_eq!(store.list("src"), [10]);
        store.delete_source("src");
        assert!(store.list("src").is_empty());
        assert_eq!(store.list("other"), [1]);
    }

    #[test]
    fn a_hostile_source_id_stays_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConsoleStore::new(dir.path());
        store.save("../../x", 1, "t").unwrap();
        assert!(!dir.path().parent().unwrap().join("x").exists());
        assert_eq!(store.list("../../x"), [1]);
    }
}
