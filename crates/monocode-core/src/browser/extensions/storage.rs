//! `chrome.storage` for one extension in one profile: `local` and `sync`
//! in a SQLite file (one row per key, so a write touches only its keys),
//! `session` in memory, `managed` empty. Semantics after Chrome's
//! `ValueStore`: `get` takes nothing, a key, keys or defaults; a write
//! reports only values that changed; Chrome's quotas, error texts
//! included (`sync` is local only, as in Chrome signed out).

use std::{collections::BTreeMap, path::Path};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};

pub const LOCAL_QUOTA: u64 = 10 * 1024 * 1024;
pub const SESSION_QUOTA: u64 = 10 * 1024 * 1024;
pub const SYNC_QUOTA: u64 = 102_400;
pub const SYNC_QUOTA_PER_ITEM: u64 = 8_192;
pub const SYNC_MAX_ITEMS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Local,
    Sync,
    Session,
    Managed,
}

impl Area {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "local" => Area::Local,
            "sync" => Area::Sync,
            "session" => Area::Session,
            "managed" => Area::Managed,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Area::Local => "local",
            Area::Sync => "sync",
            Area::Session => "session",
            Area::Managed => "managed",
        }
    }
}

/// `{key: {oldValue?, newValue?}}`, as `storage.onChanged` gives it.
pub type Changes = Map<String, Value>;

pub struct Storage {
    db: Connection,
    session: BTreeMap<String, Value>,
    /// `unlimitedStorage`: `local` has no quota.
    unlimited: bool,
}

/// What an item weighs against a quota: its key and its JSON, as Chrome
/// counts.
fn weight(key: &str, value: &Value) -> u64 {
    (key.len() + value.to_string().len()) as u64
}

