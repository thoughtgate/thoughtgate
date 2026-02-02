//! RAII config backup/restore guard with advisory file locking.
//!
//! Implements: REQ-CORE-008 §10.2 (ConfigGuard), §5.3 (Filesystem Constraints)
//!
//! `ConfigGuard` acquires an advisory flock on a separate lock file
//! (`<config_path>.thoughtgate-lock`) and ensures the original config is
//! restored on drop — whether the process exits normally, panics, or receives
//! a signal.
//!
//! The lock file is separate from the config file because editors (VS Code,
//! Cursor) perform atomic saves by writing a temp file and renaming over the
//! target, which changes the inode and silently breaks locks held on the
//! original file.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;

use crate::wrap::config_adapter::ConfigError;

/// RAII guard that restores the original config file on drop.
///
/// Holds an exclusive advisory lock on `<config_path>.thoughtgate-lock` for
/// its entire lifetime, preventing concurrent `thoughtgate wrap` invocations
/// from corrupting the same config file.
///
/// Implements: REQ-CORE-008 §10.2
pub struct ConfigGuard {
    /// Path to the agent's config file.
    config_path: PathBuf,
    /// Path to the backup created before rewriting.
    backup_path: PathBuf,
    /// Open file handle holding the advisory flock.
    /// The flock is released when this handle is dropped — the field is intentionally
    /// never read; it exists solely to keep the lock alive for the guard's lifetime.
    _lock_file: File,
    /// Path to the lock file (for cleanup on drop).
    lock_path: PathBuf,
    /// Whether `restore()` has already been called (prevents double-restore).
    restored: AtomicBool,
}

impl ConfigGuard {
    /// Create a new config guard with an exclusive advisory lock.
    ///
    /// Acquires `flock(LOCK_EX | LOCK_NB)` on `<config_path>.thoughtgate-lock`.
    /// Returns `ConfigError::Locked` if another ThoughtGate instance already
    /// holds the lock.
    ///
    /// # Arguments
    ///
    /// * `config_path` - The agent's config file path (will be restored on drop).
    /// * `backup_path` - The backup file created by the adapter's `rewrite_config`.
    ///
    /// Implements: REQ-CORE-008 §5.3
    pub fn new(config_path: &Path, backup_path: &Path) -> Result<Self, ConfigError> {
        let mut lock_path_os = config_path.as_os_str().to_os_string();
        lock_path_os.push(".thoughtgate-lock");
        let lock_path = PathBuf::from(lock_path_os);

        let lock_file = File::create(&lock_path)?;
        lock_file
            .try_lock_exclusive()
            .map_err(|_| ConfigError::Locked)?;

        Ok(Self {
            config_path: config_path.to_path_buf(),
            backup_path: backup_path.to_path_buf(),
            _lock_file: lock_file,
            lock_path,
            restored: AtomicBool::new(false),
        })
    }

    /// Restore the original config from backup.
    ///
    /// This method is idempotent: the first call performs the restore, subsequent
    /// calls return `Ok(())` immediately. Uses `AtomicBool::swap` with `SeqCst`
    /// ordering to ensure exactly-once semantics across concurrent callers
    /// (e.g., `Drop` + signal handler).
    ///
    /// Implements: REQ-CORE-008 §10.2
    pub fn restore(&self) -> Result<(), ConfigError> {
        // Swap returns the *previous* value. If it was already true, we've
        // already restored — return immediately.
        if self.restored.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // Copy backup over config.
        std::fs::copy(&self.backup_path, &self.config_path)?;

        // Best-effort removal of backup file.
        let _ = std::fs::remove_file(&self.backup_path);

        Ok(())
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        let _ = self.restore();
        // Advisory flock is released when the File handle is dropped (implicit).
        // Remove the lock file from disk.
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir_unique() -> PathBuf {
        use std::time::SystemTime;
        let d = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let dir =
            std::env::temp_dir().join(format!("tg-guard-{}-{}", d.as_secs(), d.subsec_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_config_guard_restore_idempotent() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");
        let backup_path = dir.join("config.json.thoughtgate-backup");

        // Write original config and backup.
        std::fs::write(&config_path, r#"{"rewritten": true}"#).unwrap();
        std::fs::write(&backup_path, r#"{"original": true}"#).unwrap();

        let guard = ConfigGuard::new(&config_path, &backup_path).unwrap();

        // First restore succeeds.
        guard.restore().unwrap();
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("original"));

        // Second restore is a no-op (idempotent).
        guard.restore().unwrap();

        // Cleanup: drop guard to release lock, then remove dir.
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_guard_creates_lock_file() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");
        let backup_path = dir.join("config.json.thoughtgate-backup");

        std::fs::write(&config_path, "{}").unwrap();
        std::fs::write(&backup_path, "{}").unwrap();

        let guard = ConfigGuard::new(&config_path, &backup_path).unwrap();

        let mut lock_path_os = config_path.as_os_str().to_os_string();
        lock_path_os.push(".thoughtgate-lock");
        let lock_path = PathBuf::from(lock_path_os);
        assert!(lock_path.exists());

        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_guard_drop_restores() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");
        let backup_path = dir.join("config.json.thoughtgate-backup");

        // Write "rewritten" as current config, "original" as backup.
        std::fs::write(&config_path, r#"{"rewritten": true}"#).unwrap();
        std::fs::write(&backup_path, r#"{"original": true}"#).unwrap();

        {
            let _guard = ConfigGuard::new(&config_path, &backup_path).unwrap();
            // Guard is dropped here.
        }

        // Config should be restored to original.
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("original"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
