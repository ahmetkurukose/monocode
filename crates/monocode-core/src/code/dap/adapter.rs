//! The debug adapters the app knows and what each can debug. An adapter says
//! how its process is started from a checkout's toolchain and turns a process
//! to run into the launch request that debugs it. Only Python (debugpy) is
//! here; another language is one more implementation of `Adapter` in the
//! registry.
use super::breakpoints::ExceptionFilters;
use super::client::{AdapterStart, Transport};
use super::types::DapError;
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the debuggee's input and output go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Console {
    /// The adapter asks for the debuggee to be started in a terminal of the
    /// app (`SessionEvent::RunInTerminal`); its output is that terminal's.
    #[default]
    IntegratedTerminal,
    /// The adapter starts the debuggee itself and its output arrives as
    /// session output. For callers without a terminal: agents and tests.
    InternalConsole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    pub console: Console,
    /// Step through and stop in the project's own code only.
    pub just_my_code: bool,
    pub stop_on_entry: bool,
    /// Debug the processes the debuggee starts, as child sessions.
    pub subprocesses: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            console: Console::default(),
            just_my_code: true,
            stop_on_entry: false,
            subprocesses: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Launch,
    Attach,
}

/// What to debug, ready to send to an adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugRequest {
    /// The id of the adapter in the registry.
    pub adapter: &'static str,
    pub kind: RequestKind,
    /// Shown on the session's tab.
    pub label: String,
    /// The folder the adapter runs in.
    pub cwd: PathBuf,
    /// The arguments of the `launch` or `attach` request.
    pub arguments: Map<String, Value>,
}

impl DebugRequest {
    /// Whether the debuggee is to be started in a terminal of the app.
    pub fn uses_terminal(&self) -> bool {
        self.arguments.get("console").and_then(Value::as_str) == Some("integratedTerminal")
    }
}

pub trait Adapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// The adapter process for this toolchain, or which tool is missing.
    /// May run the interpreter briefly; not for the interface thread.
    fn locate(&self, toolchain: &Toolchain, cwd: &Path) -> Result<AdapterStart, DapError>;

    /// The request that debugs `spawn`, or `None` when this adapter cannot.
    fn launch(&self, spawn: &Spawn, options: &LaunchOptions) -> Option<DebugRequest>;

    /// The request that attaches to a running process, where supported.
    fn attach_to_process(
        &self,
        _pid: u32,
        _cwd: &Path,
        _options: &LaunchOptions,
    ) -> Option<DebugRequest> {
        None
    }

    /// The adapter's names for the store's exception filters.
    fn exception_filters(&self, filters: &ExceptionFilters) -> Vec<String>;

    /// The configuration of a child session carried by one of the adapter's
    /// own events, for adapters that predate `startDebugging`.
    fn child_from_event(&self, _event: &str, _body: &Value) -> Option<Map<String, Value>> {
        None
    }

    /// Where a child session with this configuration connects.
    fn child_endpoint(&self, _configuration: &Map<String, Value>) -> Option<(String, u16)> {
        None
    }
}

static ADAPTERS: [&dyn Adapter; 1] = [&Debugpy];

pub fn adapters() -> &'static [&'static dyn Adapter] {
    &ADAPTERS
}

pub fn adapter(id: &str) -> Option<&'static dyn Adapter> {
    ADAPTERS.iter().copied().find(|adapter| adapter.id() == id)
}

/// The debug request for a process to run, from the first adapter that can
/// debug it. `None` means the run cannot be debugged (yet).
pub fn derive(spawn: &Spawn, options: &LaunchOptions) -> Option<DebugRequest> {
    ADAPTERS
        .iter()
        .find_map(|adapter| adapter.launch(spawn, options))
}

/// The request that attaches to a running Python process. debugpy injects
/// itself with the system's debugger, which the system may refuse.
pub fn attach_to_process(pid: u32, cwd: &Path, options: &LaunchOptions) -> Option<DebugRequest> {
    ADAPTERS
        .iter()
        .find_map(|adapter| adapter.attach_to_process(pid, cwd, options))
}

/// Python, through debugpy's own adapter (`python -m debugpy.adapter`).
pub struct Debugpy;

pub const DEBUGPY: &str = "debugpy";

