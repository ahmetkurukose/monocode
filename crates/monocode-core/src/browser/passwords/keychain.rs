//! Where the vault key lives (context.md K56): one generic-password item in
//! the macOS Keychain (the OS credential store elsewhere), read once per
//! run. Not one item per password: each item the app did not create asks
//! the user once, and an ad-hoc signed development build asks again after
//! every build (K59).
//!
//! The item is named by the data folder, so a temporary data folder (tests,
//! `--data-dir`) never reads or replaces the real one's key.
//!
//! A read that fails (the user clicked "Deny", the keychain is locked) is
//! [`KeyError::Unavailable`]: passwords are off for now and nothing is
//! written. A key that is not there while vaults already exist is
//! [`KeyError::Lost`]: a new key would not open them, so none is made.

use std::path::{Path, PathBuf};

use super::crypto::VaultKey;

/// The Keychain item's service.
pub const KEYCHAIN_SERVICE: &str = "dev.usemono.gpui.browser-passwords";

/// Development hook: keep the key in this file instead of the Keychain
/// (insecure; for test runs of the app with a throwaway data folder).
pub const KEY_FILE_ENV: &str = "MONOCODE_BROWSER_PASSWORDS_KEY_FILE";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// The store would not give the key: denied, cancelled, locked.
    Unavailable(String),
    /// Vaults exist but their key is gone.
    Lost,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Unavailable(why) => write!(f, "the Keychain did not give the key: {why}"),
            KeyError::Lost => f.write_str(
                "the saved passwords' key is no longer in the Keychain; they cannot be opened",
            ),
        }
    }
}

/// Reads and keeps the vault key.
pub trait KeySource: Send + Sync {
    /// The key; `Ok(None)` when none is stored yet.
    fn read(&self) -> Result<Option<VaultKey>, KeyError>;
    fn write(&self, key: &VaultKey) -> Result<(), KeyError>;
}

/// The key, made on first use unless `vaults_exist` (then [`KeyError::Lost`]).
pub fn get_or_create(source: &dyn KeySource, vaults_exist: bool) -> Result<VaultKey, KeyError> {
    if let Some(key) = source.read()? {
        return Ok(key);
    }
    if vaults_exist {
        return Err(KeyError::Lost);
    }
    let key = VaultKey::generate().map_err(KeyError::Unavailable)?;
    source.write(&key)?;
    Ok(key)
}

/// The item's account for the data folder `data_dir`.
pub fn account_for(data_dir: Option<&Path>) -> String {
    use sha2::Digest;
    match data_dir {
        None => "vault".to_owned(),
        Some(dir) => {
            let digest = sha2::Sha256::digest(dir.to_string_lossy().as_bytes());
            let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            format!("vault-{hex}")
        }
    }
}

/// The OS credential store.
pub struct Keychain {
    pub account: String,
}

impl KeySource for Keychain {
    fn read(&self) -> Result<Option<VaultKey>, KeyError> {
        let entry = crate::credentials::entry(KEYCHAIN_SERVICE, &self.account)
            .map_err(|e| KeyError::Unavailable(e.to_string()))?;
        match entry.get_password() {
            Ok(text) => VaultKey::decode(&text)
                .map(Some)
                .ok_or_else(|| KeyError::Unavailable("the stored key is not a key".into())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(KeyError::Unavailable(error.to_string())),
        }
    }

    fn write(&self, key: &VaultKey) -> Result<(), KeyError> {
        crate::credentials::entry(KEYCHAIN_SERVICE, &self.account)
            .and_then(|entry| entry.set_password(&key.encode()))
            .map_err(|e| KeyError::Unavailable(e.to_string()))
    }
}

/// [`KEY_FILE_ENV`]'s file.
pub struct KeyFile(pub PathBuf);

impl KeySource for KeyFile {
    fn read(&self) -> Result<Option<VaultKey>, KeyError> {
        match std::fs::read_to_string(&self.0) {
            Ok(text) => VaultKey::decode(&text)
                .map(Some)
                .ok_or_else(|| KeyError::Unavailable("the key file holds no key".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(KeyError::Unavailable(e.to_string())),
        }
    }

    fn write(&self, key: &VaultKey) -> Result<(), KeyError> {
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&self.0, key.encode()).map_err(|e| KeyError::Unavailable(e.to_string()))
    }
}

/// The source the app uses: [`KEY_FILE_ENV`]'s file when set, else the
/// Keychain item of `data_dir`.
pub fn default_source(data_dir: Option<&Path>) -> Box<dyn KeySource> {
    match std::env::var_os(KEY_FILE_ENV).filter(|v| !v.is_empty()) {
        Some(path) => Box::new(KeyFile(PathBuf::from(path))),
        None => Box::new(Keychain {
            account: account_for(data_dir),
        }),
    }
}

/// A key kept in memory (tests), or a store that refuses.
#[derive(Default)]
pub struct MemoryKey {
    pub key: std::sync::Mutex<Option<VaultKey>>,
    pub refuse: Option<String>,
}

impl KeySource for MemoryKey {
    fn read(&self) -> Result<Option<VaultKey>, KeyError> {
        if let Some(why) = &self.refuse {
            return Err(KeyError::Unavailable(why.clone()));
        }
        Ok(self.key.lock().unwrap().clone())
    }

    fn write(&self, key: &VaultKey) -> Result<(), KeyError> {
        if let Some(why) = &self.refuse {
            return Err(KeyError::Unavailable(why.clone()));
        }
        *self.key.lock().unwrap() = Some(key.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_made_once_and_never_over_existing_vaults() {
        let memory = MemoryKey::default();
        let key = get_or_create(&memory, false).unwrap();
        let again = get_or_create(&memory, true).unwrap();
        assert_eq!(key.bytes(), again.bytes());

        // Vaults without their key: no new key that would not open them.
        let empty = MemoryKey::default();
        assert_eq!(get_or_create(&empty, true).unwrap_err(), KeyError::Lost);
        assert!(empty.key.lock().unwrap().is_none());

        // "Deny": unavailable, nothing written.
        let denied = MemoryKey {
            refuse: Some("User canceled the operation.".into()),
            ..Default::default()
        };
        assert!(matches!(
            get_or_create(&denied, false),
            Err(KeyError::Unavailable(_))
        ));
    }

    #[test]
    fn a_key_file_keeps_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = KeyFile(dir.path().join("k/key"));
        assert!(file.read().unwrap().is_none());
        let key = get_or_create(&file, false).unwrap();
        assert_eq!(file.read().unwrap().unwrap().bytes(), key.bytes());
    }

    #[test]
    fn each_data_folder_has_its_own_item() {
        let a = account_for(Some(Path::new("/a")));
        assert_ne!(a, account_for(Some(Path::new("/b"))));
        assert_eq!(a, account_for(Some(Path::new("/a"))));
        assert!(a.starts_with("vault-"));
    }
}
