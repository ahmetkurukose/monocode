//! The variables a run configuration may use (`$FILE`, `$SYMBOL`, ...), the
//! context their values come from, and the substitution itself. A variable
//! the context has no value for makes the configuration unavailable instead
//! of running a half-filled command.
use super::resolve::Unresolved;
use crate::code::toolchain::Toolchain;
use std::cell::OnceCell;
use std::path::{Path, PathBuf};

/// What the caller knows when something is run: the checkout, and where the
/// caret is if a file is focused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunContext {
    /// The worktree the run happens in.
    pub checkout: PathBuf,
    /// The project the checkout belongs to; equal to `checkout` for the
    /// project's own folder.
    pub project: PathBuf,
    /// The focused file. A relative path is taken from the checkout.
    pub file: Option<PathBuf>,
    /// The symbol at the caret, outermost first: `TestOrders::test_post`.
    pub symbol: Option<String>,
    /// The caret's line and column as the editor shows them (from 1).
    pub row: Option<u32>,
    pub column: Option<u32>,
    pub selected_text: Option<String>,
}

impl RunContext {
    /// A context with no focused file.
    pub fn new(checkout: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            checkout: checkout.into(),
            project: project.into(),
            ..Self::default()
        }
    }

    /// The same context with `file` focused and no caret.
    pub fn with_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// The focused file as an absolute path.
    pub fn absolute_file(&self) -> Option<PathBuf> {
        self.file.as_ref().map(|file| match file.is_absolute() {
            true => file.clone(),
            false => self.checkout.join(file),
        })
    }

    /// The focused file from the checkout; absolute when it is outside it.
    pub fn relative_file(&self) -> Option<PathBuf> {
        let file = self.absolute_file()?;
        Some(match file.strip_prefix(&self.checkout) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => file,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variable {
    File,
    RelativeFile,
    FileDir,
    FileName,
    FileStem,
    Checkout,
    Project,
    Symbol,
    Row,
    Column,
    SelectedText,
    Python,
    Module,
}

impl Variable {
    pub const ALL: [Variable; 13] = [
        Variable::File,
        Variable::RelativeFile,
        Variable::FileDir,
        Variable::FileName,
        Variable::FileStem,
        Variable::Checkout,
        Variable::Project,
        Variable::Symbol,
        Variable::Row,
        Variable::Column,
        Variable::SelectedText,
        Variable::Python,
        Variable::Module,
    ];

    /// The name as typed after the dollar sign.
    pub fn name(self) -> &'static str {
        match self {
            Variable::File => "FILE",
            Variable::RelativeFile => "RELATIVE_FILE",
            Variable::FileDir => "FILE_DIR",
            Variable::FileName => "FILE_NAME",
            Variable::FileStem => "FILE_STEM",
            Variable::Checkout => "CHECKOUT",
            Variable::Project => "PROJECT",
            Variable::Symbol => "SYMBOL",
            Variable::Row => "ROW",
            Variable::Column => "COLUMN",
            Variable::SelectedText => "SELECTED_TEXT",
            Variable::Python => "PYTHON",
            Variable::Module => "MODULE",
        }
    }

    /// One line for the settings pane's list of variables.
    pub fn description(self) -> &'static str {
        match self {
            Variable::File => "The focused file, as an absolute path",
            Variable::RelativeFile => "The focused file, from the checkout",
            Variable::FileDir => "The folder of the focused file",
            Variable::FileName => "The focused file's name",
            Variable::FileStem => "The focused file's name without its extension",
            Variable::Checkout => "The worktree the command runs in",
            Variable::Project => "The project's own folder",
            Variable::Symbol => "The symbol at the caret, such as TestOrders::test_post",
            Variable::Row => "The caret's line",
            Variable::Column => "The caret's column",
            Variable::SelectedText => "The selected text",
            Variable::Python => "The checkout's Python interpreter",
            Variable::Module => "The focused Python file as a dotted module path",
        }
    }

    /// Why a configuration that uses this variable cannot run right now.
    pub fn missing(self) -> &'static str {
        match self {
            Variable::File
            | Variable::RelativeFile
            | Variable::FileDir
            | Variable::FileName
            | Variable::FileStem => "No file is focused.",
            Variable::Checkout | Variable::Project => "No checkout is open.",
            Variable::Symbol => "The caret is not on a symbol.",
            Variable::Row | Variable::Column => "The caret's position is not known.",
            Variable::SelectedText => "No text is selected.",
            Variable::Python => "No Python interpreter was found for this checkout.",
            Variable::Module => "The focused file is not a Python module.",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|variable| variable.name() == name)
    }
}

