//! Code insight from language servers: a client for the Language Server
//! Protocol and the manager that decides which servers run. `Manager` is the
//! way in for both the editor and the agents' tools; the rest is how it works
//! (docs/native/code.md, section 4).
pub mod client;
// The scripted server, also for the interface's tests (`test-support`).
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod manager;
#[cfg(test)]
mod manager_tests;
pub mod position;
mod protocol;
#[cfg(test)]
mod real_tests;
pub mod registry;
pub mod types;

pub use client::CancelToken;
pub use manager::{
    Clock, Coverage, DocumentContext, DocumentDiagnostics, Manager, ManagerOptions,
    ProjectSettings, ServerDiagnostics,
};
pub use position::{Encoding, LineIndex, Point, Position, Span};
pub use registry::{Language, Registry, Role, ServerSpec, Strictness};
pub use types::{
    CodeAction, CompletionItem, CompletionKind, CompletionList, Diagnostic, DocumentSymbol, Hover,
    Location, LspError, LspEvent, ParameterInfo, RenameTarget, ServerId, ServerState,
    ServerStatus, Severity, SignatureHelp, SignatureInfo, SymbolKind, TextEdit, Triggers,
    WorkspaceEdit,
};
