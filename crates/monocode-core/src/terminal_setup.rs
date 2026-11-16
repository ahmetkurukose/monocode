//! What a project's terminals start with: environment variables, an
//! environment file, a Python virtual environment and commands typed at the
//! first prompt. The same environment can be given to the project's agents.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Venv {
    #[default]
    Off,
    /// `.venv` or `venv`, in the terminal's folder or the project's.
    Auto,
    /// A folder, absolute or relative to the terminal's folder or the project's.
    Path(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct TerminalSetup {
    /// `KEY=VALUE` lines as they were typed.
    pub environment: String,
    /// A file of `KEY=VALUE` lines read each time a terminal starts, so
    /// that secrets can stay out of this app's own files.
    pub env_file: String,
    pub venv: Venv,
    /// One command per line, run at the first prompt.
    pub commands: String,
    /// Agents of the project get the environment and the virtual
    /// environment too. Commands are for terminals only.
    pub agents: bool,
}

impl Default for TerminalSetup {
    fn default() -> Self {
        Self {
            environment: String::new(),
            env_file: String::new(),
            venv: Venv::Off,
            commands: String::new(),
            agents: true,
        }
    }
}

/// What one terminal is started with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub environment: Vec<(String, String)>,
    pub venv: Option<PathBuf>,
    pub commands: Vec<String>,
    /// What was asked for and not found; shown in the terminal.
    pub notes: Vec<String>,
}

impl Launch {
    pub fn is_empty(&self) -> bool {
        self.environment.is_empty()
            && self.venv.is_none()
            && self.commands.is_empty()
            && self.notes.is_empty()
    }

    /// The lines typed at the first prompt of a shell called `shell`
    /// (`zsh`, `fish`, ...): the virtual environment first, since the
    /// commands after it usually need it.
    pub fn startup_lines(&self, shell: &str) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(venv) = &self.venv {
            lines.extend(activate_line(venv, shell));
        }
        lines.extend(self.commands.iter().cloned());
        lines
    }

    /// The environment for a process that is not a shell: the variables,
    /// and the virtual environment the way its `activate` sets it up.
    pub fn process_environment(&self, search_path: &str) -> Vec<(String, String)> {
        let mut environment = self.environment.clone();
        if let Some(venv) = &self.venv {
            let base = environment
                .iter()
                .rev()
                .find(|(key, _)| key == "PATH")
                .map_or(search_path, |(_, value)| value.as_str())
                .to_owned();
            environment.retain(|(key, _)| key != "PATH" && key != "VIRTUAL_ENV");
            let bin = venv.join(bin_folder());
            let separator = if cfg!(windows) { ";" } else { ":" };
            let path = match base.is_empty() {
                true => bin.to_string_lossy().into_owned(),
                false => format!("{}{separator}{base}", bin.to_string_lossy()),
            };
            environment.push(("VIRTUAL_ENV".into(), venv.to_string_lossy().into_owned()));
            environment.push(("PATH".into(), path));
        }
        environment
    }
}

fn bin_folder() -> &'static str {
    if cfg!(windows) {
        "Scripts"
    } else {
        "bin"
    }
}

/// The command that activates `venv` in a shell called `shell`.
fn activate_line(venv: &Path, shell: &str) -> Option<String> {
    let shell = Path::new(shell)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
        .trim_start_matches('-')
        .to_ascii_lowercase();
    let bin = venv.join(bin_folder());
    let script = |name: &str| bin.join(name).to_string_lossy().into_owned();
    Some(match shell.as_str() {
        "fish" => format!("source {}", quote(&script("activate.fish"))),
        "csh" | "tcsh" => format!("source {}", quote(&script("activate.csh"))),
        "nu" => format!("overlay use {}", quote(&script("activate.nu"))),
        "pwsh" | "powershell" => format!("& {}", quote(&script("Activate.ps1"))),
        "cmd" => format!("\"{}\"", script("activate.bat")),
        _ => format!("source {}", quote(&script("activate"))),
    })
}

