//! The manager against the real basedpyright and ruff. These need a Python
//! environment with both installed: point `MONOCODE_TEST_PYTHON` at its
//! interpreter. Without it they pass without doing anything.
use super::client::CancelToken;
use super::manager::{DocumentContext, Manager, ManagerOptions};
use super::position::LineIndex;
use super::types::{LspEvent, ServerState, Severity, SymbolKind};
use crate::code::toolchain::Toolchain;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SHAPES: &str = r#"class Circle:
    """A circle."""

    def __init__(self, radius: float) -> None:
        self.radius = radius

    def area(self) -> float:
        """The area of the circle."""
        return 3.14159 * self.radius**2


def describe(shape: Circle, label: str) -> str:
    """Say what a shape is."""
    return f"{label}: {shape.area()}"
"#;

const MAIN: &str = r#"import os
from shapes import Circle, describe

circle = Circle(2.0)
text: int = describe(circle, "label")
note = "ş😀"; print(circle.area())
"#;

struct Project {
    _folder: tempfile::TempDir,
    root: PathBuf,
    context: DocumentContext,
}

/// A two-file project and the toolchain of the test interpreter's venv.
fn project() -> Option<Project> {
    let python = PathBuf::from(std::env::var_os("MONOCODE_TEST_PYTHON")?);
    let bin = python.parent()?.to_path_buf();
    let folder = tempfile::tempdir().ok()?;
    let root = folder.path().canonicalize().ok()?;
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"sample\"\nversion = \"0.1.0\"\n",
    )
    .ok()?;
    std::fs::write(root.join("shapes.py"), SHAPES).ok()?;
    std::fs::write(root.join("main.py"), MAIN).ok()?;
    let mut folders = vec![bin.clone()];
    folders.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let search_path = std::env::join_paths(folders)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let context = DocumentContext {
        checkout: root.clone(),
        project: root.clone(),
        toolchain: Toolchain {
            venv: bin.parent().map(Path::to_path_buf),
            environment: vec![("PATH".into(), search_path.clone())],
            search_path,
        },
    };
    Some(Project {
        _folder: folder,
        root,
        context,
    })
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

const PATIENCE: Duration = Duration::from_secs(90);

