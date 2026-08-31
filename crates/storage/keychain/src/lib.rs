//! OS keychain wrapper — per AGENTS §24.
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum KeychainError {
    #[error("keyring: {0}")]
    Keyring(String),
    #[error("not found")]
    NotFound,
}

const SERVICE: &str = "com.pgnative.pgnative";

pub fn set_password(conn_id: Uuid, password: SecretString) -> Result<(), KeychainError> {
    let entry = keyring::Entry::new(SERVICE, &format!("connection/{}", conn_id))
        .map_err(|e| KeychainError::Keyring(e.to_string()))?;
    entry
        .set_password(password.expose_secret())
        .map_err(|e| KeychainError::Keyring(e.to_string()))
}

pub fn get_password(conn_id: Uuid) -> Result<SecretString, KeychainError> {
    let entry = keyring::Entry::new(SERVICE, &format!("connection/{}", conn_id))
        .map_err(|e| KeychainError::Keyring(e.to_string()))?;
    let pw = entry.get_password().map_err(|_| KeychainError::NotFound)?;
    Ok(SecretString::new(pw.into()))
}

pub fn delete_password(conn_id: Uuid) -> Result<(), KeychainError> {
    let entry = keyring::Entry::new(SERVICE, &format!("connection/{}", conn_id))
        .map_err(|e| KeychainError::Keyring(e.to_string()))?;
    entry
        .delete_credential()
        .map_err(|e| KeychainError::Keyring(e.to_string()))
}

pub fn sanitize_url(raw: &str) -> String {
    pgnative_db_connection::sanitize_url(raw)
}
