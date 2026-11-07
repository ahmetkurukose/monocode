//! PostgreSQL tools for the Database view: saved data sources, connections,
//! statement execution and catalog introspection. UI-independent; all work
//! runs on the shared tokio runtime ([`runtime`]).

pub mod analysis;
pub mod conn;
pub mod consoles;
pub mod export;
pub mod format;
pub mod history;
pub mod introspect;
pub mod lexer;
pub mod params;
pub mod runtime;
pub mod source;
pub mod split;

pub use conn::{Connection, DbError, QueryResult, ResultColumn};
pub use consoles::ConsoleStore;
pub use export::{format_rows, ExportFormat};
pub use history::{HistoryEntry, HistoryStore};
pub use introspect::{
    CatalogEntry, ColumnInfo, ConstraintInfo, IndexInfo, TableDetails, TableInfo, TableKind,
    TriggerInfo,
};
pub use source::{DataSource, DataSourceStore, DbColor};
