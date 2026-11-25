//! The browser's AI settings model: data types only, without HTTP or
//! credential stores.
mod config;
pub use config::*;
mod links;
pub use links::WebLink;
pub mod credentials;
