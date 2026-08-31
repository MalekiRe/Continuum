use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StateLockError {
    #[error("create state directory {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("open state lock {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("another Continuum process already owns {0}")]
    AlreadyRunning(PathBuf),
    #[error("lock state directory {path}: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub struct StateLock {
    file: File,
}

impl StateLock {
    pub fn acquire(root: impl AsRef<Path>) -> Result<Self, StateLockError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| StateLockError::Create {
            path: root.to_path_buf(),
            source,
        })?;
        let path = root.join("continuum.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| StateLockError::Open {
                path: path.clone(),
                source,
            })?;
        // SAFETY: flock uses a valid owned file descriptor and no pointers.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Self { file });
        }
        let source = io::Error::last_os_error();
        if source
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Err(StateLockError::AlreadyRunning(root.to_path_buf()))
        } else {
            Err(StateLockError::Lock { path, source })
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        // SAFETY: this unlocks the same valid descriptor acquired above.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_directory_has_exactly_one_owner() {
        let root =
            std::env::temp_dir().join(format!("continuum-state-lock-{}", uuid::Uuid::new_v4()));
        let first = StateLock::acquire(&root).unwrap();
        assert!(matches!(
            StateLock::acquire(&root),
            Err(StateLockError::AlreadyRunning(_))
        ));
        drop(first);
        StateLock::acquire(&root).unwrap();
    }
}
