//! What the syntax tree says about a file's structure: its outline (classes,
//! functions and the like with their nesting), the chain of them around an
//! offset, and the lines of a Python file that can be run. Plain data from
//! the Tree-sitter grammars the editor already loads; nothing here needs a
//! language server.
use crate::editor_syntax::{self, Session};
use std::{ops::Range, sync::Mutex};
use tree_sitter::{Node, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    /// A Rust `impl` block, named by its type.
    Impl,
    Module,
}
impl SymbolKind {
    /// Whether functions declared directly inside are methods.
    fn holds_methods(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Struct | Self::Interface | Self::Impl | Self::Enum
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The whole declaration, body included.
    pub range: Range<usize>,
    /// Just the name, where a jump to the symbol puts the caret.
    pub name_range: Range<usize>,
    /// Zero-based row of the declaration's first line.
    pub row: usize,
    /// How many symbols enclose it.
    pub depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunnableKind {
    TestFunction,
    TestClass,
    TestMethod,
    /// The `if __name__ == "__main__":` line.
    Main,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Runnable {
    pub kind: RunnableKind,
    pub name: String,
    /// The class of a test method.
    pub class_name: Option<String>,
    /// Zero-based row of the `def`, `class` or `if` line.
    pub row: usize,
}

/// The outline of a file in source order, parsed from scratch. Empty for a
/// language without a grammar or without symbol rules.
pub(crate) fn outline(path: &str, text: &str) -> Vec<Symbol> {
    let Some(mut session) = Session::new(path) else {
        return Vec::new();
    };
    match (session.tree(text), editor_syntax::language_id(path)) {
        (Some(tree), Some(language)) => outline_in(&tree, language, text),
        _ => Vec::new(),
    }
}

/// The outline from an editor's own session, which already has the tree.
pub(crate) fn outline_with(
    session: &Mutex<Option<Session>>,
    path: &str,
    text: &str,
) -> Vec<Symbol> {
    editor_syntax::with_tree(session, path, text, |tree, language| {
        outline_in(tree, language, text)
    })
    .unwrap_or_default()
}

/// The symbols around `offset`, outermost first: `[class, method]`.
pub(crate) fn chain_at(symbols: &[Symbol], offset: usize) -> Vec<&Symbol> {
    let mut chain: Vec<&Symbol> = Vec::new();
    for symbol in symbols {
        if symbol.range.start > offset {
            break;
        }
        if offset <= symbol.range.end {
            // Source order lists a parent before its children, so a symbol
            // at the depth of the last link replaces nothing: it nests.
            chain.truncate(symbol.depth);
            chain.push(symbol);
        }
    }
    chain
}

/// The chain around `offset` in one call, for callers without an outline.
pub(crate) fn enclosing(path: &str, text: &str, offset: usize) -> Vec<Symbol> {
    let symbols = outline(path, text);
    chain_at(&symbols, offset).into_iter().cloned().collect()
}

/// What pytest and unittest would collect from a Python file, and its
/// `__main__` guard. The file's name is not judged here: pytest only looks
/// in `test_*.py` and `*_test.py` unless configured otherwise.
pub(crate) fn python_runnables(text: &str) -> Vec<Runnable> {
    Session::new("file.py")
        .and_then(|mut session| session.tree(text))
        .map(|tree| python_runnables_in(&tree, text))
        .unwrap_or_default()
}

pub(crate) fn python_runnables_with(
    session: &Mutex<Option<Session>>,
    path: &str,
    text: &str,
) -> Vec<Runnable> {
    editor_syntax::with_tree(session, path, text, |tree, language| {
        if language == "python" {
            python_runnables_in(tree, text)
        } else {
            Vec::new()
        }
    })
    .unwrap_or_default()
}

fn text_of<'a>(node: Node, text: &'a str) -> &'a str {
    text.get(node.byte_range()).unwrap_or("")
}

/// The name node of a C declarator, under its pointers and parameter lists.
fn c_declarator_name(mut node: Node) -> Option<Node> {
    for _ in 0..8 {
        match node.kind() {
            "identifier"
            | "field_identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name"
            | "type_identifier" => return Some(node),
            _ => node = node.child_by_field_name("declarator")?,
        }
    }
    None
}

fn classify<'t>(
    node: Node<'t>,
    language: &str,
    parent: Option<SymbolKind>,
) -> Option<(SymbolKind, Node<'t>)> {
    let name = || node.child_by_field_name("name");
    let function = if parent.is_some_and(SymbolKind::holds_methods) {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };
    let kind = node.kind();
    Some(match language {
        "python" => match kind {
            "function_definition" => (function, name()?),
            "class_definition" => (SymbolKind::Class, name()?),
            _ => return None,
        },
        "javascript" | "typescript" | "typescriptreact" => match kind {
            "function_declaration" | "generator_function_declaration" => (function, name()?),
            "class_declaration" | "abstract_class_declaration" => (SymbolKind::Class, name()?),
            "method_definition" => (SymbolKind::Method, name()?),
            "interface_declaration" => (SymbolKind::Interface, name()?),
            "enum_declaration" => (SymbolKind::Enum, name()?),
            "variable_declarator"
                if node.child_by_field_name("value").is_some_and(|value| {
                    matches!(
                        value.kind(),
                        "arrow_function" | "function_expression" | "function"
                    )
                }) =>
            {
                (function, name().filter(|name| name.kind() == "identifier")?)
            }
            _ => return None,
        },
        "rust" => match kind {
            "function_item" => (function, name()?),
            "struct_item" => (SymbolKind::Struct, name()?),
            "enum_item" => (SymbolKind::Enum, name()?),
            "trait_item" => (SymbolKind::Interface, name()?),
            "impl_item" => (SymbolKind::Impl, node.child_by_field_name("type")?),
            "mod_item" => (SymbolKind::Module, name()?),
            _ => return None,
        },
        "go" => match kind {
            "function_declaration" => (SymbolKind::Function, name()?),
            "method_declaration" => (SymbolKind::Method, name()?),
            "type_spec" => (
                if node
                    .child_by_field_name("type")
                    .is_some_and(|ty| ty.kind() == "interface_type")
                {
                    SymbolKind::Interface
                } else {
                    SymbolKind::Struct
                },
                name()?,
            ),
            _ => return None,
        },
        "java" => match kind {
            "class_declaration" => (SymbolKind::Class, name()?),
            "interface_declaration" => (SymbolKind::Interface, name()?),
            "enum_declaration" => (SymbolKind::Enum, name()?),
            "method_declaration" | "constructor_declaration" => (SymbolKind::Method, name()?),
            _ => return None,
        },
        "c" | "cpp" => match kind {
            "function_definition" => (
                function,
                c_declarator_name(node.child_by_field_name("declarator")?)?,
            ),
            "class_specifier" if node.child_by_field_name("body").is_some() => {
                (SymbolKind::Class, name()?)
            }
            "struct_specifier" if node.child_by_field_name("body").is_some() => {
                (SymbolKind::Struct, name()?)
            }
            "enum_specifier" if node.child_by_field_name("body").is_some() => {
                (SymbolKind::Enum, name()?)
            }
            "namespace_definition" => (SymbolKind::Module, name()?),
            _ => return None,
        },
        "ruby" => match kind {
            "method" | "singleton_method" => (function, name()?),
            "class" => (SymbolKind::Class, name()?),
            "module" => (SymbolKind::Module, name()?),
            _ => return None,
        },
        _ => return None,
    })
}

