//! Browser profiles (docs/native/browser/context.md K15).
//!
//! Every page shares the default profile, CEF's global request context at
//! `cache_root/Profiles/default`, unless its project asks for an isolated
//! one: then the page gets a request context of its own at
//! `cache_root/Profiles/<id>`, with its own cookies, storage and cache.
//! Contexts are created once and reused; Chromium must never see two
//! contexts on the same folder.
//!
//! An agent may also open a page in a throwaway profile (K22): a request
//! context with no folder, kept in memory until [`forget_profile`].

use std::{cell::RefCell, collections::HashMap, path::PathBuf};

/// Which profile a page browses with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    #[default]
    Default,
    /// A profile of its own, named by [`profile_id`].
    Isolated(String),
    /// A throwaway profile in memory, by a name unique for the app's run.
    /// Nothing of it reaches the disk.
    Ephemeral(String),
}

impl Profile {
    /// The isolated profile of the project at `path`.
    pub fn for_project(path: &str) -> Self {
        Profile::Isolated(profile_id(path))
    }
}

/// A stable folder name for a project's profile: the folder's name, for
/// people looking in the cache, and a hash of the whole path, so that two
/// projects of the same name never share one.
pub fn profile_id(path: &str) -> String {
    let path = path.trim_end_matches(['/', '\\']);
    let name: String = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    let name = name.trim_matches('-');
    // FNV-1a: std's hasher may change between Rust releases, and the id
    // names a folder that must outlive them.
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    if name.is_empty() {
        format!("project-{hash:016x}")
    } else {
        format!("{name}-{hash:016x}")
    }
}

thread_local! {
    /// `cache_root/Profiles`, set when CEF starts.
    static PROFILES: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static CONTEXTS: RefCell<HashMap<String, cef::RequestContext>> = RefCell::new(HashMap::new());
}

pub(crate) fn set_root(profiles: PathBuf) {
    PROFILES.with(|p| *p.borrow_mut() = Some(profiles));
}

/// Before CEF shuts down: lets go of the request contexts.
pub(crate) fn forget_all() {
    CONTEXTS.with(|c| c.borrow_mut().clear());
}

/// The request context a page of `profile` is created with. `None` is the
/// global context, which is the default profile.
pub(crate) fn request_context(profile: &Profile) -> Option<cef::RequestContext> {
    let key = context_key(profile)?;
    if let Some(context) = CONTEXTS.with(|c| c.borrow().get(&key).cloned()) {
        return Some(context);
    }
    let id = match profile {
        Profile::Isolated(id) => id,
        _ => {
            // An empty cache path keeps everything in memory.
            let settings = cef::RequestContextSettings::default();
            let context = cef::request_context_create_context(Some(&settings), None)?;
            CONTEXTS.with(|c| c.borrow_mut().insert(key, context.clone()));
            return Some(context);
        }
    };
    let dir = PROFILES.with(|p| p.borrow().clone())?.join(id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!(
            "browser: could not create the profile {}: {e}",
            dir.display()
        );
        return None;
    }
    let settings = cef::RequestContextSettings {
        cache_path: dir.to_string_lossy().as_ref().into(),
        persist_session_cookies: 1,
        ..Default::default()
    };
    let context = cef::request_context_create_context(Some(&settings), None)?;
    CONTEXTS.with(|c| c.borrow_mut().insert(key, context.clone()));
    Some(context)
}

/// Where `CONTEXTS` keeps a profile's context; a throwaway name never meets
/// a folder's. `None` for the default profile, which is CEF's global one.
fn context_key(profile: &Profile) -> Option<String> {
    match profile {
        Profile::Default => None,
        Profile::Isolated(id) => Some(id.clone()),
        Profile::Ephemeral(id) => Some(format!("ephemeral:{id}")),
    }
}

/// Lets go of a throwaway profile once its page closed; its context ends
/// with the last page holding it. Other profiles stay.
pub fn forget_profile(profile: &Profile) {
    if let (Profile::Ephemeral(_), Some(key)) = (profile, context_key(profile)) {
        CONTEXTS.with(|c| c.borrow_mut().remove(&key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_projects_profile_id_is_stable_readable_and_unique() {
        let id = profile_id("/Users/me/Dev/Mono Code");
        assert_eq!(id, profile_id("/Users/me/Dev/Mono Code/"));
        assert!(id.starts_with("mono-code-"), "{id}");
        assert_eq!(id.len(), "mono-code-".len() + 16);
        assert_ne!(id, profile_id("/Users/other/Mono Code"));
        // Pinned: the id names a folder on disk.
        assert_eq!(profile_id("/p"), "p-07d65607b49cbc4a");
        assert!(profile_id("/").starts_with("project-"));
        assert!(
            profile_id("/x/../é")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );
    }

    #[test]
    fn a_throwaway_profile_never_shares_a_folders_context() {
        let id = profile_id("/p");
        assert_eq!(
            context_key(&Profile::Isolated(id.clone())),
            Some(id.clone())
        );
        assert_ne!(
            context_key(&Profile::Ephemeral(id.clone())),
            Some(id.clone())
        );
        assert_eq!(context_key(&Profile::Default), None);
    }
}
