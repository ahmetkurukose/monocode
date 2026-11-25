//! The browser's Safe Storage password, from the user's login Keychain.
//!
//! Reading another app's item makes macOS ask once ("Monocode wants to use
//! your confidential information stored in "Chrome Safe Storage""); the
//! import asks once per browser and keeps nothing.

use super::SourceBrowser;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeychainError {
    /// The user chose Deny, or cancelled.
    Denied,
    NotFound,
    Unsupported,
    Other(String),
}

/// Where the Safe Storage password comes from: the Keychain, or a fixture.
pub trait SafeStorage: Send + Sync {
    fn password(&self, browser: SourceBrowser) -> Result<Vec<u8>, KeychainError>;
}

/// The login Keychain.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

impl SafeStorage for Keychain {
    #[cfg(target_os = "macos")]
    fn password(&self, browser: SourceBrowser) -> Result<Vec<u8>, KeychainError> {
        let (service, account) = browser.keychain_item();
        let entry = crate::credentials::entry(service, account)
            .map_err(|e| KeychainError::Other(e.to_string()))?;
        match entry.get_secret() {
            Ok(secret) => Ok(secret),
            Err(keyring::Error::NoEntry) => Err(KeychainError::NotFound),
            Err(e) => Err(classify(&format!("{e:?} {e}"))),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn password(&self, _: SourceBrowser) -> Result<Vec<u8>, KeychainError> {
        Err(KeychainError::Unsupported)
    }
}

/// errSecUserCanceled (-128) and errSecAuthFailed (-25293) are the answers
/// to a denied prompt; anything else is reported as it is.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn classify(error: &str) -> KeychainError {
    let lower = error.to_ascii_lowercase();
    if error.contains("-128")
        || error.contains("-25293")
        || lower.contains("cancel")
        || lower.contains("denied")
    {
        KeychainError::Denied
    } else {
        KeychainError::Other(error.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_prompt_reads_as_denied() {
        assert_eq!(
            classify(
                "PlatformFailure(Error { code: -128, message: \"User canceled the operation.\" })"
            ),
            KeychainError::Denied
        );
        assert_eq!(classify("code: -25293"), KeychainError::Denied);
        assert!(matches!(classify("code: -25308"), KeychainError::Other(_)));
    }
}
