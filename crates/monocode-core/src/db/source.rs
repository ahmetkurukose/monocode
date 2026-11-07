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

fn entry(id: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, id).ok()
}

pub fn saved_password(id: &str) -> Option<String> {
    entry(id)?.get_password().ok()
}

pub fn save_password(id: &str, password: &str) -> Result<(), String> {
    entry(id)
        .ok_or("The credential store is unavailable.")?
        .set_password(password)
        .map_err(|e| e.to_string())
}

pub fn forget_password(id: &str) {
    if let Some(entry) = entry(id) {
        let _ = entry.delete_credential();
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

    #[test]
    fn new_sources_are_read_only_and_older_files_fill_defaults() {
        assert!(DataSource::new("x").read_only);
        let parsed: Vec<DataSource> =
            serde_json::from_str(r#"[{"id":"1","name":"n","host":"h"}]"#).unwrap();
        assert!(parsed[0].read_only);
        assert_eq!(parsed[0].port, 5432);
    }
}
