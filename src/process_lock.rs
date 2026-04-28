use crate::config::Config;
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;

#[derive(Debug)]
pub(crate) struct ProcessLock {
    #[cfg(unix)]
    file: File,
}

impl ProcessLock {
    pub(crate) fn acquire(operation: &str) -> io::Result<Self> {
        let path = Config::lock_path()?;
        Self::acquire_at(&path, operation, false)
    }

    #[cfg(unix)]
    fn acquire_at(path: &Path, operation: &str, nonblocking: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        crate::logging::debug(&format!(
            "process lock waiting operation={operation} path={}",
            path.display()
        ));
        flock(file.as_raw_fd(), nonblocking).map_err(|err| {
            if nonblocking && err.kind() == io::ErrorKind::WouldBlock {
                return io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "another relay process is already running a mutating operation: {}",
                        path.display()
                    ),
                );
            }
            err
        })?;
        crate::logging::debug(&format!(
            "process lock acquired operation={operation} path={}",
            path.display()
        ));
        Ok(Self { file })
    }

    #[cfg(not(unix))]
    fn acquire_at(_path: &Path, _operation: &str, _nonblocking: bool) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "relay process locking requires Unix flock",
        ))
    }
}

#[cfg(unix)]
impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = unlock(self.file.as_raw_fd());
    }
}

#[cfg(unix)]
fn flock(fd: i32, nonblocking: bool) -> io::Result<()> {
    let mut flags = libc::LOCK_EX;
    if nonblocking {
        flags |= libc::LOCK_NB;
    }
    loop {
        // SAFETY: `fd` comes from a live `File`, and `flock` does not take ownership.
        let result = unsafe { libc::flock(fd, flags) };
        if result == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err);
    }
}

#[cfg(unix)]
fn unlock(fd: i32) -> io::Result<()> {
    loop {
        // SAFETY: `fd` comes from a live `File`, and `flock` does not take ownership.
        let result = unsafe { libc::flock(fd, libc::LOCK_UN) };
        if result == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_creates_runtime_lock_file() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("runtime/relay.lock");

        let _lock = ProcessLock::acquire_at(&path, "test", false)?;

        assert!(path.exists());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn second_nonblocking_acquire_reports_contention() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("runtime/relay.lock");
        let _lock = ProcessLock::acquire_at(&path, "first", false)?;

        let err = ProcessLock::acquire_at(&path, "second", true).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(err.to_string().contains("another relay process"));
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn lock_releases_when_guard_drops() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("runtime/relay.lock");
        {
            let _lock = ProcessLock::acquire_at(&path, "first", false)?;
        }

        let _lock = ProcessLock::acquire_at(&path, "second", true)?;

        Ok(())
    }

    #[test]
    fn acquire_uses_config_lock_path() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home)?;
        std::env::set_var("RELAY_HOME", home.to_string_lossy().as_ref());

        let lock = ProcessLock::acquire("test");
        let lock_path = Config::lock_path();
        std::env::remove_var("RELAY_HOME");
        let _lock = lock?;
        assert!(lock_path?.ends_with("relay/runtime/relay.lock"));
        Ok(())
    }
}
