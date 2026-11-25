//! Database tools for the Database view: saved data sources, connections,
//! statement execution and catalog introspection, for PostgreSQL and MongoDB.
//! UI-independent; all work runs on the shared tokio runtime ([`runtime`]).

pub mod analysis;
pub mod conn;
pub mod consoles;
pub mod discover;
pub mod export;
pub mod format;
pub mod history;
pub mod introspect;
pub mod lexer;
pub mod mongo;
pub mod params;
pub mod plan;
pub mod runtime;
pub mod source;
pub mod split;
pub mod tunnel;

pub use conn::{Connection, DbError, PgConnection, QueryResult, ResultColumn};
pub use consoles::ConsoleStore;
pub use export::{format_rows, ExportFormat, ExportOptions};
pub use history::{HistoryEntry, HistoryStore};
pub use introspect::{
    CatalogEntry, ColumnInfo, ConstraintInfo, IndexInfo, RoutineInfo, RoutineKind, SchemaObjects,
    SequenceInfo, TableDetails, TableInfo, TableKind, TriggerInfo,
};
pub use plan::{Plan, PlanNode, PlanRow};
pub use source::{DataSource, DataSourceStore, DbColor, DbKind, McpAccess, Tunnel, TunnelKind};