/// A Python file as `python -m` wants it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModulePath {
    /// `orders.tests.test_api`
    pub module: String,
    /// The folder the module path starts from, which must be the working
    /// directory (or on `sys.path`) for the module to be found.
    pub root: PathBuf,
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The dotted module path of `file`: its folders are followed upwards for
/// as long as they are packages (hold an `__init__.py`), stopping at the
/// checkout at the latest. `__init__.py` and `__main__.py` name their
/// package. None for a file that is not Python or whose path has a part
/// that is not an identifier.
pub fn module_path(file: &Path, checkout: &Path) -> Option<ModulePath> {
    if file.extension()?.to_str()? != "py" {
        return None;
    }
    let stem = file.file_stem()?.to_str()?;
    let mut parts = Vec::new();
    if stem != "__init__" && stem != "__main__" {
        parts.push(stem.to_owned());
    }
    let mut folder = file.parent()?;
    while folder != checkout && folder.join("__init__.py").is_file() {
        parts.push(folder.file_name()?.to_str()?.to_owned());
        folder = folder.parent()?;
    }
    if parts.is_empty() || !parts.iter().all(|part| is_identifier(part)) {
        return None;
    }
    parts.reverse();
    Some(ModulePath {
        module: parts.join("."),
        root: folder.to_path_buf(),
    })
}

