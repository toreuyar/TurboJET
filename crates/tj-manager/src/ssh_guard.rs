//! Explicit SSH provisioning for repository clone/push (TJ-035).
//!
//! Both the private key and the verified `known_hosts` identity are project
//! secrets. Files are created atomically with mode 0600 in a 0700 directory;
//! SSH is forced to use only that key and host identity (no ambient agent,
//! user config, password prompt or `StrictHostKeyChecking=no` fallback).

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use tj_secrets::SecretsStore;

pub const SSH_PRIVATE_KEY_SECRET: &str = "__ssh_private_key";
pub const SSH_PUBLIC_KEY_SECRET: &str = "__ssh_public_key";
pub const SSH_KNOWN_HOSTS_SECRET: &str = "__ssh_known_hosts";

#[derive(Debug)]
pub struct SshKeyGuard {
    key_path: PathBuf,
    known_hosts_path: PathBuf,
    home: PathBuf,
}

impl SshKeyGuard {
    /// Returns `Ok(None)` only when the project has no SSH key configured. A
    /// configured key without a verified host identity is an explicit error.
    pub async fn new(
        secrets_store: &dyn SecretsStore,
        project_id: &str,
        home: &Path,
    ) -> Result<Option<Self>> {
        tracing::debug!(project_id = %project_id, "SshKeyGuard: checking for SSH key");

        let private_key = match secrets_store.get(project_id, SSH_PRIVATE_KEY_SECRET).await {
            Ok(Some(key)) if !key.trim().is_empty() => key,
            Ok(_) => {
                tracing::debug!(project_id = %project_id, "SshKeyGuard: no SSH key configured");
                return Ok(None);
            }
            Err(error) => {
                tracing::error!(project_id = %project_id, %error, "SshKeyGuard: failed to get SSH key");
                return Err(error);
            }
        };
        let known_hosts = match secrets_store.get(project_id, SSH_KNOWN_HOSTS_SECRET).await {
            Ok(Some(value)) if !value.trim().is_empty() => value,
            Ok(_) => anyhow::bail!(
                "Project has an SSH key but no verified host identity (secret {SSH_KNOWN_HOSTS_SECRET}); refusing an unverified SSH connection"
            ),
            Err(error) => return Err(error).context("failed to read verified SSH host identity"),
        };
        validate_known_hosts(&known_hosts)?;

        let home = PathBuf::from(home);
        std::fs::create_dir_all(&home)?;
        let tmp_dir = home.join("tmp");
        std::fs::create_dir_all(&tmp_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(&tmp_dir)?;
            anyhow::ensure!(
                meta.file_type().is_dir(),
                "SSH temporary path {} is not a directory",
                tmp_dir.display()
            );
            std::fs::set_permissions(&tmp_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        let id = uuid::Uuid::new_v4();
        let key_path = tmp_dir.join(format!("ssh_key_{id}"));
        write_secret_file(&key_path, private_key.as_bytes())?;
        let known_hosts_path = tmp_dir.join(format!("known_hosts_{id}"));
        if let Err(error) = write_secret_file(&known_hosts_path, known_hosts.as_bytes()) {
            let _ = std::fs::remove_file(&key_path);
            return Err(error);
        }

        tracing::debug!(key_path = %key_path.display(), known_hosts = %known_hosts_path.display(),
            "SshKeyGuard: key and verified host identity created with 0600 permissions");

        Ok(Some(Self {
            key_path,
            known_hosts_path,
            home: tmp_dir,
        }))
    }

    pub fn ssh_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(
            "GIT_SSH_COMMAND".to_string(),
            format!(
                "ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o BatchMode=yes \
                 -o StrictHostKeyChecking=yes -o UserKnownHostsFile={} \
                 -o GlobalKnownHostsFile=/dev/null -o ConnectTimeout=15 \
                 -o ServerAliveInterval=15 -o ServerAliveCountMax=2",
                shell_quote(&self.key_path.display().to_string()),
                shell_quote(&self.known_hosts_path.display().to_string())
            ),
        );
        env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
        env.insert("GIT_ASKPASS".to_string(), "/bin/false".to_string());
        env
    }

    pub fn key_path(&self) -> &Path {
        &self.key_path
    }
    pub fn known_hosts_path(&self) -> &Path {
        &self.known_hosts_path
    }
    pub fn temp_dir(&self) -> &Path {
        &self.home
    }
}

impl Drop for SshKeyGuard {
    fn drop(&mut self) {
        tracing::debug!(key_path = %self.key_path.display(), "SshKeyGuard: cleaning up temporary SSH material");
        let _ = std::fs::remove_file(&self.key_path);
        let _ = std::fs::remove_file(&self.known_hosts_path);
    }
}

/// Atomic, restrictive secret file creation. `create_new` refuses an existing
/// symlink/regular file; data is written and synced before a rename inside the
/// same 0700 directory.
pub fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .context("secret path has no parent directory")?;
    let meta = std::fs::symlink_metadata(parent)?;
    anyhow::ensure!(
        meta.file_type().is_dir(),
        "secret parent is not a directory: {}",
        parent.display()
    );
    anyhow::ensure!(path.file_name().is_some(), "secret path has no file name");
    // Never replace an existing file or symlink. `rename` would replace the
    // link itself, but refusing early makes accidental reuse explicit and
    // closes the window for a pre-created attacker path.
    if std::fs::symlink_metadata(path).is_ok() {
        anyhow::bail!(
            "secret file already exists; refusing to replace {}",
            path.display()
        );
    }
    let temporary = parent.join(format!(".tj-secret-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "cannot create restricted secret file {}",
                    temporary.display()
                )
            })?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        drop(file);
        std::fs::rename(&temporary, path)
            .with_context(|| format!("cannot atomically install secret file {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn validate_known_hosts(value: &str) -> Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "verified host identity is empty");
    for line in value
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let mut fields = line.split_whitespace();
        let host = fields.next().unwrap_or_default();
        let key_type = fields.next().unwrap_or_default();
        let key = fields.next().unwrap_or_default();
        anyhow::ensure!(
            !host.is_empty()
                && matches!(
                    key_type,
                    "ssh-ed25519"
                        | "ssh-rsa"
                        | "ecdsa-sha2-nistp256"
                        | "ecdsa-sha2-nistp384"
                        | "ecdsa-sha2-nistp521"
                )
                && !key.is_empty(),
            "known_hosts line is not a recognised host/key entry: {line}"
        );
    }
    Ok(())
}