impl Adapter for Debugpy {
    fn id(&self) -> &'static str {
        DEBUGPY
    }

    fn locate(&self, toolchain: &Toolchain, cwd: &Path) -> Result<AdapterStart, DapError> {
        let python = toolchain.python().ok_or_else(|| DapError::ToolMissing {
            tool: "python".into(),
            hint: "Create a virtual environment in the project or put python3 on the PATH.".into(),
        })?;
        // Asked of the interpreter itself: only it knows what it can import.
        let mut probe = Command::new(&python);
        probe
            .args(["-c", "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec('debugpy') else 3)"])
            .envs(toolchain.environment.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::process::configure_child(&mut probe);
        match probe.status() {
            Ok(status) if status.success() => {}
            Ok(_) => {
                return Err(DapError::ToolMissing {
                    tool: DEBUGPY.into(),
                    hint: format!(
                        "Install it in the project's environment: {} -m pip install debugpy",
                        python.display()
                    ),
                })
            }
            Err(error) => {
                return Err(DapError::Start(format!(
                    "Could not run {}: {error}.",
                    python.display()
                )))
            }
        }
        Ok(AdapterStart {
            program: python,
            args: vec!["-m".into(), "debugpy.adapter".into()],
            cwd: cwd.to_path_buf(),
            env: toolchain.environment.clone(),
            transport: Transport::Stdio,
        })
    }

    fn launch(&self, spawn: &Spawn, options: &LaunchOptions) -> Option<DebugRequest> {
        let target = python_target(spawn)?;
        let mut arguments = common_arguments(&spawn.label, options);
        arguments.insert("request".into(), json!("launch"));
        let (key, value) = match target.what {
            What::Program(path) => ("program", path),
            What::Module(name) => ("module", name),
            What::Code(code) => ("code", code),
        };
        arguments.insert(key.into(), json!(value));
        arguments.insert("args".into(), json!(target.args));
        arguments.insert("cwd".into(), json!(spawn.cwd));
        let env: Map<String, Value> = spawn
            .env
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect();
        arguments.insert("env".into(), Value::Object(env));
        if let Some(python) = target.interpreter {
            arguments.insert("python".into(), json!([python]));
        }
        if !target.interpreter_args.is_empty() {
            arguments.insert("pythonArgs".into(), json!(target.interpreter_args));
        }
        let console = match options.console {
            Console::IntegratedTerminal => "integratedTerminal",
            Console::InternalConsole => "internalConsole",
        };
        arguments.insert("console".into(), json!(console));
        // Without a terminal the output must come through the protocol.
        arguments.insert(
            "redirectOutput".into(),
            json!(options.console == Console::InternalConsole),
        );
        arguments.insert("stopOnEntry".into(), json!(options.stop_on_entry));
        Some(DebugRequest {
            adapter: DEBUGPY,
            kind: RequestKind::Launch,
            label: spawn.label.clone(),
            cwd: spawn.cwd.clone(),
            arguments,
        })
    }

    fn attach_to_process(
        &self,
        pid: u32,
        cwd: &Path,
        options: &LaunchOptions,
    ) -> Option<DebugRequest> {
        let label = format!("Process {pid}");
        let mut arguments = common_arguments(&label, options);
        arguments.insert("request".into(), json!("attach"));
        arguments.insert("processId".into(), json!(pid));
        Some(DebugRequest {
            adapter: DEBUGPY,
            kind: RequestKind::Attach,
            label,
            cwd: cwd.to_path_buf(),
            arguments,
        })
    }

    fn exception_filters(&self, filters: &ExceptionFilters) -> Vec<String> {
        let mut names = Vec::new();
        if filters.raised {
            names.push("raised".to_string());
        }
        if filters.uncaught {
            names.push("uncaught".to_string());
        }
        names
    }

    fn child_from_event(&self, event: &str, body: &Value) -> Option<Map<String, Value>> {
        // What debugpy sends to a client that did not announce `startDebugging`.
        (event == "debugpyAttach")
            .then(|| body.as_object().cloned())
            .flatten()
    }

    fn child_endpoint(&self, configuration: &Map<String, Value>) -> Option<(String, u16)> {
        let connect = configuration.get("connect")?;
        let host = connect.get("host").and_then(Value::as_str)?;
        let port = connect.get("port").and_then(Value::as_u64)?;
        Some((host.to_string(), u16::try_from(port).ok()?))
    }
}

fn common_arguments(label: &str, options: &LaunchOptions) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert("name".into(), json!(label));
    arguments.insert("type".into(), json!("python"));
    arguments.insert("justMyCode".into(), json!(options.just_my_code));
    arguments.insert("subProcess".into(), json!(options.subprocesses));
    arguments
}

enum What {
    Program(String),
    Module(String),
    Code(String),
}

