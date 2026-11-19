//! Which language servers serve which language: the executables to look for,
//! how to start them, what marks a project root for them and the settings
//! they are given. Nothing here starts anything; the manager reads it.
use crate::code::toolchain::Toolchain;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    TypeScript,
    TypeScriptReact,
    JavaScript,
    JavaScriptReact,
    Go,
    Rust,
}

impl Language {
    /// The language of a file, from its extension alone.
    pub fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "py" | "pyi" => Self::Python,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::TypeScriptReact,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::JavaScriptReact,
            "go" => Self::Go,
            "rs" => Self::Rust,
            _ => return None,
        })
    }

    /// The protocol's name for the language.
    pub fn id(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::TypeScriptReact => "typescriptreact",
            Self::JavaScript => "javascript",
            Self::JavaScriptReact => "javascriptreact",
            Self::Go => "go",
            Self::Rust => "rust",
        }
    }
}

/// What a server is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Everything: diagnostics, navigation, hover, completion.
    Insight,
    /// Its diagnostics only; it is never asked anything else.
    DiagnosticsOnly,
}

/// One way to start a server: the first whose executable is found is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerCommand {
    pub executable: &'static str,
    pub args: &'static [&'static str],
}

/// What settings are worked out from.
pub struct SettingsContext<'a> {
    pub root: &'a Path,
    pub toolchain: &'a Toolchain,
    pub strictness: Strictness,
}

/// How much the servers say about a project's code. A type checker's full
/// set is written for annotated code: on a codebase without annotations it
/// marks nearly every line, which hides the few marks that matter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Strictness {
    /// What is wrong whatever the style: syntax, names that do not exist,
    /// imports that do not resolve, unused imports and variables.
    #[default]
    Essential,
    /// Also what the type checker can prove wrong from what it infers.
    Standard,
    /// Everything the servers report by default.
    Strict,
}

