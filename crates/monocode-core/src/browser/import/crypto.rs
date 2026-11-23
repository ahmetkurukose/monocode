//! Chromium's encrypted values on macOS (OSCrypt): the Keychain's "Safe
//! Storage" password → PBKDF2-HMAC-SHA1 (salt `saltysalt`, 1003 rounds,
//! 16 bytes) → AES-128-CBC with an IV of 16 spaces and PKCS#7 padding,
//! behind a `v10` prefix. `v20` values are app-bound (sealed to the browser
//! itself) and cannot be decrypted by anyone else.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

const SALT: &[u8] = b"saltysalt";
const ROUNDS: u32 = 1003;
const IV: [u8; 16] = [b' '; 16];

type Decryptor = cbc::Decryptor<aes::Aes128>;

/// The AES key of one browser, derived from its Safe Storage password.
#[derive(Clone, PartialEq, Eq)]
pub struct SafeStorageKey([u8; 16]);

impl std::fmt::Debug for SafeStorageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SafeStorageKey(..)")
    }
}

impl SafeStorageKey {
    pub fn derive(password: &[u8]) -> Self {
        let mut key = [0u8; 16];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, SALT, ROUNDS, &mut key);
        SafeStorageKey(key)
    }

    /// Encrypts `plain` the way Chromium does, for test fixtures.
    #[cfg(test)]
    pub(crate) fn encrypt_v10(&self, plain: &[u8]) -> Vec<u8> {
        use aes::cipher::BlockEncryptMut;
        let encryptor = cbc::Encryptor::<aes::Aes128>::new(&self.0.into(), &IV.into());
        let mut out = b"v10".to_vec();
        out.extend(encryptor.encrypt_padded_vec_mut::<Pkcs7>(plain));
        out
    }
}

/// What an encrypted value turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decrypted {
    Plain(Vec<u8>),
    /// `v20`: app-bound, never decryptable here.
    AppBound,
    /// A wrong key, a damaged value, or a version this code does not know.
    Failed,
}

/// Decrypts a `v10` value with `key`.
pub fn decrypt(value: &[u8], key: &SafeStorageKey) -> Decrypted {
    match value.get(..3) {
        Some(b"v10") => {}
        Some(b"v20") => return Decrypted::AppBound,
        _ => return Decrypted::Failed,
    }
    let body = &value[3..];
    if body.is_empty() || !body.len().is_multiple_of(16) {
        return Decrypted::Failed;
    }
    match Decryptor::new(&key.0.into(), &IV.into()).decrypt_padded_vec_mut::<Pkcs7>(body) {
        Ok(plain) => Decrypted::Plain(plain),
        Err(_) => Decrypted::Failed,
    }
}

/// From cookie database version 24 on, a decrypted cookie value starts
/// with SHA-256 of its `host_key`, which binds it to its domain. Strips it,
/// or fails when the prefix is not that hash.
pub fn strip_host_hash(plain: Vec<u8>, host_key: &str) -> Option<Vec<u8>> {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(host_key.as_bytes());
    if plain.len() >= 32 && plain[..32] == hash[..] {
        Some(plain[32..].to_vec())
    } else {
        None
    }
}
