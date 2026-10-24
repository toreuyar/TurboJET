//! Self-hosted backup/restore, readiness and versioned packaging (TJ-025).
//!
//! Everything here operates on explicit paths supplied by the operator. The
//! test suite uses disposable directories; there is no implicit system install,
//! service mutation or production database access.

#[cfg(test)]
use aes_gcm::aead::AeadCore;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub const BACKUP_FORMAT_VERSION: u32 = 1;
pub const PACKAGE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub format_version: u32,
    pub turbojet_version: String,
    pub created_at: String,
    pub database_sha256: String,
    pub database_bytes: u64,
    pub database_schema_version: i64,
    pub artifacts_sha256: Option<String>,
    pub artifacts_files: u64,
    pub secret_rows: u64,
    pub master_key_fingerprint: Option<String>,
    pub encrypted_secrets_require_external_key: bool,
    /// True when the snapshot ran under the exclusive database instance lock
    /// with no live service. Older backups deserialize as `false`.
    #[serde(default)]
    pub offline_exclusive_lock: bool,
    pub exclusions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageManifest {
    pub format_version: u32,
    pub turbojet_version: String,
    pub created_at: String,
    pub files: Vec<String>,
    pub binary_sha256: String,
    pub webui_sha256: String,
    /// Explicit packaging policy. Older manifests default to an empty list.
    #[serde(default)]
    pub exclusions: Vec<String>,
}

/// Local, secret-adjacent, runtime and tool-home paths that must never be
/// copied into a package. Git ignore rules are not an authority here: packaging
/// applies its own policy even when a path is untracked or ignored.
pub const PACKAGE_EXCLUSIONS: &[&str] = &[
    "config/local/**",
    ".env",
    ".env.*",
    "**/.git/**",
    "**/node_modules/**",
    "**/.config/**",
    "**/.local/**",
    ".factory/**",
    ".isolator-staging/**",
    ".opencode/**",
    ".turbojet/**",
    "target/**",
    "check-results/**",
    "webui/test-results/**",
    "webui/playwright-report/**",
    "**/*.db",
    "**/*.db-wal",
    "**/*.db-shm",
    "**/*.db.lock",
    "**/*.log",
];

fn package_path_excluded(relative: &Path) -> bool {
    let components: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_string())
        .collect();
    // In the packaged config tree a top-level `local/` is the local-only
    // subtree (`config/local` in the checkout), regardless of whether the
    // caller passed the repository config root or renamed it.
    if components
        .first()
        .is_some_and(|first| first == "local" || first == "config")
        && (components.first().is_some_and(|first| first == "local")
            || components.get(1).is_some_and(|second| second == "local"))
    {
        return true;
    }
    if components.iter().any(|component| {
        matches!(
            component.as_str(),
            ".git"
                | "node_modules"
                | ".config"
                | ".local"
                | ".factory"
                | ".isolator-staging"
                | ".opencode"
                | ".turbojet"
                | "target"
                | "check-results"
        )
    }) {
        return true;
    }
    let joined = components.join("/");
    if joined.starts_with("webui/test-results") || joined.starts_with("webui/playwright-report") {
        return true;
    }
    let name = relative
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    name.ends_with(".db")
        || name.ends_with(".db-wal")
        || name.ends_with(".db-shm")
        || name.ends_with(".db.lock")
        || name.ends_with(".log")
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn tree_digest(root: &Path) -> Result<(String, u64)> {
    if !root.exists() {
        return Ok((String::new(), 0));
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    for (relative, path) in files {
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
        count += 1;
    }
    Ok((hex::encode(hasher.finalize()), count))
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in
        fs::read_dir(current).with_context(|| format!("cannot read {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "refusing to back up symlink {} (prevents escaping the artifact root)",
                path.display()
            );
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, path));
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(destination)?;
    let mut files = Vec::new();
    collect_files(source, source, &mut files)?;
    for (relative, path) in files {
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&path, &target)
            .with_context(|| format!("cannot copy {} to {}", path.display(), target.display()))?;
    }
    Ok(())
}

