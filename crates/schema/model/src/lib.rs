//! Canonical in-memory PostgreSQL schema model.
//!
//! This crate is the single source of truth for schema metadata used by the
//! schema tree, autocomplete, table metadata, and editing safety. See `ADR-0007`
//! (schema cache strategy) for how it is populated and refreshed.

pub mod build;
pub mod column;
pub mod index;
pub mod relation;
pub mod schema;
pub mod types;

pub use index::SchemaModel;

#[cfg(test)]
mod tests;
