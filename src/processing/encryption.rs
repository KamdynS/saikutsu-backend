use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, AeadCore,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

/// Encrypt an API key using AES-256-GCM.
/// Returns base64-encoded nonce + ciphertext.
pub fn encrypt_api_key(plaintext: &str, encryption_key: &str) -> anyhow::Result<String> {
    let key_bytes = derive_key(encryption_key);
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid encryption key: {}", e))?;

    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    // Prepend nonce (12 bytes) to ciphertext
    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(&combined))
}

/// Decrypt an API key encrypted with encrypt_api_key.
pub fn decrypt_api_key(encrypted: &str, encryption_key: &str) -> anyhow::Result<String> {
    let key_bytes = derive_key(encryption_key);
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid encryption key: {}", e))?;

    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| anyhow::anyhow!("Invalid base64: {}", e))?;

    if combined.len() < 12 {
        return Err(anyhow::anyhow!("Invalid encrypted data"));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("Invalid UTF-8: {}", e))
}

/// Derive a 32-byte key from the encryption key string.
fn derive_key(key: &str) -> [u8; 32] {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut result = [0u8; 32];
    let bytes = key.as_bytes();

    for (i, byte) in result.iter_mut().enumerate() {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        bytes.hash(&mut hasher);
        *byte = (hasher.finish() % 256) as u8;
    }

    result
}
