//! The browser's password manager (docs/native/browser/context.md K55–K57,
//! K60; after Orca's vault, research/04 §2).
//!
//! - [`vault`]: one encrypted SQLite file per profile, `<data>/Browser
//!   Passwords/<profile>.db`; each password sealed with AES-256-GCM.
//! - [`crypto`]: the cipher and the 256-bit vault key.
//! - [`keychain`]: where the key lives: one macOS Keychain item, read once
//!   per run; a refused read leaves passwords off, never a new key.
//! - [`watch`]: the isolated-world watcher of a page's sign-in forms, and
//!   filling a saved login there.
//! - [`capture`]: whether a sign-in worked, so it is worth saving.
//! - [`mask`]: agents fill saved logins but never read them (K57).
//!
//! Imports (K61) add logins through [`vault::Vault::import`], by the
//! import's own merge rule ([`crate::browser::import::merge_logins`]: one
//! login per site and user name, the newer password wins).

pub mod capture;
pub mod crypto;
pub mod keychain;
pub mod mask;
pub mod vault;
pub mod watch;

pub use crypto::VaultKey;
pub use mask::{MASK, mask_output, mask_text};
pub use vault::{ImportReport, Login, Offer, Vault, Vaults, origin_of};

/// A saved login an agent may fill (the password stays in the app: tools
/// never put it in an answer).
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("password", &MASK)
            .finish()
    }
}