/// Config templates are human-managed files. Skip symlinks (for example a
/// nested tool cache) instead of refusing the whole package; backups of runtime
/// artifacts use the strict variant above.
fn copy_tree_skip_symlinks(
    root: &Path,
    current: &Path,
    destination: &Path,
    files: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if metadata.file_type().is_symlink() || package_path_excluded(relative) {
            continue;
        }
        if !destination.exists() {
            fs::create_dir_all(destination)?;
        }
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree_skip_symlinks(root, &path, &target, files)?;
        } else if metadata.is_file() {
            fs::copy(&path, &target)?;
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn key_fingerprint() -> Option<String> {
    let key = std::env::var("TURBOJET_MASTER_KEY").ok()?;
    if key.len() != 64 || hex::decode(&key).ok()?.len() != 32 {
        return None;
    }
    Some(hex::encode(&Sha256::digest(key.as_bytes()))[..16].to_string())
}

/// Supported backup policy: **offline/quiesced only**.
///
/// The backup takes the same exclusive `<database>.lock` that a running
/// `tj-api` holds and that `restore` already respects. It keeps that lock for
/// the entire DB snapshot + artifact copy, so a live service is refused rather
/// than producing a cross-store snapshot that mixes different points in time.
/// The supported procedure is stop -> backup -> start.
///
/// The DB is snapshotted with SQLite `VACUUM INTO` (consistent under WAL);
/// artifacts are hashed and copied while no service can write them.
pub fn create_backup(
    db_path: &Path,
    artifacts: Option<&Path>,
    out: &Path,
) -> Result<BackupManifest> {
    create_backup_observed(db_path, artifacts, out, || {})
}

/// Internal seam that lets the unit tests observe that the exclusive lock is
/// held for the whole snapshot. Production calls the wrapper above with a
/// no-op observer.
fn create_backup_observed<F: FnOnce()>(
    db_path: &Path,
    artifacts: Option<&Path>,
    out: &Path,
    after_lock: F,
) -> Result<BackupManifest> {
    anyhow::ensure!(
        !out.exists(),
        "backup destination already exists: {}",
        out.display()
    );
    anyhow::ensure!(
        db_path.is_file(),
        "database not found: {}",
        db_path.display()
    );
    // Hold the instance lock for the entire DB + artifact snapshot. A live
    // server owns this lock for its lifetime, so this fails explicitly instead
    // of silently taking a non-atomic hot backup.
    let _instance_lock = tj_db::try_acquire_for_database(&db_path.to_string_lossy())?
        .with_context(|| {
            format!(
                "backup refused: database {} is in use by a running TurboJet process. \
                 Stop the service, run the offline backup, then start it again (stop -> backup -> start)",
                db_path.display()
            )
        })?;
    after_lock();
    fs::create_dir_all(out)?;

    let result = (|| -> Result<BackupManifest> {
        let snapshot = out.join("state.db");
        let db = tj_db::Database::open(&db_path.to_string_lossy())?;
        db.backup_to(&snapshot)?;
        let secret_rows = db.secret_ciphertexts()?.len() as u64;
        let schema = db.schema_version()?;
        let (artifact_hash, artifact_files) = match artifacts {
            Some(root) => tree_digest(root)?,
            None => (String::new(), 0),
        };
        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            turbojet_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            database_sha256: sha256_file(&snapshot)?,
            database_bytes: fs::metadata(&snapshot)?.len(),
            database_schema_version: schema,
            artifacts_sha256: artifacts.map(|_| artifact_hash),
            artifacts_files: artifact_files,
            secret_rows,
            master_key_fingerprint: key_fingerprint(),
            encrypted_secrets_require_external_key: true,
            offline_exclusive_lock: true,
            exclusions: vec![
                "TURBOJET_MASTER_KEY (external secret; never written to backup)".into(),
                ".env files (operator-managed)".into(),
                "runtime logs (retained separately by policy)".into(),
            ],
        };
        if let Some(root) = artifacts {
            copy_tree(root, &out.join("artifacts"))?;
            let (copied_hash, copied_files) = tree_digest(&out.join("artifacts"))?;
            anyhow::ensure!(
                copied_hash == manifest.artifacts_sha256.clone().unwrap_or_default()
                    && copied_files == manifest.artifacts_files,
                "artifact backup verification failed"
            );
        }
        fs::write(
            out.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(manifest)
    })();
    if result.is_err() {
        // The destination did not exist before this call; never leave a
        // plausible-looking partial backup that an operator might restore.
        let _ = fs::remove_dir_all(out);
    }
    result
}

pub fn verify_backup(backup: &Path, verify_key: bool) -> Result<BackupManifest> {
    let manifest: BackupManifest = serde_json::from_slice(
        &fs::read(backup.join("manifest.json")).context("backup manifest.json is missing")?,
    )?;
    anyhow::ensure!(
        manifest.format_version == BACKUP_FORMAT_VERSION,
        "unsupported backup format version {}",
        manifest.format_version
    );
    let db = backup.join("state.db");
    anyhow::ensure!(db.is_file(), "backup state.db is missing");
    anyhow::ensure!(
        sha256_file(&db)? == manifest.database_sha256,
        "backup database checksum mismatch"
    );
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    anyhow::ensure!(
        integrity == "ok",
        "backup integrity check failed: {integrity}"
    );
    let artifacts = backup.join("artifacts");
    let (artifact_hash, artifact_files) = tree_digest(&artifacts)?;
    if let Some(expected) = &manifest.artifacts_sha256 {
        anyhow::ensure!(
            &artifact_hash == expected,
            "backup artifact checksum mismatch"
        );
        anyhow::ensure!(
            artifact_files == manifest.artifacts_files,
            "backup artifact count mismatch"
        );
    }
    if verify_key && manifest.secret_rows > 0 {
        verify_secret_key(&conn)?;
    }
    Ok(manifest)
}

fn verify_secret_key(conn: &rusqlite::Connection) -> Result<()> {
    let key = std::env::var("TURBOJET_MASTER_KEY")
        .context("restored backup contains encrypted secrets but TURBOJET_MASTER_KEY is not set")?;
    let raw = hex::decode(&key).context("TURBOJET_MASTER_KEY is not hex")?;
    anyhow::ensure!(
        raw.len() == 32,
        "TURBOJET_MASTER_KEY must be 32 bytes (64 hex chars)"
    );
    let cipher =
        Aes256Gcm::new_from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid master key"))?;
    let mut stmt = conn.prepare("SELECT project_id, key, encrypted_value FROM project_secrets")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (project, secret_key, encrypted) = row?;
        let combined = BASE64
            .decode(&encrypted)
            .context("secret ciphertext is not base64")?;
        anyhow::ensure!(combined.len() >= 13, "secret ciphertext is too short");
        let (nonce, ciphertext) = combined.split_at(12);
        cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot decrypt secret {project}/{secret_key} with the supplied external key"
                )
            })?;
    }
    Ok(())
}

