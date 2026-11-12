//! The API client: Postman collections and environments, `{{variables}}`,
//! cURL import and export, and sending a request. UI-independent; the view
//! runs [`send::send`] on a background thread.
//!
//! The Postman file is the format. It is read and written in place
//! ([`collection`]), so a collection round-trips through Monocode without
//! losing the parts Monocode does not use.

pub mod agent;
pub mod collection;
pub mod curl;
pub mod env;
pub mod exchange;
pub mod model;
pub mod prepare;
pub mod script;
pub mod send;
pub mod vars;

pub use collection::{Collection, ItemPath, TreeEntry};
pub use curl::{parse_curl, to_curl, CurlImport};
pub use env::{EnvVar, Environment};
pub use exchange::{ExchangeInput, ExchangeOutput, VarChange};
pub use model::{ApiRequest, Auth, Body, FormPart, KeyValue, PartKind, RawLanguage};
pub use prepare::{prepare, Prepared, PreparedBody};
pub use script::TestResult;
pub use send::{send, Response, SendOptions};
pub use vars::Scope;

pub const COLLECTION_SUFFIX: &str = ".postman_collection.json";
pub const ENVIRONMENT_SUFFIX: &str = ".postman_environment.json";

/// Where a project keeps its collections and environments.
pub fn api_dir(project: &str) -> std::path::PathBuf {
    std::path::Path::new(project).join(".monocode").join("api")
}

/// A message meant to be shown to the user as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(String);

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

/// JSON the way Postman writes it: tab-indented.
pub(crate) fn pretty_tabs(value: &serde_json::Value) -> String {
    use serde::Serialize;
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    // Serializing a `Value` cannot fail.
    let _ = value.serialize(&mut serializer);
    String::from_utf8(out).unwrap_or_default()
}