pub(crate) fn outline_in(tree: &Tree, language: &str, text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    // The symbols the walk is inside of: where each ends, and its kind.
    let mut open: Vec<(usize, SymbolKind)> = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        while open
            .last()
            .is_some_and(|(end, _)| *end <= node.start_byte())
        {
            open.pop();
        }
        if let Some((kind, name)) = classify(node, language, open.last().map(|(_, kind)| *kind)) {
            symbols.push(Symbol {
                name: text_of(name, text).to_owned(),
                kind,
                range: node.byte_range(),
                name_range: name.byte_range(),
                row: node.start_position().row,
                depth: open.len(),
            });
            open.push((node.end_byte(), kind));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return symbols;
            }
        }
    }
}

/// A definition under its decorators.
fn undecorated(node: Node) -> Node {
    if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    }
}

fn python_runnables_in(tree: &Tree, text: &str) -> Vec<Runnable> {
    let mut runnables = Vec::new();
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        let node = undecorated(child);
        let name = node
            .child_by_field_name("name")
            .map(|name| text_of(name, text));
        match (node.kind(), name) {
            ("function_definition", Some(name)) if name.starts_with("test_") => {
                runnables.push(Runnable {
                    kind: RunnableKind::TestFunction,
                    name: name.to_owned(),
                    class_name: None,
                    row: node.start_position().row,
                });
            }
            ("class_definition", Some(class)) => {
                let unittest = node
                    .child_by_field_name("superclasses")
                    .is_some_and(|bases| {
                        let mut cursor = bases.walk();
                        bases
                            .named_children(&mut cursor)
                            .any(|base| text_of(base, text).ends_with("TestCase"))
                    });
                let Some(body) = node.child_by_field_name("body") else {
                    continue;
                };
                let mut cursor = body.walk();
                let methods: Vec<(Node, &str)> = body
                    .named_children(&mut cursor)
                    .map(undecorated)
                    .filter(|method| method.kind() == "function_definition")
                    .filter_map(|method| {
                        let name = method.child_by_field_name("name")?;
                        Some((method, text_of(name, text)))
                    })
                    .collect();
                // pytest skips a Test* class that has a constructor.
                let pytest = class.starts_with("Test")
                    && !methods.iter().any(|(_, name)| *name == "__init__");
                if !unittest && !pytest {
                    continue;
                }
                runnables.push(Runnable {
                    kind: RunnableKind::TestClass,
                    name: class.to_owned(),
                    class_name: None,
                    row: node.start_position().row,
                });
                // unittest collects every method named test*.
                let prefix = if unittest { "test" } else { "test_" };
                for (method, name) in methods {
                    if name.starts_with(prefix) {
                        runnables.push(Runnable {
                            kind: RunnableKind::TestMethod,
                            name: name.to_owned(),
                            class_name: Some(class.to_owned()),
                            row: method.start_position().row,
                        });
                    }
                }
            }
            ("if_statement", _) => {
                let guard: String = node
                    .child_by_field_name("condition")
                    .map(|condition| text_of(condition, text))
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .map(|c| if c == '\'' { '"' } else { c })
                    .collect();
                if guard == "__name__==\"__main__\"" || guard == "\"__main__\"==__name__" {
                    runnables.push(Runnable {
                        kind: RunnableKind::Main,
                        name: "__main__".into(),
                        class_name: None,
                        row: node.start_position().row,
                    });
                }
            }
            _ => {}
        }
    }
    runnables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(symbols: &[Symbol]) -> Vec<(&str, SymbolKind, usize)> {
        symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.kind, symbol.depth))
            .collect()
    }

    const PYTHON: &str = r#"import unittest

