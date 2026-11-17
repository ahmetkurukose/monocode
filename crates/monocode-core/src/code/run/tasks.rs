//! The tasks offered without the user defining anything: run this file,
//! run this test. Each language has a small provider that knows its
//! commands; the UI only finds the places in a file (`Runnable`) and asks
//! here what they run.
//!
//! A task is an ordinary `RunConfig` whose id starts with `builtin:`. It
//! names its file from the checkout and its interpreter as `$PYTHON`, so a
//! remembered task repeats the same target in any checkout of the project.
use super::config::{RunConfig, TestRunner, BUILTIN_PREFIX};
use super::variables::{module_path, RunContext};
use std::path::{Path, PathBuf};

/// What a run marker in the gutter stands on. The UI crate finds these
/// with tree-sitter by the rules in docs/native/code.md §3 and knows
/// nothing about commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunnableKind {
    /// A function or method whose name starts with `test`.
    PythonTestFunction,
    /// A class of tests: named `Test...` or derived from a `...TestCase`.
    PythonTestClass,
    /// A top-level `if __name__ == "__main__":`.
    PythonMain,
    /// `func TestXxx(t *testing.T)` in a `_test.go` file.
    GoTest,
    /// `func main()` in `package main`.
    GoMain,
    /// A function under `#[test]`.
    RustTest,
    /// `fn main()` at the top level.
    RustMain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runnable {
    pub kind: RunnableKind,
    /// The function or class name; the file's name for a main block.
    pub name: String,
    /// The classes around it, outermost first and joined with `::`
    /// (`TestOrders`, `Outer::Inner`). For Rust, the modules around a test.
    pub class_name: Option<String>,
    /// The line the marker is drawn on, from 0. Not used for the command.
    pub row: u32,
}

/// Whether pytest's default rules collect `path`: `test_*.py` or `*_test.py`.
pub fn is_python_test_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.strip_suffix(".py")
        .is_some_and(|stem| stem.starts_with("test_") || stem.ends_with("_test"))
}

/// The focused file, as the providers need it.
struct Target<'a> {
    checkout: &'a Path,
    file: PathBuf,
    /// From the checkout, or absolute for a file outside it.
    relative: String,
    name: String,
    /// The file's folder as a configuration's `cwd`.
    folder: String,
    symbol: Option<&'a str>,
    runner: TestRunner,
}

struct Provider {
    language: &'static str,
    extensions: &'static [&'static str],
    /// The tasks for the file, the most specific first.
    file_tasks: fn(&Target) -> Vec<RunConfig>,
    runnable_tasks: fn(&Runnable, &Target) -> Vec<RunConfig>,
}

const PROVIDERS: &[Provider] = &[
    Provider {
        language: "python",
        extensions: &["py"],
        file_tasks: python_file_tasks,
        runnable_tasks: python_runnable_tasks,
    },
    Provider {
        language: "go",
        extensions: &["go"],
        file_tasks: go_file_tasks,
        runnable_tasks: go_runnable_tasks,
    },
    Provider {
        language: "rust",
        extensions: &["rs"],
        file_tasks: rust_file_tasks,
        runnable_tasks: rust_runnable_tasks,
    },
    Provider {
        language: "javascript",
        extensions: &["js", "mjs", "cjs", "ts", "mts", "cts", "tsx"],
        file_tasks: javascript_file_tasks,
        runnable_tasks: |_, _| Vec::new(),
    },
];

/// One word of a command line that must come out exactly as `text`:
/// quoted for the splitting, and with its dollar signs doubled so that a
/// file called `$FILE.py` is not taken for a variable.
fn literal(text: &str) -> String {
    let quoted = match shlex::try_quote(text) {
        Ok(quoted) => quoted.into_owned(),
        Err(_) => text.to_owned(),
    };
    quoted.replace('$', "$$")
}

/// `folder` as a configuration's `cwd`: empty for the checkout itself.
fn cwd_of(folder: &Path, checkout: &Path) -> String {
    let folder = folder.strip_prefix(checkout).unwrap_or(folder);
    folder.to_string_lossy().replace('$', "$$")
}

fn task(provider: &str, kind: &str, target: &str, name: String, command: String) -> RunConfig {
    RunConfig {
        id: format!("{BUILTIN_PREFIX}{provider}:{kind}:{target}"),
        name,
        command,
        ..RunConfig::default()
    }
}

