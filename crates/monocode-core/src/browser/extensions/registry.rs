//! Installed extensions on disk, under `<data>/Browser Extensions/`:
//!
//! - `extensions.json`: what is installed, enabled, granted, and held.
//! - `<id>/<version>/`: the unpacked package (a new version unpacks beside
//!   the old one, which goes once the new one is in use).
//! - `Data/<profile>/<id>/`: the extension's data in a browser profile
//!   (`storage.sqlite`, `runtime.json`). Removed with the extension.
//!
//! An update whose permissions show new warnings is held: unpacked, but the
//! old version keeps running until the user approves (as Chrome and Orca).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{crx, manifest::Manifest, permissions};

pub const FOLDER: &str = "Browser Extensions";
const STATE_FILE: &str = "extensions.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub version: String,
    /// The warnings the update adds.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    #[serde(default)]
    pub from_store: bool,
    /// Seconds since the epoch.
    #[serde(default)]
    pub installed_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    /// What the user agreed to, at install or the last approved update.
    #[serde(default)]
    pub granted_permissions: Vec<String>,
    #[serde(default)]
    pub granted_hosts: Vec<String>,
    #[serde(default)]
    pub held: Option<Held>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    extensions: Vec<Installed>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Installed(String),
    Updated {
        from: String,
        to: String,
    },
    /// The same version is installed already.
    Current,
    /// The update waits for the user's approval.
    Held(Held),
}

