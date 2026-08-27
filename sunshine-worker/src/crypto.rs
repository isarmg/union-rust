//! Module-owned encryption for Sunshine upstream passwords.

use std::collections::BTreeMap;

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::error::{AppError, AppResult};

const PREFIX: &str = "sunshine:v1:";
const LEGACY_PREFIX: &str = "enc:v2:";

#[derive(Clone)]
pub struct SecretBox {
    current_id: String,
    current: [u8; 32],
    previous: BTreeMap<String, [u8; 32]>,
}

impl SecretBox {
    pub fn new(current_id: impl Into<String>, current: [u8; 32]) -> anyhow::Result<Self> {
        let current_id = validate_key_id(current_id.into())?;
        Ok(Self {
            current_id,
            current,
            previous: BTreeMap::new(),
        })
    }

    pub fn with_previous(mut self, id: impl Into<String>, key: [u8; 32]) -> anyhow::Result<Self> {
        let id = validate_key_id(id.into())?;
        anyhow::ensure!(
            id != self.current_id,
            "previous key id duplicates current key id"
        );
        anyhow::ensure!(
            self.previous.insert(id.clone(), key).is_none(),
            "duplicate previous key id {id}"
        );
        Ok(self)
    }

    pub fn encrypt(&self, value: &str) -> AppResult<String> {
        let payload = seal(&self.current, value.as_bytes())?;
        Ok(format!(
            "{PREFIX}{}:{}",
            self.current_id,
            STANDARD.encode(payload)
        ))
    }

    pub fn decrypt(&self, value: &str) -> AppResult<String> {
        let rest = value.strip_prefix(PREFIX).ok_or(AppError::Crypto)?;
        let (id, payload) = rest.split_once(':').ok_or(AppError::Crypto)?;
        let key = if id == self.current_id {
            &self.current
        } else {
            self.previous.get(id).ok_or(AppError::Crypto)?
        };
        let plaintext = open(key, &decode_payload(payload)?)?;
        String::from_utf8(plaintext).map_err(|_| AppError::Crypto)
    }

    pub fn hmac_key(&self) -> &[u8; 32] {
        &self.current
    }
}

/// Decrypt a legacy UnionC `external_hosts.secret` without importing UnionC's
/// process-global keyring or AppState into the worker.
pub fn decrypt_legacy_union(
    value: &str,
    current_id: &str,
    current: &[u8; 32],
    previous: &BTreeMap<String, [u8; 32]>,
) -> AppResult<String> {
    let rest = value.strip_prefix(LEGACY_PREFIX).ok_or(AppError::Crypto)?;
    let (id, payload) = rest.split_once(':').ok_or(AppError::Crypto)?;
    let key = if id == current_id {
        current
    } else {
        previous.get(id).ok_or(AppError::Crypto)?
    };
    let plaintext = open(key, &decode_payload(payload)?)?;
    String::from_utf8(plaintext).map_err(|_| AppError::Crypto)
}

#[cfg(test)]
pub(crate) fn encrypt_legacy_for_test(id: &str, key: &[u8; 32], value: &str) -> String {
    format!(
        "{LEGACY_PREFIX}{id}:{}",
        STANDARD.encode(seal(key, value.as_bytes()).unwrap())
    )
}

fn seal(key: &[u8; 32], plaintext: &[u8]) -> AppResult<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut payload = nonce.to_vec();
    payload.extend(
        cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| AppError::Crypto)?,
    );
    Ok(payload)
}

fn open(key: &[u8; 32], payload: &[u8]) -> AppResult<Vec<u8>> {
    if payload.len() <= 12 {
        return Err(AppError::Crypto);
    }
    let (nonce, ciphertext) = payload.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| AppError::Crypto)
}

fn decode_payload(value: &str) -> AppResult<Vec<u8>> {
    STANDARD.decode(value).map_err(|_| AppError::Crypto)
}

fn validate_key_id(value: String) -> anyhow::Result<String> {
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "key id must contain 1-64 ASCII letters, digits, '-' or '_'"
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_ciphertext_round_trips_and_is_randomized() {
        let secrets = SecretBox::new("primary", [3; 32]).unwrap();
        let first = secrets.encrypt("password").unwrap();
        let second = secrets.encrypt("password").unwrap();
        assert_ne!(first, second);
        assert_eq!(secrets.decrypt(&first).unwrap(), "password");
        assert!(!first.contains("password"));
    }

    #[test]
    fn legacy_union_ciphertext_can_be_mapped_without_union_state() {
        let key = [4; 32];
        let encrypted = encrypt_legacy_for_test("old", &key, "legacy password");
        assert_eq!(
            decrypt_legacy_union(&encrypted, "old", &key, &BTreeMap::new()).unwrap(),
            "legacy password"
        );
    }
}