fn in_folder(mut config: RunConfig, cwd: String) -> RunConfig {
    config.cwd = cwd;
    config
}

fn with_target<T>(
    context: &RunContext,
    runner: TestRunner,
    run: impl FnOnce(&Provider, &Target) -> Vec<T>,
) -> Vec<T> {
    let Some(file) = context.absolute_file() else {
        return Vec::new();
    };
    let extension = file.extension().and_then(|extension| extension.to_str());
    let Some(provider) = PROVIDERS
        .iter()
        .find(|provider| extension.is_some_and(|found| provider.extensions.contains(&found)))
    else {
        return Vec::new();
    };
    let (Some(name), Some(folder), Some(relative)) =
        (file.file_name(), file.parent(), context.relative_file())
    else {
        return Vec::new();
    };
    let target = Target {
        checkout: &context.checkout,
        relative: relative.to_string_lossy().into_owned(),
        name: name.to_string_lossy().into_owned(),
        folder: cwd_of(folder, &context.checkout),
        symbol: context
            .symbol
            .as_deref()
            .filter(|symbol| !symbol.is_empty()),
        runner,
        file,
    };
    run(provider, &target)
}

/// The tasks for the focused file, the most specific first: the test at
/// the caret, the test file, the file itself, the file as a module. Empty
/// with no focused file or one of a language without a provider.
///
/// Looks for `__init__.py` files on disk for Python; nothing else is read.
pub fn builtin_tasks(context: &RunContext, runner: TestRunner) -> Vec<RunConfig> {
    with_target(context, runner, |provider, target| {
        (provider.file_tasks)(target)
    })
}

/// What a run marker runs, the first being what a click starts and the
/// rest its menu. `context.file` is the file the marker is in. Empty when
/// the kind does not belong to the file's language or the project's test
/// runner cannot address it.
pub fn runnable_tasks(
    runnable: &Runnable,
    context: &RunContext,
    runner: TestRunner,
) -> Vec<RunConfig> {
    with_target(context, runner, |provider, target| {
        (provider.runnable_tasks)(runnable, target)
    })
}

/// The languages that have built-in tasks, for a settings pane.
pub fn task_languages() -> Vec<&'static str> {
    PROVIDERS.iter().map(|provider| provider.language).collect()
}

// Python

/// One test target: the whole file, a class, or a function (in a class).
fn python_test(target: &Target, class: Option<&str>, function: Option<&str>) -> Option<RunConfig> {
    let parts: Vec<&str> = class.into_iter().chain(function).collect();
    match target.runner {
        TestRunner::Pytest => {
            let mut node = target.relative.clone();
            for part in &parts {
                node.push_str("::");
                node.push_str(part);
            }
            let shown = match parts.is_empty() {
                true => target.name.clone(),
                false => parts.join("::"),
            };
            Some(task(
                "python",
                "pytest",
                &node,
                format!("pytest {shown}"),
                format!("$PYTHON -m pytest {}", literal(&node)),
            ))
        }
        TestRunner::Unittest => {
            // unittest has no way to name a function outside a class.
            if function.is_some() && class.is_none() {
                return None;
            }
            let Some(module) = module_path(&target.file, target.checkout) else {
                // A file that is no importable module can still be loaded
                // by path, though only whole.
                return parts.is_empty().then(|| {
                    task(
                        "python",
                        "unittest",
                        &target.relative,
                        format!("unittest {}", target.name),
                        format!("$PYTHON -m unittest {}", literal(&target.relative)),
                    )
                });
            };
            let mut name = module.module.clone();
            for part in &parts {
                name.push('.');
                name.push_str(&part.replace("::", "."));
            }
            let shown = match parts.is_empty() {
                true => target.name.clone(),
                false => parts.join(".").replace("::", "."),
            };
            Some(in_folder(
                task(
                    "python",
                    "unittest",
                    &name,
                    format!("unittest {shown}"),
                    format!("$PYTHON -m unittest {}", literal(&name)),
                ),
                cwd_of(&module.root, target.checkout),
            ))
        }
    }
}

