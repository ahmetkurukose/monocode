//! Unpacks a package's zip archive into a folder: stored and deflated
//! entries, read through the central directory, each checked against its
//! CRC. Entries that would land outside the folder (absolute paths, `..`,
//! backslashes) refuse the whole archive, as do zip64 and encrypted ones.

use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

/// More than any extension ships (Proton Pass: ~40 MB unpacked).
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;

#[derive(Debug)]
pub enum ZipError {
    Malformed(&'static str),
    Unsupported(&'static str),
    UnsafePath(String),
    TooLarge,
    Crc(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::Malformed(what) => write!(f, "the archive is malformed ({what})"),
            ZipError::Unsupported(what) => write!(f, "the archive uses {what}"),
            ZipError::UnsafePath(path) => write!(f, "the archive has an unsafe path: {path}"),
            ZipError::TooLarge => f.write_str("the archive is too large"),
            ZipError::Crc(path) => write!(f, "{path} is damaged in the archive"),
            ZipError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ZipError {}

impl From<std::io::Error> for ZipError {
    fn from(e: std::io::Error) -> Self {
        ZipError::Io(e)
    }
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, ZipError> {
    let s = b.get(at..at + 2).ok_or(ZipError::Malformed("short"))?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ZipError> {
    let s = b.get(at..at + 4).ok_or(ZipError::Malformed("short"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// One file of the archive.
pub struct Entry {
    pub name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    data_at: usize,
}

/// The entries of `zip`, in the central directory's order.
pub fn entries(zip: &[u8]) -> Result<Vec<Entry>, ZipError> {
    // End of central directory: 22 bytes plus a comment of up to 64 KB.
    let floor = zip.len().saturating_sub(22 + 0xffff);
    let eocd = (floor..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or(ZipError::Malformed("no end of central directory"))?;
    let count = u16_at(zip, eocd + 10)? as usize;
    let dir_at = u32_at(zip, eocd + 16)? as usize;
    if count == 0xffff || dir_at == 0xffff_ffff {
        return Err(ZipError::Unsupported("zip64"));
    }
    if count > MAX_ENTRIES {
        return Err(ZipError::TooLarge);
    }
    let mut out = Vec::with_capacity(count);
    let mut at = dir_at;
    let mut total = 0u64;
    for _ in 0..count {
        if u32_at(zip, at)? != 0x0201_4b50 {
            return Err(ZipError::Malformed("central directory"));
        }
        let flags = u16_at(zip, at + 8)?;
        let method = u16_at(zip, at + 10)?;
        let crc = u32_at(zip, at + 16)?;
        let compressed = u64::from(u32_at(zip, at + 20)?);
        let size = u64::from(u32_at(zip, at + 24)?);
        let name_len = u16_at(zip, at + 28)? as usize;
        let extra_len = u16_at(zip, at + 30)? as usize;
        let comment_len = u16_at(zip, at + 32)? as usize;
        let local = u32_at(zip, at + 42)? as usize;
        let name = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or(ZipError::Malformed("name"))?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if flags & 1 != 0 {
            return Err(ZipError::Unsupported("encryption"));
        }
        if compressed == 0xffff_ffff || size == 0xffff_ffff || local == 0xffff_ffff {
            return Err(ZipError::Unsupported("zip64"));
        }
        if u32_at(zip, local)? != 0x0403_4b50 {
            return Err(ZipError::Malformed("local header"));
        }
        let data_at =
            local + 30 + u16_at(zip, local + 26)? as usize + u16_at(zip, local + 28)? as usize;
        if data_at as u64 + compressed > zip.len() as u64 {
            return Err(ZipError::Malformed("entry past the end"));
        }
        total += size;
        if total > MAX_TOTAL {
            return Err(ZipError::TooLarge);
        }
        out.push(Entry {
            name,
            method,
            crc,
            compressed,
            size,
            data_at,
        });
    }
    Ok(out)
}

/// The path an entry unpacks to under the folder, or why it may not.
pub fn safe_path(name: &str) -> Result<PathBuf, ZipError> {
    let unsafe_path = || ZipError::UnsafePath(name.to_owned());
    if name.contains('\\') || name.contains('\0') || name.starts_with('/') {
        return Err(unsafe_path());
    }
    let path = Path::new(name.trim_end_matches('/'));
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return Err(unsafe_path()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(unsafe_path());
    }
    Ok(out)
}

/// The bytes of one entry, checked.
pub fn read(zip: &[u8], entry: &Entry) -> Result<Vec<u8>, ZipError> {
    let raw = &zip[entry.data_at..entry.data_at + entry.compressed as usize];
    let data = match entry.method {
        0 => raw.to_vec(),
        8 => {
            let mut out = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(raw)
                .take(entry.size + 1)
                .read_to_end(&mut out)?;
            out
        }
        _ => return Err(ZipError::Unsupported("a compression other than deflate")),
    };
    let mut crc = flate2::Crc::new();
    crc.update(&data);
    if data.len() as u64 != entry.size || crc.sum() != entry.crc {
        return Err(ZipError::Crc(entry.name.clone()));
    }
    Ok(data)
}

/// Unpacks `zip` into `dest`, which must not exist yet or be empty.
pub fn extract(zip: &[u8], dest: &Path) -> Result<usize, ZipError> {
    let entries = entries(zip)?;
    // Every path first: nothing is written from an archive that is unsafe.
    let paths: Vec<PathBuf> = (entries.iter())
        .map(|e| safe_path(&e.name))
        .collect::<Result<_, _>>()?;
    std::fs::create_dir_all(dest)?;
    let mut files = 0;
    for (entry, path) in entries.iter().zip(paths) {
        let target = dest.join(&path);
        if entry.name.ends_with('/') {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, read(zip, entry)?)?;
        files += 1;
    }
    Ok(files)
}

/// Test fixtures: a zip writer for stored and deflated files.
#[cfg(test)]
pub(crate) fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (i, (name, data)) in files.iter().enumerate() {
        let deflate = i % 2 == 1;
        let body = if deflate {
            let mut e =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        } else {
            data.to_vec()
        };
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let method: u16 = if deflate { 8 } else { 0 };
        let local = out.len() as u32;
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&20u16.to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
            v.extend_from_slice(&method.to_le_bytes());
            v.extend_from_slice(&[0; 4]);
            v.extend_from_slice(&crc.sum().to_le_bytes());
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        common(&mut central);
        // Comment length, disk, internal and external attributes.
        central.extend_from_slice(&[0; 10]);
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let dir_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_stored_and_deflated_files() {
        let zip = build(&[
            ("manifest.json", b"{}"),
            ("assets/icon.png", &[7u8; 3000]),
            ("assets/", b""),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        assert_eq!(extract(&zip, &dest).unwrap(), 2);
        assert_eq!(std::fs::read(dest.join("manifest.json")).unwrap(), b"{}");
        assert_eq!(
            std::fs::read(dest.join("assets/icon.png")).unwrap(),
            vec![7u8; 3000]
        );
    }

    #[test]
    fn unsafe_paths_refuse_the_whole_archive() {
        for name in ["../evil", "/etc/x", "a/../../b", "a\\b", "./"] {
            assert!(
                matches!(safe_path(name), Err(ZipError::UnsafePath(_))),
                "{name}"
            );
        }
        assert_eq!(safe_path("./a/b.js").unwrap(), PathBuf::from("a/b.js"));
        let zip = build(&[("ok.js", b"1"), ("../evil.js", b"2")]);
        let dir = tempfile::tempdir().unwrap();
        assert!(extract(&zip, &dir.path().join("out")).is_err());
        assert!(!dir.path().join("out/ok.js").exists());
        assert!(!dir.path().join("evil.js").exists());
    }

    #[test]
    fn damaged_entries_are_caught() {
        let mut zip = build(&[("a.txt", b"hello world")]);
        // The stored data sits right after the 30-byte header and the name.
        zip[30 + 5] ^= 0xff;
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(extract(&zip, dir.path()), Err(ZipError::Crc(_))));
        assert!(matches!(entries(b"not a zip"), Err(ZipError::Malformed(_))));
    }
}
