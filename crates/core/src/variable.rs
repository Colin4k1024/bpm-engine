//! Process variable encryption support.
//!
//! Provides [`VariableValue`] for representing plain/encrypted variables,
//! [`VariableEncryptor`] trait for encryption backends, and [`AesGcmEncryptor`]
//! as a production-ready AES-256-GCM implementation.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use thiserror::Error;

/// Prefix marking an encrypted variable value in `ProcessInstance.variables`.
///
/// Encrypted values are stored as `__encrypted:<base64(nonce)>:<base64(ciphertext)>`.
pub const ENCRYPTED_PREFIX: &str = "__encrypted:";

/// Errors from variable encryption/decryption operations.
#[derive(Debug, Error)]
pub enum EncryptionError {
    /// The encryption operation failed (e.g., crypto library error).
    #[error("encryption failed: {0}")]
    EncryptFailed(String),
    /// The decryption operation failed (wrong key, corrupted data).
    #[error("decryption failed: {0}")]
    DecryptFailed(String),
    /// The provided key is invalid (wrong length, bad encoding).
    #[error("invalid key: {0}")]
    InvalidKey(String),
    /// The encrypted value format is malformed (bad nonce, bad ciphertext).
    #[error("invalid encrypted value format: {0}")]
    InvalidFormat(String),
}

/// Value of a process variable, either plaintext or encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableValue {
    /// Plaintext variable value.
    Plain(String),
    /// Encrypted variable value with separate nonce and ciphertext.
    Encrypted {
        /// Base64-encoded ciphertext.
        ciphertext: String,
        /// Base64-encoded nonce (12 bytes for AES-GCM).
        nonce: String,
    },
}

impl VariableValue {
    /// Returns the plaintext value if this is a `Plain` variant.
    pub fn as_plain(&self) -> Option<&str> {
        match self {
            VariableValue::Plain(v) => Some(v),
            _ => None,
        }
    }

    /// Encode an encrypted value into the storage string format.
    pub fn to_storage_string(&self) -> String {
        match self {
            VariableValue::Plain(v) => v.clone(),
            VariableValue::Encrypted { ciphertext, nonce } => {
                format!("{}{}:{}", ENCRYPTED_PREFIX, nonce, ciphertext)
            }
        }
    }

    /// Parse a storage string into a `VariableValue`.
    ///
    /// Strings with the `__encrypted:` prefix are parsed as encrypted values;
    /// all others are treated as plain.
    pub fn from_storage_string(s: &str) -> Self {
        if let Some(rest) = s.strip_prefix(ENCRYPTED_PREFIX) {
            if let Some((nonce, ciphertext)) = rest.split_once(':') {
                return VariableValue::Encrypted {
                    ciphertext: ciphertext.to_string(),
                    nonce: nonce.to_string(),
                };
            }
        }
        VariableValue::Plain(s.to_string())
    }

    /// Returns true if this value is encrypted.
    pub fn is_encrypted(&self) -> bool {
        matches!(self, VariableValue::Encrypted { .. })
    }
}

/// Trait for variable encryption backends.
///
/// Implementations provide encrypt/decrypt operations for process variables.
/// The `key` parameter is the encryption key identifier (e.g., key material
/// or key name depending on the backend).
pub trait VariableEncryptor: Send + Sync {
    /// Encrypt a plaintext value. Returns an encrypted [`VariableValue`].
    fn encrypt(&self, key: &str, value: &str) -> Result<VariableValue, EncryptionError>;

    /// Decrypt a [`VariableValue`] back to plaintext.
    fn decrypt(&self, key: &str, value: &VariableValue) -> Result<String, EncryptionError>;
}

/// AES-256-GCM encryptor for process variables.
///
/// Uses a 256-bit key (32 bytes) provided as a hex or base64 string.
/// Each encryption operation generates a random 96-bit nonce (recommended for AES-GCM).
///
/// # Key format
///
/// The `key` parameter to encrypt/decrypt should be a 32-byte key encoded as:
/// - Hex string (64 characters), or
/// - Base64 string (44 characters)
///
/// The key is decoded on each call to allow runtime key rotation via the
/// `BPM_VARIABLE_ENCRYPTION_KEY` environment variable.
pub struct AesGcmEncryptor;

impl AesGcmEncryptor {
    /// Create a new AES-GCM encryptor.
    pub fn new() -> Self {
        Self
    }

    /// Decode a hex or base64 encoded key into 32 bytes.
    fn decode_key(key: &str) -> Result<[u8; 32], EncryptionError> {
        // Try hex first (64 hex chars = 32 bytes)
        if key.len() == 64 {
            if let Ok(bytes) = hex_decode(key) {
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    return Ok(arr);
                }
            }
        }
        // Try base64
        if let Ok(bytes) = BASE64.decode(key) {
            if bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                return Ok(arr);
            }
            return Err(EncryptionError::InvalidKey(format!(
                "key must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        Err(EncryptionError::InvalidKey(
            "key must be 64-char hex or 44-char base64 encoding of 32 bytes".to_string(),
        ))
    }
}

impl Default for AesGcmEncryptor {
    fn default() -> Self {
        Self::new()
    }
}