fn python_run(target: &Target) -> Vec<RunConfig> {
    let mut tasks = vec![task(
        "python",
        "file",
        &target.relative,
        format!("Run {}", target.name),
        format!("$PYTHON {}", literal(&target.relative)),
    )];
    let in_package = target
        .file
        .parent()
        .is_some_and(|folder| folder.join("__init__.py").is_file());
    let module = module_path(&target.file, target.checkout).filter(|_| in_package);
    if let Some(module) = module {
        tasks.push(in_folder(
            task(
                "python",
                "module",
                &module.module,
                format!("Run module {}", module.module),
                format!("$PYTHON -m {}", literal(&module.module)),
            ),
            cwd_of(&module.root, target.checkout),
        ));
    }
    tasks
}

/// The test the caret's symbol names, if it names one.
fn python_symbol(symbol: &str) -> Option<Runnable> {
    let (class_name, name) = match symbol.rsplit_once("::") {
        Some((classes, name)) => (Some(classes.to_owned()), name),
        None => (None, symbol),
    };
    let kind = if name.starts_with("test") {
        RunnableKind::PythonTestFunction
    } else if name.starts_with("Test") {
        RunnableKind::PythonTestClass
    } else {
        return None;
    };
    Some(Runnable {
        kind,
        name: name.to_owned(),
        class_name,
        row: 0,
    })
}

fn python_file_tasks(target: &Target) -> Vec<RunConfig> {
    let mut tasks = Vec::new();
    if is_python_test_file(&target.file) {
        if let Some(runnable) = target.symbol.and_then(python_symbol) {
            tasks.extend(python_runnable_tasks(&runnable, target));
        }
        tasks.extend(python_test(target, None, None));
    }
    tasks.extend(python_run(target));
    tasks
}

fn python_runnable_tasks(runnable: &Runnable, target: &Target) -> Vec<RunConfig> {
    let outer = runnable
        .class_name
        .as_deref()
        .filter(|name| !name.is_empty());
    match runnable.kind {
        RunnableKind::PythonTestFunction => python_test(target, outer, Some(&runnable.name))
            .into_iter()
            .collect(),
        RunnableKind::PythonTestClass => {
            let class = match outer {
                Some(outer) => format!("{outer}::{}", runnable.name),
                None => runnable.name.clone(),
            };
            python_test(target, Some(&class), None)
                .into_iter()
                .collect()
        }
        RunnableKind::PythonMain => python_run(target),
        _ => Vec::new(),
    }
}

// Go: commands run in the file's folder, which is its package.

fn shown_folder<'a>(target: &'a Target) -> &'a str {
    match target.folder.is_empty() {
        true => ".",
        false => &target.folder,
    }
}

fn go_test(target: &Target, name: Option<&str>) -> RunConfig {
    let config = match name {
        Some(name) => task(
            "go",
            "test",
            &format!("{}:{name}", target.folder),
            format!("go test {name}"),
            format!("go test -run {} .", literal(&format!("^{name}$"))),
        ),
        None => task(
            "go",
            "test",
            &target.folder,
            format!("go test {}", shown_folder(target)),
            "go test .".into(),
        ),
    };
    in_folder(config, target.folder.clone())
}

fn go_run(target: &Target) -> RunConfig {
    in_folder(
        task(
            "go",
            "run",
            &target.folder,
            format!("go run {}", shown_folder(target)),
            "go run .".into(),
        ),
        target.folder.clone(),
    )
}

fn go_file_tasks(target: &Target) -> Vec<RunConfig> {
    match target.name.ends_with("_test.go") {
        true => vec![go_test(target, None)],
        false => vec![go_run(target)],
    }
}

fn go_runnable_tasks(runnable: &Runnable, target: &Target) -> Vec<RunConfig> {
    match runnable.kind {
        RunnableKind::GoTest => vec![go_test(target, Some(&runnable.name)), go_test(target, None)],
        RunnableKind::GoMain => vec![go_run(target)],
        _ => Vec::new(),
    }
}

// Rust: cargo finds the nearest manifest above the file's folder.

fn cargo(target: &Target, kind: &str, name: String, command: String) -> RunConfig {
    let id = match kind {
        "run" => target.folder.clone(),
        _ => format!("{}:{command}", target.folder),
    };
    in_folder(
        task("rust", kind, &id, name, command),
        target.folder.clone(),
    )
}

