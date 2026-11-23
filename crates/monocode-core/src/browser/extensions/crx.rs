//! CRX3 packages, read and verified the way Chromium does before it
//! installs one (`components/crx_file/crx_verifier.cc`; Orca ports the same
//! checks in `browser-extension-crx.ts`).
//!
//! Layout: `"Cr24"`, version 3 (u32 LE), header size (u32 LE), a
//! `CrxFileHeader` protobuf, then the zip archive. The header carries
//! RSA and ECDSA proofs (public key + signature) over
//! `"CRX3 SignedData\0" | u32 LE len | signed_header_data | archive`, and the
//! signed header data names the extension id. Every proof must verify, one
//! proof's key must hash to that id (the developer's key), and a Web Store
//! package also carries a proof from the Web Store's publisher key.

use ring::signature;
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 4] = b"Cr24";
const SIGNED_DATA_PREFIX: &[u8] = b"CRX3 SignedData\x00";
/// Chromium caps the header too.
const MAX_HEADER: usize = 256 * 1024;
/// SHA-256 of the Chrome Web Store's publisher key: every package the store
/// serves carries a proof made with it.
pub const WEB_STORE_PUBLISHER_KEY_SHA256: &str =
    "61f7f2a6bfcf74cd0bc1fe2497cc9b04254c658f79f2145392867ea8366367cf";

// crx3.proto field numbers.
const HEADER_SHA256_WITH_RSA: u64 = 2;
const HEADER_SHA256_WITH_ECDSA: u64 = 3;
const HEADER_SIGNED_HEADER_DATA: u64 = 10000;
const PROOF_PUBLIC_KEY: u64 = 1;
const PROOF_SIGNATURE: u64 = 2;
const SIGNED_DATA_CRX_ID: u64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrxError {
    NotCrx,
    /// CRX2 or a newer format.
    Version(u32),
    Malformed,
    NoSignedId,
    /// The package is another extension's.
    WrongId {
        expected: String,
        found: String,
    },
    Unsigned,
    UnreadableKey,
    BadSignature,
    /// No proof's key hashes to the package's id.
    NotSignedByDeveloper,
    NotSignedByStore,
}

impl std::fmt::Display for CrxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CrxError::NotCrx => f.write_str("not a CRX package"),
            CrxError::Version(v) => write!(f, "CRX version {v} is not supported (only CRX3)"),
            CrxError::Malformed => f.write_str("the package's header is malformed"),
            CrxError::NoSignedId => f.write_str("the package names no signed extension id"),
            CrxError::WrongId { expected, found } => {
                write!(f, "the package is extension {found}, not {expected}")
            }
            CrxError::Unsigned => f.write_str("the package is not signed"),
            CrxError::UnreadableKey => f.write_str("the package has an unreadable key"),
            CrxError::BadSignature => f.write_str("the package's signature does not verify"),
            CrxError::NotSignedByDeveloper => {
                f.write_str("the package is not signed with its own key")
            }
            CrxError::NotSignedByStore => {
                f.write_str("the package is not signed by the Chrome Web Store")
            }
        }
    }
}

impl std::error::Error for CrxError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Rsa,
    Ecdsa,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    pub algorithm: Algorithm,
    /// DER `SubjectPublicKeyInfo`.
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

/// A package's header, read but not verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub proofs: Vec<Proof>,
    pub signed_header_data: Vec<u8>,
    /// The id the signed header data names.
    pub declared_id: String,
    /// Where the zip archive starts.
    pub archive_offset: usize,
}

/// A verified package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub id: String,
    /// The developer's key (DER `SubjectPublicKeyInfo`), whose hash is the id.
    pub public_key: Vec<u8>,
    pub from_store: bool,
    pub archive_offset: usize,
}

/// Chromium's id from a hash: its first 16 bytes in hex, digits 0-f as a-p.
pub fn id_from_hash(hash: &[u8]) -> String {
    hash.iter()
        .take(16)
        .flat_map(|b| [b >> 4, b & 0xf])
        .map(|n| (b'a' + n) as char)
        .collect()
}

/// An extension's id from its public key (DER `SubjectPublicKeyInfo`).
pub fn id_from_public_key(der: &[u8]) -> String {
    id_from_hash(&Sha256::digest(der))
}

fn varint(bytes: &[u8], at: &mut usize) -> Result<u64, CrxError> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = *bytes.get(*at).ok_or(CrxError::Malformed)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << (7 * shift);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CrxError::Malformed)
}