impl VariableEncryptor for AesGcmEncryptor {
    fn encrypt(&self, key: &str, value: &str) -> Result<VariableValue, EncryptionError> {
        let key_bytes = Self::decode_key(key)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

        // Generate random 12-byte nonce
        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, value.as_bytes())
            .map_err(|e| EncryptionError::EncryptFailed(e.to_string()))?;

        Ok(VariableValue::Encrypted {
            ciphertext: BASE64.encode(&ciphertext),
            nonce: BASE64.encode(nonce_bytes),
        })
    }

    fn decrypt(&self, key: &str, value: &VariableValue) -> Result<String, EncryptionError> {
        let VariableValue::Encrypted { ciphertext, nonce } = value else {
            return Err(EncryptionError::DecryptFailed(
                "value is not encrypted".to_string(),
            ));
        };

        let key_bytes = Self::decode_key(key)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

        let nonce_bytes = BASE64
            .decode(nonce)
            .map_err(|e| EncryptionError::InvalidFormat(format!("invalid nonce: {e}")))?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext_bytes = BASE64
            .decode(ciphertext)
            .map_err(|e| EncryptionError::InvalidFormat(format!("invalid ciphertext: {e}")))?;

        let plaintext = cipher
            .decrypt(nonce, ciphertext_bytes.as_ref())
            .map_err(|e| EncryptionError::DecryptFailed(e.to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|e| EncryptionError::DecryptFailed(format!("invalid UTF-8: {e}")))
    }
}

/// Simple hex decode (no external crate dependency).
fn hex_decode(hex: &str) -> Result<Vec<u8>, ()> {
    if !hex.len().is_multiple_of(2) {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let high = hex_val(chunk[0]).ok_or(())?;
        let low = hex_val(chunk[1]).ok_or(())?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-byte key encoded as hex (64 chars).
    const TEST_KEY_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn round_trip_encrypt_decrypt() {
        let enc = AesGcmEncryptor::new();
        let plaintext = "sensitive-data-123";

        let encrypted = enc.encrypt(TEST_KEY_HEX, plaintext).unwrap();
        assert!(encrypted.is_encrypted());

        let decrypted = enc.decrypt(TEST_KEY_HEX, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypted_value_to_storage_string_and_back() {
        let enc = AesGcmEncryptor::new();
        let encrypted = enc.encrypt(TEST_KEY_HEX, "secret").unwrap();

        let storage = encrypted.to_storage_string();
        assert!(storage.starts_with(ENCRYPTED_PREFIX));

        let parsed = VariableValue::from_storage_string(&storage);
        assert!(parsed.is_encrypted());
        assert_eq!(parsed, encrypted);
    }

    #[test]
    fn plain_value_round_trip() {
        let plain = VariableValue::Plain("hello".to_string());
        let storage = plain.to_storage_string();
        assert_eq!(storage, "hello");
        assert!(!storage.starts_with(ENCRYPTED_PREFIX));

        let parsed = VariableValue::from_storage_string(&storage);
        assert_eq!(parsed, VariableValue::Plain("hello".to_string()));
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let enc = AesGcmEncryptor::new();
        let encrypted = enc.encrypt(TEST_KEY_HEX, "secret").unwrap();

        let wrong_key = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        let result = enc.decrypt(wrong_key, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_plain_value_fails() {
        let enc = AesGcmEncryptor::new();
        let plain = VariableValue::Plain("not encrypted".to_string());
        let result = enc.decrypt(TEST_KEY_HEX, &plain);
        assert!(result.is_err());
    }

    #[test]
    fn different_nonces_for_same_plaintext() {
        let enc = AesGcmEncryptor::new();
        let e1 = enc.encrypt(TEST_KEY_HEX, "same").unwrap();
        let e2 = enc.encrypt(TEST_KEY_HEX, "same").unwrap();

        // Nonces should differ (random)
        assert_ne!(e1, e2, "random nonces should produce different ciphertext");

        // Both should decrypt to the same value
        let d1 = enc.decrypt(TEST_KEY_HEX, &e1).unwrap();
        let d2 = enc.decrypt(TEST_KEY_HEX, &e2).unwrap();
        assert_eq!(d1, "same");
        assert_eq!(d2, "same");
    }

    #[test]
    fn base64_key_works() {
        // 32 zero bytes in base64
        let key_b64 = BASE64.encode([0u8; 32]);
        let enc = AesGcmEncryptor::new();
        let encrypted = enc.encrypt(&key_b64, "test").unwrap();
        let decrypted = enc.decrypt(&key_b64, &encrypted).unwrap();
        assert_eq!(decrypted, "test");
    }

    #[test]
    fn invalid_key_length_rejected() {
        let enc = AesGcmEncryptor::new();
        let result = enc.encrypt("short", "test");
        assert!(result.is_err());
    }

    #[test]
    fn empty_value_encrypts() {
        let enc = AesGcmEncryptor::new();
        let encrypted = enc.encrypt(TEST_KEY_HEX, "").unwrap();
        let decrypted = enc.decrypt(TEST_KEY_HEX, &encrypted).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn large_value_encrypts() {
        let enc = AesGcmEncryptor::new();
        let large = "x".repeat(10_000);
        let encrypted = enc.encrypt(TEST_KEY_HEX, &large).unwrap();
        let decrypted = enc.decrypt(TEST_KEY_HEX, &encrypted).unwrap();
        assert_eq!(decrypted, large);
    }
}
