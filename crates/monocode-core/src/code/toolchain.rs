//! Where a checkout's tools are: its Python interpreter, and the language
//! servers and debug adapters installed for it. Nothing is ever downloaded;
//! a tool that is not found is reported by name so the user can install it.
use crate::terminal_setup::{find_venv, TerminalSetup, Venv};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Toolchain {
    /// The checkout's virtual environment, if it has one.
    pub venv: Option<PathBuf>,
    /// The `PATH` tools are looked up in, the venv's `bin` first.
    pub search_path: String,
    /// The project's terminal environment, `PATH` included, for a child process.
    pub environment: Vec<(String, String)>,
}

fn bin_folder() -> &'static str {
    if cfg!(windows) {
        "Scripts"
    } else {
        "bin"
    }
}

fn executable(folder: &Path, name: &str) -> Option<PathBuf> {
    let plain = folder.join(name);
    if plain.is_file() {
        return Some(plain);
    }
    if cfg!(windows) {
        for extension in ["exe", "cmd", "bat"] {
            let path = folder.join(format!("{name}.{extension}"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

impl Toolchain {
    /// The toolchain of `folder`, a checkout of `project`. The project's
    /// terminal setup names the venv; with none named, `.venv` and `venv`
    /// are still looked for, since an editor needs the interpreter even when
    /// terminals do not activate it.
    pub fn for_folder(setup: &TerminalSetup, folder: &Path, project: &Path) -> Self {
        Self::with_search_path(setup, folder, project, &crate::scm::terminal_search_path())
    }

    pub fn with_search_path(
        setup: &TerminalSetup,
        folder: &Path,
        project: &Path,
        search_path: &str,
    ) -> Self {
        let launch = setup.launch(folder, project);
        let venv = launch
            .venv
            .clone()
            .or_else(|| find_venv(&Venv::Auto, folder, project));
        let mut environment = launch.process_environment(search_path);
        let mut search_path = environment
            .iter()
            .find(|(key, _)| key == "PATH")
            .map_or_else(|| search_path.to_string(), |(_, value)| value.clone());
        if let Some(venv) = &venv {
            let bin = venv.join(bin_folder());
            let already = std::env::split_paths(&search_path)
                .next()
                .is_some_and(|first| first == bin);
            if !already {
                let mut parts = vec![bin];
                parts.extend(std::env::split_paths(&search_path));
                if let Ok(joined) = std::env::join_paths(parts) {
                    search_path = joined.to_string_lossy().into_owned();
                }
            }
            if !environment.iter().any(|(key, _)| key == "VIRTUAL_ENV") {
                environment.push(("VIRTUAL_ENV".into(), venv.to_string_lossy().into_owned()));
            }
        }
        environment.retain(|(key, _)| key != "PATH");
        environment.push(("PATH".into(), search_path.clone()));
        Self {
            venv,
            search_path,
            environment,
        }
    }

    /// The interpreter: the venv's, else the first `python3` or `python` on the path.
    pub fn python(&self) -> Option<PathBuf> {
        ["python", "python3"]
            .iter()
            .find_map(|name| {
                self.venv
                    .as_ref()
                    .and_then(|venv| executable(&venv.join(bin_folder()), name))
            })
            .or_else(|| self.tool(None, "python3"))
            .or_else(|| self.tool(None, "python"))
    }

    /// A tool by name: the path the user set, then the venv, then the path.
    pub fn tool(&self, explicit: Option<&Path>, name: &str) -> Option<PathBuf> {
        if let Some(path) = explicit {
            return path.is_file().then(|| path.to_path_buf());
        }
        std::env::split_paths(&self.search_path).find_map(|folder| executable(&folder, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn the_venv_comes_before_the_path() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let elsewhere = root.path().join("elsewhere");
        touch(&project.join(".venv/bin/activate"));
        touch(&project.join(".venv/bin/python"));
        touch(&project.join(".venv/bin/ruff"));
        touch(&elsewhere.join("ruff"));
        touch(&elsewhere.join("gopls"));
        let toolchain = Toolchain::with_search_path(
            &TerminalSetup::default(),
            &project,
            &project,
            &elsewhere.to_string_lossy(),
        );
        assert_eq!(toolchain.venv, Some(project.join(".venv")));
        assert_eq!(toolchain.python(), Some(project.join(".venv/bin/python")));
        assert_eq!(
            toolchain.tool(None, "ruff"),
            Some(project.join(".venv/bin/ruff"))
        );
        assert_eq!(toolchain.tool(None, "gopls"), Some(elsewhere.join("gopls")));
        assert_eq!(toolchain.tool(None, "absent"), None);
        assert_eq!(
            toolchain.tool(Some(&elsewhere.join("ruff")), "ruff"),
            Some(elsewhere.join("ruff"))
        );
        assert_eq!(
            toolchain.tool(Some(&elsewhere.join("absent")), "ruff"),
            None
        );
        assert!(toolchain
            .environment
            .iter()
            .any(|(key, value)| key == "PATH" && value == &toolchain.search_path));
    }

    #[test]
    fn without_a_venv_the_path_is_used() {
        let root = tempfile::tempdir().unwrap();
        touch(&root.path().join("bin/python3"));
        let toolchain = Toolchain::with_search_path(
            &TerminalSetup::default(),
            root.path(),
            root.path(),
            &root.path().join("bin").to_string_lossy(),
        );
        assert_eq!(toolchain.venv, None);
        assert_eq!(toolchain.python(), Some(root.path().join("bin/python3")));
    }
}
