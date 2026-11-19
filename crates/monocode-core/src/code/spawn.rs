//! One process to start, fully resolved: what a run configuration becomes,
//! what a terminal runs and what a debug session is derived from.
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spawn {
    /// Shown on the terminal tab and in lists of running work.
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the checkout's own environment (`Toolchain::environment`).
    pub env: Vec<(String, String)>,
}
