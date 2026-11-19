//! Turns a run configuration into the one process to start in a checkout:
//! the command line is split by shell rules, variables are filled in word
//! by word (so a path with spaces stays one argument), and a bare `python`
//! becomes the checkout's interpreter.
use super::config::RunConfig;
use super::variables::{used, RunContext, Values, Variable};
use crate::code::spawn::Spawn;
use crate::code::toolchain::Toolchain;
use crate::terminal_setup::parse_environment;
use std::path::{Path, PathBuf};

/// Why a configuration cannot be run in a context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unresolved {
    /// The context has no value for a variable the configuration uses. Such
    /// a configuration is not offered, rather than shown as broken.
    Needs(Variable),
    /// There is no command.
    Empty,
    /// The command line has a quote that is never closed.
    Unbalanced,
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Unresolved::Needs(variable) => variable.missing(),
            Unresolved::Empty => "The configuration has no command.",
            Unresolved::Unbalanced => "The command has a quote that is never closed.",
        })
    }
}

impl std::error::Error for Unresolved {}

fn split(line: &str) -> Result<Vec<String>, Unresolved> {
    shlex::split(line).ok_or(Unresolved::Unbalanced)
}

fn is_bare_python(command: &str) -> bool {
    matches!(command, "python" | "python3")
}

/// The process `config` starts in `context`. The result's `env` holds only
/// the configuration's own variables; whoever starts the process applies
/// `toolchain.environment` first.
///
/// Reads the disk only to find the interpreter, and only when the command
/// asks for it (`$PYTHON`, or a bare `python`/`python3`).
pub fn resolve(
    config: &RunConfig,
    context: &RunContext,
    toolchain: &Toolchain,
) -> Result<Spawn, Unresolved> {
    let values = Values::new(context, toolchain);
    let mut typed = split(&config.command)?;
    typed.extend(split(&config.args)?);
    let mut words = Vec::with_capacity(typed.len());
    for word in &typed {
        words.push(values.substitute(word)?);
    }
    let mut words = words.into_iter();
    let mut command = words.next().filter(|command| !command.is_empty());
    if command.as_deref().is_some_and(is_bare_python) {
        // The venv's interpreter is all `activate` amounts to for Python.
        command = values.python().or(command);
    }
    let command = command.ok_or(Unresolved::Empty)?;
    let cwd = values.substitute(config.cwd.trim())?;
    let cwd = match cwd.is_empty() {
        true => context.checkout.clone(),
        false if Path::new(&cwd).is_absolute() => PathBuf::from(cwd),
        false => context.checkout.join(cwd),
    };
    let mut env = Vec::new();
    for (key, value) in parse_environment(&config.env) {
        env.push((key, values.substitute(&value)?));
    }
    Ok(Spawn {
        label: config.label(),
        command,
        args: words.collect(),
        cwd,
        env,
    })
}

