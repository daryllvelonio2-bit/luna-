//! Atomic file writes and a cooperative store lock.
//!
//! No extra dependencies: the lock is an exclusive-create file with stale
//! detection, and writes go through a temp file in the same directory followed
//! by `rename`, which is atomic on every supported filesystem.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use harness_core::HarnessError;

const STALE_LOCK: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(25);
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "record".to_string());
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{name}.tmp.{}.{nonce}", std::process::id()))
}

/// Write `bytes` to `path` atomically: temp file, `sync_all`, rename.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn write_str(path: &Path, contents: &str) -> io::Result<()> {
    write(path, contents.as_bytes())
}

/// Remove a temp file left behind by a crashed writer, if any.
pub fn cleanup_temp(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return;
    };
    let prefix = format!(".{name}.tmp.");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// A cooperative exclusive lock held for the duration of a write batch.
pub struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquire the store lock. Waits up to ten seconds, clearing a stale lock
/// (older than thirty seconds) so a crashed process cannot wedge the store.
pub fn lock(path: &Path) -> Result<StoreLock, HarnessError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let deadline = SystemTime::now() + LOCK_TIMEOUT;
    loop {
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} at={}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                );
                return Ok(StoreLock {
                    path: path.to_path_buf(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(path) {
                    let _ = fs::remove_file(path);
                    continue;
                }
                if SystemTime::now() >= deadline {
                    return Err(HarnessError::MemoryError(format!(
                        "Timed out waiting for the memory store lock at {}",
                        path.display()
                    )));
                }
                std::thread::sleep(LOCK_RETRY);
            }
            Err(error) => return Err(io_err(error)),
        }
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age > STALE_LOCK)
        .unwrap_or(false)
}

fn io_err(error: io::Error) -> HarnessError {
    HarnessError::MemoryError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_atomically_and_replaces() {
        let dir = std::env::temp_dir().join(format!("luna-atomic-{}", std::process::id()));
        let path = dir.join("nested/record.md");
        write_str(&path, "one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        write_str(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("luna-lock-{}", std::process::id()));
        let path = dir.join(".lock");
        let held = lock(&path).unwrap();
        assert!(path.exists());
        drop(held);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