pub struct Registry {
    root: PathBuf,
    state: State,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A folder name for a profile's data.
pub fn profile_folder(profile: &str) -> String {
    profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

impl Registry {
    /// The registry under `data_dir` (the app's data folder).
    pub fn open(data_dir: &Path) -> Self {
        let root = data_dir.join(FOLDER);
        let state = std::fs::read(root.join(STATE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Registry { root, state }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(&self) -> &[Installed] {
        &self.state.extensions
    }

    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.state.extensions.iter().find(|e| e.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Installed> {
        self.state.extensions.iter_mut().find(|e| e.id == id)
    }

    /// The running version's folder.
    pub fn dir(&self, id: &str) -> Option<PathBuf> {
        let e = self.get(id)?;
        Some(self.root.join(&e.id).join(&e.version))
    }

    /// Where the extension keeps its data in `profile`.
    pub fn data_dir(&self, profile: &str, id: &str) -> PathBuf {
        self.root
            .join("Data")
            .join(profile_folder(profile))
            .join(id)
    }

    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let tmp = self.root.join(format!("{STATE_FILE}.tmp"));
        let body = serde_json::to_vec_pretty(&self.state).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.root.join(STATE_FILE)).map_err(|e| e.to_string())
    }

    /// Verifies, unpacks and installs (or updates to) a package.
    /// `expected_id`: the extension asked for; `require_store`: the package
    /// must carry the Web Store's proof.
    pub fn install_crx(
        &mut self,
        bytes: &[u8],
        expected_id: Option<&str>,
        require_store: bool,
    ) -> Result<Outcome, String> {
        let verified = crx::verify(bytes, expected_id, require_store).map_err(|e| e.to_string())?;
        let id = verified.id.clone();
        let ext_dir = self.root.join(&id);
        let staging = ext_dir.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
        let unpack = || -> Result<Manifest, String> {
            super::zip::extract(&bytes[verified.archive_offset..], &staging)
                .map_err(|e| e.to_string())?;
            let manifest = Manifest::load(&staging, "en").map_err(|e| e.to_string())?;
            if let Some(key) = &manifest.key {
                use base64::Engine as _;
                let der = base64::engine::general_purpose::STANDARD
                    .decode(key.trim())
                    .map_err(|_| "the manifest's key is not base64".to_string())?;
                if crx::id_from_public_key(&der) != id {
                    return Err("the manifest's key is another extension's".into());
                }
            }
            Ok(manifest)
        };
        let manifest = match unpack() {
            Ok(m) => m,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };
        let version = manifest.version.clone();
        let target = ext_dir.join(&version);
        let existing = self.get(&id).cloned();
        if let Some(e) = &existing {
            if super::compare_versions(&version, &e.version).is_le() && e.held.is_none() {
                let _ = std::fs::remove_dir_all(&staging);
                return Ok(Outcome::Current);
            }
        }
        if target.exists() {
            let _ = std::fs::remove_dir_all(&target);
        }
        std::fs::rename(&staging, &target).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            e.to_string()
        })?;
        let permissions = manifest.permissions.clone();
        let hosts = manifest.host_permissions.clone();
        let outcome = match existing {
            None => {
                self.state.extensions.push(Installed {
                    id: id.clone(),
                    version: version.clone(),
                    enabled: true,
                    from_store: verified.from_store,
                    installed_at: now(),
                    updated_at: now(),
                    granted_permissions: permissions,
                    granted_hosts: hosts,
                    held: None,
                });
                Outcome::Installed(version)
            }
            Some(e) => {
                let added = permissions::added_warnings(
                    (&permissions, &hosts),
                    (&e.granted_permissions, &e.granted_hosts),
                );
                if added.is_empty() {
                    let from = e.version.clone();
                    let entry = self.get_mut(&id).expect("installed");
                    entry.version = version.clone();
                    entry.updated_at = now();
                    entry.held = None;
                    entry.granted_permissions = permissions;
                    entry.granted_hosts = hosts;
                    self.forget_old_versions(&id);
                    Outcome::Updated { from, to: version }
                } else {
                    let held = Held {
                        version: version.clone(),
                        warnings: added,
                    };
                    self.get_mut(&id).expect("installed").held = Some(held.clone());
                    Outcome::Held(held)
                }
            }
        };
        self.save()?;
        Ok(outcome)
    }

    /// The user approved a held update: it is the running version now.
    pub fn approve_held(&mut self, id: &str) -> Result<Option<String>, String> {
        let root = self.root.clone();
        let Some(entry) = self.get_mut(id) else {
            return Ok(None);
        };
        let Some(held) = entry.held.take() else {
            return Ok(None);
        };
        let dir = root.join(id).join(&held.version);
        let manifest = Manifest::load(&dir, "en").map_err(|e| e.to_string())?;
        entry.version = held.version.clone();
        entry.updated_at = now();
        entry.granted_permissions = manifest.permissions;
        entry.granted_hosts = manifest.host_permissions;
        self.forget_old_versions(id);
        self.save()?;
        Ok(Some(held.version))
    }

    /// Every version folder but the running one (and a held one).
    fn forget_old_versions(&self, id: &str) {
        let Some(e) = self.get(id) else { return };
        let keep: Vec<&str> = std::iter::once(e.version.as_str())
            .chain(e.held.as_ref().map(|h| h.version.as_str()))
            .collect();
        let Ok(dirs) = std::fs::read_dir(self.root.join(id)) else {
            return;
        };
        for dir in dirs.flatten() {
            let name = dir.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name.as_str()) {
                let _ = std::fs::remove_dir_all(dir.path());
            }
        }
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let entry = self.get_mut(id).ok_or("not installed")?;
        entry.enabled = enabled;
        self.save()
    }

    /// Removes the extension, its files and its data in every profile.
    pub fn uninstall(&mut self, id: &str) -> Result<(), String> {
        if !super::is_valid_id(id) {
            return Err("not an extension id".into());
        }
        self.state.extensions.retain(|e| e.id != id);
        self.save()?;
        let _ = std::fs::remove_dir_all(self.root.join(id));
        if let Ok(profiles) = std::fs::read_dir(self.root.join("Data")) {
            for profile in profiles.flatten() {
                let _ = std::fs::remove_dir_all(profile.path().join(id));
            }
        }
        Ok(())
    }

    /// Enabled extensions and their running folders.
    pub fn enabled(&self) -> Vec<(Installed, PathBuf)> {
        (self.state.extensions.iter())
            .filter(|e| e.enabled)
            .map(|e| (e.clone(), self.root.join(&e.id).join(&e.version)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::extensions::crx::build;
    use serde_json::json;

    fn package(
        key: &ring::signature::EcdsaKeyPair,
        version: &str,
        permissions: &[&str],
    ) -> Vec<u8> {
        let manifest = json!({"name": "Test", "version": version, "manifest_version": 3,
            "permissions": permissions, "host_permissions": ["https://a.com/*"]});
        let zip = super::super::zip::build(&[
            ("manifest.json", manifest.to_string().as_bytes()),
            ("background.js", b"1"),
        ]);
        build::crx(&zip, &[key], None, key)
    }

    #[test]
    fn install_update_hold_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let key = build::key();
        let id = crx::id_from_public_key(&build::spki(&key));
        let mut reg = Registry::open(dir.path());
        let outcome = reg
            .install_crx(&package(&key, "1.0", &["storage"]), Some(&id), false)
            .unwrap();
        assert_eq!(outcome, Outcome::Installed("1.0".into()));
        assert!(reg.dir(&id).unwrap().join("background.js").exists());
        assert_eq!(
            reg.install_crx(&package(&key, "1.0", &["storage"]), Some(&id), false)
                .unwrap(),
            Outcome::Current
        );
        // No new warning: the update applies, the old folder goes.
        let outcome = reg
            .install_crx(&package(&key, "1.1", &["storage", "alarms"]), None, false)
            .unwrap();
        assert_eq!(
            outcome,
            Outcome::Updated {
                from: "1.0".into(),
                to: "1.1".into()
            }
        );
        assert!(!dir.path().join(FOLDER).join(&id).join("1.0").exists());
        // A new warning: held, 1.1 keeps running.
        let outcome = reg
            .install_crx(
                &package(&key, "1.2", &["storage", "clipboardRead"]),
                None,
                false,
            )
            .unwrap();
        assert!(
            matches!(&outcome, Outcome::Held(h) if h.warnings == vec!["Read data you copy and paste".to_string()])
        );
        assert_eq!(reg.get(&id).unwrap().version, "1.1");
        // Survives a restart.
        let mut reg = Registry::open(dir.path());
        assert_eq!(reg.get(&id).unwrap().held.as_ref().unwrap().version, "1.2");
        assert_eq!(reg.approve_held(&id).unwrap().as_deref(), Some("1.2"));
        assert_eq!(reg.get(&id).unwrap().version, "1.2");
        assert!(
            reg.get(&id)
                .unwrap()
                .granted_permissions
                .contains(&"clipboardRead".to_string())
        );
        reg.set_enabled(&id, false).unwrap();
        assert!(reg.enabled().is_empty());
        let data = reg.data_dir("default", &id);
        std::fs::create_dir_all(&data).unwrap();
        reg.uninstall(&id).unwrap();
        assert!(reg.get(&id).is_none());
        assert!(!data.exists());
        assert!(!dir.path().join(FOLDER).join(&id).exists());
        assert!(Registry::open(dir.path()).list().is_empty());
    }

    #[test]
    fn a_wrong_or_broken_package_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let key = build::key();
        let mut reg = Registry::open(dir.path());
        let err = reg
            .install_crx(
                &package(&key, "1.0", &[]),
                Some(crate::browser::extensions::PROTON_PASS_ID),
                false,
            )
            .unwrap_err();
        assert!(err.contains("not ghmbeld"), "{err}");
        let v2 = super::super::zip::build(&[(
            "manifest.json",
            br#"{"name":"x","version":"1","manifest_version":2}"#,
        )]);
        let err = reg
            .install_crx(&build::crx(&v2, &[&key], None, &key), None, false)
            .unwrap_err();
        assert!(err.contains("Manifest V2"), "{err}");
        let id = crx::id_from_public_key(&build::spki(&key));
        let left: Vec<_> = std::fs::read_dir(dir.path().join(FOLDER).join(&id))
            .map(|d| d.flatten().collect())
            .unwrap_or_default();
        assert!(left.is_empty());
        assert!(reg.list().is_empty());
        assert!(
            reg.install_crx(&package(&key, "1.0", &[]), None, true)
                .is_err()
        );
    }
}