/// Single quotes, for a path typed at a prompt.
fn quote(text: &str) -> String {
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c));
    if plain && !text.is_empty() {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// `KEY=VALUE` lines: blank lines and `#` comments are skipped, a leading
/// `export` is allowed, and a value may sit in single or double quotes.
/// A later line for the same key wins.
pub fn parse_environment(text: &str) -> Vec<(String, String)> {
    let mut variables: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some((key, value)) = parse_line(line) {
            variables.retain(|(existing, _)| *existing != key);
            variables.push((key, value));
        }
    }
    variables
}

/// The numbers (from 1) of the lines that are neither a variable, a
/// comment nor blank.
pub fn invalid_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#') && parse_line(line).is_none()
        })
        .map(|(index, _)| index + 1)
        .collect()
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = match line.strip_prefix("export") {
        Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
        _ => line,
    };
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let mut letters = key.chars();
    let first = letters.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !letters.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some((key.to_owned(), unquote(value.trim())))
}

fn unquote(value: &str) -> String {
    for mark in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(mark) && value.ends_with(mark) {
            let inner = &value[1..value.len() - 1];
            return match mark {
                '"' => inner.replace("\\\"", "\"").replace("\\n", "\n"),
                _ => inner.to_owned(),
            };
        }
    }
    // Outside quotes a comment may follow the value.
    match value.find(" #") {
        Some(end) => value[..end].trim_end().to_owned(),
        None => value.to_owned(),
    }
}

fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match path.strip_prefix("~/") {
        Some(rest) => home().map_or_else(
            || PathBuf::from(path),
            |home| PathBuf::from(home).join(rest),
        ),
        None if path == "~" => home().map_or_else(|| PathBuf::from(path), PathBuf::from),
        None => PathBuf::from(path),
    }
}

/// A path as written in the setup: absolute, or under the terminal's
/// folder (a worktree has its own files), or under the project.
fn candidates(path: &str, folder: &Path, project: &Path) -> Vec<PathBuf> {
    let path = expand_home(path.trim());
    if path.is_absolute() {
        return vec![path];
    }
    let mut found = vec![folder.join(&path)];
    if folder != project {
        found.push(project.join(&path));
    }
    found
}

fn is_venv(folder: &Path) -> bool {
    let bin = folder.join(bin_folder());
    bin.join("activate").is_file() || bin.join("activate.bat").is_file()
}

/// The virtual environment `venv` names for a terminal in `folder`.
pub fn find_venv(venv: &Venv, folder: &Path, project: &Path) -> Option<PathBuf> {
    match venv {
        Venv::Off => None,
        Venv::Auto => {
            let found: Vec<PathBuf> = [".venv", "venv"]
                .iter()
                .flat_map(|name| candidates(name, folder, project))
                .filter(|path| is_venv(path))
                .collect();
            // The terminal's own folder first, whatever the name.
            let own = found.iter().find(|path| path.starts_with(folder)).cloned();
            own.or_else(|| found.into_iter().next())
        }
        Venv::Path(path) if path.trim().is_empty() => None,
        Venv::Path(path) => candidates(path, folder, project)
            .into_iter()
            .find(|path| is_venv(path)),
    }
}

impl TerminalSetup {
    /// Nothing is asked for: the project needs no entry.
    pub fn is_empty(&self) -> bool {
        self.environment.trim().is_empty()
            && self.env_file.trim().is_empty()
            && self.commands.trim().is_empty()
            && match &self.venv {
                Venv::Off => true,
                Venv::Auto => false,
                Venv::Path(path) => path.trim().is_empty(),
            }
    }

    /// Trimmed, with an empty custom path meaning no virtual environment.
    pub fn normalized(mut self) -> Self {
        self.environment = self.environment.trim().to_owned();
        self.env_file = self.env_file.trim().to_owned();
        self.commands = self.commands.trim().to_owned();
        self.venv = match self.venv {
            Venv::Path(path) if path.trim().is_empty() => Venv::Off,
            Venv::Path(path) => Venv::Path(path.trim().to_owned()),
            other => other,
        };
        self
    }