fn rust_file_tasks(target: &Target) -> Vec<RunConfig> {
    vec![
        cargo(target, "run", "cargo run".into(), "cargo run".into()),
        cargo(target, "test", "cargo test".into(), "cargo test".into()),
    ]
}

fn rust_runnable_tasks(runnable: &Runnable, target: &Target) -> Vec<RunConfig> {
    match runnable.kind {
        RunnableKind::RustTest => {
            let path = match runnable
                .class_name
                .as_deref()
                .filter(|name| !name.is_empty())
            {
                Some(modules) => format!("{modules}::{}", runnable.name),
                None => runnable.name.clone(),
            };
            vec![cargo(
                target,
                "test",
                format!("cargo test {}", runnable.name),
                format!("cargo test {}", literal(&path)),
            )]
        }
        RunnableKind::RustMain => {
            vec![cargo(target, "run", "cargo run".into(), "cargo run".into())]
        }
        _ => Vec::new(),
    }
}

// JavaScript and TypeScript

fn javascript_file_tasks(target: &Target) -> Vec<RunConfig> {
    let typescript = [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|extension| target.name.ends_with(extension));
    let runner = match typescript {
        true => "npx tsx",
        false => "node",
    };
    vec![task(
        "javascript",
        "file",
        &target.relative,
        format!("Run {}", target.name),
        format!("{runner} {}", literal(&target.relative)),
    )]
}

#[cfg(test)]
mod tests {
    use super::super::resolve::resolve;
    use super::*;
    use crate::code::toolchain::Toolchain;

    /// A checkout with a package, a loose test file and awkward names.
    fn checkout() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("tree");
        for file in [
            "manage.py",
            "test_top.py",
            "src/orders/__init__.py",
            "src/orders/api.py",
            "src/orders/tests/__init__.py",
            "src/orders/tests/test_api.py",
            "tests/test_loose.py",
            "my scripts/$cost report.py",
            "bad-name/__init__.py",
            "bad-name/test_x.py",
            ".venv/bin/python",
        ] {
            let path = checkout.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        (root, checkout)
    }

    fn context(checkout: &Path, file: &str) -> RunContext {
        RunContext::new(checkout, checkout).with_file(checkout.join(file))
    }

