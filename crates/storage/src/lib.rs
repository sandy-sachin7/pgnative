//! pgNative local application state (SQLite + OS keychain).
//!
//! Stores hosts, history, and preferences. Secrets live in the OS keychain;
//! SQLite never holds plaintext passwords.

/// Saved (non-secret) connection entries.
pub mod connections;
/// Persisted editor tabs across restarts.
pub mod editor_state;
/// Local query history (FTS5 search).
pub mod history;
/// OS keychain access for passwords.
pub mod keychain;
/// Theme and UI preferences.
pub mod preferences;
