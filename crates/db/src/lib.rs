//! pgNative database layer: connections, introspection, and cancellation.
//!
//! Long-lived PostgreSQL sessions with explicit state machines; the UI talks
//! to this crate's execution model, never directly to driver types.

/// Connection configs, session state, live connect, and TLS cancel.
pub mod connection;
/// Schema introspection: relations, keys, and hydration into the schema model.
pub mod introspection;
