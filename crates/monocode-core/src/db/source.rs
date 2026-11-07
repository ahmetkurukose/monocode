//! Saved data sources (`datasources.json`, no secrets) and their passwords
//! (OS credential store).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Keep the password in the OS credential store.
    pub save_password: bool,
    /// The session refuses writes (`default_transaction_read_only=on`).
    pub read_only: bool,
    pub color: Option<DbColor>,
    pub comment: String,
    pub application_name: String,
    pub statement_timeout_ms: Option<u32>,
    pub search_path: Option<String>,
}

impl Default for DataSource {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            host: "127.0.0.1".into(),
            port: 5432,
            database: String::new(),
            user: String::new(),
            save_password: false,
            read_only: true,
            color: None,
            comment: String::new(),
            application_name: "Monocode".into(),
            statement_timeout_ms: None,
            search_path: None,
        }
    }
}

impl std::fmt::Debug for DataSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataSource")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl DataSource {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            ..Self::default()
        }
    }
}

/// The saved list, one JSON file in `dir`.
pub struct DataSourceStore {
    path: PathBuf,
}

const KEYRING_SERVICE: &str = "com.monocode.db";

impl DataSourceStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("datasources.json"),
        }
    }

    pub fn load(&self) -> Vec<DataSource> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, sources: &[DataSource]) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(sources).map_err(|e| e.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

/// Where secrets live. The OS credential store in the app; a map in tests.
trait Backend {
    fn get(&self, account: &str) -> Option<String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
}

struct Keyring;

impl Backend for Keyring {
    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(KEYRING_SERVICE, account)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(KEYRING_SERVICE, account)
            .map_err(|e| e.to_string())?
            .set_password(value)
            .map_err(|e| e.to_string())
    }
}

/// The account holding every password as one JSON map.
const VAULT_ACCOUNT: &str = "passwords";

/// Data source passwords, kept as one credential store item and read at
/// most once per run. macOS asks for permission once per item it did not
/// create (and again for each rebuilt, differently signed app); one item
/// and one read means one question, not one per data source per connect.
/// Older versions kept an item per data source: those are read when a
/// source is first used, and move into the single item.
struct Vault<B: Backend> {
    backend: B,
    map: Option<std::collections::BTreeMap<String, String>>,
}

impl<B: Backend> Vault<B> {
    fn new(backend: B) -> Self {
        Self { backend, map: None }
    }

    fn map(&mut self) -> &mut std::collections::BTreeMap<String, String> {
        let backend = &self.backend;
        self.map.get_or_insert_with(|| {
            backend
                .get(VAULT_ACCOUNT)
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default()
        })
    }

    fn persist(&mut self) -> Result<(), String> {
        let json = serde_json::to_string(self.map()).map_err(|e| e.to_string())?;
        self.backend.set(VAULT_ACCOUNT, &json)
    }

    fn get(&mut self, id: &str) -> Option<String> {
        if let Some(found) = self.map().get(id) {
            return Some(found.clone());
        }
        // A password saved by an older version, in its own item.
        let legacy = self.backend.get(id)?;
        self.map().insert(id.to_owned(), legacy.clone());
        let _ = self.persist();
        Some(legacy)
    }

    fn save(&mut self, id: &str, password: &str) -> Result<(), String> {
        self.map().insert(id.to_owned(), password.to_owned());
        self.persist()
    }

    fn forget(&mut self, id: &str) {
        if self.map().remove(id).is_some() {
            let _ = self.persist();
        }
    }
}

fn vault() -> &'static std::sync::Mutex<Vault<Keyring>> {
    static VAULT: std::sync::OnceLock<std::sync::Mutex<Vault<Keyring>>> =
        std::sync::OnceLock::new();
    VAULT.get_or_init(|| std::sync::Mutex::new(Vault::new(Keyring)))
}

pub fn saved_password(id: &str) -> Option<String> {
    vault().lock().ok()?.get(id)
}

pub fn save_password(id: &str, password: &str) -> Result<(), String> {
    vault()
        .lock()
        .map_err(|_| "The credential store is busy.".to_owned())?
        .save(id, password)
}

pub fn forget_password(id: &str) {
    if let Ok(mut vault) = vault().lock() {
        vault.forget(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_round_trip_without_a_password_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataSourceStore::new(dir.path());
        assert!(store.load().is_empty());
        let mut a = DataSource::new("socradar@prod");
        a.port = 5400;
        a.color = Some(DbColor::Red);
        store.save(std::slice::from_ref(&a)).unwrap();
        assert_eq!(store.load(), vec![a]);
        let text = std::fs::read_to_string(dir.path().join("datasources.json")).unwrap();
        assert!(!text.to_lowercase().contains("\"password\""));
    }

    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// A credential store in memory that counts how often it is read.
    #[derive(Clone, Default)]
    struct Fake {
        items: Rc<RefCell<HashMap<String, String>>>,
        reads: Rc<RefCell<usize>>,
    }

    impl Backend for Fake {
        fn get(&self, account: &str) -> Option<String> {
            *self.reads.borrow_mut() += 1;
            self.items.borrow().get(account).cloned()
        }
        fn set(&self, account: &str, value: &str) -> Result<(), String> {
            self.items.borrow_mut().insert(account.into(), value.into());
            Ok(())
        }
    }

    #[test]
    fn passwords_share_one_item_and_it_is_read_once() {
        let fake = Fake::default();
        let mut vault = Vault::new(fake.clone());
        vault.save("a", "pw-a").unwrap();
        vault.save("b", "pw-b").unwrap();
        *fake.reads.borrow_mut() = 0;
        // A new run: nothing cached yet.
        let mut vault = Vault::new(fake.clone());
        for _ in 0..5 {
            assert_eq!(vault.get("a").as_deref(), Some("pw-a"));
            assert_eq!(vault.get("b").as_deref(), Some("pw-b"));
        }
        assert_eq!(
            *fake.reads.borrow(),
            1,
            "ten lookups, one read of the store"
        );
        assert_eq!(
            fake.items.borrow().len(),
            1,
            "every password is in the one item"
        );
    }

    #[test]
    fn a_password_forgotten_is_gone_for_good() {
        let fake = Fake::default();
        let mut vault = Vault::new(fake.clone());
        vault.save("a", "pw").unwrap();
        vault.forget("a");
        assert_eq!(Vault::new(fake).get("a"), None);
    }

    #[test]
    fn a_password_from_an_older_version_moves_into_the_single_item() {
        let fake = Fake::default();
        fake.items
            .borrow_mut()
            .insert("old-id".into(), "legacy-pw".into());
        let mut vault = Vault::new(fake.clone());
        assert_eq!(vault.get("old-id").as_deref(), Some("legacy-pw"));
        *fake.reads.borrow_mut() = 0;
        let mut vault = Vault::new(fake.clone());
        assert_eq!(vault.get("old-id").as_deref(), Some("legacy-pw"));
        assert_eq!(
            *fake.reads.borrow(),
            1,
            "next run it comes from the single item"
        );
        assert_eq!(vault.get("never-saved"), None);
    }

    #[test]
    fn new_sources_are_read_only_and_older_files_fill_defaults() {
        assert!(DataSource::new("x").read_only);
        let parsed: Vec<DataSource> =
            serde_json::from_str(r#"[{"id":"1","name":"n","host":"h"}]"#).unwrap();
        assert!(parsed[0].read_only);
        assert_eq!(parsed[0].port, 5432);
    }
}
