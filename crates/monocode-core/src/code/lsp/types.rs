//! What the language server client hands to its callers: plain values with
//! paths and points instead of URIs and protocol positions, so neither the
//! editor nor an agent's tool has to know the protocol.
use super::position::Span;
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};

/// Names one server process for as long as the manager knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerId(pub u64);

/// Why a question to a language server has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspError {
    /// No language server serves this file, or none of them is running.
    NoServer,
    /// The document was not opened with the manager.
    NotOpen,
    /// The server does not offer this feature.
    Unsupported,
    Timeout,
    Cancelled,
    /// The server process ended, or its stream broke.
    Closed,
    /// The server answered with an error.
    Server {
        code: i64,
        message: String,
    },
    Io(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServer => write!(f, "No language server is available for this file."),
            Self::NotOpen => write!(f, "The file is not open in a language server."),
            Self::Unsupported => write!(f, "The language server does not support this."),
            Self::Timeout => write!(f, "The language server did not answer in time."),
            Self::Cancelled => write!(f, "The request was cancelled."),
            Self::Closed => write!(f, "The language server stopped."),
            Self::Server { message, .. } => write!(f, "The language server failed: {message}"),
            Self::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for LspError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub range: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Span,
    pub severity: Severity,
    pub message: String,
    /// The rule or error code, e.g. `F401` or `reportArgumentType`.
    pub code: Option<String>,
    /// A page describing the code, when the server names one.
    pub code_href: Option<String>,
    /// What the server calls itself in the diagnostic, e.g. `Ruff`.
    pub source: Option<String>,
    /// The registry id of the server it came from, e.g. `ruff`.
    pub server: String,
    /// Unused code, to be drawn faded.
    pub unnecessary: bool,
    /// Deprecated code, to be drawn struck through.
    pub deprecated: bool,
    pub related: Vec<(Location, String)>,
    /// The diagnostic as the server sent it, handed back in the context of
    /// a code action request so the server can find its own fixes.
    pub raw: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    /// Markdown; plain text and code are already fenced or escaped into it.
    pub contents: String,
    /// The word or expression the hover is about, when the server says.
    pub range: Option<Span>,
}

macro_rules! coded_kind {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $code:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)+
            Other,
        }

        impl $name {
            pub fn from_code(code: u64) -> Self {
                match code {
                    $($code => Self::$variant,)+
                    _ => Self::Other,
                }
            }
        }
    };
}

coded_kind!(
    /// What a completion item is, for its icon.
    CompletionKind {
        Text = 1,
        Method = 2,
        Function = 3,
        Constructor = 4,
        Field = 5,
        Variable = 6,
        Class = 7,
        Interface = 8,
        Module = 9,
        Property = 10,
        Unit = 11,
        Value = 12,
        Enum = 13,
        Keyword = 14,
        Snippet = 15,
        Color = 16,
        File = 17,
        Reference = 18,
        Folder = 19,
        EnumMember = 20,
        Constant = 21,
        Struct = 22,
        Event = 23,
        Operator = 24,
        TypeParameter = 25,
    }
);

coded_kind!(
    /// What a document symbol is, for its icon in an outline.
    SymbolKind {
        File = 1,
        Module = 2,
        Namespace = 3,
        Package = 4,
        Class = 5,
        Method = 6,
        Property = 7,
        Field = 8,
        Constructor = 9,
        Enum = 10,
        Interface = 11,
        Function = 12,
        Variable = 13,
        Constant = 14,
        String = 15,
        Number = 16,
        Boolean = 17,
        Array = 18,
        Object = 19,
        Key = 20,
        Null = 21,
        EnumMember = 22,
        Struct = 23,
        Event = 24,
        Operator = 25,
        TypeParameter = 26,
    }
);

/// Text to put in place of a range of the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Span,
    pub text: String,
}

/// Changes a server asks for across files, as steps to take in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceEdit {
    pub changes: Vec<DocumentChange>,
}

