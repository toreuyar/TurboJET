use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

pub fn encrypt(plaintext: &str, cipher: &Aes256Gcm) -> Result<String> {
    tracing::debug!(plaintext_len = plaintext.len(), "Encrypting secret");
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| anyhow::anyhow!("Encryption failed"))?;
    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);
    let encoded = BASE64.encode(&combined);
    tracing::debug!(encoded_len = encoded.len(), "Secret encrypted");
    Ok(encoded)
}

pub fn decrypt(encrypted: &str, cipher: &Aes256Gcm) -> Result<String> {
    tracing::debug!(encrypted_len = encrypted.len(), "Decrypting secret");
    let combined = BASE64.decode(encrypted).context("Base64 decode failed")?;
    if combined.len() < 13 {
        anyhow::bail!("Encrypted data too short: {} bytes", combined.len());
    }
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Decryption failed — wrong key or corrupted data"))?;
    let result = String::from_utf8(plaintext).context("Decrypted data is not valid UTF-8")?;
    tracing::debug!("Secret decrypted successfully");
    Ok(result)
}

pub fn create_cipher() -> Result<Aes256Gcm> {
    tracing::debug!("Creating AES-256-GCM cipher from TURBOJET_MASTER_KEY");
    let hex_key = std::env::var("TURBOJET_MASTER_KEY")
        .context("TURBOJET_MASTER_KEY environment variable not set")?;
    if hex_key.len() != 64 {
        anyhow::bail!(
            "TURBOJET_MASTER_KEY must be 64 hex characters (32 bytes), got {}",
            hex_key.len()
        );
    }
    let key_bytes = hex::decode(&hex_key).context("TURBOJET_MASTER_KEY is not valid hex")?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|_| anyhow::anyhow!("Invalid key length for AES-256-GCM"))?;
    tracing::debug!("AES-256-GCM cipher created successfully");
    Ok(cipher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::KeyInit;

    fn test_cipher() -> Aes256Gcm {
        let key = [42u8; 32];
        Aes256Gcm::new_from_slice(&key).unwrap()
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let cipher = test_cipher();
        let plaintext = "hello world secret value";
        let encrypted = encrypt(plaintext, &cipher).unwrap();
        let decrypted = decrypt(&encrypted, &cipher).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let cipher1 = Aes256Gcm::new_from_slice(&[1u8; 32]).unwrap();
        let cipher2 = Aes256Gcm::new_from_slice(&[2u8; 32]).unwrap();
        let encrypted = encrypt("secret", &cipher1).unwrap();
        let result = decrypt(&encrypted, &cipher2);
        assert!(result.is_err());
    }

    #[test]
    fn different_ciphertext_for_same_plaintext() {
        let cipher = test_cipher();
        let plaintext = "same plaintext";
        let encrypted1 = encrypt(plaintext, &cipher).unwrap();
        let encrypted2 = encrypt(plaintext, &cipher).unwrap();
        assert_ne!(
            encrypted1, encrypted2,
            "Random nonce should produce different ciphertexts"
        );
        assert_eq!(
            decrypt(&encrypted1, &cipher).unwrap(),
            decrypt(&encrypted2, &cipher).unwrap()
        );
    }
}