    /// What a terminal opened in `folder` of `project` starts with. Files
    /// are read now: an edited environment file applies to the next
    /// terminal without saving the setup again.
    pub fn launch(&self, folder: &Path, project: &Path) -> Launch {
        let mut launch = Launch::default();
        let file = self.env_file.trim();
        if !file.is_empty() {
            let read = candidates(file, folder, project)
                .into_iter()
                .find_map(|path| std::fs::read_to_string(path).ok());
            match read {
                Some(text) => launch.environment = parse_environment(&text),
                None => launch
                    .notes
                    .push(format!("Environment file not found: {file}")),
            }
        }
        // What is typed in the setup wins over the file.
        for (key, value) in parse_environment(&self.environment) {
            launch.environment.retain(|(existing, _)| *existing != key);
            launch.environment.push((key, value));
        }
        launch.venv = find_venv(&self.venv, folder, project);
        if launch.venv.is_none() {
            match &self.venv {
                Venv::Off => {}
                Venv::Auto => launch
                    .notes
                    .push("No Python virtual environment (.venv or venv) found".into()),
                Venv::Path(path) => launch
                    .notes
                    .push(format!("Python virtual environment not found: {path}")),
            }
        }
        launch.commands = (self.commands.lines())
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect();
        launch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn venv(root: &Path, name: &str) -> PathBuf {
        let folder = root.join(name);
        std::fs::create_dir_all(folder.join(bin_folder())).unwrap();
        std::fs::write(folder.join(bin_folder()).join("activate"), "").unwrap();
        folder
    }

    #[test]
    fn environment_lines_allow_export_quotes_and_comments_and_the_last_one_wins() {
        let text = "# database\nexport DB_URL=\"postgres://u:p@h/db?x=1\"\n\nNAME='two words'\n\
                    PLAIN=a=b # trailing\nEMPTY=\nNAME=again\n1BAD=x\nnot a variable\n";
        assert_eq!(
            parse_environment(text),
            vec![
                ("DB_URL".to_owned(), "postgres://u:p@h/db?x=1".to_owned()),
                ("PLAIN".to_owned(), "a=b".to_owned()),
                ("EMPTY".to_owned(), String::new()),
                ("NAME".to_owned(), "again".to_owned()),
            ]
        );
        assert_eq!(invalid_lines(text), vec![8, 9]);
        assert!(invalid_lines("A=1\n# note\n\nexport B=2").is_empty());
    }

    #[test]
    fn an_empty_setup_asks_for_nothing_and_normalizes_a_blank_path_away() {
        assert!(TerminalSetup::default().is_empty());
        let blank = TerminalSetup {
            venv: Venv::Path("  ".into()),
            environment: " \n".into(),
            ..Default::default()
        };
        assert!(blank.is_empty());
        assert_eq!(blank.normalized().venv, Venv::Off);
        let auto = TerminalSetup {
            venv: Venv::Auto,
            ..Default::default()
        };
        assert!(!auto.is_empty());
        let saved = serde_json::to_string(&auto).unwrap();
        assert_eq!(serde_json::from_str::<TerminalSetup>(&saved).unwrap(), auto);
        // Agents are included unless the setup says otherwise.
        assert!(serde_json::from_str::<TerminalSetup>("{}").unwrap().agents);
    }

    #[test]
    fn the_file_is_read_at_launch_and_typed_variables_win_over_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "TOKEN=from-file\nONLY_FILE=1\n").unwrap();
        let setup = TerminalSetup {
            environment: "TOKEN=typed\nMODE=dev".into(),
            env_file: ".env".into(),
            commands: "nvm use\n# not run\n\n  make setup  ".into(),
            ..Default::default()
        };
        let launch = setup.launch(root.path(), root.path());
        assert_eq!(
            launch.environment,
            vec![
                ("ONLY_FILE".to_owned(), "1".to_owned()),
                ("TOKEN".to_owned(), "typed".to_owned()),
                ("MODE".to_owned(), "dev".to_owned()),
            ]
        );
        assert_eq!(launch.commands, vec!["nvm use", "make setup"]);
        assert!(launch.notes.is_empty());
        // Edited on disk: the next terminal has the new value.
        std::fs::write(root.path().join(".env"), "ONLY_FILE=2\n").unwrap();
        let again = setup.launch(root.path(), root.path());
        assert_eq!(again.environment[0], ("ONLY_FILE".into(), "2".into()));

