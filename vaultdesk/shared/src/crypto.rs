//! Authenticated encryption for data at rest (XChaCha20-Poly1305).
//!
//! Sealed format: `[version=1][24-byte nonce][ciphertext+tag]`.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};

pub const KEY_LEN: usize = 32;
pub type Key = [u8; KEY_LEN];

const VERSION: u8 = 1;
const NONCE_LEN: usize = 24;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("sealed data is truncated or has an unknown version")]
    Malformed,
    #[error("decryption failed (wrong key or corrupted data)")]
    Decrypt,
}

pub fn random_key() -> Key {
    let mut key = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}

/// 256-bit random token, hex encoded.
pub fn random_token() -> String {
    hex::encode(random_key())
}

pub fn seal(key: &Key, plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .expect("XChaCha20-Poly1305 encryption is infallible for in-memory buffers");
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

pub fn open(key: &Key, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < 1 + NONCE_LEN || sealed[0] != VERSION {
        return Err(CryptoError::Malformed);
    }
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    let nonce = XNonce::from_slice(&sealed[1..1 + NONCE_LEN]);
    cipher
        .decrypt(nonce, &sealed[1 + NONCE_LEN..])
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = random_key();
        let sealed = seal(&key, b"hunter2");
        assert_eq!(open(&key, &sealed).unwrap(), b"hunter2");
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = seal(&random_key(), b"secret");
        assert!(open(&random_key(), &sealed).is_err());
    }
}
