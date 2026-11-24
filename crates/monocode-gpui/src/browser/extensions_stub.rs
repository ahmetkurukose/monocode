//! Windows and Linux get the browser in phase 6 (context.md §12); the
//! extension layer with it. Until then nothing is installed or runs.
#![allow(dead_code)]

use std::path::PathBuf;

pub(crate) type ProfileKey = Option<String>;

pub(crate) fn app_loads(_: &str) {}

pub(crate) fn profile_key(_: &super::monocode_cef::Profile) -> Option<ProfileKey> {
    None
}

#[derive(Clone, Debug)]
pub(crate) struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub enabled: bool,
    pub warnings: Vec<String>,
    pub unsupported: Vec<String>,
    pub held: Option<monocode_core::browser::extensions::registry::Held>,
    pub icon: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Work {
    #[default]
    Idle,
    Busy(String),
    Done(String),
    Failed(String),
}

pub(crate) fn init(_: Option<PathBuf>, _: &mut gpui::App) {}
pub(crate) fn password_manager_enabled() -> bool {
    false
}
pub(crate) fn listings() -> Vec<Listing> {
    Vec::new()
}
pub(crate) fn work() -> Work {
    Work::Idle
}
pub(crate) fn install(_: &str) -> Result<(), String> {
    Err("Extensions are not available on this platform yet".into())
}
pub(crate) fn set_enabled(_: &str, _: bool) {}
pub(crate) fn uninstall(_: &str) {}
pub(crate) fn approve_update(_: &str) {}
pub(crate) fn check_updates(_: bool) {}

pub(crate) mod toolbar {
    pub(crate) fn buttons(
        _: &super::super::monocode_cef::Profile,
        _: i64,
    ) -> Vec<gpui::AnyElement> {
        Vec::new()
    }
}