        let missing = TerminalSetup {
            env_file: "nowhere.env".into(),
            ..Default::default()
        };
        let launch = missing.launch(root.path(), root.path());
        assert!(launch.environment.is_empty());
        assert_eq!(
            launch.notes,
            vec!["Environment file not found: nowhere.env"]
        );
    }

    #[test]
    fn a_worktree_uses_its_own_virtual_environment_and_else_the_projects() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let worktree = root.path().join("worktrees/feature");
        std::fs::create_dir_all(&worktree).unwrap();
        let shared = venv(&project, ".venv");
        std::fs::write(project.join(".env"), "FROM=project\n").unwrap();

        assert_eq!(find_venv(&Venv::Off, &worktree, &project), None);
        assert_eq!(
            find_venv(&Venv::Auto, &worktree, &project),
            Some(shared.clone())
        );
        // Its own, even under the second name looked for.
        let own = venv(&worktree, "venv");
        assert_eq!(find_venv(&Venv::Auto, &worktree, &project), Some(own));
        assert_eq!(
            find_venv(&Venv::Auto, &project, &project),
            Some(shared.clone())
        );
        assert_eq!(
            find_venv(&Venv::Path(".venv".into()), &worktree, &project),
            Some(shared.clone())
        );
        // A folder that is not a virtual environment is not one.
        std::fs::create_dir_all(project.join("env")).unwrap();
        let setup = TerminalSetup {
            venv: Venv::Path("env".into()),
            env_file: ".env".into(),
            ..Default::default()
        };
        let launch = setup.launch(&worktree, &project);
        assert_eq!(launch.venv, None);
        assert_eq!(
            launch.notes,
            vec!["Python virtual environment not found: env"]
        );
        // The file comes from the project when the worktree has none.
        assert_eq!(
            launch.environment,
            vec![("FROM".to_owned(), "project".to_owned())]
        );
    }

    #[test]
    fn the_virtual_environment_is_activated_before_the_commands_in_the_shells_own_way() {
        let launch = Launch {
            venv: Some(PathBuf::from("/p/my project/.venv")),
            commands: vec!["make setup".into()],
            ..Default::default()
        };
        let bin = bin_folder();
        assert_eq!(
            launch.startup_lines("/bin/zsh"),
            vec![
                format!("source '/p/my project/.venv/{bin}/activate'"),
                "make setup".to_owned()
            ]
        );
        assert_eq!(
            launch.startup_lines("-fish")[0],
            format!("source '/p/my project/.venv/{bin}/activate.fish'")
        );
        let plain = Launch {
            venv: Some(PathBuf::from("/p/.venv")),
            ..Default::default()
        };
        assert_eq!(
            plain.startup_lines("bash"),
            vec![format!("source /p/.venv/{bin}/activate")]
        );
        assert!(Launch::default().startup_lines("zsh").is_empty());
    }

    #[test]
    fn a_process_gets_the_virtual_environment_as_activate_would_set_it() {
        let bin = bin_folder();
        let separator = if cfg!(windows) { ";" } else { ":" };
        let launch = Launch {
            environment: vec![("MODE".into(), "dev".into())],
            venv: Some(PathBuf::from("/p/.venv")),
            ..Default::default()
        };
        assert_eq!(
            launch.process_environment("/usr/bin"),
            vec![
                ("MODE".to_owned(), "dev".to_owned()),
                ("VIRTUAL_ENV".to_owned(), "/p/.venv".to_owned()),
                (
                    "PATH".to_owned(),
                    format!("/p/.venv/{bin}{separator}/usr/bin")
                ),
            ]
        );
        // A PATH of the project's own is kept behind the environment's.
        let own = Launch {
            environment: vec![("PATH".into(), "/opt/tools".into())],
            venv: Some(PathBuf::from("/p/.venv")),
            ..Default::default()
        };
        assert_eq!(
            own.process_environment("/usr/bin").last().unwrap().1,
            format!("/p/.venv/{bin}{separator}/opt/tools")
        );
        let none = Launch {
            environment: vec![("MODE".into(), "dev".into())],
            ..Default::default()
        };
        assert_eq!(
            none.process_environment("/usr/bin"),
            vec![("MODE".to_owned(), "dev".to_owned())]
        );
    }
}