pub fn is_ssh_remote(url: &str) -> bool {
    url.starts_with("ssh://")
        || url.starts_with("git+ssh://")
        // scp-like syntax: user@host:path (but not https://...)
        || (!url.contains("://") && url.split_once(':').is_some_and(|(head, _)| head.contains('@')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap as StdHashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemorySecrets(Mutex<StdHashMap<(String, String), String>>);

    #[async_trait]
    impl SecretsStore for MemorySecrets {
        async fn get(&self, project_id: &str, key: &str) -> Result<Option<String>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(&(project_id.into(), key.into()))
                .cloned())
        }
        async fn set(&self, project_id: &str, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert((project_id.into(), key.into()), value.into());
            Ok(())
        }
        async fn delete(&self, project_id: &str, key: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .remove(&(project_id.into(), key.into()));
            Ok(())
        }
        async fn list(&self, project_id: &str) -> Result<Vec<String>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .keys()
                .filter(|(p, _)| p == project_id)
                .map(|(_, k)| k.clone())
                .collect())
        }
    }

    const KEY: &str =
        "-----BEGIN OPENSSH PRIVATE KEY-----\ntest\n-----END OPENSSH PRIVATE KEY-----\n";
    const HOSTS: &str = "forge.example.test ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestOnly\n";

    #[tokio::test]
    async fn key_and_known_hosts_are_restrictive_and_cleaned_up() {
        let secrets = MemorySecrets::default();
        secrets.set("p", SSH_PRIVATE_KEY_SECRET, KEY).await.unwrap();
        secrets
            .set("p", SSH_KNOWN_HOSTS_SECRET, HOSTS)
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let guard = SshKeyGuard::new(&secrets, "p", dir.path())
            .await
            .unwrap()
            .unwrap();
        let key = guard.key_path().to_path_buf();
        let hosts = guard.known_hosts_path().to_path_buf();
        for path in [&key, &hosts] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let env = guard.ssh_env();
        let command = &env["GIT_SSH_COMMAND"];
        assert!(command.contains("StrictHostKeyChecking=yes"));
        assert!(command.contains("UserKnownHostsFile="));
        assert!(command.contains("BatchMode=yes"));
        assert!(!command.contains("StrictHostKeyChecking=no"));
        drop(guard);
        assert!(!key.exists());
        assert!(!hosts.exists());
    }

    #[tokio::test]
    async fn configured_key_without_verified_host_fails_closed() {
        let secrets = MemorySecrets::default();
        secrets.set("p", SSH_PRIVATE_KEY_SECRET, KEY).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let error = SshKeyGuard::new(&secrets, "p", dir.path())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("verified host identity"), "{error}");
    }

    #[tokio::test]
    async fn missing_key_is_explicitly_absent_not_unverified() {
        let secrets = MemorySecrets::default();
        let dir = tempfile::tempdir().unwrap();
        assert!(
            SshKeyGuard::new(&secrets, "p", dir.path())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn ssh_url_detection_covers_scp_and_url_forms() {
        assert!(is_ssh_remote("git@forge.example:owner/repo.git"));
        assert!(is_ssh_remote("ssh://git@forge.example/owner/repo.git"));
        assert!(!is_ssh_remote("https://forge.example/owner/repo.git"));
        assert!(!is_ssh_remote("file:///tmp/repo.git"));
    }

    #[test]
    fn atomic_secret_writer_refuses_existing_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"attacker").unwrap();
        let path = dir.path().join("key");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(write_secret_file(&path, b"secret").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker");
    }

    #[test]
    fn invalid_known_hosts_is_rejected() {
        assert!(validate_known_hosts("not a host entry").is_err());
        assert!(validate_known_hosts(HOSTS).is_ok());
    }
}