/// Restore into an explicit directory. Never overwrites an existing TurboJet
/// state directory unless `force` is set, and even then only replaces the
/// known database/artifacts names.
pub fn restore_backup(
    backup: &Path,
    home: &Path,
    force: bool,
    verify_key: bool,
) -> Result<BackupManifest> {
    let manifest = verify_backup(backup, verify_key)?;
    if home.exists() && fs::read_dir(home)?.next().is_some() && !force {
        anyhow::bail!(
            "restore target is not empty: {} (use --force only after moving the current state aside)",
            home.display()
        );
    }
    fs::create_dir_all(home)?;
    let target_db = home.join("state.db");

    // Refuse to replace a database that a live process still owns. The server
    // holds <db>.lock for its lifetime; taking the same non-blocking lock is
    // safer than deleting/renaming files under an active WAL connection.
    let _live_lock = if target_db.exists() {
        match tj_db::try_acquire_for_database(&target_db.to_string_lossy())? {
            Some(lock) => Some(lock),
            None => anyhow::bail!(
                "restore refused: database {} is in use by a running TurboJet process; stop it first",
                target_db.display()
            ),
        }
    } else {
        None
    };

    if target_db.exists() {
        anyhow::ensure!(
            force,
            "restore would replace existing state.db; use --force"
        );
    }

    // Stage both stores first, then install artifacts and finally the DB. A
    // failed copy never leaves a partially written target; if the final DB
    // install fails, the previous artifact tree is restored.
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let staged_db = home.join(format!(".state.db.restore-{suffix}"));
    fs::copy(backup.join("state.db"), &staged_db)
        .with_context(|| format!("cannot stage restored database {}", staged_db.display()))?;
    restrict_file(&staged_db)?;

    let target_artifacts = home.join("artifacts");
    let backup_artifacts = backup.join("artifacts");
    let staged_artifacts = home.join(format!(".artifacts.restore-{suffix}"));
    let old_artifacts = home.join(format!(".artifacts.previous-{suffix}"));
    let mut installed_artifacts = false;
    if backup_artifacts.exists() {
        copy_tree(&backup_artifacts, &staged_artifacts)?;
        restrict_tree(&staged_artifacts)?;
        if target_artifacts.exists() {
            anyhow::ensure!(
                force,
                "restore would replace existing artifacts; use --force"
            );
            fs::rename(&target_artifacts, &old_artifacts).with_context(|| {
                format!(
                    "cannot move existing artifacts {} aside",
                    target_artifacts.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&staged_artifacts, &target_artifacts) {
            if old_artifacts.exists() {
                let _ = fs::rename(&old_artifacts, &target_artifacts);
            }
            return Err(error).with_context(|| {
                format!(
                    "cannot atomically install restored artifacts {}",
                    target_artifacts.display()
                )
            });
        }
        installed_artifacts = true;
    } else if target_artifacts.exists() {
        anyhow::ensure!(
            force,
            "restore would replace existing artifacts; use --force"
        );
        fs::rename(&target_artifacts, &old_artifacts).with_context(|| {
            format!(
                "cannot move existing artifacts {} aside",
                target_artifacts.display()
            )
        })?;
    }

    if let Err(error) = fs::rename(&staged_db, &target_db) {
        // Best effort rollback of the artifact side; the old DB was never
        // replaced, so the failed restore remains the previous state.
        if installed_artifacts && target_artifacts.exists() {
            let _ = fs::remove_dir_all(&target_artifacts);
        }
        if old_artifacts.exists() {
            let _ = fs::rename(&old_artifacts, &target_artifacts);
        }
        let _ = fs::remove_file(&staged_db);
        return Err(error).with_context(|| {
            format!(
                "cannot atomically install restored database {}",
                target_db.display()
            )
        });
    }
    if old_artifacts.exists() {
        fs::remove_dir_all(&old_artifacts).ok();
    }

    let restored = BackupManifest {
        created_at: chrono::Utc::now().to_rfc3339(),
        ..manifest
    };
    fs::write(
        home.join("restore-manifest.json"),
        serde_json::to_vec_pretty(&restored)?,
    )?;
    Ok(restored)
}

fn restrict_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn restrict_tree(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "restored artifact tree contains a symlink: {}",
                path.display()
            );
            if metadata.is_dir() {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                restrict_tree(&path)?;
            } else {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            }
        }
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Versioned local package. It never installs, starts or removes anything.
pub fn create_package(
    out: &Path,
    binary: &Path,
    webui: &Path,
    config: Option<&Path>,
    workflows: Option<&Path>,
    version: &str,
) -> Result<PackageManifest> {
    anyhow::ensure!(binary.is_file(), "binary not found: {}", binary.display());
    anyhow::ensure!(webui.is_file(), "built UI not found: {}", webui.display());
    let root = out.join(format!("turbojet-{version}"));
    anyhow::ensure!(!root.exists(), "package already exists: {}", root.display());
    fs::create_dir_all(root.join("bin"))?;
    fs::create_dir_all(root.join("share/webui"))?;
    fs::copy(binary, root.join("bin/tj-api"))?;
    fs::copy(webui, root.join("share/webui/index.html"))?;
    let mut files = vec![
        "bin/tj-api".to_string(),
        "share/webui/index.html".to_string(),
    ];
    if let Some(config) = config {
        anyhow::ensure!(
            config.exists(),
            "config path not found: {}",
            config.display()
        );
        let mut config_files = Vec::new();
        copy_tree_skip_symlinks(config, config, &root.join("config"), &mut config_files)?;
        files.extend(config_files.into_iter().map(|rel| format!("config/{rel}")));
    }
    if let Some(workflows) = workflows {
        anyhow::ensure!(
            workflows.is_dir(),
            "workflow package directory not found: {}",
            workflows.display()
        );
        let mut workflow_files = Vec::new();
        copy_tree_skip_symlinks(
            workflows,
            workflows,
            &root.join("workflows"),
            &mut workflow_files,
        )?;
        files.extend(
            workflow_files
                .into_iter()
                .map(|rel| format!("workflows/{rel}")),
        );
    }
    fs::write(root.join("VERSION"), format!("{version}\n"))?;
    fs::write(
        root.join("start.sh"),
        "#!/usr/bin/env bash\nset -euo pipefail\nroot=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\n\
         export TURBOJET_WEBUI_PATH=\"${TURBOJET_WEBUI_PATH:-$root/share/webui/index.html}\"\n\
         default_config=\"$root/config/turbojet.selfhosted.json\"\n\
         [ -f \"$default_config\" ] || default_config=\"$root/config/turbojet.json\"\n\
         exec \"$root/bin/tj-api\" \"${1:-$default_config}\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("start.sh"), fs::Permissions::from_mode(0o755))?;
    }
    files.push("VERSION".into());
    files.push("start.sh".into());
    files.sort();
    let mut sums = String::new();
    for relative in &files {
        let path = root.join(relative);
        sums.push_str(&format!("{}  {}\n", sha256_file(&path)?, relative));
    }
    fs::write(root.join("SHA256SUMS"), sums)?;
    let manifest = PackageManifest {
        format_version: PACKAGE_FORMAT_VERSION,
        turbojet_version: version.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        files: files.clone(),
        binary_sha256: sha256_file(&root.join("bin/tj-api"))?,
        webui_sha256: sha256_file(&root.join("share/webui/index.html"))?,
        exclusions: PACKAGE_EXCLUSIONS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect(),
    };
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static ENV_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    fn seed_home(root: &Path) -> (PathBuf, PathBuf) {
        let home = root.join("home");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(artifacts.join("runs/run-1")).unwrap();
        let db_path = home.join("state.db");
        let db = tj_db::Database::open(&db_path.to_string_lossy()).unwrap();
        db.save_project(&tj_db::Project {
            id: "project".into(),
            name: "Demo".into(),
            repo_url: None,
            base_branch: "main".into(),
            workspace_path: None,
            git_provider: None,
            profile: "flow-a".into(),
            workspace_kind: None,
            catalog_id: None,
            provisioning_state: None,
            provisioning_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .unwrap();
        // Encrypt with a test key so the restore/verify-key path is exercised.
        let key = [17u8; 32];
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce = Aes256Gcm::generate_nonce(&mut aes_gcm::aead::OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, b"real secret value".as_slice())
            .unwrap();
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        db.save_secret("project", "api_token", &BASE64.encode(combined))
            .unwrap();
        fs::write(artifacts.join("runs/run-1/artifact.md"), "durable artifact").unwrap();
        (db_path, artifacts)
    }

    #[test]
    fn backup_restores_consistent_db_artifacts_and_encrypted_secrets() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup");
        let manifest = create_backup(&db_path, Some(&artifacts), &backup).unwrap();
        assert_eq!(manifest.secret_rows, 1);
        assert_eq!(manifest.artifacts_files, 1);
        assert!(manifest.encrypted_secrets_require_external_key);
        assert!(!backup.join("TURBOJET_MASTER_KEY").exists());

        let restored = dir.path().join("restored");
        restore_backup(&backup, &restored, false, false).unwrap();
        let db = tj_db::Database::open(&restored.join("state.db").to_string_lossy()).unwrap();
        assert_eq!(db.list_projects().unwrap()[0].name, "Demo");
        assert_eq!(db.secret_ciphertexts().unwrap()[0].1, "api_token");
        assert_eq!(
            fs::read_to_string(restored.join("artifacts/runs/run-1/artifact.md")).unwrap(),
            "durable artifact"
        );
        assert!(restored.join("restore-manifest.json").exists());
    }

    #[test]
    fn backup_requires_the_exclusive_instance_lock_and_releases_it() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup-locked");

        // A live service (or any process) holding <db>.lock owns the same
        // exclusive lock: an online backup is refused, not silently attempted.
        let live = tj_db::try_acquire_for_database(&db_path.to_string_lossy())
            .unwrap()
            .expect("live lock");
        let error = create_backup(&db_path, Some(&artifacts), &backup)
            .unwrap_err()
            .to_string();
        assert!(error.contains("in use"), "{error}");
        assert!(error.contains("stop -> backup -> start"), "{error}");
        assert!(!backup.join("state.db").exists());
        drop(live);

        // The lock is held for the whole snapshot (observed here immediately
        // after acquisition) and released when the backup returns.
        let observed = std::cell::Cell::new(false);
        let manifest = create_backup_observed(&db_path, Some(&artifacts), &backup, || {
            assert!(
                tj_db::try_acquire_for_database(&db_path.to_string_lossy())
                    .unwrap()
                    .is_none(),
                "backup must hold the exclusive lock while it snapshots DB + artifacts"
            );
            observed.set(true);
        })
        .unwrap();
        assert!(observed.get());
        assert!(manifest.offline_exclusive_lock);
        assert!(
            tj_db::try_acquire_for_database(&db_path.to_string_lossy())
                .unwrap()
                .is_some(),
            "backup must release the lock when it finishes"
        );
    }

    #[test]
    fn failed_offline_backup_releases_the_lock_and_never_claims_consistency() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        // Symlinks are refused by the strict artifact walker, so the failure
        // happens after the lock is taken.
        std::os::unix::fs::symlink("/etc/passwd", artifacts.join("runs/run-1/escape")).unwrap();
        let backup = dir.path().join("backup-failed");
        let error = create_backup(&db_path, Some(&artifacts), &backup)
            .unwrap_err()
            .to_string();
        assert!(error.contains("symlink"), "{error}");
        assert!(
            !backup.exists(),
            "a failed backup must not leave a partial destination"
        );
        assert!(
            tj_db::try_acquire_for_database(&db_path.to_string_lossy())
                .unwrap()
                .is_some(),
            "failed backup must not leave the instance lock held"
        );
    }

    #[test]
    fn offline_backup_is_a_point_in_time_snapshot_even_after_later_live_writes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup-snapshot");
        create_backup(&db_path, Some(&artifacts), &backup).unwrap();

        // Simulate writes after the backup: DB project rename and artifact
        // replacement. Restore must yield the backed-up pair, not the newer
        // mixed state.
        let live = tj_db::Database::open(&db_path.to_string_lossy()).unwrap();
        let mut project = live.list_projects().unwrap().remove(0);
        project.name = "Renamed after backup".into();
        live.save_project(&project).unwrap();
        drop(live);
        fs::write(
            artifacts.join("runs/run-1/artifact.md"),
            "changed after backup",
        )
        .unwrap();

        let restored = dir.path().join("restored-snapshot");
        restore_backup(&backup, &restored, false, false).unwrap();
        let restored_db =
            tj_db::Database::open(&restored.join("state.db").to_string_lossy()).unwrap();
        assert_eq!(restored_db.list_projects().unwrap()[0].name, "Demo");
        assert_eq!(
            fs::read_to_string(restored.join("artifacts/runs/run-1/artifact.md")).unwrap(),
            "durable artifact"
        );
        let manifest = verify_backup(&backup, false).unwrap();
        assert!(manifest.offline_exclusive_lock);
        assert_eq!(manifest.secret_rows, 1);
    }

    #[test]
    fn failed_upgrade_restores_the_pre_upgrade_snapshot() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup");
        create_backup(&db_path, Some(&artifacts), &backup).unwrap();

        // Simulate a failed upgrade by corrupting the live database.
        fs::write(&db_path, b"not a sqlite database").unwrap();
        assert!(tj_db::Database::open(&db_path.to_string_lossy()).is_err());

        let restored = dir.path().join("restored");
        let manifest = restore_backup(&backup, &restored, false, false).unwrap();
        assert_eq!(manifest.format_version, BACKUP_FORMAT_VERSION);
        let db = tj_db::Database::open(&restored.join("state.db").to_string_lossy()).unwrap();
        assert_eq!(db.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn restore_refuses_to_overwrite_nonempty_state_without_force() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup");
        create_backup(&db_path, Some(&artifacts), &backup).unwrap();
        let target = dir.path().join("occupied");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("state.db"), b"existing").unwrap();
        assert!(restore_backup(&backup, &target, false, false).is_err());
        assert_eq!(fs::read(target.join("state.db")).unwrap(), b"existing");
    }

    #[test]
    fn external_master_key_is_required_to_verify_restored_secrets() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::remove_var("TURBOJET_MASTER_KEY");
        }
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup");
        create_backup(&db_path, Some(&artifacts), &backup).unwrap();
        assert!(
            verify_backup(&backup, true).is_err(),
            "missing external key must fail"
        );
        unsafe {
            std::env::set_var("TURBOJET_MASTER_KEY", "00".repeat(32));
        }
        assert!(verify_backup(&backup, true).is_err(), "wrong key must fail");
        unsafe {
            std::env::set_var("TURBOJET_MASTER_KEY", "11".repeat(32));
        }
        verify_backup(&backup, true).expect("correct external key decrypts restored secrets");
        unsafe {
            std::env::remove_var("TURBOJET_MASTER_KEY");
        }
    }

    #[test]
    fn restore_refuses_an_active_database_lock_and_restores_with_restrictive_modes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db_path, artifacts) = seed_home(dir.path());
        let backup = dir.path().join("backup");
        create_backup(&db_path, Some(&artifacts), &backup).unwrap();

        // Simulate a running server holding the process lock for the target.
        let target = dir.path().join("active-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("state.db"), b"live state").unwrap();
        let live = tj_db::try_acquire_for_database(&target.join("state.db").to_string_lossy())
            .unwrap()
            .expect("fresh lock");
        let error = restore_backup(&backup, &target, true, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("in use"), "{error}");
        assert_eq!(
            std::fs::read(target.join("state.db")).unwrap(),
            b"live state"
        );
        drop(live);

        let restored = dir.path().join("restored-modes");
        restore_backup(&backup, &restored, false, false).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let db_mode = std::fs::metadata(restored.join("state.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                db_mode, 0o600,
                "restored DB must not be group/world readable"
            );
            let artifact_mode =
                std::fs::metadata(restored.join("artifacts/runs/run-1/artifact.md"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
            assert_eq!(artifact_mode, 0o600);
        }
    }

    #[test]
    fn package_is_versioned_checksummed_and_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("tj-api");
        let webui = dir.path().join("index.html");
        fs::write(&binary, b"binary").unwrap();
        fs::write(&webui, b"<html></html>").unwrap();
        let config = dir.path().join("config");
        fs::create_dir_all(config.join("stages/stage-a")).unwrap();
        fs::create_dir_all(config.join("local")).unwrap();
        fs::create_dir_all(config.join("example-stage")).unwrap();
        let tool_home = config.join("stages/stage-a/.local/share/opencode/storage");
        fs::create_dir_all(tool_home.join("session_diff")).unwrap();
        fs::write(config.join("fixture.json"), "{}").unwrap();
        fs::write(config.join("stages/stage-a/agent.json"), "{}").unwrap();
        fs::write(config.join("local/private-workflow.smg.json"), "{}").unwrap();
        fs::write(config.join("local/private-assets.smg.json"), "{}").unwrap();
        fs::write(config.join("example-stage/agent.json"), "{}").unwrap();
        fs::write(tool_home.join("migration"), "1").unwrap();
        fs::write(
            tool_home.join("session_diff/ses_placeholder.json"),
            "{\"runtime\":true}",
        )
        .unwrap();
        fs::write(config.join("server.log"), "secret-adjacent log").unwrap();
        fs::write(config.join(".env"), "PROVIDER_API_KEY=must-not-ship").unwrap();
        let workflows = dir.path().join("workflows");
        fs::create_dir_all(workflows.join("example/agents/step")).unwrap();
        fs::create_dir_all(workflows.join("example/.local")).unwrap();
        fs::write(workflows.join("example/workflow.smg.json"), "{}").unwrap();
        fs::write(workflows.join("example/agents/step/agent.json"), "{}").unwrap();
        fs::write(workflows.join("example/agents/step/prompt.md"), "prompt").unwrap();
        fs::write(workflows.join("example/.local/runtime-state"), "hidden").unwrap();
        let out = dir.path().join("dist");
        fs::create_dir_all(&out).unwrap();
        let manifest = create_package(
            &out,
            &binary,
            &webui,
            Some(&config),
            Some(&workflows),
            "0.1.0-test",
        )
        .unwrap();
        let root = out.join("turbojet-0.1.0-test");
        assert!(root.join("bin/tj-api").is_file());
        assert!(root.join("share/webui/index.html").is_file());
        assert_eq!(
            fs::read_to_string(root.join("VERSION")).unwrap().trim(),
            "0.1.0-test"
        );
        assert!(root.join("SHA256SUMS").is_file());
        assert!(
            !root.join("config/.env").exists(),
            "package must not contain runtime credentials"
        );
        assert!(
            !root.join("config/local").exists(),
            "package must exclude ignored local workflows and assets"
        );
        assert!(
            !root.join("config/stages/stage-a/.local").exists(),
            "package must exclude nested tool-home state such as ~/.local"
        );
        assert!(
            manifest
                .files
                .iter()
                .all(|file| !file.split('/').any(|component| component == ".local")),
            "package manifest must not list tool-home runtime state"
        );
        assert!(
            !root.join("config/server.log").exists(),
            "package must exclude runtime logs"
        );
        assert!(root.join("config/stages/stage-a/agent.json").is_file());
        assert!(root.join("config/example-stage/agent.json").is_file());
        assert!(
            root.join("workflows/example/workflow.smg.json").is_file(),
            "package must include shipped workflow packages"
        );
        assert!(
            root.join("workflows/example/agents/step/prompt.md")
                .is_file(),
            "package must include workflow package assets"
        );
        assert!(
            !root.join("workflows/example/.local").exists(),
            "package must exclude tool-home state under workflows"
        );
        assert!(
            manifest
                .files
                .iter()
                .any(|file| file == "workflows/example/workflow.smg.json"),
            "package manifest must list workflow packages"
        );
        assert_eq!(manifest.files.len(), 10);
        let local_pattern = format!("config/{}/**", "local");
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|pattern| pattern == &local_pattern)
        );
        assert!(manifest.exclusions.iter().any(|pattern| pattern == ".env"));
        assert!(
            manifest
                .exclusions
                .iter()
                .any(|pattern| pattern == "**/.local/**")
        );
        // Format v1 readers still accept a manifest written before exclusions.
        let mut old = serde_json::to_value(&manifest).unwrap();
        old.as_object_mut().unwrap().remove("exclusions");
        let read: PackageManifest = serde_json::from_value(old).unwrap();
        assert!(read.exclusions.is_empty());
    }
}
