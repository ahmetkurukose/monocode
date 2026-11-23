//! The vault's cipher: AES-256-GCM (`ring`) under one random 256-bit key.
//!
//! Each sealed value is `nonce (12 bytes) || ciphertext || tag (16 bytes)`
//! with a fresh random nonce, and binds its context as associated data (the
//! row's origin): a value moved to another row does not open there.

use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};

/// The vault key's length in bytes.
pub const KEY_LEN: usize = 32;

/// The key every vault of the app is sealed with.
#[derive(Clone)]
pub struct VaultKey([u8; KEY_LEN]);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(…)")
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        // Best effort: the bytes do not linger in freed memory.
        for byte in self.0.iter_mut() {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl VaultKey {
    /// A new random key.
    pub fn generate() -> Result<VaultKey, String> {
        let mut bytes = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "the system has no random numbers".to_owned())?;
        Ok(VaultKey(bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<VaultKey> {
        Some(VaultKey(bytes.try_into().ok()?))
    }

    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The key as stored in the Keychain: base64.
    pub fn encode(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }

    pub fn decode(text: &str) -> Option<VaultKey> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()?;
        VaultKey::from_bytes(&bytes)
    }

    fn cipher(&self) -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &self.0).expect("a 256-bit key"))
    }

    /// Seals `plain`, bound to `context`.
    pub fn seal(&self, context: &str, plain: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "the system has no random numbers".to_owned())?;
        let mut sealed = plain.to_vec();
        self.cipher()
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(context.as_bytes()),
                &mut sealed,
            )
            .map_err(|_| "could not encrypt".to_owned())?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Opens what [`VaultKey::seal`] sealed with the same `context`; `None`
    /// for another key, another context or changed bytes.
    pub fn open(&self, context: &str, sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < NONCE_LEN + AES_256_GCM.tag_len() {
            return None;
        }
        let (nonce, body) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut body = body.to_vec();
        let plain = self
            .cipher()
            .open_in_place(nonce, Aad::from(context.as_bytes()), &mut body)
            .ok()?;
        Some(plain.to_vec())
    }

    pub fn seal_text(&self, context: &str, plain: &str) -> Result<Vec<u8>, String> {
        self.seal(context, plain.as_bytes())
    }

    pub fn open_text(&self, context: &str, sealed: &[u8]) -> Option<String> {
        String::from_utf8(self.open(context, sealed)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_value_opens_only_with_its_key_and_context() {
        let key = VaultKey::generate().unwrap();
        let sealed = key.seal_text("https://a.test", "hunter2").unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(
            key.open_text("https://a.test", &sealed).as_deref(),
            Some("hunter2")
        );
        // Another row, another key, a flipped bit: nothing.
        assert_eq!(key.open_text("https://b.test", &sealed), None);
        let other = VaultKey::generate().unwrap();
        assert_eq!(other.open_text("https://a.test", &sealed), None);
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(key.open_text("https://a.test", &changed), None);
        assert_eq!(key.open_text("https://a.test", &sealed[..10]), None);
        // Fresh nonces: the same value seals differently each time.
        assert_ne!(sealed, key.seal_text("https://a.test", "hunter2").unwrap());
    }

    #[test]
    fn a_key_survives_its_keychain_text() {
        let key = VaultKey::generate().unwrap();
        let again = VaultKey::decode(&key.encode()).unwrap();
        assert_eq!(key.bytes(), again.bytes());
        assert!(VaultKey::decode("short").is_none());
        assert!(VaultKey::decode("not base64 !!").is_none());
    }
}
