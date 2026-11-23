//! Which of Chrome, Arc and Brave are installed, and their profiles. Only
//! folder names and the profile names in `Local State` are read here;
//! nothing of a profile's contents.

use super::{cookies, logins, SourceBrowser};
use std::path::{Path, PathBuf};

/// One profile of a browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceProfile {
    pub browser: SourceBrowser,
    /// The name the browser shows ("Work", "Person 1").
    pub name: String,
    /// The folder under the user data folder ("Default", "Profile 1").
    pub folder: String,
    pub dir: PathBuf,
    pub has_cookies: bool,
    pub has_logins: bool,
}

/// A browser with at least one profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledBrowser {
    pub browser: SourceBrowser,
    pub root: PathBuf,
    pub profiles: Vec<SourceProfile>,
}

/// The browsers installed for the user whose home is `home`, in
/// [`SourceBrowser::ALL`]'s order.
pub fn discover(home: &Path) -> Vec<InstalledBrowser> {
    SourceBrowser::ALL
        .into_iter()
        .filter_map(|browser| {
            let root = browser.user_data_dir(home);
            let profiles = profiles(browser, &root);
            (!profiles.is_empty()).then_some(InstalledBrowser {
                browser,
                root,
                profiles,
            })
        })
        .collect()
}

/// A folder name from `Local State` becomes a path segment: nothing that
/// leaves the user data folder.
fn safe_folder(folder: &str) -> bool {
    !folder.is_empty()
        && folder != "."
        && !folder.contains(['/', '\\', '\0'])
        && !folder.contains("..")
}

/// The profiles listed in `root/Local State` whose folders exist, the
/// browser's order kept; `Default` alone when there is no list.
pub fn profiles(browser: SourceBrowser, root: &Path) -> Vec<SourceProfile> {
    if !root.is_dir() {
        return Vec::new();
    }
    let listed: Vec<(String, String)> = std::fs::read(root.join("Local State"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|state| {
            let cache = state.get("profile")?.get("info_cache")?.as_object()?;
            Some(
                cache
                    .iter()
                    .map(|(folder, info)| {
                        let name = (info.get("name").and_then(|n| n.as_str()))
                            .filter(|n| !n.trim().is_empty())
                            .unwrap_or(folder);
                        (folder.clone(), name.to_owned())
                    })
                    .collect(),
            )
        })
        .unwrap_or_else(|| vec![("Default".into(), "Default".into())]);
    listed
        .into_iter()
        .filter(|(folder, _)| safe_folder(folder))
        .filter_map(|(folder, name)| {
            let dir = root.join(&folder);
            if !dir.is_dir() {
                return None;
            }
            Some(SourceProfile {
                browser,
                name,
                has_cookies: cookies::cookies_path(&dir).is_some(),
                has_logins: !logins::login_paths(&dir).is_empty(),
                folder,
                dir,
            })
        })
        .collect()
}