/// One step of a `WorkspaceEdit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentChange {
    /// Edits to a file in its positions as the steps before left its text,
    /// by position, none overlapping.
    Edit { path: PathBuf, edits: Vec<TextEdit> },
    /// A new empty file. One that exists is emptied with `overwrite`, left
    /// alone with `ignore_if_exists`, else the edit fails.
    Create {
        path: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    /// A file or folder moved; `overwrite` and `ignore_if_exists` are about
    /// `to` as for `Create`.
    Rename {
        from: PathBuf,
        to: PathBuf,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    /// A file, or with `recursive` a folder and all it holds.
    Delete {
        path: PathBuf,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

impl WorkspaceEdit {
    pub fn is_empty(&self) -> bool {
        self.changes.iter().all(|change| match change {
            DocumentChange::Edit { edits, .. } => edits.is_empty(),
            _ => false,
        })
    }

    /// The text edits, file by file, in the order they come.
    pub fn text_edits(&self) -> impl Iterator<Item = (&Path, &[TextEdit])> {
        self.changes.iter().filter_map(|change| match change {
            DocumentChange::Edit { path, edits } => Some((path.as_path(), edits.as_slice())),
            _ => None,
        })
    }

    /// How many steps create, rename or delete files.
    pub fn file_operations(&self) -> usize {
        (self.changes.iter())
            .filter(|change| !matches!(change, DocumentChange::Edit { .. }))
            .count()
    }
}

/// Something a server offers to do at a place: a quick fix for a
/// diagnostic, a refactoring, or a source action such as organizing imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    /// `quickfix`, `refactor.extract`, `source.organizeImports`, ...
    pub kind: Option<String>,
    /// The fix the server would apply without asking.
    pub preferred: bool,
    /// Why the action cannot be taken now, when the server lists it anyway.
    pub disabled: Option<String>,
    /// The changes, when the server sent them; `Manager::resolve_code_action`
    /// asks for them otherwise.
    pub edit: Option<WorkspaceEdit>,
    /// The action (also) runs a server command: after its edit, when it
    /// has one (`Manager::run_code_action_command`).
    pub command: bool,
    pub server: ServerId,
    pub raw: Value,
}

/// What a rename at a place would replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameTarget {
    /// The symbol's range, when the server says; else the word there.
    pub range: Option<Span>,
    /// The name to start from, when the server suggests one.
    pub placeholder: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    /// A short note beside the label: a type or a signature.
    pub detail: Option<String>,
    /// Markdown. Often absent until `Manager::resolve_completion`.
    pub documentation: Option<String>,
    /// What to order by; the label when the server gives nothing else.
    pub sort_text: String,
    /// What to match the typed prefix against; the label by default.
    pub filter_text: String,
    /// The text to insert, with any snippet syntax already reduced to plain text.
    pub insert_text: String,
    /// The range `insert_text` replaces. `None` means the word before the
    /// cursor, which the editor works out itself.
    pub replace: Option<Span>,
    /// Further edits elsewhere in the document, such as an added import.
    pub additional_edits: Vec<TextEdit>,
    pub deprecated: bool,
    pub preselect: bool,
    /// The server that offered it, and its own form of the item, for resolving.
    pub(crate) server: ServerId,
    pub(crate) raw: Value,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    /// More typing narrows the list on the server: ask again rather than
    /// filter what is here.
    pub is_incomplete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterInfo {
    pub label: String,
    /// Where the parameter sits in its signature's label, in bytes.
    pub label_range: Option<(usize, usize)>,
    pub documentation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterInfo>,
    /// The parameter being typed, when it is within `parameters`.
    pub active_parameter: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    /// The whole declaration, body included.
    pub range: Span,
    /// The name alone, where a jump should land.
    pub selection_range: Span,
    /// The enclosing symbol's name, from servers that report a flat list.
    pub container: Option<String>,
    pub children: Vec<DocumentSymbol>,
}

/// The characters after which the editor should ask without being asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Triggers {
    pub completion: Vec<String>,
    pub signature_help: Vec<String>,
}

/// What a server is doing, for the footer chip and the resource view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Ready,
    /// Busy with reported background work; `title` is the server's own word for it.
    Indexing {
        title: Option<String>,
    },
    /// It did not start, or it ended on its own. `log_tail` is the end of
    /// what it wrote to standard error.
    Failed {
        message: String,
        log_tail: String,
    },
    Stopped,
    /// None of `looked_for` is installed; `hint` says what to install.
    ToolNotFound {
        looked_for: Vec<String>,
        hint: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerStatus {
    pub id: ServerId,
    /// The registry id, e.g. `basedpyright`.
    pub server: String,
    /// The name to show, e.g. `basedpyright`.
    pub name: String,
    pub root: PathBuf,
    pub state: ServerState,
    /// The process, which leads its own process group: the server may be a
    /// launcher whose real work happens in a child (basedpyright runs node),
    /// so memory is the sum over the group.
    pub pid: Option<u32>,
    /// The command it was started with, for diagnosis.
    pub command: String,
    pub open_documents: usize,
}

/// What the manager tells its subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspEvent {
    /// `Manager::diagnostics(path)` has a new answer.
    DiagnosticsChanged { path: PathBuf },
    /// A server changed state; a stopped server that nothing uses any more
    /// is reported once here and then forgotten.
    ServerChanged(ServerStatus),
}
