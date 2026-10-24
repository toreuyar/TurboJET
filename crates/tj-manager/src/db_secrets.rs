use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use async_trait::async_trait;
use tj_db::Database;
use tj_secrets::SecretsStore;

use crate::secrets_crypto;

pub struct DbSecretsStore {
    db: Arc<Database>,
    cipher: Aes256Gcm,
}

impl DbSecretsStore {
    pub fn open(db: Arc<Database>) -> anyhow::Result<Self> {
        tracing::debug!("Opening DbSecretsStore");
        let cipher = secrets_crypto::create_cipher()?;
        tracing::debug!("DbSecretsStore opened successfully");
        Ok(Self { db, cipher })
    }
}

#[async_trait]
impl SecretsStore for DbSecretsStore {
    async fn set(&self, project_id: &str, key: &str, value: &str) -> anyhow::Result<()> {
        tracing::debug!(project_id = %project_id, key = %key, value_len = value.len(), "DbSecretsStore::set");

        let encrypted = secrets_crypto::encrypt(value, &self.cipher)?;
        tracing::debug!(project_id = %project_id, key = %key, encrypted_len = encrypted.len(), "Value encrypted, saving to DB");

        let db = Arc::clone(&self.db);
        let project_id = project_id.to_string();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || db.save_secret(&project_id, &key, &encrypted))
            .await??;

        Ok(())
    }

    async fn get(&self, project_id: &str, key: &str) -> anyhow::Result<Option<String>> {
        tracing::debug!(project_id = %project_id, key = %key, "DbSecretsStore::get");

        let db = Arc::clone(&self.db);
        let project_id_owned = project_id.to_string();
        let key_owned = key.to_string();
        let encrypted =
            tokio::task::spawn_blocking(move || db.load_secret(&project_id_owned, &key_owned))
                .await??;

        match encrypted {
            Some(enc) => {
                tracing::debug!(project_id = %project_id, key = %key, encrypted_len = enc.len(), "Secret found, decrypting");
                let plaintext = secrets_crypto::decrypt(&enc, &self.cipher)?;
                tracing::debug!(project_id = %project_id, key = %key, "Secret decrypted successfully");
                Ok(Some(plaintext))
            }
            None => {
                tracing::debug!(project_id = %project_id, key = %key, "Secret not found");
                Ok(None)
            }
        }
    }

    async fn delete(&self, project_id: &str, key: &str) -> anyhow::Result<()> {
        tracing::debug!(project_id = %project_id, key = %key, "DbSecretsStore::delete");

        let db = Arc::clone(&self.db);
        let project_id = project_id.to_string();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || db.delete_secret(&project_id, &key)).await??;

        Ok(())
    }

    async fn list(&self, project_id: &str) -> anyhow::Result<Vec<String>> {
        tracing::debug!(project_id = %project_id, "DbSecretsStore::list");

        let db = Arc::clone(&self.db);
        let project_id_owned = project_id.to_string();
        let keys =
            tokio::task::spawn_blocking(move || db.list_secrets(&project_id_owned)).await??;

        tracing::debug!(project_id = %project_id, count = keys.len(), "DbSecretsStore::list completed");
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static ENV_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    fn temp_db() -> Arc<Database> {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path.to_str().unwrap();
        Arc::new(Database::open(db_path_str).unwrap())
    }

    fn set_test_master_key() {
        unsafe {
            std::env::set_var(
                "TURBOJET_MASTER_KEY",
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            );
        }
    }

    fn clear_test_master_key() {
        unsafe {
            std::env::remove_var("TURBOJET_MASTER_KEY");
        }
    }

    #[tokio::test]
    async fn encrypt_decrypt_roundtrip_through_store() {
        let _lock = ENV_LOCK.lock().unwrap();
        set_test_master_key();
        let db = temp_db();
        let store = DbSecretsStore::open(db).unwrap();

        let project_id = "test-project";
        let key = "api_token";
        let value = "super-secret-value-12345";

        store.set(project_id, key, value).await.unwrap();
        let result = store.get(project_id, key).await.unwrap();

        assert_eq!(result, Some(value.to_string()));
    }

    #[tokio::test]
    async fn get_returns_none_for_missing_key() {
        let _lock = ENV_LOCK.lock().unwrap();
        set_test_master_key();
        let db = temp_db();
        let store = DbSecretsStore::open(db).unwrap();

        let result = store
            .get("nonexistent-project", "missing-key")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let _lock = ENV_LOCK.lock().unwrap();
        set_test_master_key();
        let db = temp_db();
        let store = DbSecretsStore::open(db).unwrap();

        let project_id = "test-project";
        let key = "temp_key";

        store.set(project_id, key, "temp-value").await.unwrap();
        store.delete(project_id, key).await.unwrap();
        store.delete(project_id, key).await.unwrap();

        let result = store.get(project_id, key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn list_returns_sorted_keys() {
        let _lock = ENV_LOCK.lock().unwrap();
        set_test_master_key();
        let db = temp_db();
        let store = DbSecretsStore::open(db).unwrap();

        let project_id = "test-project";

        store.set(project_id, "zebra_key", "val1").await.unwrap();
        store.set(project_id, "alpha_key", "val2").await.unwrap();
        store.set(project_id, "middle_key", "val3").await.unwrap();

        let keys = store.list(project_id).await.unwrap();
        assert_eq!(keys, vec!["alpha_key", "middle_key", "zebra_key"]);
    }

    #[tokio::test]
    async fn missing_master_key_returns_error() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_test_master_key();
        let db = temp_db();
        let result = DbSecretsStore::open(db);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn overwrite_secret() {
        let _lock = ENV_LOCK.lock().unwrap();
        set_test_master_key();
        let db = temp_db();
        let store = DbSecretsStore::open(db).unwrap();

        let project_id = "test-project";
        let key = "rotating_key";

        store.set(project_id, key, "old-value").await.unwrap();
        store.set(project_id, key, "new-value").await.unwrap();

        let result = store.get(project_id, key).await.unwrap();
        assert_eq!(result, Some("new-value".to_string()));
    }
}