impl Strictness {
    pub fn id(self) -> &'static str {
        match self {
            Self::Essential => "essential",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "standard" => Self::Standard,
            "strict" => Self::Strict,
            _ => Self::Essential,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServerSpec {
    /// Stable id: tags diagnostics and keys the user's explicit tool path.
    pub id: &'static str,
    pub languages: &'static [Language],
    pub role: Role,
    /// In order of preference; later ones are fallbacks.
    pub commands: &'static [ServerCommand],
    /// Files whose presence makes a folder the server's root.
    pub root_markers: &'static [&'static str],
    /// One line saying what to install when no executable is found.
    pub install_hint: &'static str,
    pub initialization_options: fn(&SettingsContext) -> Value,
    /// Answers `workspace/configuration`, by dotted section.
    pub settings: fn(&SettingsContext) -> Value,
}

fn nothing(_: &SettingsContext) -> Value {
    Value::Null
}

/// Pyright and basedpyright read the same settings under different names.
/// Analysis is limited to open files so a large monorepo is not walked.
fn pyright_settings(context: &SettingsContext) -> Value {
    let mut analysis = json!({
        "diagnosticMode": "openFilesOnly",
        "autoSearchPaths": true,
        "useLibraryCodeForTypes": true,
    });
    match context.strictness {
        // Type checking off still reports syntax; the overrides add back
        // the few rules that are right for any code.
        Strictness::Essential => {
            analysis["typeCheckingMode"] = json!("off");
            analysis["diagnosticSeverityOverrides"] = json!({
                "reportUndefinedVariable": "error",
                "reportMissingImports": "warning",
                "reportPossiblyUnbound": "warning",
                "reportUnusedCoroutine": "warning",
                "reportRedeclaration": "warning",
                "reportSelfClsParameterName": "warning",
            });
        }
        Strictness::Standard => analysis["typeCheckingMode"] = json!("basic"),
        // The server's own default.
        Strictness::Strict => {}
    }
    let mut python = json!({ "analysis": analysis });
    if let Some(interpreter) = context.toolchain.python() {
        python["pythonPath"] = json!(interpreter.to_string_lossy());
    }
    json!({
        "python": python,
        "basedpyright": { "analysis": analysis },
        "pyright": {},
    })
}

/// Ruff's own defaults include style rules (import order, simplifications,
/// modernisations). For the essentials only what points at a mistake is
/// asked for, without the two rules that mark every name a star import
/// brings. A project's own ruff configuration is always the one followed.
fn ruff_options(context: &SettingsContext) -> Value {
    if context.strictness != Strictness::Essential {
        return Value::Null;
    }
    json!({
        "settings": {
            "configurationPreference": "filesystemFirst",
            "lint": {
                "select": ["F", "E9"],
                "ignore": ["F403", "F405"],
            },
        }
    })
}

const STDIO: &[&str] = &["--stdio"];
const PYTHON: &[Language] = &[Language::Python];
const SCRIPT: &[Language] = &[
    Language::TypeScript,
    Language::TypeScriptReact,
    Language::JavaScript,
    Language::JavaScriptReact,
];

/// The servers known, in the order they are tried for a language.
#[derive(Clone, Debug)]
pub struct Registry {
    pub servers: Vec<ServerSpec>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Registry {
    pub fn builtin() -> Self {
        Self {
            servers: vec![
                ServerSpec {
                    id: "basedpyright",
                    languages: PYTHON,
                    role: Role::Insight,
                    commands: &[
                        ServerCommand {
                            executable: "basedpyright-langserver",
                            args: STDIO,
                        },
                        ServerCommand {
                            executable: "pyright-langserver",
                            args: STDIO,
                        },
                    ],
                    root_markers: &[
                        "pyproject.toml",
                        "setup.py",
                        "setup.cfg",
                        "pyrightconfig.json",
                        "requirements.txt",
                    ],
                    install_hint:
                        "Install it into the project's virtual environment: pip install basedpyright",
                    initialization_options: nothing,
                    settings: pyright_settings,
                },
                ServerSpec {
                    id: "ruff",
                    languages: PYTHON,
                    role: Role::DiagnosticsOnly,
                    commands: &[ServerCommand {
                        executable: "ruff",
                        args: &["server"],
                    }],
                    root_markers: &["pyproject.toml", "ruff.toml", ".ruff.toml", "setup.py"],
                    install_hint:
                        "Install it into the project's virtual environment: pip install ruff",
                    initialization_options: ruff_options,
                    settings: nothing,
                },
                ServerSpec {
                    id: "typescript",
                    languages: SCRIPT,
                    role: Role::Insight,
                    commands: &[
                        ServerCommand {
                            executable: "vtsls",
                            args: STDIO,
                        },
                        ServerCommand {
                            executable: "typescript-language-server",
                            args: STDIO,
                        },
                    ],
                    root_markers: &["package.json", "tsconfig.json", "jsconfig.json"],
                    install_hint: "Install it: npm install -g @vtsls/language-server",
                    initialization_options: nothing,
                    settings: nothing,
                },
                ServerSpec {
                    id: "gopls",
                    languages: &[Language::Go],
                    role: Role::Insight,
                    commands: &[ServerCommand {
                        executable: "gopls",
                        args: &[],
                    }],
                    root_markers: &["go.mod", "go.work"],
                    install_hint: "Install it: go install golang.org/x/tools/gopls@latest",
                    initialization_options: nothing,
                    settings: nothing,
                },
                ServerSpec {
                    id: "rust-analyzer",
                    languages: &[Language::Rust],
                    role: Role::Insight,
                    commands: &[ServerCommand {
                        executable: "rust-analyzer",
                        args: &[],
                    }],
                    root_markers: &["Cargo.toml"],
                    install_hint: "Install it: rustup component add rust-analyzer",
                    initialization_options: nothing,
                    settings: nothing,
                },
            ],
        }
    }

    /// The servers for `language`, most preferred first.
    pub fn for_language(&self, language: Language) -> impl Iterator<Item = &ServerSpec> {
        self.servers
            .iter()
            .filter(move |server| server.languages.contains(&language))
    }

    pub fn server(&self, id: &str) -> Option<&ServerSpec> {
        self.servers.iter().find(|server| server.id == id)
    }
}

impl ServerSpec {
    /// The executable to run and its arguments: the path the user set for
    /// this server, else the first command found in the venv or on the path.
    pub fn resolve(
        &self,
        toolchain: &Toolchain,
        explicit: Option<&Path>,
    ) -> Option<(PathBuf, &'static [&'static str])> {
        if let (Some(path), Some(first)) = (explicit, self.commands.first()) {
            // A path the user set is that or nothing: no silent fallback.
            return toolchain
                .tool(Some(path), first.executable)
                .map(|program| (program, first.args));
        }
        self.commands.iter().find_map(|command| {
            toolchain
                .tool(None, command.executable)
                .map(|program| (program, command.args))
        })
    }

