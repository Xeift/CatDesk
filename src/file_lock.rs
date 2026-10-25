use fs4::fs_std::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

/// OS-backed advisory lock on a persistent inode. Never unlink a lock file:
/// removing it permits another process to lock a different inode at the same path.
pub struct OwnedFileLock {
    _file: File,
}

pub fn acquire(path: &Path, retries: usize) -> io::Result<OwnedFileLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    for _ in 0..retries {
        match file.try_lock_exclusive() {
            Ok(true) => return Ok(OwnedFileLock { _file: file }),
            Ok(false) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(format!(
        "timed out waiting for file lock: {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn live_owner_is_not_stolen_and_file_remains_after_unlock() {
        let path = std::env::temp_dir().join(format!("catdesk-lock-{}.toml", Uuid::new_v4()));
        let guard = acquire(&path, 1).unwrap();
        assert!(acquire(&path, 2).is_err());
        assert!(path.exists());
        drop(guard);
        assert!(path.exists());
        let second = acquire(&path, 1).unwrap();
        drop(second);
        fs::remove_file(path).unwrap();
    }
}