/// The length-delimited fields of one protobuf message, in order; other
/// wire types are skipped.
fn fields(bytes: &[u8]) -> Result<Vec<(u64, &[u8])>, CrxError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let key = varint(bytes, &mut at)?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                varint(bytes, &mut at)?;
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let len =
                    usize::try_from(varint(bytes, &mut at)?).map_err(|_| CrxError::Malformed)?;
                let end = at.checked_add(len).ok_or(CrxError::Malformed)?;
                out.push((field, bytes.get(at..end).ok_or(CrxError::Malformed)?));
                at = end;
            }
            _ => return Err(CrxError::Malformed),
        }
        if at > bytes.len() {
            return Err(CrxError::Malformed);
        }
    }
    Ok(out)
}

fn proof(algorithm: Algorithm, bytes: &[u8]) -> Result<Proof, CrxError> {
    let fields = fields(bytes)?;
    let find = |n| {
        fields
            .iter()
            .find(|(f, _)| *f == n)
            .map(|(_, b)| b.to_vec())
    };
    match (find(PROOF_PUBLIC_KEY), find(PROOF_SIGNATURE)) {
        (Some(public_key), Some(signature)) => Ok(Proof {
            algorithm,
            public_key,
            signature,
        }),
        _ => Err(CrxError::Unsigned),
    }
}

/// Reads a package's header without checking a signature.
pub fn read_header(crx: &[u8]) -> Result<Header, CrxError> {
    if crx.len() < 12 || &crx[..4] != MAGIC {
        return Err(CrxError::NotCrx);
    }
    let u32_at = |at: usize| u32::from_le_bytes([crx[at], crx[at + 1], crx[at + 2], crx[at + 3]]);
    let version = u32_at(4);
    if version != 3 {
        return Err(CrxError::Version(version));
    }
    let size = u32_at(8) as usize;
    if size > MAX_HEADER || 12 + size > crx.len() {
        return Err(CrxError::Malformed);
    }
    let mut proofs = Vec::new();
    let mut signed = None;
    for (field, bytes) in fields(&crx[12..12 + size])? {
        match field {
            HEADER_SHA256_WITH_RSA => proofs.push(proof(Algorithm::Rsa, bytes)?),
            HEADER_SHA256_WITH_ECDSA => proofs.push(proof(Algorithm::Ecdsa, bytes)?),
            HEADER_SIGNED_HEADER_DATA => signed = Some(bytes.to_vec()),
            _ => {}
        }
    }
    let signed = signed.ok_or(CrxError::NoSignedId)?;
    let id = fields(&signed)?
        .into_iter()
        .find(|(f, _)| *f == SIGNED_DATA_CRX_ID)
        .map(|(_, b)| b)
        .filter(|b| b.len() == 16)
        .ok_or(CrxError::NoSignedId)?;
    Ok(Header {
        proofs,
        declared_id: id_from_hash(id),
        signed_header_data: signed,
        archive_offset: 12 + size,
    })
}