#[test]
fn real_basedpyright_and_ruff_answer_for_a_small_project() {
    let Some(project) = project() else {
        return;
    };
    let options = ManagerOptions {
        request_timeout: Duration::from_secs(60),
        ..ManagerOptions::default()
    };
    let manager = Manager::new(options);
    let events = manager.subscribe();
    let main = project.root.join("main.py");
    let shapes = project.root.join("shapes.py");
    let none = CancelToken::new();
    manager.open(&main, MAIN, &project.context);

    // A type error from basedpyright and a lint from ruff, each tagged.
    let found = manager.diagnostics_settled(&main, PATIENCE).unwrap();
    let type_error = found
        .iter()
        .find(|diagnostic| diagnostic.server == "basedpyright" && diagnostic.range.start.line == 4)
        .unwrap_or_else(|| panic!("no type error in {found:#?}"));
    assert_eq!(type_error.severity, Severity::Error);
    assert_eq!(type_error.code.as_deref(), Some("reportAssignmentType"));
    assert!(type_error.message.contains("int"), "{type_error:?}");
    // The squiggle is under the call, `describe(circle, "label")`.
    let index = LineIndex::new(MAIN);
    let start = index.offset_of(MAIN, type_error.range.start);
    let end = index.offset_of(MAIN, type_error.range.end);
    assert_eq!(&MAIN[start..end], "describe(circle, \"label\")");
    // The user's own ruff settings may add rules; the unused import is
    // reported under any of them.
    let lint = found
        .iter()
        .find(|diagnostic| {
            diagnostic.server == "ruff" && diagnostic.code.as_deref() == Some("F401")
        })
        .unwrap_or_else(|| panic!("no lint in {found:#?}"));
    assert_eq!(lint.range.start.line, 0);
    assert!(lint.message.contains("os"), "{lint:?}");
    let told = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| event == LspEvent::DiagnosticsChanged { path: main.clone() });
    assert!(told);

    // Both are running as real processes, rooted at the project.
    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 2);
    for status in &statuses {
        assert!(
            matches!(
                status.state,
                ServerState::Ready | ServerState::Indexing { .. }
            ),
            "{status:?}"
        );
        assert_eq!(status.root, project.root);
        assert!(status.pid.is_some_and(alive));
    }

    // Definition in the other file, asked from behind an emoji.
    let area = MAIN.rfind("area").unwrap();
    let definition = manager.definition(&main, area, &none).unwrap();
    assert_eq!(definition.len(), 1, "{definition:?}");
    assert_eq!(definition[0].path, shapes);
    let shapes_index = LineIndex::new(SHAPES);
    let at = shapes_index.offset_of(SHAPES, definition[0].range.start);
    assert!(SHAPES[at..].starts_with("area(self)"));

    // References to `Circle` are found in both files.
    let circle = MAIN.find("Circle(2.0)").unwrap();
    let references = manager.references(&main, circle, true, &none).unwrap();
    assert!(references.iter().any(|location| location.path == shapes));
    assert!(
        references
            .iter()
            .filter(|location| location.path == main)
            .count()
            >= 2,
        "{references:?}"
    );

    let hover = manager
        .hover(&main, MAIN.find("describe(circle").unwrap(), &none)
        .unwrap()
        .expect("a hover");
    assert!(hover.contents.contains("describe"), "{hover:?}");
    assert!(hover.contents.contains("Say what a shape is."), "{hover:?}");

    // Members of `circle`, after typing a dot.
    let typing = format!("{MAIN}circle.");
    manager.change(&main, &typing);
    let list = manager
        .completion(&main, typing.len(), Some('.'), &none)
        .unwrap();
    let labels: Vec<&str> = list.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"area") && labels.contains(&"radius"),
        "{labels:?}"
    );
    let item = list.items.iter().find(|item| item.label == "area").unwrap();
    assert_eq!(item.insert_text, "area");
    let resolved = manager.resolve_completion(&main, item, &none).unwrap();
    let documentation = resolved.documentation.unwrap_or_default();
    assert!(
        documentation.contains("The area of the circle."),
        "{documentation}"
    );
    assert!(manager
        .triggers(&main)
        .completion
        .contains(&".".to_string()));

    // Signature help inside a call being typed.
    let calling = format!("{MAIN}describe(circle, ");
    manager.change(&main, &calling);
    let help = manager
        .signature_help(&main, calling.len(), &none)
        .unwrap()
        .expect("signature help");
    let signature = &help.signatures[help.active_signature];
    assert!(signature.label.contains("shape: Circle"), "{signature:?}");
    assert_eq!(signature.active_parameter, Some(1));
    let (from, to) = signature.parameters[1].label_range.unwrap();
    assert_eq!(&signature.label[from..to], "label: str");

    // A one-off question about a file nobody opened: its outline.
    let symbols = manager
        .with_file(&shapes, &project.context, |manager| {
            manager.document_symbols(&shapes, &none)
        })
        .unwrap()
        .unwrap();
    let class = symbols
        .iter()
        .find(|symbol| symbol.name == "Circle")
        .unwrap();
    assert_eq!(class.kind, SymbolKind::Class);
    assert!(class.children.iter().any(|child| child.name == "area"));
    assert!(symbols.iter().any(|symbol| symbol.name == "describe"));
    assert!(!manager.is_open(&shapes));

    // Fixing the file clears the diagnostics.
    let fixed = MAIN
        .replace("import os\n", "")
        .replace("text: int", "text: str");
    manager.change(&main, &fixed);
    let found = manager.diagnostics_settled(&main, PATIENCE).unwrap();
    assert!(
        found
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error
                && diagnostic.code.as_deref() != Some("F401")),
        "{found:#?}"
    );

    // Dropping the manager takes the processes, children included, with it.
    let pids: Vec<u32> = statuses.iter().filter_map(|status| status.pid).collect();
    drop(manager);
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|pid| alive(*pid)) {
        assert!(Instant::now() < deadline, "servers still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn real_servers_stop_when_idle_and_missing_ones_are_named() {
    let Some(project) = project() else {
        return;
    };
    let manager = Manager::new(ManagerOptions {
        idle_period: Duration::ZERO,
        ..ManagerOptions::default()
    });
    let main = project.root.join("main.py");
    manager.open(&main, MAIN, &project.context);
    manager.diagnostics_settled(&main, PATIENCE).unwrap();
    let pids: Vec<u32> = manager
        .statuses()
        .iter()
        .filter_map(|status| status.pid)
        .collect();
    assert_eq!(pids.len(), 2);
    manager.close(&main);
    manager.sweep();
    assert!(manager.statuses().is_empty());
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|pid| alive(*pid)) {
        assert!(Instant::now() < deadline, "servers still running");
        std::thread::sleep(Duration::from_millis(20));
    }

    // With nothing on the path, both are reported missing by name.
    let mut context = project.context.clone();
    context.toolchain.search_path = project.root.join("nowhere").to_string_lossy().into_owned();
    manager.open(&main, MAIN, &context);
    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 2);
    for status in statuses {
        let ServerState::ToolNotFound { looked_for, hint } = status.state else {
            panic!("{status:?}");
        };
        assert!(!looked_for.is_empty());
        assert!(hint.contains("pip install"));
    }
}
