//! Whether the running app is signed with a team's identity.
//!
//! macOS ties a keychain item's access list to the signer. For a team
//! identity (Developer ID, Apple Development) that survives updates; for an
//! ad-hoc signature it is the exact code hash, which every build changes, so
//! each new build would ask for the keychain password again.

use std::ffi::c_void;

type OsStatus = i32;
type CfTypeRef = *const c_void;

/// `kSecCSSigningInformation`.
const SIGNING_INFORMATION: u32 = 1 << 1;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySelf(flags: u32, code: *mut CfTypeRef) -> OsStatus;
    fn SecCodeCopySigningInformation(
        code: CfTypeRef,
        flags: u32,
        information: *mut CfTypeRef,
    ) -> OsStatus;
    static kSecCodeInfoTeamIdentifier: CfTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dictionary: CfTypeRef, key: CfTypeRef) -> CfTypeRef;
    fn CFRelease(object: CfTypeRef);
}

/// True when the app's signature names a team.
pub(crate) fn has_team_identity() -> bool {
    // SAFETY: plain Security/CoreFoundation calls; every object we own is
    // released, and the team value is only compared with null (it belongs to
    // the dictionary).
    unsafe {
        let mut code: CfTypeRef = std::ptr::null();
        if SecCodeCopySelf(0, &mut code) != 0 || code.is_null() {
            return false;
        }
        let mut information: CfTypeRef = std::ptr::null();
        let status = SecCodeCopySigningInformation(code, SIGNING_INFORMATION, &mut information);
        CFRelease(code);
        if status != 0 || information.is_null() {
            return false;
        }
        let team = CFDictionaryGetValue(information, kSecCodeInfoTeamIdentifier);
        let signed = !team.is_null();
        CFRelease(information);
        signed
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_test_binary_has_no_team_identity() {
        // Cargo's test binaries are only linker-signed (ad hoc).
        assert!(!super::has_team_identity());
    }
}