    /// The names looked for, to tell the user when none was found.
    pub fn executables(&self) -> Vec<String> {
        self.commands
            .iter()
            .map(|command| command.executable.to_string())
            .collect()
    }
}

/// The nearest folder at or above `file`'s, within `checkout`, that holds one
/// of `markers`; `checkout` itself when there is none or the file lies
/// outside it.
pub fn find_root(file: &Path, checkout: &Path, markers: &[&str]) -> PathBuf {
    let mut folder = file.parent();
    while let Some(at) = folder {
        if !at.starts_with(checkout) {
            break;
        }
        if markers.iter().any(|marker| at.join(marker).is_file()) {
            return at.to_path_buf();
        }
        folder = at.parent();
    }
    checkout.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn toolchain(search: &Path, venv: Option<&Path>) -> Toolchain {
        Toolchain {
            venv: venv.map(Path::to_path_buf),
            search_path: search.to_string_lossy().into_owned(),
            environment: Vec::new(),
        }
    }

    #[test]
    fn languages_come_from_extensions() {
        let of = |name: &str| Language::of(Path::new(name));
        assert_eq!(of("/a/b.py"), Some(Language::Python));
        assert_eq!(of("stubs/x.PYI"), Some(Language::Python));
        assert_eq!(of("a.tsx"), Some(Language::TypeScriptReact));
        assert_eq!(of("a.mjs"), Some(Language::JavaScript));
        assert_eq!(of("main.go"), Some(Language::Go));
        assert_eq!(of("lib.rs"), Some(Language::Rust));
        assert_eq!(of("README.md"), None);
        assert_eq!(of("Makefile"), None);
        assert_eq!(Language::TypeScriptReact.id(), "typescriptreact");
    }

    #[test]
    fn python_gets_insight_then_lint() {
        let registry = Registry::builtin();
        let python: Vec<_> = registry.for_language(Language::Python).collect();
        assert_eq!(
            python.iter().map(|server| server.id).collect::<Vec<_>>(),
            ["basedpyright", "ruff"]
        );
        assert_eq!(python[0].role, Role::Insight);
        assert_eq!(python[1].role, Role::DiagnosticsOnly);
        assert_eq!(
            python[0].executables(),
            ["basedpyright-langserver", "pyright-langserver"]
        );
        for (language, id) in [
            (Language::TypeScript, "typescript"),
            (Language::JavaScriptReact, "typescript"),
            (Language::Go, "gopls"),
            (Language::Rust, "rust-analyzer"),
        ] {
            let ids: Vec<_> = registry
                .for_language(language)
                .map(|server| server.id)
                .collect();
            assert_eq!(ids, [id]);
        }
        assert!(registry.server("ruff").is_some());
        assert!(registry.server("absent").is_none());
    }

    #[test]
    fn the_first_command_found_wins_and_an_explicit_path_is_final() {
        let folder = tempfile::tempdir().unwrap();
        let bin = folder.path().join("bin");
        let registry = Registry::builtin();
        let pyright = registry.server("basedpyright").unwrap();
        let tools = toolchain(&bin, None);
        assert_eq!(pyright.resolve(&tools, None), None);
        touch(&bin.join("pyright-langserver"));
        let (program, args) = pyright.resolve(&tools, None).unwrap();
        assert_eq!(program, bin.join("pyright-langserver"));
        assert_eq!(args, ["--stdio"]);
        touch(&bin.join("basedpyright-langserver"));
        assert_eq!(
            pyright.resolve(&tools, None).unwrap().0,
            bin.join("basedpyright-langserver")
        );
        let ruff = registry.server("ruff").unwrap();
        touch(&bin.join("ruff"));
        assert_eq!(ruff.resolve(&tools, None).unwrap().1, ["server"]);
        let own = folder.path().join("own/ruff");
        assert_eq!(ruff.resolve(&tools, Some(&own)), None);
        touch(&own);
        assert_eq!(ruff.resolve(&tools, Some(&own)).unwrap().0, own);
    }

    #[test]
    fn pyright_is_told_the_interpreter_and_to_stay_in_open_files() {
        let folder = tempfile::tempdir().unwrap();
        let venv = folder.path().join(".venv");
        touch(&venv.join("bin/python"));
        let tools = toolchain(&venv.join("bin"), Some(&venv));
        let registry = Registry::builtin();
        let spec = registry.server("basedpyright").unwrap();
        let settings = (spec.settings)(&SettingsContext {
            strictness: Strictness::default(),
            root: folder.path(),
            toolchain: &tools,
        });
        assert_eq!(
            settings["python"]["pythonPath"],
            venv.join("bin/python").to_string_lossy().as_ref()
        );
        assert_eq!(
            settings["python"]["analysis"]["diagnosticMode"],
            "openFilesOnly"
        );
        assert_eq!(
            settings["basedpyright"]["analysis"]["diagnosticMode"],
            "openFilesOnly"
        );
    }

    #[test]
    fn the_root_is_the_nearest_marked_folder_inside_the_checkout() {
        let folder = tempfile::tempdir().unwrap();
        let checkout = folder.path().join("repo");
        touch(&folder.path().join("pyproject.toml"));
        touch(&checkout.join("services/api/pyproject.toml"));
        touch(&checkout.join("services/api/app/deep/mod.py"));
        touch(&checkout.join("tools/script.py"));
        let markers = &["pyproject.toml", "setup.py"];
        assert_eq!(
            find_root(
                &checkout.join("services/api/app/deep/mod.py"),
                &checkout,
                markers
            ),
            checkout.join("services/api")
        );
        // No marker below the checkout: the checkout, never a folder above it.
        assert_eq!(
            find_root(&checkout.join("tools/script.py"), &checkout, markers),
            checkout
        );
        touch(&checkout.join("setup.py"));
        assert_eq!(
            find_root(&checkout.join("tools/script.py"), &checkout, markers),
            checkout
        );
        // A file outside the checkout has the checkout as its root.
        assert_eq!(
            find_root(&folder.path().join("elsewhere/x.py"), &checkout, markers),
            checkout
        );
    }
}
