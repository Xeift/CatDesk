use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct Owner {
    pid: u32,
    token: String,
}

pub struct OwnedFileLock {
    path: PathBuf,
    token: String,
}

impl Drop for OwnedFileLock {
    fn drop(&mut self) {
        if let Ok(text) = fs::read_to_string(&self.path) {
            if toml::from_str::<Owner>(&text).is_ok_and(|owner| owner.token == self.token) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

pub fn acquire(path: &Path, retries: usize) -> io::Result<OwnedFileLock> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    for _ in 0..retries {
        let token = Uuid::new_v4().to_string();
        let owner = Owner {
            pid: std::process::id(),
            token: token.clone(),
        };
        let temporary = path.with_extension(format!("{token}.tmp"));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(
                toml::to_string(&owner)
                    .map_err(io::Error::other)?
                    .as_bytes(),
            )?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let link_result = fs::hard_link(&temporary, path);
        let _ = fs::remove_file(&temporary);
        match link_result {
            Ok(()) => {
                return Ok(OwnedFileLock {
                    path: path.to_path_buf(),
                    token,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let owner = fs::read_to_string(path)
                    .ok()
                    .and_then(|text| toml::from_str::<Owner>(&text).ok());
                // Never evict a live owner based solely on an elapsed timestamp.
                if owner.is_some_and(|owner| !crate::workspace_router::process_is_alive(owner.pid))
                {
                    let _ = fs::remove_file(path);
                    continue;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(format!(
        "timed out waiting for owned file lock: {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_owner_is_not_stolen_and_drop_removes_only_own_lock() {
        let path = std::env::temp_dir().join(format!("catdesk-lock-{}.toml", Uuid::new_v4()));
        let guard = acquire(&path, 1).unwrap();
        assert!(acquire(&path, 2).is_err());
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
        let _second = acquire(&path, 1).unwrap();
    }
}
