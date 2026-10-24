//! Process-level database ownership lock (TJ-025 restore safety).
//!
//! A running `tj-api` holds an advisory `flock` on `<database>.lock` for its
//! whole lifetime. `tj-ops restore` takes the same non-blocking lock before it
//! replaces a live database/artifact tree, so an active server is refused
//! instead of having its files swapped underneath it. The lock is advisory and
//! local to this host; it does not replace filesystem permissions.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct DatabaseProcessLock {
    file: File,
    path: PathBuf,
}

impl DatabaseProcessLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DatabaseProcessLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this guard and remains valid.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn lock_path_for_database(database_path: &str) -> Result<PathBuf> {
    let expanded = shellexpand::env(database_path)
        .map_err(|error| {
            anyhow::anyhow!("Failed to expand database path '{database_path}': {error}")
        })?
        .to_string();
    Ok(PathBuf::from(format!("{expanded}.lock")))
}

/// Try once. `None` means another process currently owns the lock.
pub fn try_acquire_for_database(database_path: &str) -> Result<Option<DatabaseProcessLock>> {
    let path = lock_path_for_database(database_path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create database directory {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot open database lock {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    // SAFETY: flock only inspects the valid descriptor.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK)
            || error.raw_os_error() == Some(libc::EAGAIN)
        {
            return Ok(None);
        }
        return Err(error).with_context(|| format!("cannot lock {}", path.display()));
    }
    Ok(Some(DatabaseProcessLock { file, path }))
}

pub fn acquire_for_database(database_path: &str) -> Result<DatabaseProcessLock> {
    try_acquire_for_database(database_path)?.with_context(|| {
        format!(
            "database '{}' is in use by another TurboJet process; stop it before starting another instance",
            database_path
        )
    })
}
