use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use hkdf::Hkdf;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

/// Current ciphertext format version.
/// Prepended to every encrypted blob so we can migrate algorithms in the future.
const FORMAT_VERSION: u8 = 0x01;

/// Fixed, application-specific salt for HKDF extraction step.
/// Using a non-trivial salt strengthens key derivation when the input secret has
/// low entropy.
const HKDF_SALT: &[u8] = b"wishlist-pulse-hkdf-salt-v1";

/// Derive a 256-bit AES key from the user-provided secret using HKDF-SHA256.
fn derive_key(secret: &SecretString) -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), secret.expose_secret().as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(b"wishlist-pulse-aes-key", &mut key)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    key
}

/// Compute a hex-encoded SHA-256 hash of the secret (stored in DB to detect rotation).
pub fn hash_secret(secret: &SecretString) -> String {
    let mut hasher = Sha256::new();
    // Domain-separated input so the key derivation and hash aren't identical
    hasher.update(b"wishlist-pulse-secret-hash:");
    hasher.update(secret.expose_secret().as_bytes());
    hex::encode(hasher.finalize())
}

/// Encrypt plaintext using AES-256-GCM.
/// Returns hex-encoded `version (1) || nonce (12) || ciphertext+tag`.
pub fn encrypt(secret: &SecretString, plaintext: &str) -> Result<String, String> {
    let key = derive_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;

    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).map_err(|e| e.to_string())?;
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| e.to_string())?;

    // version (1 byte) || nonce (12 bytes) || ciphertext + GCM tag
    let mut out = Vec::with_capacity(1 + 12 + ciphertext.len());
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(hex::encode(out))
}

/// Decrypt hex-encoded `version || nonce || ciphertext+tag` using AES-256-GCM.
pub fn decrypt(secret: &SecretString, hex_data: &str) -> Result<String, String> {
    let data = hex::decode(hex_data).map_err(|e| e.to_string())?;

    // Minimum: 1 (version) + 12 (nonce) + 16 (GCM auth tag) = 29 bytes for empty plaintext
    if data.len() < 29 {
        return Err("Encrypted data too short".to_string());
    }

    let version = data[0];

    // Support legacy v0 format (no version prefix): nonce (12) || ciphertext+tag
    // This branch can be removed once all stored data has been re-encrypted.
    if version != FORMAT_VERSION {
        return decrypt_v0(secret, &data);
    }

    let key = derive_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;

    let nonce = Nonce::from_slice(&data[1..13]);
    let plaintext = cipher
        .decrypt(nonce, &data[13..])
        .map_err(|_| "Decryption failed (wrong key or corrupted data)".to_string())?;

    String::from_utf8(plaintext).map_err(|e| e.to_string())
}

/// Decrypt legacy format (v0): nonce (12) || ciphertext+tag, no version prefix,
/// and key derived without HKDF salt.
fn decrypt_v0(secret: &SecretString, data: &[u8]) -> Result<String, String> {
    // v0 minimum: 12 (nonce) + 16 (GCM auth tag) = 28 bytes
    if data.len() < 28 {
        return Err("Encrypted data too short".to_string());
    }

    // v0 used HKDF with no salt
    let hkdf = Hkdf::<Sha256>::new(None, secret.expose_secret().as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(b"wishlist-pulse-aes-key", &mut key)
        .expect("32 bytes is a valid length for HKDF-SHA256");

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let nonce = Nonce::from_slice(&data[..12]);
    let plaintext = cipher
        .decrypt(nonce, &data[12..])
        .map_err(|_| "Decryption failed (wrong key or corrupted data)".to_string())?;

    String::from_utf8(plaintext).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn round_trip() {
        let secret = test_secret("test-secret-key");
        let plaintext = "ABCDEF123456";
        let encrypted = encrypt(&secret, plaintext).unwrap();
        let decrypted = decrypt(&secret, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let encrypted = encrypt(&test_secret("key1"), "hello").unwrap();
        assert!(decrypt(&test_secret("key2"), &encrypted).is_err());
    }

    #[test]
    fn hash_is_stable() {
        let h1 = hash_secret(&test_secret("my-secret"));
        let h2 = hash_secret(&test_secret("my-secret"));
        assert_eq!(h1, h2);
        assert_ne!(h1, hash_secret(&test_secret("other-secret")));
    }

    #[test]
    fn version_prefix_present() {
        let secret = test_secret("test");
        let encrypted = encrypt(&secret, "data").unwrap();
        let raw = hex::decode(&encrypted).unwrap();
        assert_eq!(
            raw[0], FORMAT_VERSION,
            "first byte should be the format version"
        );
    }

    #[test]
    fn legacy_v0_decrypt() {
        // Simulate v0 format: HKDF with no salt, no version prefix
        let secret_str = "legacy-secret";
        let plaintext = "legacy-data";

        // Encrypt using v0 method (no salt, no version prefix)
        let hkdf = Hkdf::<Sha256>::new(None, secret_str.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(b"wishlist-pulse-aes-key", &mut key).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();

        let mut nonce_bytes = [0u8; 12];
        getrandom::fill(&mut nonce_bytes).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, plaintext.as_bytes()).unwrap();

        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        let hex_data = hex::encode(out);

        // Decrypt with current code — should fall through to v0 path
        let secret = test_secret(secret_str);
        let decrypted = decrypt(&secret, &hex_data).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
