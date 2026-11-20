//! Installing the Chromium runtime (CEF) on first use.
//!
//! CEF is not shipped inside the app (docs/native/browser/context.md K7–K10).
//! The first time a browser opens, the exact CEF build this crate was compiled
//! against is downloaded from Spotify's CEF build server, verified against its
//! published SHA-1, unpacked into a versioned folder and marked complete. A
//! folder for any other version is removed once the pinned one is in place.
//!
//! Layout under `base` (the caller picks it, e.g. `~/Library/Application
//! Support/Monocode/cef`):
//!
//! ```text
//! base/
//!   downloads/<archive>.part        partial download, resumed with HTTP Range
//!   <CEF_VERSION>/                   the installed runtime
//!     .complete                      marker: the archive's SHA-1
//!     Chromium Embedded Framework.framework/   (macOS)
//!     libcef.so / libcef.dll, *.pak, locales/  (Linux, Windows)
//! ```
//!
//! Everything here is plain blocking I/O meant for a background thread; it
//! never touches CEF itself, so it works before the framework exists.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// The CEF build the `cef` crate (=156.7.0) wraps. The downloaded runtime must
/// be this exact build: the wrapper is compiled against its API hash.
pub const CEF_VERSION: &str = "156.0.31+ge603d97+chromium-156.0.8078.100";

/// Where archives come from. Spotify hosts CEF's official builds.
pub const CDN_URL: &str = "https://cef-builds.spotifycdn.com";

/// One downloadable archive, as listed in the CDN's `index.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    pub file: &'static str,
    pub size: u64,
    pub sha1: &'static str,
}

/// The "minimal" distribution for the platform this binary was built for, or
/// `None` where the browser is not supported.
pub fn pinned() -> Option<Archive> {
    let (file, size, sha1) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_macosarm64_minimal.tar.bz2",
            134_916_866,
            "7d1d513c779db3829f69a35567f3fd8e641ed5a9",
        ),
        ("macos", "x86_64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_macosx64_minimal.tar.bz2",
            141_698_452,
            "e52c87e35b44b526268f79f5fe63e2519c0ec6a8",
        ),
        ("windows", "x86_64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_windows64_minimal.tar.bz2",
            175_156_160,
            "d179fb251141d6c30992a1775db80a6a879852ba",
        ),
        ("windows", "aarch64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_windowsarm64_minimal.tar.bz2",
            170_159_472,
            "0d1a346ac91fef15ad32e81db68a19a0c547e2b0",
        ),
        ("linux", "x86_64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_linux64_minimal.tar.bz2",
            333_321_290,
            "0adb813face4ca098f17210fe471b172c2bc1950",
        ),
        ("linux", "aarch64") => (
            "cef_binary_156.0.31+ge603d97+chromium-156.0.8078.100_linuxarm64_minimal.tar.bz2",
            431_212_107,
            "a253f6ba3e790be18057f5d89533bac373cde19c",
        ),
        _ => return None,
    };
    Some(Archive { file, size, sha1 })
}

/// An installed, verified runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRuntime {
    /// `base/<CEF_VERSION>`.
    pub root: PathBuf,
}

impl InstalledRuntime {
    /// macOS: the `.framework` bundle. Elsewhere: the folder holding `libcef`.
    pub fn framework_dir(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.root.join("Chromium Embedded Framework.framework")
        } else {
            self.root.clone()
        }
    }

    /// The library `cef_load_library` is given.
    pub fn library_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.framework_dir().join("Chromium Embedded Framework")
        } else if cfg!(windows) {
            self.root.join("libcef.dll")
        } else {
            self.root.join("libcef.so")
        }
    }

    /// macOS: the sandbox library a helper initializes before loading CEF.
    pub fn sandbox_library_path(&self) -> PathBuf {
        self.framework_dir().join("Libraries/libcef_sandbox.dylib")
    }
}

/// What an install is doing, for a progress display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    /// Bytes so far out of the archive's size.
    Downloading { done: u64, total: u64 },
    Verifying,
    Extracting,
}

