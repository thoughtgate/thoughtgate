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
/// # Two-phase usage
///
/// For race-free config rewriting, acquire the lock **before** mutating the
/// config and set the backup path afterwards:
///
/// ```ignore
/// let guard = ConfigGuard::lock(&config_path)?;   // 1. acquire lock
/// let backup = adapter.rewrite_config(...)?;       // 2. rewrite under lock
/// guard.set_backup(&backup);                       // 3. enable restore-on-drop
/// ```
///
/// Implements: REQ-CORE-008 §10.2
pub struct ConfigGuard {
    /// Path to the agent's config file.
    config_path: PathBuf,
    /// Path to the backup created before rewriting.
    /// `None` until `set_backup()` is called — if the guard is dropped before
    /// a backup path is set, no restore is attempted (the config was never modified).
    backup_path: Option<PathBuf>,
    /// Open file handle holding the advisory flock.
    /// The flock is released when this handle is dropped — the field is intentionally
    /// never read; it exists solely to keep the lock alive for the guard's lifetime.
    _lock_file: File,
    /// Whether `restore()` has already been called (prevents double-restore).
    restored: AtomicBool,
}

impl ConfigGuard {
    /// Acquire an exclusive advisory lock on the config file.
    ///
    /// Acquires `flock(LOCK_EX | LOCK_NB)` on `<config_path>.thoughtgate-lock`.
    /// Returns `ConfigError::Locked` if another ThoughtGate instance already
    /// holds the lock.
    ///
    /// The returned guard does **not** yet know about a backup file. Call
    /// [`set_backup`](Self::set_backup) after rewriting the config to enable
    /// restore-on-drop.
    ///
    /// Implements: REQ-CORE-008 §5.3
    pub fn lock(config_path: &Path) -> Result<Self, ConfigError> {
        let mut lock_path_os = config_path.as_os_str().to_os_string();
        lock_path_os.push(".thoughtgate-lock");
        let lock_path = PathBuf::from(lock_path_os);

        let lock_file = File::create(&lock_path)?;
        lock_file
            .try_lock_exclusive()
            .map_err(|_| ConfigError::Locked)?;

        Ok(Self {
            config_path: config_path.to_path_buf(),
            backup_path: None,
            _lock_file: lock_file,
            restored: AtomicBool::new(false),
        })
    }

    /// Convenience constructor that acquires the lock and sets the backup path
    /// in one step.
    ///
    /// Equivalent to calling [`lock`](Self::lock) followed by
    /// [`set_backup`](Self::set_backup). Useful in tests and cases where the
    /// backup already exists before locking.
    ///
    /// Implements: REQ-CORE-008 §5.3
    pub fn new(config_path: &Path, backup_path: &Path) -> Result<Self, ConfigError> {
        let mut guard = Self::lock(config_path)?;
        guard.set_backup(backup_path);
        Ok(guard)
    }

    /// Set the backup path for restore-on-drop.
    ///
    /// Must be called after the config has been rewritten and a backup created.
    /// Until this is called, dropping the guard only releases the lock without
    /// attempting any file restoration.
    pub fn set_backup(&mut self, backup_path: &Path) {
        self.backup_path = Some(backup_path.to_path_buf());
    }

    /// Disable config restoration on drop.
    ///
    /// Sets the internal `restored` flag to `true` so that neither `restore()`
    /// nor `Drop` will copy the backup over the config. Used when `--no-restore`
    /// is set — the lock is still held for the guard's lifetime, but the config
    /// is left in its rewritten state.
    pub fn skip_restore(&self) {
        self.restored.store(true, Ordering::SeqCst);
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

        // If no backup path was set, the config was never rewritten — nothing
        // to restore.
        let backup_path = match &self.backup_path {
            Some(p) => p,
            None => return Ok(()),
        };

        // Copy backup over config.
        std::fs::copy(backup_path, &self.config_path)?;

        // Best-effort removal of backup file.
        let _ = std::fs::remove_file(backup_path);

        Ok(())
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        let _ = self.restore();
        // Advisory flock is released when the File handle (_lock_file) is dropped
        // immediately after this. The lock file is intentionally left on disk —
        // removing it creates a TOCTOU race where a new process could create a
        // fresh inode and acquire a lock on it before the old handle is closed.
        //
        // Lock files are zero-byte sentinels and accumulate across sessions.
        // A future `thoughtgate wrap --cleanup` command can safely remove stale
        // lock files when no instances are running.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir_unique() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering as AtOrd};
        use std::time::SystemTime;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let d = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let seq = COUNTER.fetch_add(1, AtOrd::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tg-guard-{}-{}-{}",
            d.as_secs(),
            d.subsec_nanos(),
            seq
        ));
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

    #[test]
    fn test_two_phase_lock_then_set_backup() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");
        let backup_path = dir.join("config.json.thoughtgate-backup");

        std::fs::write(&config_path, r#"{"original": true}"#).unwrap();

        // Phase 1: acquire lock (no backup yet).
        let mut guard = ConfigGuard::lock(&config_path).unwrap();

        // Simulate rewrite: overwrite config and create backup.
        std::fs::write(&backup_path, r#"{"original": true}"#).unwrap();
        std::fs::write(&config_path, r#"{"rewritten": true}"#).unwrap();

        // Phase 2: set backup path.
        guard.set_backup(&backup_path);

        // Drop should restore.
        drop(guard);

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            content.contains("original"),
            "config should be restored after two-phase lock+set_backup"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_lock_drop_without_backup_does_not_restore() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");

        std::fs::write(&config_path, r#"{"original": true}"#).unwrap();

        {
            // Lock without setting backup — simulates rewrite failure.
            let _guard = ConfigGuard::lock(&config_path).unwrap();
            // Overwrite config (simulating partial work).
            std::fs::write(&config_path, r#"{"partially_rewritten": true}"#).unwrap();
            // Guard dropped here without set_backup — should NOT attempt restore.
        }

        // Config should remain in its (partially) rewritten state since no
        // backup path was ever set.
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            content.contains("partially_rewritten"),
            "config should not be restored when no backup was set"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_lock_blocks_concurrent_lock() {
        let dir = temp_dir_unique();
        let config_path = dir.join("config.json");
        std::fs::write(&config_path, "{}").unwrap();

        let guard1 = ConfigGuard::lock(&config_path).unwrap();
        let result = ConfigGuard::lock(&config_path);
        assert!(
            matches!(result, Err(ConfigError::Locked)),
            "second lock() should fail while first is held"
        );

        drop(guard1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
