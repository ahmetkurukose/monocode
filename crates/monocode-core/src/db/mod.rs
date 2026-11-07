//! PostgreSQL tools for the Database view: saved data sources, connections,
//! statement execution and catalog introspection. UI-independent; all work
//! runs on the shared tokio runtime ([`runtime`]).

pub mod conn;
pub mod consoles;
pub mod export;
pub mod history;
pub mod introspect;
pub mod runtime;
pub mod source;
pub mod split;

pub use conn::{Connection, DbError, QueryResult, ResultColumn};
pub use consoles::ConsoleStore;
pub use export::{format_rows, ExportFormat};
pub use history::{HistoryEntry, HistoryStore};
pub use introspect::{CatalogEntry, ColumnInfo, TableInfo, TableKind};
pub use source::{DataSource, DataSourceStore, DbColor};
