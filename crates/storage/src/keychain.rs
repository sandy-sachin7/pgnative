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
    /// Keychain unavailable on this platform (e.g. Linux without Secret Service/KWallet).
    #[error("keychain unavailable: {reason} — install Secret Service (gnome-keyring/KWallet) or use 'Ask each session'")]
    Unavailable { reason: String },
}

/// User-chosen credential policy when keychain is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPolicy {
    /// Block saving — explain how to install Secret Service/KWallet.
    Block,
    /// Do not persist — ask for password every session, store nothing.
    AskEachSession,
}

const SERVICE: &str = "com.pgnative.pgnative";

pub fn set_password(conn_id: Uuid, password: SecretString) -> Result<(), KeychainError> {
    let entry = keyring::Entry::new(SERVICE, &format!("connection/{}", conn_id)).map_err(|e| {
        // `keyring` maps platform backend absence to a string containing
        // "No default keychain" / "DBUS" / "secret service" — surface as Unavailable.
        let msg = e.to_string();
        if is_unavailable_msg(&msg) {
            KeychainError::Unavailable { reason: msg }
        } else {
            KeychainError::Keyring(msg)
        }
    })?;
    entry.set_password(password.expose_secret()).map_err(|e| {
        let msg = e.to_string();
        if is_unavailable_msg(&msg) {
            KeychainError::Unavailable { reason: msg }
        } else {
            KeychainError::Keyring(msg)
        }
    })
}

/// Returns `true` if the keyring error indicates backend absence (Linux without Secret Service/KWallet).
fn is_unavailable_msg(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("no default")
        || lower.contains("secret service")
        || lower.contains("kwallet")
        || lower.contains("dbus")
        || lower.contains("platform secure storage failure")
        || lower.contains("not available")
}

/// Decide credential policy when keychain is unavailable.
/// Callers must **block** `Block` and must not fall back to plaintext.
#[must_use]
pub fn on_keychain_unavailable(policy: CredentialPolicy) -> &'static str {
    match policy {
        CredentialPolicy::Block => {
            "Keychain unavailable — install Secret Service (gnome-keyring) or KWallet, or choose 'Ask each session' to avoid persisting."
        }
        CredentialPolicy::AskEachSession => "Ask each session — credential will not be persisted.",
    }
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
    pgnative_db::connection::sanitize_url(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn sanitize_url_never_leaks_password() {
        let clean = sanitize_url("postgres://bob:s3cret-pw@db.example:5432/mydb");
        assert!(!clean.contains("s3cret-pw"), "got: {clean}");
        assert!(clean.contains("db.example"));
    }

    #[test]
    fn unavailable_message_mapping() {
        assert!(is_unavailable_msg(
            "No default keychain (DBus secret service down)"
        ));
        assert!(is_unavailable_msg("platform secure storage failure"));
        assert!(!is_unavailable_msg("invalid password"));
        assert!(on_keychain_unavailable(CredentialPolicy::Block).contains("Ask each session"));
    }

    /// Best-effort OS roundtrip: passes with a real backend (value matches)
    /// or without one (`Unavailable`, e.g. CI with no Secret Service).
    /// Any other outcome — wrong value, unexpected error — fails.
    #[test]
    fn roundtrip_or_unavailable() {
        let id = Uuid::new_v4();
        let pw = SecretString::new("p5-probe-secret".into());
        match set_password(id, pw) {
            Ok(()) => {
                let back = get_password(id).expect("set succeeded, get must succeed");
                assert_eq!(back.expose_secret(), "p5-probe-secret");
                delete_password(id).expect("delete must succeed after set");
                assert!(matches!(get_password(id), Err(KeychainError::NotFound)));
            }
            Err(KeychainError::Unavailable { .. }) => {}
            Err(e) => panic!("unexpected keychain error: {e}"),
        }
    }
}