#[derive(Debug)]
pub enum Error {
    /// No CEF build exists for this OS/architecture.
    Unsupported,
    Cancelled,
    Http(String),
    Io(io::Error),
    /// Size or SHA-1 did not match; the partial file was discarded.
    Corrupt(String),
    /// The archive holds a path that would land outside the install folder.
    UnsafeArchive(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => write!(f, "Chromium is not available for this platform"),
            Error::Cancelled => write!(f, "the download was cancelled"),
            Error::Http(e) => write!(f, "download failed: {e}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Corrupt(e) => write!(f, "the download was damaged ({e}); it will start over"),
            Error::UnsafeArchive(p) => write!(f, "the archive holds an unsafe path: {p}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

const MARKER: &str = ".complete";

/// The pinned runtime under `base`, if it is fully installed.
pub fn installed(base: &Path) -> Option<InstalledRuntime> {
    let archive = pinned()?;
    let root = base.join(CEF_VERSION);
    let marker = fs::read_to_string(root.join(MARKER)).ok()?;
    let runtime = InstalledRuntime { root };
    (marker.trim() == archive.sha1 && runtime.library_path().is_file()).then_some(runtime)
}

/// Downloads, verifies and unpacks the pinned runtime into `base`, resuming a
/// partial download. Returns at once when it is already installed. `cancel`
/// is polled between chunks; a cancelled download keeps its partial file.
pub fn install(
    base: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<InstalledRuntime> {
    if let Some(runtime) = installed(base) {
        return Ok(runtime);
    }
    let archive = pinned().ok_or(Error::Unsupported)?;
    let downloads = base.join("downloads");
    fs::create_dir_all(&downloads)?;
    let part = downloads.join(format!("{}.part", archive.file));

    download(&archive, &part, cancel, &mut progress)?;

    progress(Progress::Verifying);
    if let Err(e) = verify(&archive, &part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }

    progress(Progress::Extracting);
    let root = base.join(CEF_VERSION);
    let staging = base.join(format!("{CEF_VERSION}.staging"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    if let Err(e) = extract(&part, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    clear_quarantine(&staging);
    fs::write(staging.join(MARKER), archive.sha1)?;
    let _ = fs::remove_dir_all(&root);
    fs::rename(&staging, &root)?;
    let _ = fs::remove_file(&part);
    remove_other_versions(base);

    installed(base).ok_or_else(|| Error::Corrupt("the installed library is missing".into()))
}

/// Removes runtimes of other CEF versions and their leftovers. The pinned one
/// and the downloads folder stay.
pub fn remove_other_versions(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    let current_part = pinned().map(|a| format!("{}.part", a.file));
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == CEF_VERSION || name == "downloads" {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
    if let Ok(entries) = fs::read_dir(base.join("downloads")) {
        for entry in entries.flatten() {
            if Some(entry.file_name().to_string_lossy().into_owned()) != current_part {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn archive_url(archive: &Archive) -> String {
    // '+' is a space in a query string and some CDNs treat it so in paths too.
    format!("{CDN_URL}/{}", archive.file.replace('+', "%2B"))
}

fn download(
    archive: &Archive,
    part: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
) -> Result<()> {
    let mut done = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if done > archive.size {
        fs::remove_file(part)?;
        done = 0;
    }
    if done == archive.size {
        return Ok(());
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent("monocode-browser-runtime")
        .build();
    let mut request = agent.get(&archive_url(archive));
    if done > 0 {
        request = request.set("Range", &format!("bytes={done}-"));
    }
    let response = request.call().map_err(|e| Error::Http(e.to_string()))?;
    let resumed = response.status() == 206;
    if !resumed {
        // The server ignored the range (or there was none): start over.
        done = 0;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)?;

    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut last_report = 0u64;
    progress(Progress::Downloading {
        done,
        total: archive.size,
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            file.flush()?;
            return Err(Error::Cancelled);
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        done += n as u64;
        if done > archive.size {
            drop(file);
            let _ = fs::remove_file(part);
            return Err(Error::Corrupt("larger than expected".into()));
        }
        if done - last_report >= 1024 * 1024 || done == archive.size {
            last_report = done;
            progress(Progress::Downloading {
                done,
                total: archive.size,
            });
        }
    }
    file.flush()?;
    if done != archive.size {
        return Err(Error::Http(format!(
            "connection closed at {done} of {} bytes",
            archive.size
        )));
    }
    Ok(())
}

fn verify(archive: &Archive, part: &Path) -> Result<()> {
    let len = fs::metadata(part)?.len();
    if len != archive.size {
        return Err(Error::Corrupt(format!("{len} bytes, expected {}", archive.size)));
    }
    let digest = sha1_file(part)?;
    if digest != archive.sha1 {
        return Err(Error::Corrupt(format!("SHA-1 {digest}")));
    }
    Ok(())
}

fn sha1_file(path: &Path) -> io::Result<String> {
    let mut hasher = sha1_smol::Sha1::new();
    let mut reader = BufReader::with_capacity(1024 * 1024, File::open(path)?);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.digest().to_string())
}

/// Where an archive entry goes inside the install folder, or `None` for
/// entries the runtime does not need (headers, the wrapper sources, cmake,
/// debug builds).
///
/// Archives are `cef_binary_<ver>_<platform>_minimal/{Release,Resources,...}`.
/// macOS keeps the framework only (its resources are inside it); Windows and
/// Linux keep `Release/*` and `Resources/*` side by side.
pub(crate) fn destination(entry: &Path) -> Option<PathBuf> {
    let mut components = entry.components();
    components.next()?; // top folder
    let section = components.next()?.as_os_str().to_str()?.to_owned();
    let rest: PathBuf = components.collect();
    if rest.as_os_str().is_empty() {
        return None;
    }
    match section.as_str() {
        "Release" if cfg!(target_os = "macos") => rest
            .starts_with("Chromium Embedded Framework.framework")
            .then_some(rest),
        "Release" | "Resources" if !cfg!(target_os = "macos") => Some(rest),
        _ => None,
    }
}

fn is_contained(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn extract(archive: &Path, into: &Path) -> Result<()> {
    let reader = BufReader::with_capacity(1024 * 1024, File::open(archive)?);
    let mut tar = tar::Archive::new(bzip2::bufread::BzDecoder::new(reader));
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(relative) = destination(&path) else {
            continue;
        };
        if !is_contained(&relative) {
            return Err(Error::UnsafeArchive(path.display().to_string()));
        }
        let target = into.join(&relative);
        match entry.header().entry_type() {
            tar::EntryType::Directory => fs::create_dir_all(&target)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                entry.unpack(&target)?;
            }
            tar::EntryType::Symlink => {
                // Framework bundles may use relative links (Versions/Current).
                let link = entry
                    .link_name()?
                    .ok_or_else(|| Error::UnsafeArchive(path.display().to_string()))?;
                if !is_contained(&link) {
                    return Err(Error::UnsafeArchive(format!(
                        "{} -> {}",
                        path.display(),
                        link.display()
                    )));
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                entry.unpack(&target)?;
            }
            // Hard links, devices and the like have no place in a runtime.
            _ => return Err(Error::UnsafeArchive(path.display().to_string())),
        }
    }
    Ok(())
}

/// The app downloads with its own HTTP client, which sets no quarantine flag;
/// clear it anyway so Gatekeeper never assesses the runtime's files.
#[cfg(target_os = "macos")]
fn clear_quarantine(dir: &Path) {
    use std::os::unix::ffi::OsStrExt;
    fn walk(path: &Path) {
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            unsafe {
                libc::removexattr(
                    c.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    libc::XATTR_NOFOLLOW,
                );
            }
        }
        if let Ok(meta) = fs::symlink_metadata(path)
            && meta.is_dir()
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                walk(&entry.path());
            }
        }
    }
    walk(dir);
}

#[cfg(not(target_os = "macos"))]
fn clear_quarantine(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_archive_matches_the_version() {
        if let Some(archive) = pinned() {
            assert!(archive.file.starts_with(&format!("cef_binary_{CEF_VERSION}_")));
            assert!(archive.file.ends_with("_minimal.tar.bz2"));
            assert_eq!(archive.sha1.len(), 40);
        }
    }

    #[test]
    fn url_escapes_plus() {
        let archive = Archive {
            file: "cef_binary_1.0+g1+chromium-2_x.tar.bz2",
            size: 1,
            sha1: "",
        };
        assert_eq!(
            archive_url(&archive),
            "https://cef-builds.spotifycdn.com/cef_binary_1.0%2Bg1%2Bchromium-2_x.tar.bz2"
        );
    }

    #[test]
    fn keeps_only_runtime_files() {
        let top = "cef_binary_x_minimal";
        let keep_framework =
            destination(Path::new(&format!("{top}/Release/Chromium Embedded Framework.framework/Resources/icudtl.dat")));
        let header = destination(Path::new(&format!("{top}/include/cef_app.h")));
        let wrapper = destination(Path::new(&format!("{top}/libcef_dll/wrapper.cc")));
        assert_eq!(header, None);
        assert_eq!(wrapper, None);
        if cfg!(target_os = "macos") {
            assert_eq!(
                keep_framework,
                Some(PathBuf::from("Chromium Embedded Framework.framework/Resources/icudtl.dat"))
            );
            assert_eq!(destination(Path::new(&format!("{top}/Release/cef_sandbox.a"))), None);
        } else {
            assert_eq!(
                destination(Path::new(&format!("{top}/Resources/locales/en-US.pak"))),
                Some(PathBuf::from("locales/en-US.pak"))
            );
        }
    }

    #[test]
    fn rejects_escaping_paths() {
        assert!(!is_contained(Path::new("../x")));
        assert!(!is_contained(Path::new("/etc/passwd")));
        assert!(is_contained(Path::new("Versions/A")));
    }

    #[test]
    fn verify_detects_damage_and_install_reports_installed() {
        let dir = tempfile::tempdir().unwrap();
        let Some(archive) = pinned() else { return };
        let part = dir.path().join("x.part");
        fs::write(&part, b"short").unwrap();
        assert!(matches!(verify(&archive, &part), Err(Error::Corrupt(_))));

        // A complete-looking install is recognised without any network.
        let root = dir.path().join(CEF_VERSION);
        let runtime = InstalledRuntime { root: root.clone() };
        fs::create_dir_all(runtime.library_path().parent().unwrap()).unwrap();
        fs::write(runtime.library_path(), b"").unwrap();
        assert_eq!(installed(dir.path()), None);
        fs::write(root.join(MARKER), archive.sha1).unwrap();
        assert_eq!(installed(dir.path()), Some(runtime));

        // Another version's leftovers are cleaned up, the pinned one stays.
        fs::create_dir_all(dir.path().join("150.0.0")).unwrap();
        remove_other_versions(dir.path());
        assert!(!dir.path().join("150.0.0").exists());
        assert!(root.exists());
    }
}