/// The variables `config` uses anywhere, in the order they first appear,
/// for a settings pane that wants to say what a configuration depends on.
pub fn variables_used(config: &RunConfig) -> Vec<Variable> {
    let mut found = Vec::new();
    for line in [&config.command, &config.args] {
        for word in shlex::split(line).unwrap_or_default() {
            used(&word, &mut found);
        }
    }
    used(&config.cwd, &mut found);
    for (_, value) in parse_environment(&config.env) {
        used(&value, &mut found);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: &str) -> RunConfig {
        RunConfig {
            name: "Serve".into(),
            command: command.into(),
            ..RunConfig::default()
        }
    }

    fn context() -> RunContext {
        RunContext {
            checkout: "/work/tree".into(),
            project: "/work/project".into(),
            file: Some("/work/tree/my app/main.py".into()),
            symbol: Some("TestOrders::test_post".into()),
            row: Some(3),
            column: Some(1),
            selected_text: None,
        }
    }

    fn words(command: &str) -> Result<Vec<String>, Unresolved> {
        resolve(&config(command), &context(), &Toolchain::default()).map(|spawn| {
            let mut words = vec![spawn.command];
            words.extend(spawn.args);
            words
        })
    }

    #[test]
    fn command_lines_are_split_by_shell_rules_then_filled_in() {
        let cases: [(&str, &[&str]); 12] = [
            ("npm run dev", &["npm", "run", "dev"]),
            ("  make   -j4  ", &["make", "-j4"]),
            ("echo 'a b' \"c d\" e\\ f", &["echo", "a b", "c d", "e f"]),
            ("echo \"it's\" ''", &["echo", "it's", ""]),
            // A path with spaces is one argument without any quoting.
            (
                "node $FILE --x",
                &["node", "/work/tree/my app/main.py", "--x"],
            ),
            ("node \"$FILE\"", &["node", "/work/tree/my app/main.py"]),
            (
                "cat $FILE_DIR/out.txt",
                &["cat", "/work/tree/my app/out.txt"],
            ),
            (
                "pytest ${RELATIVE_FILE}::$SYMBOL",
                &["pytest", "my app/main.py::TestOrders::test_post"],
            ),
            ("echo $$HOME $$$ROW", &["echo", "$HOME", "$3"]),
            ("awk '{print $1}' $HOME", &["awk", "{print $1}", "$HOME"]),
            ("$CHECKOUT/bin/tool", &["/work/tree/bin/tool"]),
            (
                "sh -c 'cd $CHECKOUT && ls'",
                &["sh", "-c", "cd /work/tree && ls"],
            ),
        ];
        for (typed, expected) in cases {
            let expected: Vec<String> = expected.iter().map(|word| word.to_string()).collect();
            assert_eq!(words(typed), Ok(expected), "{typed}");
        }
    }

    #[test]
    fn what_cannot_run_says_why() {
        let cases = [
            ("", Unresolved::Empty),
            ("   ", Unresolved::Empty),
            ("''", Unresolved::Empty),
            ("echo 'open", Unresolved::Unbalanced),
            ("echo \"open", Unresolved::Unbalanced),
            (
                "echo $SELECTED_TEXT",
                Unresolved::Needs(Variable::SelectedText),
            ),
            ("$PYTHON x.py", Unresolved::Needs(Variable::Python)),
        ];
        for (typed, expected) in cases {
            assert_eq!(words(typed), Err(expected), "{typed}");
        }
        let unfocused = RunContext::new("/work/tree", "/work/tree");
        let run_file = config("node $FILE");
        assert_eq!(
            resolve(&run_file, &unfocused, &Toolchain::default()),
            Err(Unresolved::Needs(Variable::File))
        );
        assert_eq!(
            Unresolved::Needs(Variable::File).to_string(),
            "No file is focused."
        );
    }

    #[test]
    fn arguments_follow_the_command_and_the_name_is_the_label() {
        let mut serve = config("npm run");
        serve.args = "dev -- --port 3000 '$FILE_NAME'".into();
        let spawn = resolve(&serve, &context(), &Toolchain::default()).unwrap();
        assert_eq!(spawn.label, "Serve");
        assert_eq!(spawn.command, "npm");
        assert_eq!(
            spawn.args,
            ["run", "dev", "--", "--port", "3000", "main.py"]
        );
        serve.name = "  ".into();
        let spawn = resolve(&serve, &context(), &Toolchain::default()).unwrap();
        assert_eq!(spawn.label, "npm run dev -- --port 3000 '$FILE_NAME'");
    }

    #[test]
    fn the_working_directory_is_the_checkout_unless_set() {
        let cases = [
            ("", "/work/tree"),
            ("  ", "/work/tree"),
            ("web", "/work/tree/web"),
            ("web/app", "/work/tree/web/app"),
            ("/srv/else", "/srv/else"),
            ("$FILE_DIR", "/work/tree/my app"),
            ("$PROJECT/web", "/work/project/web"),
        ];
        for (cwd, expected) in cases {
            let mut serve = config("npm start");
            serve.cwd = cwd.into();
            let spawn = resolve(&serve, &context(), &Toolchain::default()).unwrap();
            assert_eq!(spawn.cwd, PathBuf::from(expected), "{cwd:?}");
        }
        let mut serve = config("npm start");
        serve.cwd = "$SELECTED_TEXT".into();
        assert_eq!(
            resolve(&serve, &context(), &Toolchain::default()),
            Err(Unresolved::Needs(Variable::SelectedText))
        );
    }

    #[test]
    fn only_the_configurations_own_variables_are_in_the_environment() {
        let mut serve = config("npm start");
        serve.env =
            "# local\nexport MODE=dev\nROOT=$CHECKOUT/data\nQUOTED=\"a b\"\nnot a line\nMODE=test"
                .into();
        let toolchain = Toolchain {
            environment: vec![("FROM_SETUP".into(), "1".into())],
            ..Toolchain::default()
        };
        let spawn = resolve(&serve, &context(), &toolchain).unwrap();
        assert_eq!(
            spawn.env,
            [
                ("ROOT".to_owned(), "/work/tree/data".to_owned()),
                ("QUOTED".to_owned(), "a b".to_owned()),
                ("MODE".to_owned(), "test".to_owned()),
            ]
        );
    }

    #[test]
    fn a_bare_python_is_the_checkouts_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join(".venv/bin/python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        let with_venv = Toolchain {
            venv: Some(root.path().join(".venv")),
            ..Toolchain::default()
        };
        let interpreter = python.to_string_lossy().into_owned();
        let cases = [
            ("python manage.py runserver", interpreter.as_str()),
            ("python3 -m http.server", interpreter.as_str()),
            ("$PYTHON -V", interpreter.as_str()),
            // Only the exact bare names: the user chose these on purpose.
            ("python3.11 x.py", "python3.11"),
            ("/usr/bin/python3 x.py", "/usr/bin/python3"),
            ("./python x.py", "./python"),
            ("pythonw x.py", "pythonw"),
        ];
        for (typed, expected) in cases {
            let spawn = resolve(&config(typed), &context(), &with_venv).unwrap();
            assert_eq!(spawn.command, expected, "{typed}");
        }
        // Only the program is replaced, never an argument.
        let spawn = resolve(&config("env python x.py"), &context(), &with_venv).unwrap();
        assert_eq!(
            (spawn.command.as_str(), spawn.args[0].as_str()),
            ("env", "python")
        );
        // With no interpreter known the name is left for the path to find.
        let spawn = resolve(&config("python x.py"), &context(), &Toolchain::default()).unwrap();
        assert_eq!(spawn.command, "python");
    }

    #[test]
    fn the_variables_a_configuration_uses_are_listed() {
        let mut serve = config("$PYTHON -m pytest '$RELATIVE_FILE' $$ROW");
        serve.args = "-k $SYMBOL".into();
        serve.cwd = "$FILE_DIR".into();
        serve.env = "WHERE=$CHECKOUT\nPY=$PYTHON".into();
        assert_eq!(
            variables_used(&serve),
            [
                Variable::Python,
                Variable::RelativeFile,
                Variable::Symbol,
                Variable::FileDir,
                Variable::Checkout,
            ]
        );
        assert!(variables_used(&config("npm start")).is_empty());
    }
}
