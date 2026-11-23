//! Downloads: straight into `~/Downloads` under a name that clashes with
//! nothing there, without a dialog (docs/native/browser/context.md §3.3).
//! A page's next download can go into another folder instead
//! ([`crate::BrowserView::download_next_into`], K50). Progress comes back as [`BrowserEvent::Download`]. Every callback here
//! runs on CEF's UI thread, the main thread.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
};

use cef::*;

use crate::view::{BrowserEvent, PageState};

/// A download as its page last saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    /// Unique among this run's downloads.
    pub id: u32,
    /// Where the file goes; its name is the one shown.
    pub path: PathBuf,
    pub received: i64,
    /// `None` while the server has not said.
    pub total: Option<i64>,
    pub state: DownloadState,
}

impl Download {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// 0.0 to 1.0, when the size is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.received as f64 / total as f64).clamp(0., 1.))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    InProgress,
    Complete,
    Canceled,
    /// Interrupted: the network, the disk or the server gave up.
    Failed,
}

/// Stops a download in progress. A [`DownloadState::Canceled`] update follows.
pub fn cancel_download(id: u32) {
    if let Some(callback) = ACTIVE
        .with(|a| a.borrow().get(&id).map(|d| d.callback.clone()))
        .flatten()
    {
        callback.cancel();
    }
}

/// Before CEF shuts down: lets go of CEF's callbacks.
pub(crate) fn forget_all() {
    ACTIVE.with(|a| a.borrow_mut().clear());
    NEXT_INTO.with(|n| n.borrow_mut().clear());
}

/// While this lives, a page's next download goes into a folder of the
/// caller's. Dropped before that download starts, the page's downloads go
/// to `~/Downloads` again.
#[must_use]
pub struct DownloadInto {
    browser: i32,
    ticket: u64,
}

impl Drop for DownloadInto {
    fn drop(&mut self) {
        NEXT_INTO.with(|n| {
            let mut next = n.borrow_mut();
            if next
                .get(&self.browser)
                .is_some_and(|(t, _)| *t == self.ticket)
            {
                next.remove(&self.browser);
            }
        });
    }
}

/// Sends browser `browser`'s next download into `dir`, replacing an earlier
/// such wish of its.
pub(crate) fn download_into(browser: i32, dir: PathBuf) -> DownloadInto {
    thread_local!(static TICKETS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) });
    let ticket = TICKETS.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    NEXT_INTO.with(|n| n.borrow_mut().insert(browser, (ticket, dir)));
    DownloadInto { browser, ticket }
}

/// Whether a download browser `browser` starts now goes into a caller's
/// folder.
pub(crate) fn expects_download(browser: i32) -> bool {
    NEXT_INTO.with(|n| n.borrow().contains_key(&browser))
}

/// Whether we accept a page's permission prompt ourselves; else CEF's
/// default answers it. A page's second download in a row asks
/// for "multiple downloads", which an Alloy page would ignore, so the
/// download never starts. An agent's input does not count as the user's
/// gesture that lifts the limit, so its downloads are allowed while it
/// waits for one ([`download_into`]).
pub(crate) fn accept_permission(requested: u32, expecting: bool) -> bool {
    let multiple = sys::cef_permission_request_types_t::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS;
    expecting && requested == multiple as u32
}

/// The folder browser `browser`'s download starting now goes into, taken.
fn take_into(browser: i32) -> Option<PathBuf> {
    NEXT_INTO
        .with(|n| n.borrow_mut().remove(&browser))
        .map(|(_, dir)| dir)
}

struct Active {
    path: PathBuf,
    /// The latest one CEF handed over; it cancels the download.
    callback: Option<DownloadItemCallback>,
}

thread_local! {
    /// Downloads between their start and their end, by id. Their paths are
    /// taken even while nothing exists there yet.
    static ACTIVE: RefCell<HashMap<u32, Active>> = RefCell::new(HashMap::new());
    /// Where a browser's next download goes, by browser id, with the ticket
    /// of the [`DownloadInto`] that asked.
    static NEXT_INTO: RefCell<HashMap<i32, (u64, PathBuf)>> = RefCell::new(HashMap::new());
}

/// `~/Downloads`, made if it is missing.
fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = home.join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A file name a server suggested, safe to use in a folder: no folders of
/// its own, no hidden or empty names.
pub(crate) fn safe_name(suggested: &str) -> String {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let name = name.trim().trim_start_matches('.').trim();
    if name.is_empty() {
        "download".to_owned()
    } else {
        name.to_owned()
    }
}

/// `name`, or `name (1).ext`, `name (2).ext`, … : the first that `taken`
/// says is free. A double extension such as `.tar.gz` stays together.
pub(crate) fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let lower = name.to_ascii_lowercase();
    let split = ["tar.gz", "tar.bz2", "tar.xz", "tar.zst"]
        .iter()
        .find_map(|ext| {
            lower
                .strip_suffix(ext)
                .filter(|stem| stem.ends_with('.') && stem.len() > 1)
                .map(|stem| stem.len() - 1)
        })
        .or_else(|| name.rfind('.').filter(|ix| *ix > 0));
    let (stem, ext) = match split {
        Some(ix) => name.split_at(ix),
        None => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !taken(candidate))
        .expect("some name is free")
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let reserved = |path: &Path| ACTIVE.with(|a| a.borrow().values().any(|d| d.path == path));
    dir.join(unique_name(name, |candidate| {
        let path = dir.join(candidate);
        path.exists() || reserved(&path)
    }))
}

