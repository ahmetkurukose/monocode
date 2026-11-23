//! A project's tracker settings, kept in the repository at
//! `.monocode/integrations.json` so a team shares one board and one status
//! mapping. Tokens never go here: each person signs in with their own.
use super::StatusKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIntegrations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monday: Option<MondayBinding>,
}

/// The monday.com board a project works from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MondayBinding {
    pub board: String,
    #[serde(default)]
    pub board_name: String,
    /// The status column Monocode reads and writes; the board's first when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_column: Option<String>,
    /// The people column "assigned to me" looks in; any when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people_column: Option<String>,
    /// Monocode's statuses → this board's labels. Unmapped ones are never written.
    #[serde(default)]
    pub status_map: BTreeMap<StatusKey, String>,
    /// Update items when a PR opens or merges. `None` is "not decided yet":
    /// Monocode asks on the first PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_back: Option<bool>,
}

impl MondayBinding {
    pub fn label_for(&self, key: StatusKey) -> Option<&str> {
        self.status_map
            .get(&key)
            .map(String::as_str)
            .filter(|l| !l.is_empty())
    }

    /// Which of Monocode's statuses a board label stands for.
    pub fn key_for(&self, label: &str) -> Option<StatusKey> {
        self.status_map
            .iter()
            .find(|(_, mapped)| mapped.eq_ignore_ascii_case(label))
            .map(|(key, _)| *key)
    }
}

impl ProjectIntegrations {
    pub fn path(project: &Path) -> PathBuf {
        project.join(".monocode").join("integrations.json")
    }

    /// The project's settings; none when the file is missing or unreadable.
    pub fn load(project: &Path) -> Self {
        std::fs::read(Self::path(project))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, project: &Path) -> Result<(), String> {
        let path = Self::path(project);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        json.push('\n');
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_round_trips_through_the_project_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            ProjectIntegrations::load(dir.path()),
            ProjectIntegrations::default()
        );
        let mut binding = MondayBinding {
            board: "123".into(),
            board_name: "Sprint".into(),
            status_column: Some("status".into()),
            ..Default::default()
        };
        binding
            .status_map
            .insert(StatusKey::InReview, "Review".into());
        binding.status_map.insert(StatusKey::Done, "Done".into());
        let config = ProjectIntegrations {
            monday: Some(binding),
        };
        config.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(ProjectIntegrations::path(dir.path())).unwrap();
        assert!(text.contains("\"in-review\": \"Review\""), "{text}");
        assert!(!text.to_lowercase().contains("token"));
        let loaded = ProjectIntegrations::load(dir.path());
        assert_eq!(loaded, config);
        let monday = loaded.monday.unwrap();
        assert_eq!(monday.label_for(StatusKey::Done), Some("Done"));
        assert_eq!(monday.label_for(StatusKey::Todo), None);
        assert_eq!(monday.key_for("review"), Some(StatusKey::InReview));
    }
}