impl Storage {
    pub fn open(path: &Path, unlimited: bool) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        Self::with(db, unlimited)
    }

    pub fn in_memory(unlimited: bool) -> Self {
        Self::with(
            Connection::open_in_memory().expect("in-memory SQLite"),
            unlimited,
        )
        .expect("in-memory schema")
    }

    fn with(db: Connection, unlimited: bool) -> Result<Self, String> {
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS items (
               area TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
               PRIMARY KEY (area, key));",
        )
        .map_err(|e| e.to_string())?;
        Ok(Storage {
            db,
            session: BTreeMap::new(),
            unlimited,
        })
    }

    fn all(&self, area: Area) -> BTreeMap<String, Value> {
        match area {
            Area::Session => self.session.clone(),
            Area::Managed => BTreeMap::new(),
            _ => {
                let mut out = BTreeMap::new();
                let Ok(mut q) = self
                    .db
                    .prepare("SELECT key, value FROM items WHERE area = ?1")
                else {
                    return out;
                };
                let Ok(rows) = q.query_map(params![area.as_str()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                }) else {
                    return out;
                };
                for (k, v) in rows.flatten() {
                    out.insert(k, serde_json::from_str(&v).unwrap_or(Value::Null));
                }
                out
            }
        }
    }

    fn one(&self, area: Area, key: &str) -> Option<Value> {
        match area {
            Area::Session => self.session.get(key).cloned(),
            Area::Managed => None,
            _ => {
                let text: Option<String> = self
                    .db
                    .query_row(
                        "SELECT value FROM items WHERE area = ?1 AND key = ?2",
                        params![area.as_str(), key],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten();
                text.and_then(|t| serde_json::from_str(&t).ok())
            }
        }
    }

    /// `get(keys)`: `null` for everything, a key, keys, or `{key: default}`.
    pub fn get(&self, area: Area, keys: &Value) -> Result<Value, String> {
        let mut out = Map::new();
        match keys {
            Value::Null => {
                for (k, v) in self.all(area) {
                    out.insert(k, v);
                }
            }
            Value::String(k) => {
                if let Some(v) = self.one(area, k) {
                    out.insert(k.clone(), v);
                }
            }
            Value::Array(ks) => {
                for k in ks.iter().filter_map(Value::as_str) {
                    if let Some(v) = self.one(area, k) {
                        out.insert(k.to_owned(), v);
                    }
                }
            }
            Value::Object(defaults) => {
                for (k, d) in defaults {
                    out.insert(k.clone(), self.one(area, k).unwrap_or_else(|| d.clone()));
                }
            }
            _ => return Err("Invalid keys".into()),
        }
        Ok(Value::Object(out))
    }

    /// What `keys` weigh (all of the area for `null`).
    pub fn bytes_in_use(&self, area: Area, keys: &Value) -> u64 {
        let all = self.all(area);
        let wanted: Option<Vec<&str>> = match keys {
            Value::Null => None,
            Value::String(k) => Some(vec![k.as_str()]),
            Value::Array(ks) => Some(ks.iter().filter_map(Value::as_str).collect()),
            _ => Some(Vec::new()),
        };
        all.iter()
            .filter(|(k, _)| wanted.as_ref().is_none_or(|w| w.contains(&k.as_str())))
            .map(|(k, v)| weight(k, v))
            .sum()
    }

    fn check_quota(&self, area: Area, items: &Map<String, Value>) -> Result<(), String> {
        let mut all = self.all(area);
        for (k, v) in items {
            if area == Area::Sync && weight(k, v) > SYNC_QUOTA_PER_ITEM {
                return Err("QUOTA_BYTES_PER_ITEM quota exceeded".into());
            }
            all.insert(k.clone(), v.clone());
        }
        let total: u64 = all.iter().map(|(k, v)| weight(k, v)).sum();
        let quota = match area {
            Area::Local if self.unlimited => u64::MAX,
            Area::Local => LOCAL_QUOTA,
            Area::Session => SESSION_QUOTA,
            Area::Sync => SYNC_QUOTA,
            Area::Managed => 0,
        };
        if area == Area::Sync && all.len() > SYNC_MAX_ITEMS {
            return Err("MAX_ITEMS quota exceeded".into());
        }
        if total > quota {
            return Err("QUOTA_BYTES quota exceeded".into());
        }
        Ok(())
    }

    fn write(&mut self, area: Area, key: &str, value: Option<&Value>) -> Result<(), String> {
        if area == Area::Session {
            match value {
                Some(v) => self.session.insert(key.to_owned(), v.clone()),
                None => self.session.remove(key),
            };
            return Ok(());
        }
        let result = match value {
            Some(v) => self.db.execute(
                "INSERT INTO items (area, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (area, key) DO UPDATE SET value = excluded.value",
                params![area.as_str(), key, v.to_string()],
            ),
            None => self.db.execute(
                "DELETE FROM items WHERE area = ?1 AND key = ?2",
                params![area.as_str(), key],
            ),
        };
        result.map(|_| ()).map_err(|e| e.to_string())
    }

    fn read_only(area: Area) -> Result<(), String> {
        if area == Area::Managed {
            return Err("This is a read-only store.".into());
        }
        Ok(())
    }

    /// `set(items)`: the changes (unchanged values left out).
    pub fn set(&mut self, area: Area, items: &Value) -> Result<Changes, String> {
        Self::read_only(area)?;
        let Value::Object(items) = items else {
            return Err("Invalid items".into());
        };
        self.check_quota(area, items)?;
        let mut changes = Changes::new();
        let tx_ok = self.db.execute_batch("BEGIN").is_ok();
        for (k, v) in items {
            let old = self.one(area, k);
            if old.as_ref() == Some(v) {
                continue;
            }
            if let Err(e) = self.write(area, k, Some(v)) {
                if tx_ok {
                    let _ = self.db.execute_batch("ROLLBACK");
                }
                return Err(e);
            }
            let mut change = json!({ "newValue": v });
            if let Some(old) = old {
                change["oldValue"] = old;
            }
            changes.insert(k.clone(), change);
        }
        if tx_ok {
            self.db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        }
        Ok(changes)
    }

    pub fn remove(&mut self, area: Area, keys: &Value) -> Result<Changes, String> {
        Self::read_only(area)?;
        let keys: Vec<String> = match keys {
            Value::String(k) => vec![k.clone()],
            Value::Array(ks) => ks
                .iter()
                .filter_map(|k| k.as_str().map(str::to_owned))
                .collect(),
            _ => return Err("Invalid keys".into()),
        };
        let mut changes = Changes::new();
        for k in keys {
            if let Some(old) = self.one(area, &k) {
                self.write(area, &k, None)?;
                changes.insert(k, json!({ "oldValue": old }));
            }
        }
        Ok(changes)
    }

    pub fn clear(&mut self, area: Area) -> Result<Changes, String> {
        Self::read_only(area)?;
        let mut changes = Changes::new();
        for (k, old) in self.all(area) {
            changes.insert(k, json!({ "oldValue": old }));
        }
        match area {
            Area::Session => self.session.clear(),
            _ => {
                self.db
                    .execute("DELETE FROM items WHERE area = ?1", params![area.as_str()])
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_takes_every_shape_of_keys() {
        let mut s = Storage::in_memory(false);
        s.set(Area::Local, &json!({"a": 1, "b": {"x": [1, 2]}, "c": null}))
            .unwrap();
        assert_eq!(
            s.get(Area::Local, &Value::Null).unwrap(),
            json!({"a": 1, "b": {"x": [1, 2]}, "c": null})
        );
        assert_eq!(s.get(Area::Local, &json!("a")).unwrap(), json!({"a": 1}));
        assert_eq!(
            s.get(Area::Local, &json!(["a", "zz"])).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            s.get(Area::Local, &json!({"a": 0, "d": "dflt"})).unwrap(),
            json!({"a": 1, "d": "dflt"})
        );
        assert_eq!(s.get(Area::Local, &json!("missing")).unwrap(), json!({}));
        assert!(s.get(Area::Local, &json!(5)).is_err());
    }

    #[test]
    fn writes_report_only_real_changes() {
        let mut s = Storage::in_memory(false);
        let first = s.set(Area::Local, &json!({"a": 1})).unwrap();
        assert_eq!(Value::Object(first), json!({"a": {"newValue": 1}}));
        assert!(s.set(Area::Local, &json!({"a": 1})).unwrap().is_empty());
        let second = s.set(Area::Local, &json!({"a": 2})).unwrap();
        assert_eq!(
            Value::Object(second),
            json!({"a": {"oldValue": 1, "newValue": 2}})
        );
        let removed = s.remove(Area::Local, &json!(["a", "nope"])).unwrap();
        assert_eq!(Value::Object(removed), json!({"a": {"oldValue": 2}}));
        s.set(Area::Local, &json!({"x": 1, "y": 2})).unwrap();
        assert_eq!(s.clear(Area::Local).unwrap().len(), 2);
        assert_eq!(s.get(Area::Local, &Value::Null).unwrap(), json!({}));
    }

    #[test]
    fn areas_are_apart_and_session_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x/storage.sqlite");
        {
            let mut s = Storage::open(&path, false).unwrap();
            s.set(Area::Local, &json!({"k": "local"})).unwrap();
            s.set(Area::Sync, &json!({"k": "sync"})).unwrap();
            s.set(Area::Session, &json!({"k": "session"})).unwrap();
            assert_eq!(
                s.get(Area::Session, &json!("k")).unwrap(),
                json!({"k": "session"})
            );
        }
        let s = Storage::open(&path, false).unwrap();
        assert_eq!(
            s.get(Area::Local, &json!("k")).unwrap(),
            json!({"k": "local"})
        );
        assert_eq!(
            s.get(Area::Sync, &json!("k")).unwrap(),
            json!({"k": "sync"})
        );
        assert_eq!(s.get(Area::Session, &json!("k")).unwrap(), json!({}));
        assert_eq!(s.get(Area::Managed, &Value::Null).unwrap(), json!({}));
    }

    #[test]
    fn quotas_and_bytes_in_use() {
        let mut s = Storage::in_memory(false);
        s.set(Area::Sync, &json!({"ab": "xyz"})).unwrap();
        // "ab" + "\"xyz\"".
        assert_eq!(s.bytes_in_use(Area::Sync, &Value::Null), 7);
        assert_eq!(s.bytes_in_use(Area::Sync, &json!(["nope"])), 0);
        let big = "x".repeat(9000);
        assert_eq!(
            s.set(Area::Sync, &json!({"big": big})),
            Err("QUOTA_BYTES_PER_ITEM quota exceeded".into())
        );
        let many: Map<String, Value> = (0..600).map(|i| (format!("k{i}"), json!(i))).collect();
        assert_eq!(
            s.set(Area::Sync, &Value::Object(many)),
            Err("MAX_ITEMS quota exceeded".into())
        );
        let huge = "y".repeat(11 * 1024 * 1024);
        assert_eq!(
            s.set(Area::Local, &json!({"h": huge})),
            Err("QUOTA_BYTES quota exceeded".into())
        );
        let mut unlimited = Storage::in_memory(true);
        assert!(unlimited.set(Area::Local, &json!({"h": huge})).is_ok());
        assert!(unlimited.set(Area::Managed, &json!({"a": 1})).is_err());
    }
}