    type Shape = (&'static str, &'static str, &'static str);

    /// Name, command and cwd of each task.
    fn shape(tasks: &[RunConfig]) -> Vec<(&str, &str, &str)> {
        tasks
            .iter()
            .map(|task| (task.name.as_str(), task.command.as_str(), task.cwd.as_str()))
            .collect()
    }

    fn runnable(kind: RunnableKind, name: &str, class_name: Option<&str>) -> Runnable {
        Runnable {
            kind,
            name: name.into(),
            class_name: class_name.map(str::to_owned),
            row: 4,
        }
    }

    #[test]
    fn python_files_offer_run_and_run_as_module() {
        let (_root, checkout) = checkout();
        let cases: [(&str, &[Shape]); 3] = [
            ("manage.py", &[("Run manage.py", "$PYTHON manage.py", "")]),
            (
                "src/orders/api.py",
                &[
                    ("Run api.py", "$PYTHON src/orders/api.py", ""),
                    ("Run module orders.api", "$PYTHON -m orders.api", "src"),
                ],
            ),
            (
                "my scripts/$cost report.py",
                &[(
                    "Run $cost report.py",
                    "$PYTHON 'my scripts/$$cost report.py'",
                    "",
                )],
            ),
        ];
        for (file, expected) in cases {
            let tasks = builtin_tasks(&context(&checkout, file), TestRunner::Pytest);
            assert_eq!(shape(&tasks), expected, "{file}");
            assert!(tasks.iter().all(RunConfig::is_builtin));
        }
    }

    #[test]
    fn python_test_files_offer_the_runners_tasks_first() {
        let (_root, checkout) = checkout();
        let file = "src/orders/tests/test_api.py";
        let mut focused = context(&checkout, file);
        assert_eq!(
            shape(&builtin_tasks(&focused, TestRunner::Pytest)),
            [
                (
                    "pytest test_api.py",
                    "$PYTHON -m pytest src/orders/tests/test_api.py",
                    ""
                ),
                (
                    "Run test_api.py",
                    "$PYTHON src/orders/tests/test_api.py",
                    ""
                ),
                (
                    "Run module orders.tests.test_api",
                    "$PYTHON -m orders.tests.test_api",
                    "src"
                ),
            ]
        );
        assert_eq!(
            shape(&builtin_tasks(&focused, TestRunner::Unittest))[0],
            (
                "unittest test_api.py",
                "$PYTHON -m unittest orders.tests.test_api",
                "src"
            )
        );

        // The caret in a test puts that test first.
        focused.symbol = Some("TestOrders::test_post".into());
        assert_eq!(
            shape(&builtin_tasks(&focused, TestRunner::Pytest))[..2],
            [
                (
                    "pytest TestOrders::test_post",
                    "$PYTHON -m pytest src/orders/tests/test_api.py::TestOrders::test_post",
                    ""
                ),
                (
                    "pytest test_api.py",
                    "$PYTHON -m pytest src/orders/tests/test_api.py",
                    ""
                ),
            ]
        );
        assert_eq!(
            shape(&builtin_tasks(&focused, TestRunner::Unittest))[0],
            (
                "unittest TestOrders.test_post",
                "$PYTHON -m unittest orders.tests.test_api.TestOrders.test_post",
                "src"
            )
        );
        focused.symbol = Some("TestOrders".into());
        assert_eq!(
            builtin_tasks(&focused, TestRunner::Pytest)[0].name,
            "pytest TestOrders"
        );
        // A helper is not a test: the file's tasks only.
        focused.symbol = Some("TestOrders::make_order".into());
        assert_eq!(
            builtin_tasks(&focused, TestRunner::Pytest)[0].name,
            "pytest test_api.py"
        );
        // A test-like symbol in a file pytest would not collect is ignored.
        let mut plain = context(&checkout, "src/orders/api.py");
        plain.symbol = Some("test_connection".into());
        assert_eq!(
            builtin_tasks(&plain, TestRunner::Pytest)[0].name,
            "Run api.py"
        );
    }

    #[test]
    fn nothing_is_offered_without_a_file_or_for_an_unknown_language() {
        let (_root, checkout) = checkout();
        let unfocused = RunContext::new(&checkout, &checkout);
        assert!(builtin_tasks(&unfocused, TestRunner::Pytest).is_empty());
        assert!(builtin_tasks(&context(&checkout, "notes.txt"), TestRunner::Pytest).is_empty());
        assert!(builtin_tasks(&context(&checkout, "Makefile"), TestRunner::Pytest).is_empty());
        let marker = runnable(RunnableKind::PythonMain, "x", None);
        assert!(runnable_tasks(&marker, &unfocused, TestRunner::Pytest).is_empty());
        assert_eq!(task_languages()[0], "python");
    }

    #[test]
    fn test_files_follow_pytests_naming() {
        let cases = [
            ("test_api.py", true),
            ("api_test.py", true),
            ("pkg/tests/test_a.py", true),
            ("testing.py", false),
            ("conftest.py", false),
            ("api.py", false),
            ("test_api.txt", false),
            ("test_.pyc", false),
        ];
        for (path, expected) in cases {
            assert_eq!(is_python_test_file(Path::new(path)), expected, "{path}");
        }
    }

    #[test]
    fn python_markers_map_to_commands() {
        use RunnableKind::*;
        use TestRunner::*;
        let (_root, checkout) = checkout();
        let packaged = "src/orders/tests/test_api.py";
        let loose = "tests/test_loose.py";
        let cases: [(&str, Runnable, TestRunner, &[Shape]); 12] = [
            (
                packaged,
                runnable(PythonTestFunction, "test_post", Some("TestOrders")),
                Pytest,
                &[(
                    "pytest TestOrders::test_post",
                    "$PYTHON -m pytest src/orders/tests/test_api.py::TestOrders::test_post",
                    "",
                )],
            ),
            (
                packaged,
                runnable(PythonTestFunction, "test_plain", None),
                Pytest,
                &[(
                    "pytest test_plain",
                    "$PYTHON -m pytest src/orders/tests/test_api.py::test_plain",
                    "",
                )],
            ),
            (
                packaged,
                runnable(PythonTestClass, "TestOrders", None),
                Pytest,
                &[(
                    "pytest TestOrders",
                    "$PYTHON -m pytest src/orders/tests/test_api.py::TestOrders",
                    "",
                )],
            ),
            (
                packaged,
                runnable(PythonTestClass, "TestInner", Some("TestOuter")),
                Pytest,
                &[(
                    "pytest TestOuter::TestInner",
                    "$PYTHON -m pytest src/orders/tests/test_api.py::TestOuter::TestInner",
                    "",
                )],
            ),
            (
                packaged,
                runnable(PythonTestFunction, "test_post", Some("TestOrders")),
                Unittest,
                &[(
                    "unittest TestOrders.test_post",
                    "$PYTHON -m unittest orders.tests.test_api.TestOrders.test_post",
                    "src",
                )],
            ),
            (
                packaged,
                runnable(
                    PythonTestFunction,
                    "test_deep",
                    Some("TestOuter::TestInner"),
                ),
                Unittest,
                &[(
                    "unittest TestOuter.TestInner.test_deep",
                    "$PYTHON -m unittest orders.tests.test_api.TestOuter.TestInner.test_deep",
                    "src",
                )],
            ),
            (
                packaged,
                runnable(PythonTestClass, "TestOrders", None),
                Unittest,
                &[(
                    "unittest TestOrders",
                    "$PYTHON -m unittest orders.tests.test_api.TestOrders",
                    "src",
                )],
            ),
            // unittest cannot address a function outside a class.
            (
                packaged,
                runnable(PythonTestFunction, "test_plain", None),
                Unittest,
                &[],
            ),
            // Outside a package the module is the file, found from its folder.
            (
                loose,
                runnable(PythonTestClass, "TestLoose", None),
                Unittest,
                &[(
                    "unittest TestLoose",
                    "$PYTHON -m unittest test_loose.TestLoose",
                    "tests",
                )],
            ),
            // A package whose name is no identifier cannot be imported.
            (
                "bad-name/test_x.py",
                runnable(PythonTestClass, "TestX", None),
                Unittest,
                &[],
            ),
            (
                packaged,
                runnable(PythonMain, "test_api.py", None),
                Pytest,
                &[
                    (
                        "Run test_api.py",
                        "$PYTHON src/orders/tests/test_api.py",
                        "",
                    ),
                    (
                        "Run module orders.tests.test_api",
                        "$PYTHON -m orders.tests.test_api",
                        "src",
                    ),
                ],
            ),
            // A marker of another language's kind in a Python file.
            (packaged, runnable(GoTest, "TestX", None), Pytest, &[]),
        ];
        for (file, marker, runner, expected) in cases {
            let tasks = runnable_tasks(&marker, &context(&checkout, file), runner);
            assert_eq!(shape(&tasks), expected, "{file} {marker:?} {runner:?}");
        }
        // The whole unittest file by path when it has no module name.
        let whole = builtin_tasks(&context(&checkout, "bad-name/test_x.py"), Unittest);
        assert_eq!(
            shape(&whole)[0],
            (
                "unittest test_x.py",
                "$PYTHON -m unittest bad-name/test_x.py",
                ""
            )
        );
    }

    #[test]
    fn tasks_resolve_to_exact_arguments_in_any_checkout() {
        let (_root, checkout) = checkout();
        let toolchain = Toolchain {
            venv: Some(checkout.join(".venv")),
            ..Toolchain::default()
        };
        let python = checkout
            .join(".venv/bin/python")
            .to_string_lossy()
            .into_owned();

        let focused = context(&checkout, "my scripts/$cost report.py");
        let run = &builtin_tasks(&focused, TestRunner::Pytest)[0];
        let spawn = resolve(run, &focused, &toolchain).unwrap();
        assert_eq!(spawn.label, "Run $cost report.py");
        assert_eq!(spawn.command, python);
        assert_eq!(spawn.args, ["my scripts/$cost report.py"]);
        assert_eq!(spawn.cwd, checkout);

        // A remembered task needs no focused file, and follows the checkout.
        let focused = context(&checkout, "src/orders/tests/test_api.py");
        let marker = runnable(
            RunnableKind::PythonTestFunction,
            "test_post",
            Some("TestOrders"),
        );
        let single = &runnable_tasks(&marker, &focused, TestRunner::Unittest)[0];
        let elsewhere = RunContext::new("/other/worktree", &checkout);
        let spawn = resolve(single, &elsewhere, &toolchain).unwrap();
        assert_eq!(
            spawn.args,
            [
                "-m",
                "unittest",
                "orders.tests.test_api.TestOrders.test_post"
            ]
        );
        assert_eq!(spawn.cwd, PathBuf::from("/other/worktree/src"));

        // A file outside the checkout is named in full.
        let outside =
            RunContext::new(checkout.join("src"), &checkout).with_file(checkout.join("manage.py"));
        let run = &builtin_tasks(&outside, TestRunner::Pytest)[0];
        let spawn = resolve(run, &outside, &toolchain).unwrap();
        assert_eq!(spawn.args, [checkout.join("manage.py").to_string_lossy()]);
    }

    #[test]
    fn ids_name_the_target() {
        let (_root, checkout) = checkout();
        let focused = context(&checkout, "src/orders/tests/test_api.py");
        let ids: Vec<String> = builtin_tasks(&focused, TestRunner::Pytest)
            .into_iter()
            .map(|task| task.id)
            .collect();
        assert_eq!(
            ids,
            [
                "builtin:python:pytest:src/orders/tests/test_api.py",
                "builtin:python:file:src/orders/tests/test_api.py",
                "builtin:python:module:orders.tests.test_api",
            ]
        );
    }

    #[test]
    fn other_languages_have_their_obvious_tasks() {
        use RunnableKind::*;
        let checkout = Path::new("/work/tree");
        let file_cases: [(&str, &[Shape]); 7] = [
            (
                "cmd/api/main.go",
                &[("go run cmd/api", "go run .", "cmd/api")],
            ),
            ("main.go", &[("go run .", "go run .", "")]),
            (
                "pkg/orders/orders_test.go",
                &[("go test pkg/orders", "go test .", "pkg/orders")],
            ),
            (
                "crates/core/src/lib.rs",
                &[
                    ("cargo run", "cargo run", "crates/core/src"),
                    ("cargo test", "cargo test", "crates/core/src"),
                ],
            ),
            (
                "scripts/build.mjs",
                &[("Run build.mjs", "node scripts/build.mjs", "")],
            ),
            (
                "scripts/seed data.ts",
                &[("Run seed data.ts", "npx tsx 'scripts/seed data.ts'", "")],
            ),
            ("web/App.tsx", &[("Run App.tsx", "npx tsx web/App.tsx", "")]),
        ];
        for (file, expected) in file_cases {
            let tasks = builtin_tasks(&context(checkout, file), TestRunner::Pytest);
            assert_eq!(shape(&tasks), expected, "{file}");
        }
        let marker_cases: [(&str, Runnable, &[Shape]); 6] = [
            (
                "pkg/orders/orders_test.go",
                runnable(GoTest, "TestPost", None),
                &[
                    (
                        "go test TestPost",
                        "go test -run '^TestPost$$' .",
                        "pkg/orders",
                    ),
                    ("go test pkg/orders", "go test .", "pkg/orders"),
                ],
            ),
            (
                "cmd/api/main.go",
                runnable(GoMain, "main", None),
                &[("go run cmd/api", "go run .", "cmd/api")],
            ),
            (
                "src/lib.rs",
                runnable(RustTest, "parses", Some("tests")),
                &[("cargo test parses", "cargo test tests::parses", "src")],
            ),
            (
                "src/main.rs",
                runnable(RustMain, "main", None),
                &[("cargo run", "cargo run", "src")],
            ),
            ("src/main.rs", runnable(PythonMain, "main", None), &[]),
            ("web/app.ts", runnable(RustMain, "main", None), &[]),
        ];
        for (file, marker, expected) in marker_cases {
            let tasks = runnable_tasks(&marker, &context(checkout, file), TestRunner::Pytest);
            assert_eq!(shape(&tasks), expected, "{file} {marker:?}");
        }
        // The anchors survive splitting and substitution.
        let go = context(checkout, "pkg/orders/orders_test.go");
        let single =
            &runnable_tasks(&runnable(GoTest, "TestPost", None), &go, TestRunner::Pytest)[0];
        let spawn = resolve(single, &go, &Toolchain::default()).unwrap();
        assert_eq!(spawn.command, "go");
        assert_eq!(spawn.args, ["test", "-run", "^TestPost$", "."]);
        assert_eq!(spawn.cwd, PathBuf::from("/work/tree/pkg/orders"));
    }
}