fn snapshot(item: &DownloadItem, path: PathBuf) -> Download {
    let state = if item.is_complete() != 0 {
        DownloadState::Complete
    } else if item.is_canceled() != 0 {
        DownloadState::Canceled
    } else if item.is_interrupted() != 0 {
        DownloadState::Failed
    } else {
        DownloadState::InProgress
    };
    let total = item.total_bytes();
    Download {
        id: item.id(),
        path,
        received: item.received_bytes().max(0),
        total: (total > 0).then_some(total),
        state,
    }
}

wrap_download_handler! {
    pub(crate) struct PageDownloadHandler {
        state: PageState,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            // The default the bindings return (0) would refuse every one.
            1
        }

        fn on_before_download(
            &self,
            browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(item), Some(callback)) = (download_item, callback) else {
                return 0;
            };
            let suggested = suggested_name.map(|s| s.to_string()).unwrap_or_default();
            let dir = (browser.map(|b| b.identifier()).and_then(take_into))
                .filter(|dir| std::fs::create_dir_all(dir).is_ok())
                .unwrap_or_else(downloads_dir);
            let path = unique_path(&dir, &safe_name(&suggested));
            let id = item.id();
            ACTIVE.with(|a| {
                a.borrow_mut().insert(id, Active { path: path.clone(), callback: None })
            });
            callback.cont(Some(&path.to_string_lossy().as_ref().into()), 0);
            self.state.send(BrowserEvent::Download(Download {
                id,
                path,
                received: 0,
                total: None,
                state: DownloadState::InProgress,
            }));
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            callback: Option<&mut DownloadItemCallback>,
        ) {
            let Some(item) = download_item else { return };
            let id = item.id();
            // Before its path is chosen a download is not reported.
            let Some(path) = ACTIVE.with(|a| {
                let mut active = a.borrow_mut();
                let entry = active.get_mut(&id)?;
                if let Some(callback) = callback {
                    entry.callback = Some(callback.clone());
                }
                Some(entry.path.clone())
            }) else {
                return;
            };
            let download = snapshot(item, path);
            if download.state != DownloadState::InProgress {
                ACTIVE.with(|a| a.borrow_mut().remove(&id));
            }
            self.state.send(BrowserEvent::Download(download));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_waiting_for_a_download_may_download_again() {
        let multiple =
            sys::cef_permission_request_types_t::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS as u32;
        let camera = sys::cef_permission_request_types_t::CEF_PERMISSION_TYPE_CAMERA_STREAM as u32;
        assert!(accept_permission(multiple, true));
        // The user's own downloads, and every other prompt, as CEF decides.
        assert!(!accept_permission(multiple, false));
        assert!(!accept_permission(camera, true));
        assert!(!accept_permission(multiple | camera, true));
        let _into = download_into(7, PathBuf::from("/tmp/x"));
        assert!(expects_download(7));
        drop(_into);
        assert!(!expects_download(7));
    }

    #[test]
    fn a_download_takes_the_first_free_name() {
        let taken = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        assert_eq!(unique_name("a.pdf", taken(&[])), "a.pdf");
        assert_eq!(unique_name("a.pdf", taken(&["a.pdf"])), "a (1).pdf");
        assert_eq!(
            unique_name("a.pdf", taken(&["a.pdf", "a (1).pdf"])),
            "a (2).pdf"
        );
        assert_eq!(unique_name("README", taken(&["README"])), "README (1)");
        assert_eq!(
            unique_name("src.tar.gz", taken(&["src.tar.gz"])),
            "src (1).tar.gz"
        );
        assert_eq!(
            unique_name("v1.2.zip", taken(&["v1.2.zip"])),
            "v1.2 (1).zip"
        );
    }

    #[test]
    fn a_suggested_name_stays_inside_the_folder() {
        assert_eq!(safe_name("report.pdf"), "report.pdf");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("a\\b.txt"), "b.txt");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name("  "), "download");
        assert_eq!(safe_name(""), "download");
        assert_eq!(safe_name("a:b\n.txt"), "a_b_.txt");
    }

    #[test]
    fn reserved_and_existing_paths_are_skipped() {
        let dir =
            std::env::temp_dir().join(format!("monocode-cef-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), "").unwrap();
        assert_eq!(unique_path(&dir, "f.txt"), dir.join("f (1).txt"));
        ACTIVE.with(|a| {
            a.borrow_mut().insert(
                7,
                Active {
                    path: dir.join("f (1).txt"),
                    callback: None,
                },
            )
        });
        assert_eq!(unique_path(&dir, "f.txt"), dir.join("f (2).txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_for_the_next_download_is_taken_once_or_given_back() {
        let wish = download_into(4, "/p/a".into());
        assert_eq!(take_into(5), None);
        assert_eq!(take_into(4), Some("/p/a".into()));
        assert_eq!(take_into(4), None);
        drop(wish);
        // Dropped before a download came: gone.
        drop(download_into(4, "/p/b".into()));
        assert_eq!(take_into(4), None);
        // A later wish replaces an earlier one; the earlier's drop leaves it.
        let first = download_into(4, "/p/c".into());
        let second = download_into(4, "/p/d".into());
        drop(first);
        assert_eq!(take_into(4), Some("/p/d".into()));
        drop(second);
    }
}