/// A DER element: (tag, contents, rest).
fn der(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *bytes.first()?;
    let first = *bytes.get(1)?;
    let (len, head) = if first & 0x80 == 0 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *bytes.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = head.checked_add(len)?;
    Some((tag, bytes.get(head..end)?, bytes.get(end..)?))
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// The key inside a `SubjectPublicKeyInfo`, as ring reads it (RSA: the
/// PKCS#1 `RSAPublicKey`; EC: the uncompressed point), if its algorithm
/// is `algorithm`.
fn spki_key(spki: &[u8], algorithm: Algorithm) -> Option<&[u8]> {
    let (0x30, body, _) = der(spki)? else {
        return None;
    };
    let (0x30, alg, rest) = der(body)? else {
        return None;
    };
    let (0x06, oid, params) = der(alg)? else {
        return None;
    };
    match algorithm {
        Algorithm::Rsa if oid == OID_RSA => {}
        Algorithm::Ecdsa if oid == OID_EC => {
            let (0x06, curve, _) = der(params)? else {
                return None;
            };
            if curve != OID_P256 {
                return None;
            }
        }
        _ => return None,
    }
    let (0x03, bits, _) = der(rest)? else {
        return None;
    };
    // No unused bits.
    bits.strip_prefix(&[0])
}

fn verify_proof(proof: &Proof, message: &[u8]) -> Result<(), CrxError> {
    let key = spki_key(&proof.public_key, proof.algorithm).ok_or(CrxError::UnreadableKey)?;
    let algorithm: &dyn signature::VerificationAlgorithm = match proof.algorithm {
        Algorithm::Rsa => &signature::RSA_PKCS1_2048_8192_SHA256,
        Algorithm::Ecdsa => &signature::ECDSA_P256_SHA256_ASN1,
    };
    signature::UnparsedPublicKey::new(algorithm, key)
        .verify(message, &proof.signature)
        .map_err(|_| CrxError::BadSignature)
}

/// Verifies a package: every proof, the developer's key against the
/// declared id (and `expected_id`, when given), and with `require_store`
/// the Web Store's proof.
pub fn verify(
    crx: &[u8],
    expected_id: Option<&str>,
    require_store: bool,
) -> Result<Verified, CrxError> {
    let header = read_header(crx)?;
    if let Some(expected) = expected_id {
        if expected != header.declared_id {
            return Err(CrxError::WrongId {
                expected: expected.to_owned(),
                found: header.declared_id,
            });
        }
    }
    if header.proofs.is_empty() {
        return Err(CrxError::Unsigned);
    }
    let signed = &header.signed_header_data;
    let mut message = Vec::with_capacity(SIGNED_DATA_PREFIX.len() + 4 + signed.len());
    message.extend_from_slice(SIGNED_DATA_PREFIX);
    message.extend_from_slice(&(signed.len() as u32).to_le_bytes());
    message.extend_from_slice(signed);
    // The archive is large: hashing it once per proof would copy it too.
    let archive = &crx[header.archive_offset..];
    let mut whole = message;
    whole.extend_from_slice(archive);
    let mut developer = None;
    let mut from_store = false;
    for proof in &header.proofs {
        verify_proof(proof, &whole)?;
        let hash = Sha256::digest(&proof.public_key);
        if id_from_hash(&hash) == header.declared_id {
            developer = Some(proof.public_key.clone());
        }
        if hex(&hash) == WEB_STORE_PUBLISHER_KEY_SHA256 {
            from_store = true;
        }
    }
    let public_key = developer.ok_or(CrxError::NotSignedByDeveloper)?;
    if require_store && !from_store {
        return Err(CrxError::NotSignedByStore);
    }
    Ok(Verified {
        id: header.declared_id,
        public_key,
        from_store,
        archive_offset: header.archive_offset,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Test fixtures: packages made and signed here.
#[cfg(test)]
pub(crate) mod build {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    pub(crate) fn field(n: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint((n << 3) | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    /// P-256's `SubjectPublicKeyInfo` around an uncompressed point.
    pub(crate) fn ec_spki(point: &[u8]) -> Vec<u8> {
        let mut out = vec![
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
            0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
        ];
        out.extend_from_slice(point);
        out
    }

    /// A key pair for packages.
    pub(crate) fn key() -> signature::EcdsaKeyPair {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = signature::EcdsaKeyPair::generate_pkcs8(
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &rng,
        )
        .unwrap();
        signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            pkcs8.as_ref(),
            &rng,
        )
        .unwrap()
    }

    pub(crate) fn spki(key: &signature::EcdsaKeyPair) -> Vec<u8> {
        use signature::KeyPair;
        ec_spki(key.public_key().as_ref())
    }

    /// A CRX3 of `archive` signed by `keys`, naming `id_key`'s id (or `id`).
    pub(crate) fn crx(
        archive: &[u8],
        keys: &[&signature::EcdsaKeyPair],
        id: Option<[u8; 16]>,
        id_key: &signature::EcdsaKeyPair,
    ) -> Vec<u8> {
        let rng = ring::rand::SystemRandom::new();
        let crx_id: [u8; 16] = id.unwrap_or_else(|| {
            let hash = Sha256::digest(spki(id_key));
            hash[..16].try_into().unwrap()
        });
        let mut signed = Vec::new();
        field(SIGNED_DATA_CRX_ID, &crx_id, &mut signed);
        let mut message = SIGNED_DATA_PREFIX.to_vec();
        message.extend_from_slice(&(signed.len() as u32).to_le_bytes());
        message.extend_from_slice(&signed);
        message.extend_from_slice(archive);
        let mut header = Vec::new();
        for key in keys {
            let signature = key.sign(&rng, &message).unwrap();
            let mut proof = Vec::new();
            field(PROOF_PUBLIC_KEY, &spki(key), &mut proof);
            field(PROOF_SIGNATURE, signature.as_ref(), &mut proof);
            field(HEADER_SHA256_WITH_ECDSA, &proof, &mut header);
        }
        field(HEADER_SIGNED_HEADER_DATA, &signed, &mut header);
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(archive);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header of Proton Pass 1.44.0 as the Web Store serves it (the
    /// archive cut off: the fixture stays small).
    const PROTON_HEADER: &[u8] = include_bytes!("fixtures/proton-pass-1.44.0-header.bin");
    /// A tiny package signed with a throwaway RSA-2048 key
    /// (`fixtures/make-rsa-crx.sh`).
    const RSA_CRX: &[u8] = include_bytes!("fixtures/rsa-signed.crx");

    #[test]
    fn ids_come_from_the_key_hash() {
        assert_eq!(id_from_hash(&[0u8; 32]), "a".repeat(32));
        assert_eq!(id_from_hash(&[0xffu8; 32]), "p".repeat(32));
        assert_eq!(id_from_hash(&[0x01, 0x23]), "abcd");
    }

    #[test]
    fn a_web_store_header_names_its_id_and_both_keys() {
        let header = read_header(PROTON_HEADER).unwrap();
        assert_eq!(header.declared_id, super::super::PROTON_PASS_ID);
        assert!(header.proofs.len() >= 2, "{}", header.proofs.len());
        let ids: Vec<String> = (header.proofs.iter())
            .map(|p| id_from_public_key(&p.public_key))
            .collect();
        assert!(ids.contains(&header.declared_id));
        assert!(
            header
                .proofs
                .iter()
                .any(|p| { hex(&Sha256::digest(&p.public_key)) == WEB_STORE_PUBLISHER_KEY_SHA256 })
        );
        // Every key is one ring can read.
        for p in &header.proofs {
            assert!(spki_key(&p.public_key, p.algorithm).is_some());
        }
        assert_eq!(header.archive_offset, PROTON_HEADER.len());
        // Without its archive no signature verifies.
        assert_eq!(
            verify(PROTON_HEADER, None, false),
            Err(CrxError::BadSignature)
        );
    }

    #[test]
    fn an_rsa_package_verifies() {
        let verified = verify(RSA_CRX, None, false).unwrap();
        assert_eq!(verified.id, id_from_public_key(&verified.public_key));
        assert!(!verified.from_store);
        assert_eq!(verify(RSA_CRX, None, true), Err(CrxError::NotSignedByStore));
        let mut tampered = RSA_CRX.to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(verify(&tampered, None, false), Err(CrxError::BadSignature));
    }

    #[test]
    fn an_ecdsa_package_verifies_and_tampering_fails() {
        let key = build::key();
        let crx = build::crx(b"PK archive bytes", &[&key], None, &key);
        let id = id_from_public_key(&build::spki(&key));
        let verified = verify(&crx, Some(&id), false).unwrap();
        assert_eq!(verified.id, id);
        assert_eq!(&crx[verified.archive_offset..], b"PK archive bytes");

        let mut tampered = crx.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(verify(&tampered, None, false), Err(CrxError::BadSignature));

        assert!(matches!(
            verify(&crx, Some(super::super::PROTON_PASS_ID), false),
            Err(CrxError::WrongId { .. })
        ));
    }

    #[test]
    fn a_package_must_be_signed_by_the_key_of_its_id() {
        let (a, b) = (build::key(), build::key());
        // Signed by `b` only, but naming `a`'s id.
        let crx = build::crx(b"zip", &[&b], None, &a);
        assert_eq!(
            verify(&crx, None, false),
            Err(CrxError::NotSignedByDeveloper)
        );
        // Both sign: fine.
        let crx = build::crx(b"zip", &[&a, &b], None, &a);
        assert!(verify(&crx, None, false).is_ok());
    }

    #[test]
    fn broken_packages_are_refused() {
        assert_eq!(read_header(b"nope"), Err(CrxError::NotCrx));
        let mut v2 = b"Cr24".to_vec();
        v2.extend_from_slice(&2u32.to_le_bytes());
        v2.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(read_header(&v2), Err(CrxError::Version(2)));
        let mut huge = b"Cr24".to_vec();
        huge.extend_from_slice(&3u32.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_header(&huge), Err(CrxError::Malformed));
        let mut empty = b"Cr24".to_vec();
        empty.extend_from_slice(&3u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(read_header(&empty), Err(CrxError::NoSignedId));
        // A truncated field.
        let mut bad = b"Cr24".to_vec();
        bad.extend_from_slice(&3u32.to_le_bytes());
        bad.extend_from_slice(&2u32.to_le_bytes());
        bad.extend_from_slice(&[0x12, 0x7f]);
        assert_eq!(read_header(&bad), Err(CrxError::Malformed));
    }
}
