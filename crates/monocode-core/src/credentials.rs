//! The one way into the OS credential store (the macOS Keychain).
//!
//! Test builds (this crate's tests, and the tests of crates that turn on
//! `test-support`) keep every credential in process memory instead. A test
//! binary is a new, differently signed program on every build, so a real
//! Keychain item would make macOS ask for the login password once per item,
//! per build, per test process.

/// The credential store entry for `service` and `account`.
pub fn entry(service: &str, account: &str) -> keyring::Result<keyring::Entry> {
    #[cfg(any(test, feature = "test-support"))]
    memory::install();
    keyring::Entry::new(service, account)
}

#[cfg(any(test, feature = "test-support"))]
mod memory {
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, Once, OnceLock};

    use keyring::credential::{
        Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence,
    };

    type Store = Arc<Mutex<HashMap<(String, String), Vec<u8>>>>;

    fn store() -> Store {
        static STORE: OnceLock<Store> = OnceLock::new();
        STORE.get_or_init(Store::default).clone()
    }

    /// Makes every entry, from now on, one of [`store`]'s.
    pub(super) fn install() {
        static INSTALLED: Once = Once::new();
        INSTALLED.call_once(|| keyring::set_default_credential_builder(Box::new(Builder)));
    }

    #[derive(Debug)]
    struct Builder;

    impl CredentialBuilderApi for Builder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(Item {
                key: (service.to_owned(), user.to_owned()),
                store: store(),
            }))
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn persistence(&self) -> CredentialPersistence {
            CredentialPersistence::ProcessOnly
        }
    }

    #[derive(Debug)]
    struct Item {
        key: (String, String),
        store: Store,
    }

    impl CredentialApi for Item {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .insert(self.key.clone(), secret.to_vec());
            Ok(())
        }

        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            self.store
                .lock()
                .unwrap()
                .get(&self.key)
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            self.store
                .lock()
                .unwrap()
                .remove(&self.key)
                .map(|_| ())
                .ok_or(keyring::Error::NoEntry)
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_builds_keep_credentials_in_memory_between_entries() {
        let account = "credentials-test-account";
        super::entry("com.monocode.test", account)
            .unwrap()
            .set_password("secret")
            .unwrap();
        let again = super::entry("com.monocode.test", account).unwrap();
        assert_eq!(again.get_password().unwrap(), "secret");
        let other = super::entry("com.monocode.test", "someone-else").unwrap();
        assert!(matches!(other.get_password(), Err(keyring::Error::NoEntry)));
    }
}