def helper():
    def inner():
        return 1
    return inner

class Shape:
    def __init__(self):
        self.sides = 0

    @property
    def area(self):
        return 0

def test_top_level():
    assert helper()

async def test_async():
    pass

class TestShape:
    def test_area(self):
        assert Shape().area == 0

    def helper(self):
        pass

    @staticmethod
    def test_static():
        pass

class TestWithInit:
    def __init__(self):
        pass

    def test_ignored(self):
        pass

class Legacy(unittest.TestCase):
    def setUp(self):
        pass

    def testOld(self):
        pass

if __name__ == '__main__':
    unittest.main()
"#;

    #[test]
    fn python_outline_nests_functions_and_methods() {
        let symbols = outline("shape.py", PYTHON);
        use SymbolKind::*;
        assert_eq!(
            &names(&symbols)[..6],
            [
                ("helper", Function, 0),
                ("inner", Function, 1),
                ("Shape", Class, 0),
                ("__init__", Method, 1),
                ("area", Method, 1),
                ("test_top_level", Function, 0),
            ]
        );
        let area = &symbols[4];
        assert_eq!(&PYTHON[area.name_range.clone()], "area");
        assert_eq!(area.row, 12, "the def line, below its decorator");
        let inside = PYTHON.find("return 0").unwrap();
        let chain: Vec<_> = chain_at(&symbols, inside)
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(chain, ["Shape", "area"]);
        assert!(chain_at(&symbols, 0).is_empty());
        let chain = enclosing("shape.py", PYTHON, PYTHON.find("return 1").unwrap());
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[1].name, "inner");
    }

    #[test]
    fn python_runnables_follow_pytest_and_unittest_rules() {
        let found: Vec<_> = python_runnables(PYTHON)
            .into_iter()
            .map(|r| (r.kind, r.name, r.class_name, r.row))
            .collect();
        use RunnableKind::*;
        let row = |needle: &str| PYTHON[..PYTHON.find(needle).unwrap()].matches('\n').count();
        let class = |name: &str| Some(name.to_owned());
        assert_eq!(
            found,
            [
                (
                    TestFunction,
                    "test_top_level".into(),
                    None,
                    row("def test_top_level")
                ),
                (
                    TestFunction,
                    "test_async".into(),
                    None,
                    row("async def test_async")
                ),
                (TestClass, "TestShape".into(), None, row("class TestShape")),
                (
                    TestMethod,
                    "test_area".into(),
                    class("TestShape"),
                    row("def test_area")
                ),
                (
                    TestMethod,
                    "test_static".into(),
                    class("TestShape"),
                    row("def test_static")
                ),
                (TestClass, "Legacy".into(), None, row("class Legacy")),
                (
                    TestMethod,
                    "testOld".into(),
                    class("Legacy"),
                    row("def testOld")
                ),
                (Main, "__main__".into(), None, row("if __name__")),
            ]
        );
    }

    #[test]
    fn outlines_cover_the_other_grammars() {
        use SymbolKind::*;
        let ts = "export class Api {\n  get(id: string) { return id }\n}\n\
                  interface Shape { area(): number }\n\
                  const handler = async (event: Event) => {}\n\
                  function main() {}\n";
        assert_eq!(
            names(&outline("api.ts", ts)),
            [
                ("Api", Class, 0),
                ("get", Method, 1),
                ("Shape", Interface, 0),
                ("handler", Function, 0),
                ("main", Function, 0),
            ]
        );
        let rust = "mod net {\n    struct Peer;\n    impl Peer {\n        fn dial(&self) {}\n    }\n}\nfn main() {}\n";
        assert_eq!(
            names(&outline("net.rs", rust)),
            [
                ("net", Module, 0),
                ("Peer", Struct, 1),
                ("Peer", Impl, 1),
                ("dial", Method, 2),
                ("main", Function, 0),
            ]
        );
        let go = "package main\n\ntype Server struct{}\n\nfunc (s *Server) Serve() {}\n\nfunc main() {}\n";
        assert_eq!(
            names(&outline("main.go", go)),
            [
                ("Server", Struct, 0),
                ("Serve", Method, 0),
                ("main", Function, 0)
            ]
        );
        let c = "struct point { int x; };\nstatic int *origin(void) { return 0; }\n";
        assert_eq!(
            names(&outline("point.c", c)),
            [("point", Struct, 0), ("origin", Function, 0)]
        );
        assert!(outline("notes.txt", "class A:\n    pass\n").is_empty());
        assert!(outline("data.json", "{\"class\": 1}").is_empty());
    }
}