/// Replaces `$NAME` and `${NAME}` with what `lookup` gives for them; `$$`
/// is one dollar sign. A dollar sign before anything that is not one of
/// our variables is left as typed, so `awk '{print $1}'` still works.
pub(crate) fn substitute(
    text: &str,
    lookup: &mut dyn FnMut(Variable) -> Result<String, Unresolved>,
) -> Result<String, Unresolved> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }
        let (name, tail) = match after.strip_prefix('{') {
            Some(inner) => match inner.find('}') {
                Some(end) => (&inner[..end], &inner[end + 1..]),
                None => ("", after),
            },
            None => {
                let length = after
                    .bytes()
                    .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    .count();
                (&after[..length], &after[length..])
            }
        };
        match Variable::named(name) {
            Some(variable) => {
                out.push_str(&lookup(variable)?);
                rest = tail;
            }
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// The variables `text` uses, in the order they first appear.
pub(crate) fn used(text: &str, into: &mut Vec<Variable>) {
    let _ = substitute(text, &mut |variable| {
        if !into.contains(&variable) {
            into.push(variable);
        }
        Ok(String::new())
    });
}

/// The values of one context. The interpreter is looked up on disk, so it
/// is found once and only when a command asks for it.
pub(crate) struct Values<'a> {
    context: &'a RunContext,
    toolchain: &'a Toolchain,
    python: OnceCell<Option<String>>,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl<'a> Values<'a> {
    pub(crate) fn new(context: &'a RunContext, toolchain: &'a Toolchain) -> Self {
        Self {
            context,
            toolchain,
            python: OnceCell::new(),
        }
    }

    pub(crate) fn python(&self) -> Option<String> {
        self.python
            .get_or_init(|| self.toolchain.python().map(|path| text(&path)))
            .clone()
    }

    fn value(&self, variable: Variable) -> Option<String> {
        let context = self.context;
        let file = || context.absolute_file();
        let filled = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
        match variable {
            Variable::File => file().map(|file| text(&file)),
            Variable::RelativeFile => context.relative_file().map(|file| text(&file)),
            Variable::FileDir => file()?.parent().map(text),
            Variable::FileName => Some(file()?.file_name()?.to_string_lossy().into_owned()),
            Variable::FileStem => Some(file()?.file_stem()?.to_string_lossy().into_owned()),
            Variable::Checkout => {
                (!context.checkout.as_os_str().is_empty()).then(|| text(&context.checkout))
            }
            Variable::Project => {
                (!context.project.as_os_str().is_empty()).then(|| text(&context.project))
            }
            Variable::Symbol => filled(&context.symbol),
            Variable::Row => context.row.map(|row| row.to_string()),
            Variable::Column => context.column.map(|column| column.to_string()),
            Variable::SelectedText => filled(&context.selected_text),
            Variable::Python => self.python(),
            Variable::Module => {
                module_path(&file()?, &context.checkout).map(|module| module.module)
            }
        }
    }

    pub(crate) fn substitute(&self, text: &str) -> Result<String, Unresolved> {
        substitute(text, &mut |variable| {
            self.value(variable).ok_or(Unresolved::Needs(variable))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RunContext {
        RunContext {
            checkout: "/work/tree".into(),
            project: "/work/project".into(),
            file: Some("/work/tree/my app/test_orders.py".into()),
            symbol: Some("TestOrders::test_post".into()),
            row: Some(12),
            column: Some(5),
            selected_text: Some("a b".into()),
        }
    }

    fn toolchain() -> Toolchain {
        Toolchain::default()
    }

    #[test]
    fn variables_are_substituted_in_both_forms() {
        let context = context();
        let toolchain = toolchain();
        let values = Values::new(&context, &toolchain);
        let cases = [
            ("$FILE", "/work/tree/my app/test_orders.py"),
            ("${FILE}", "/work/tree/my app/test_orders.py"),
            ("$RELATIVE_FILE", "my app/test_orders.py"),
            ("$FILE_DIR", "/work/tree/my app"),
            ("$FILE_NAME", "test_orders.py"),
            ("$FILE_STEM", "test_orders"),
            ("$CHECKOUT", "/work/tree"),
            ("$PROJECT", "/work/project"),
            ("$SYMBOL", "TestOrders::test_post"),
            ("$ROW:$COLUMN", "12:5"),
            ("$SELECTED_TEXT", "a b"),
            (
                "$RELATIVE_FILE::$SYMBOL",
                "my app/test_orders.py::TestOrders::test_post",
            ),
            ("--name=${FILE_STEM}_out", "--name=test_orders_out"),
            ("$FILE_STEM.log", "test_orders.log"),
            // A literal dollar sign, and one before a variable name.
            ("cost: $$5", "cost: $5"),
            ("$$FILE", "$FILE"),
            ("$$$FILE_NAME", "$test_orders.py"),
            // Not ours: left as typed.
            ("$HOME/$1 ${USER} $", "$HOME/$1 ${USER} $"),
            ("$FILES ${FILE", "$FILES ${FILE"),
            ("$file", "$file"),
            ("", ""),
        ];
        for (typed, expected) in cases {
            assert_eq!(values.substitute(typed).as_deref(), Ok(expected), "{typed}");
        }
    }

    #[test]
    fn a_variable_without_a_value_is_named() {
        let context = RunContext::new("/work/tree", "/work/tree");
        let toolchain = toolchain();
        let values = Values::new(&context, &toolchain);
        let cases = [
            ("$FILE", Variable::File),
            ("x ${RELATIVE_FILE}", Variable::RelativeFile),
            ("$FILE_DIR", Variable::FileDir),
            ("$FILE_NAME", Variable::FileName),
            ("$FILE_STEM", Variable::FileStem),
            ("$SYMBOL", Variable::Symbol),
            ("$ROW", Variable::Row),
            ("$COLUMN", Variable::Column),
            ("$SELECTED_TEXT", Variable::SelectedText),
            ("$PYTHON", Variable::Python),
            ("$MODULE", Variable::Module),
            // The first missing one is reported.
            ("$CHECKOUT $SYMBOL $FILE", Variable::Symbol),
        ];
        for (typed, variable) in cases {
            assert_eq!(
                values.substitute(typed),
                Err(Unresolved::Needs(variable)),
                "{typed}"
            );
        }
        assert_eq!(
            values.substitute("$CHECKOUT/$$x").as_deref(),
            Ok("/work/tree/$x")
        );
        // An empty selection or symbol is no value.
        let empty = RunContext {
            symbol: Some(String::new()),
            selected_text: Some(String::new()),
            ..context.clone()
        };
        let values = Values::new(&empty, &toolchain);
        assert_eq!(
            values.substitute("$SELECTED_TEXT"),
            Err(Unresolved::Needs(Variable::SelectedText))
        );
        assert_eq!(
            values.substitute("$SYMBOL"),
            Err(Unresolved::Needs(Variable::Symbol))
        );
    }

    #[test]
    fn a_relative_or_outside_file_is_placed() {
        let inside = RunContext::new("/work/tree", "/work/tree").with_file("src/a.py");
        assert_eq!(inside.absolute_file(), Some("/work/tree/src/a.py".into()));
        assert_eq!(inside.relative_file(), Some("src/a.py".into()));
        let outside = RunContext::new("/work/tree", "/work/tree").with_file("/else/a.py");
        assert_eq!(outside.relative_file(), Some("/else/a.py".into()));
    }

    #[test]
    fn used_variables_are_listed_once_in_order() {
        let mut found = Vec::new();
        used(
            "$PYTHON -m pytest $RELATIVE_FILE $$FILE ${PYTHON} $HOME",
            &mut found,
        );
        assert_eq!(found, [Variable::Python, Variable::RelativeFile]);
    }

    #[test]
    fn every_variable_has_a_unique_name() {
        for variable in Variable::ALL {
            assert_eq!(Variable::named(variable.name()), Some(variable));
            assert!(!variable.description().is_empty());
            assert!(variable.missing().ends_with('.'));
        }
    }

    #[test]
    fn module_paths_follow_packages_up_to_the_checkout() {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("tree");
        let touch = |path: &str| {
            let path = checkout.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        };
        for file in [
            "__init__.py",
            "script.py",
            "src/orders/__init__.py",
            "src/orders/__main__.py",
            "src/orders/api/__init__.py",
            "src/orders/api/views.py",
            "src/loose.py",
            "pkg/__init__.py",
            "pkg/mod.py",
            "bad-name/__init__.py",
            "bad-name/mod.py",
            "notes/read me.py",
            "notes/data.txt",
        ] {
            touch(file);
        }
        let cases: [(&str, Option<(&str, &str)>); 10] = [
            ("src/orders/api/views.py", Some(("orders.api.views", "src"))),
            ("src/orders/api/__init__.py", Some(("orders.api", "src"))),
            ("src/orders/__main__.py", Some(("orders", "src"))),
            ("src/loose.py", Some(("loose", "src"))),
            // The checkout is the limit even when it is a package itself.
            ("pkg/mod.py", Some(("pkg.mod", ""))),
            ("script.py", Some(("script", ""))),
            ("__init__.py", None),
            ("bad-name/mod.py", None),
            ("notes/read me.py", None),
            ("notes/data.txt", None),
        ];
        for (file, expected) in cases {
            let found = module_path(&checkout.join(file), &checkout);
            let expected = expected.map(|(module, root)| ModulePath {
                module: module.into(),
                root: match root {
                    "" => checkout.clone(),
                    root => checkout.join(root),
                },
            });
            assert_eq!(found, expected, "{file}");
        }
    }
}
