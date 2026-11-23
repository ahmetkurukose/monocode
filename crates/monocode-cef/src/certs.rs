//! Certificate errors (docs/native/browser/context.md K62): Chromium asks
//! before it loads a page whose certificate it does not trust. The host
//! decides: [`CertificateRequest::proceed`] loads the page anyway (Chromium
//! then remembers the certificate for the host until the app quits), and
//! [`CertificateRequest::cancel`], or dropping the last clone, fails the
//! load with the error. Every callback here runs on CEF's UI thread.

use std::sync::{Arc, Mutex};

use cef::*;
use sha2::{Digest, Sha256};

use crate::{
    dialogs::AnyThread,
    view::{BrowserEvent, PageState},
};

/// A page's navigation waits on a certificate the network stack refused.
#[derive(Clone)]
pub struct CertificateRequest {
    /// The address being loaded.
    pub url: String,
    /// Chromium's net error, e.g. -202 (`ERR_CERT_AUTHORITY_INVALID`).
    pub code: i32,
    /// SHA-256 of the leaf certificate's DER encoding, in lowercase hex;
    /// empty when Chromium gave none.
    pub fingerprint: String,
    reply: Arc<CertReply>,
}

struct CertReply(Mutex<Option<AnyThread<cef::Callback>>>);

impl CertReply {
    fn take(&self) -> Option<cef::Callback> {
        self.0.lock().ok()?.take().map(|c| c.0)
    }
}

impl Drop for CertReply {
    fn drop(&mut self) {
        if let Some(callback) = self.take() {
            if !crate::runtime::is_started() {
                std::mem::forget(callback);
                return;
            }
            callback.cancel();
        }
    }
}

impl CertificateRequest {
    /// Loads the page in spite of the certificate.
    pub fn proceed(&self) {
        if let Some(callback) = self.reply.take() {
            callback.cont();
        }
    }

    /// Fails the load with the certificate's error.
    pub fn cancel(&self) {
        if let Some(callback) = self.reply.take() {
            callback.cancel();
        }
    }

    /// A request no page waits on, for the host's tests.
    pub fn detached(url: &str, code: i32, fingerprint: &str) -> Self {
        CertificateRequest {
            url: url.to_owned(),
            code,
            fingerprint: fingerprint.to_owned(),
            reply: Arc::new(CertReply(Mutex::new(None))),
        }
    }
}

impl std::fmt::Debug for CertificateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertificateRequest")
            .field("url", &self.url)
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

impl PartialEq for CertificateRequest {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

/// Lowercase hex SHA-256 of `der`.
pub(crate) fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn leaf_der(ssl_info: Option<&mut Sslinfo>) -> Vec<u8> {
    let Some(der) = ssl_info
        .and_then(|info| info.x509_certificate())
        .and_then(|cert| cert.derencoded())
    else {
        return Vec::new();
    };
    let mut bytes = vec![0u8; der.size()];
    let read = der.data(Some(&mut bytes), 0);
    bytes.truncate(read);
    bytes
}

/// `CefRequestHandler::OnCertificateError`: hands the question to the host.
pub(crate) fn certificate_error(
    state: &PageState,
    code: i32,
    url: &str,
    ssl_info: Option<&mut Sslinfo>,
    callback: Option<&mut cef::Callback>,
) -> ::std::os::raw::c_int {
    let Some(callback) = callback else { return 0 };
    let der = leaf_der(ssl_info);
    let request = CertificateRequest {
        url: url.to_owned(),
        code,
        fingerprint: if der.is_empty() {
            String::new()
        } else {
            fingerprint(&der)
        },
        reply: Arc::new(CertReply(Mutex::new(Some(AnyThread(callback.clone()))))),
    };
    state.send(BrowserEvent::CertificateError(request));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_the_sha256_of_the_certificate() {
        assert_eq!(
            fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
