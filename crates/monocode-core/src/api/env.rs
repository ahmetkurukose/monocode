//! A Postman environment (`*.postman_environment.json`): a named list of
//! variables. Edited in place like a collection, so the export metadata
//! Postman wrote is kept.

use super::model::KeyValue;
use super::{pretty_tabs, ApiError};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
    pub enabled: bool,
    /// Postman's `secret` type: shown masked and kept out of exports.
    pub secret: bool,
}

#[derive(Debug, Clone)]
pub struct Environment {
    doc: Value,
    path: Option<PathBuf>,
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Environment {
    pub fn new(name: &str) -> Self {
        Self {
            doc: json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "name": name,
                "values": [],
                "_postman_variable_scope": "environment",
            }),
            path: None,
        }
    }

    pub fn parse(source: &str) -> Result<Self, ApiError> {
        let doc: Value = serde_json::from_str(source)
            .map_err(|e| ApiError::new(format!("This is not valid JSON: {e}")))?;
        if !doc.is_object() || !doc.get("values").is_some_and(Value::is_array) {
            return Err(ApiError::new(
                "This is not a Postman environment: it has no \"values\" list.",
            ));
        }
        Ok(Self { doc, path: None })
    }

    pub fn load(path: &Path) -> Result<Self, ApiError> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| ApiError::new(format!("Could not read {}: {e}", path.display())))?;
        let mut env = Self::parse(&source)?;
        env.path = Some(path.to_owned());
        Ok(env)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    pub fn name(&self) -> String {
        self.doc.get("name").map(text).unwrap_or_default()
    }

    pub fn set_name(&mut self, name: &str) {
        if let Some(map) = self.doc.as_object_mut() {
            map.insert("name".into(), name.into());
        }
    }

    pub fn is_globals(&self) -> bool {
        self.doc
            .get("_postman_variable_scope")
            .and_then(Value::as_str)
            == Some("globals")
    }

    pub fn values(&self) -> Vec<EnvVar> {
        self.doc
            .get("values")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| EnvVar {
                        key: row.get("key").map(text).unwrap_or_default(),
                        value: row.get("value").map(text).unwrap_or_default(),
                        enabled: row.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        secret: row.get("type").and_then(Value::as_str) == Some("secret"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The rows a [`super::Scope`] layer is made of: enabled ones only.
    pub fn variables(&self) -> Vec<KeyValue> {
        self.values()
            .into_iter()
            .filter(|row| row.enabled && !row.key.is_empty())
            .map(|row| KeyValue::new(row.key, row.value))
            .collect()
    }

    pub fn set_values(&mut self, rows: &[EnvVar]) {
        let old: Vec<Value> = self
            .doc
            .get("values")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut used = vec![false; old.len()];
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let found = old.iter().enumerate().find(|(index, value)| {
                !used[*index] && value.get("key").map(text).as_deref() == Some(row.key.as_str())
            });
            let mut object = match found {
                Some((index, value)) => {
                    used[index] = true;
                    value.clone()
                }
                None => Value::Object(Map::new()),
            };
            if let Some(map) = object.as_object_mut() {
                map.insert("key".into(), row.key.clone().into());
                map.insert("value".into(), row.value.clone().into());
                map.insert(
                    "type".into(),
                    if row.secret { "secret" } else { "default" }.into(),
                );
                map.insert("enabled".into(), row.enabled.into());
            }
            out.push(object);
        }
        if let Some(map) = self.doc.as_object_mut() {
            map.insert("values".into(), Value::Array(out));
        }
    }

    /// Applies what a script set or removed. Existing rows keep their type
    /// and flags; new ones are plain and enabled.
    pub fn apply(&mut self, changes: &[super::exchange::VarChange]) {
        let mut values = self.values();
        for change in changes {
            match &change.value {
                Some(value) => match values.iter_mut().find(|v| v.key == change.key) {
                    Some(row) => {
                        row.value = value.clone();
                        row.enabled = true;
                    }
                    None => values.push(EnvVar {
                        key: change.key.clone(),
                        value: value.clone(),
                        enabled: true,
                        secret: false,
                    }),
                },
                None => values.retain(|v| v.key != change.key),
            }
        }
        self.set_values(&values);
    }

    pub fn to_json(&self) -> String {
        pretty_tabs(&self.doc)
    }

    pub fn save(&self) -> Result<(), ApiError> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| ApiError::new("This environment has no file yet."))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ApiError::new(format!("Could not create {}: {e}", dir.display())))?;
        }
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        let temp = PathBuf::from(temp);
        let mut body = self.to_json();
        body.push('\n');
        std::fs::write(&temp, body)
            .and_then(|_| std::fs::rename(&temp, path))
            .map_err(|e| ApiError::new(format!("Could not save {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
	"id": "e1",
	"name": "staging",
	"values": [
		{ "key": "baseUrl", "value": "https://staging.test", "type": "default", "enabled": true },
		{ "key": "token", "value": "", "type": "secret", "enabled": true },
		{ "key": "old", "value": "x", "type": "default", "enabled": false }
	],
	"_postman_variable_scope": "environment",
	"_postman_exported_at": "2026-01-01T00:00:00.000Z"
}"#;

    #[test]
    fn values_read_with_their_flags_and_only_enabled_ones_become_variables() {
        let env = Environment::parse(SAMPLE).unwrap();
        assert_eq!(env.name(), "staging");
        let values = env.values();
        assert!(values[1].secret);
        assert!(!values[2].enabled);
        assert_eq!(
            env.variables(),
            vec![
                KeyValue::new("baseUrl", "https://staging.test"),
                KeyValue::new("token", ""),
            ]
        );
    }

    #[test]
    fn editing_values_keeps_the_export_metadata() {
        let mut env = Environment::parse(SAMPLE).unwrap();
        let once = env.to_json();
        assert_eq!(Environment::parse(&once).unwrap().to_json(), once);
        assert!(
            once.find("\"values\"").unwrap() < once.find("\"_postman_variable_scope\"").unwrap()
        );
        let mut values = env.values();
        values[0].value = "https://other.test".into();
        values.push(EnvVar {
            key: "new".into(),
            value: "1".into(),
            enabled: true,
            secret: false,
        });
        env.set_values(&values);
        let json: Value = serde_json::from_str(&env.to_json()).unwrap();
        assert_eq!(json["_postman_exported_at"], "2026-01-01T00:00:00.000Z");
        assert_eq!(json["values"][0]["value"], "https://other.test");
        assert_eq!(json["values"][3]["type"], "default");
    }

    #[test]
    fn script_changes_update_set_and_remove_rows() {
        use crate::api::exchange::VarChange;
        let mut env = Environment::parse(SAMPLE).unwrap();
        env.apply(&[
            VarChange {
                key: "token".into(),
                value: Some("fresh".into()),
            },
            VarChange {
                key: "baseUrl".into(),
                value: None,
            },
            VarChange {
                key: "added".into(),
                value: Some("1".into()),
            },
        ]);
        let values = env.values();
        let keys: Vec<&str> = values.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, ["token", "old", "added"]);
        assert_eq!(values[0].value, "fresh");
        assert!(values[0].secret, "the row keeps its type");
        assert!(!values[1].enabled, "untouched rows stay as they were");
    }

    #[test]
    fn a_collection_is_not_an_environment() {
        assert!(Environment::parse(r#"{"info":{"name":"x"},"item":[]}"#).is_err());
    }
}
