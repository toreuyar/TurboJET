use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub fn mask_secret(value: &str) -> String {
    if value.len() >= 6 {
        format!("{}***", &value[..3])
    } else {
        "***".to_string()
    }
}

#[derive(Debug, Deserialize)]
pub struct SetSecretRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct SecretEntry {
    pub key: String,
    pub masked_value: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SecretKeyEntry {
    pub key: String,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateSshKeyRequest {
    pub algorithm: String,
}

pub fn validate_secret_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(anyhow!("Secret key must not be empty"));
    }
    if key.len() > 128 {
        return Err(anyhow!("Secret key must be at most 128 characters"));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(anyhow!(
            "Secret key must contain only alphanumeric characters, underscores, and hyphens"
        ));
    }
    Ok(())
}

pub fn validate_secret_value(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(anyhow!("Secret value must not be empty"));
    }
    if value.len() > 8192 {
        return Err(anyhow!("Secret value must be at most 8192 characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_secret_long() {
        assert_eq!(mask_secret("ghp_abc123xyz"), "ghp***");
    }

    #[test]
    fn test_mask_secret_exactly_six() {
        assert_eq!(mask_secret("123456"), "123***");
    }

    #[test]
    fn test_mask_secret_five() {
        assert_eq!(mask_secret("short"), "***");
    }

    #[test]
    fn test_mask_secret_empty() {
        assert_eq!(mask_secret(""), "***");
    }

    #[test]
    fn test_mask_secret_one_char() {
        assert_eq!(mask_secret("x"), "***");
    }

    #[test]
    fn test_validate_secret_key_valid() {
        assert!(validate_secret_key("my_key-1").is_ok());
        assert!(validate_secret_key("ABC").is_ok());
        assert!(validate_secret_key("a".repeat(128).as_str()).is_ok());
    }

    #[test]
    fn test_validate_secret_key_empty() {
        assert!(validate_secret_key("").is_err());
    }

    #[test]
    fn test_validate_secret_key_too_long() {
        assert!(validate_secret_key(&"a".repeat(129)).is_err());
    }

    #[test]
    fn test_validate_secret_key_special_chars() {
        assert!(validate_secret_key("key.with.dots").is_err());
        assert!(validate_secret_key("key with spaces").is_err());
        assert!(validate_secret_key("key!").is_err());
    }

    #[test]
    fn test_validate_secret_value_valid() {
        assert!(validate_secret_value("some_value").is_ok());
        assert!(validate_secret_value(&"x".repeat(8192)).is_ok());
    }

    #[test]
    fn test_validate_secret_value_empty() {
        assert!(validate_secret_value("").is_err());
    }

    #[test]
    fn test_validate_secret_value_too_long() {
        assert!(validate_secret_value(&"x".repeat(8193)).is_err());
    }
}