struct PythonTarget {
    /// Set when the spawn names an interpreter by path; otherwise the
    /// adapter's own interpreter runs the debuggee.
    interpreter: Option<String>,
    interpreter_args: Vec<String>,
    what: What,
    args: Vec<String>,
}

fn file_name(command: &str) -> &str {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    name.strip_suffix(".exe").unwrap_or(name)
}

fn is_python(name: &str) -> bool {
    let Some(version) = name.strip_prefix("python") else {
        return false;
    };
    version.is_empty()
        || version == "w"
        || version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Reads a command line the way the interpreter would, as far as needed to
/// find what it runs.
fn python_target(spawn: &Spawn) -> Option<PythonTarget> {
    let name = file_name(&spawn.command);
    if matches!(name, "pytest" | "py.test") {
        return Some(PythonTarget {
            interpreter: None,
            interpreter_args: Vec::new(),
            what: What::Module("pytest".into()),
            args: spawn.args.clone(),
        });
    }
    if !is_python(name) {
        return None;
    }
    let interpreter = (name != spawn.command).then(|| spawn.command.clone());
    let mut interpreter_args = Vec::new();
    let mut rest = spawn.args.iter();
    let what = loop {
        let argument = rest.next()?;
        match argument.as_str() {
            "-m" => break What::Module(rest.next()?.clone()),
            "-c" => break What::Code(rest.next()?.clone()),
            // Options that take a value of their own.
            "-X" | "-W" => {
                interpreter_args.push(argument.clone());
                interpreter_args.push(rest.next()?.clone());
            }
            // Reading the program from standard input cannot be debugged.
            "-" => return None,
            option if option.starts_with('-') => {
                if let Some(module) = option.strip_prefix("-m") {
                    break What::Module(module.to_string());
                }
                interpreter_args.push(argument.clone());
            }
            _ => break What::Program(argument.clone()),
        }
    };
    // debugpy cannot debug itself, and a spawn that already runs under it
    // is not a plain program any more.
    if matches!(&what, What::Module(module) if module == "debugpy" || module.starts_with("debugpy."))
    {
        return None;
    }
    Some(PythonTarget {
        interpreter,
        interpreter_args,
        what,
        args: rest.cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(command: &str, args: &[&str]) -> Spawn {
        Spawn {
            label: "Run".into(),
            command: command.into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: PathBuf::from("/p"),
            env: vec![("MODE".into(), "test".into())],
        }
    }

    #[test]
    fn python_runs_become_launch_requests() {
        // command, args, the key and value of what runs, its args, python args
        type Case<'a> = (
            &'a str,
            &'a [&'a str],
            &'a str,
            &'a str,
            &'a [&'a str],
            &'a [&'a str],
        );
        let cases: &[Case] = &[
            (
                "python",
                &["app.py", "--port", "8000"],
                "program",
                "app.py",
                &["--port", "8000"],
                &[],
            ),
            (
                "python3",
                &["-m", "http.server", "9000"],
                "module",
                "http.server",
                &["9000"],
                &[],
            ),
            (
                "/p/.venv/bin/python",
                &["-u", "app.py"],
                "program",
                "app.py",
                &[],
                &["-u"],
            ),
            (
                "python3.12",
                &["-X", "dev", "-W", "error", "-m", "pkg"],
                "module",
                "pkg",
                &[],
                &["-X", "dev", "-W", "error"],
            ),
            (
                "python",
                &["-mpkg.tool", "x"],
                "module",
                "pkg.tool",
                &["x"],
                &[],
            ),
            (
                "python",
                &["-c", "print(1)", "a"],
                "code",
                "print(1)",
                &["a"],
                &[],
            ),
            (
                "pytest",
                &["tests/test_a.py::test_one", "-x"],
                "module",
                "pytest",
                &["tests/test_a.py::test_one", "-x"],
                &[],
            ),
            ("/p/.venv/bin/py.test", &[], "module", "pytest", &[], &[]),
            (
                "python",
                &["-m", "pytest", "-k", "name"],
                "module",
                "pytest",
                &["-k", "name"],
                &[],
            ),
            (
                "C:\\venv\\Scripts\\python.exe",
                &["app.py"],
                "program",
                "app.py",
                &[],
                &[],
            ),
        ];
        for (command, args, key, value, rest, python_args) in cases {
            let request = derive(&spawn(command, args), &LaunchOptions::default())
                .unwrap_or_else(|| panic!("{command} {args:?}"));
            let arguments = &request.arguments;
            assert_eq!(request.kind, RequestKind::Launch);
            assert_eq!(request.adapter, DEBUGPY);
            assert_eq!(arguments[*key], json!(value), "{command} {args:?}");
            assert_eq!(arguments["args"], json!(rest), "{command} {args:?}");
            assert_eq!(
                arguments.get("pythonArgs").cloned().unwrap_or(json!([])),
                json!(python_args)
            );
            for other in ["program", "module", "code"] {
                assert_eq!(arguments.contains_key(other), other == *key);
            }
            assert_eq!(arguments["cwd"], json!("/p"));
            assert_eq!(arguments["env"], json!({ "MODE": "test" }));
            assert_eq!(arguments["justMyCode"], json!(true));
            assert_eq!(arguments["console"], json!("integratedTerminal"));
            assert_eq!(arguments["redirectOutput"], json!(false));
            assert_eq!(arguments["request"], json!("launch"));
            assert!(request.uses_terminal());
            // Only an interpreter named by path is passed on.
            let named = command.contains(['/', '\\']) && !command.ends_with("py.test");
            assert_eq!(arguments.contains_key("python"), named, "{command}");
        }
    }

    #[test]
    fn other_runs_cannot_be_debugged() {
        let cases: &[(&str, &[&str])] = &[
            ("node", &["server.js"]),
            ("python", &[]),
            ("python", &["-"]),
            ("python", &["-m"]),
            ("python", &["-V"]),
            ("python", &["-m", "debugpy", "--listen", "5678", "app.py"]),
            ("pythonista", &["app.py"]),
            ("uv", &["run", "python", "app.py"]),
            ("cargo", &["test"]),
        ];
        for (command, args) in cases {
            assert!(
                derive(&spawn(command, args), &LaunchOptions::default()).is_none(),
                "{command} {args:?}"
            );
        }
    }

    #[test]
    fn options_shape_the_request() {
        let options = LaunchOptions {
            console: Console::InternalConsole,
            just_my_code: false,
            stop_on_entry: true,
            subprocesses: false,
        };
        let request = derive(&spawn("python", &["app.py"]), &options).unwrap();
        assert_eq!(request.arguments["console"], json!("internalConsole"));
        assert_eq!(request.arguments["redirectOutput"], json!(true));
        assert_eq!(request.arguments["justMyCode"], json!(false));
        assert_eq!(request.arguments["stopOnEntry"], json!(true));
        assert_eq!(request.arguments["subProcess"], json!(false));
        assert!(!request.uses_terminal());

        let attach = attach_to_process(4242, Path::new("/p"), &options).unwrap();
        assert_eq!(attach.kind, RequestKind::Attach);
        assert_eq!(attach.arguments["processId"], json!(4242));
        assert_eq!(attach.arguments["request"], json!("attach"));
    }

    #[test]
    fn exception_filters_and_child_endpoints_are_debugpys() {
        let adapter = adapter(DEBUGPY).unwrap();
        assert!(adapters().len() == 1 && super::adapter("delve").is_none());
        assert_eq!(
            adapter.exception_filters(&ExceptionFilters::default()),
            ["uncaught"]
        );
        let body = json!({ "request": "attach", "subProcessId": 7,
            "connect": { "host": "127.0.0.1", "port": 5001 } });
        let configuration = adapter.child_from_event("debugpyAttach", &body).unwrap();
        assert_eq!(
            adapter.child_endpoint(&configuration),
            Some(("127.0.0.1".into(), 5001))
        );
        assert!(adapter.child_from_event("output", &body).is_none());
    }

    #[test]
    fn a_missing_debugpy_is_named_with_what_to_install() {
        // A stand-in interpreter that cannot import anything.
        if !cfg!(unix) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join(".venv/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("activate"), "").unwrap();
        std::fs::write(bin.join("python"), "#!/bin/sh\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(bin.join("python"), mode).unwrap();
        }
        let toolchain = Toolchain::with_search_path(
            &crate::terminal_setup::TerminalSetup::default(),
            root.path(),
            root.path(),
            "/usr/bin:/bin",
        );
        match Debugpy.locate(&toolchain, root.path()) {
            Err(DapError::ToolMissing { tool, hint }) => {
                assert_eq!(tool, "debugpy");
                assert!(hint.contains("pip install debugpy"), "{hint}");
            }
            other => panic!("{other:?}"),
        }
        let none = Toolchain::default();
        assert!(matches!(
            Debugpy.locate(&none, root.path()),
            Err(DapError::ToolMissing { tool, .. }) if tool == "python"
        ));
    }
}
