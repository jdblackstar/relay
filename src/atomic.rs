use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let target = atomic_target_path(path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let permissions = match fs::metadata(&target) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "expected file path but found directory",
            ));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };

    let temp_path = atomic_temp_path(&target);
    remove_temp_path_if_exists(&temp_path)?;

    let result = write_atomic_inner(&target, &temp_path, contents, permissions.as_ref());
    if result.is_err() {
        let _ = remove_temp_path_if_exists(&temp_path);
    }
    result
}

fn write_atomic_inner(
    target: &Path,
    temp_path: &Path,
    contents: &[u8],
    permissions: Option<&fs::Permissions>,
) -> io::Result<()> {
    let mut file = open_temp_file(temp_path, permissions)?;
    file.write_all(contents)?;
    if let Some(permissions) = permissions {
        fs::set_permissions(temp_path, permissions.clone())?;
    }
    file.sync_all()?;
    drop(file);

    fs::rename(temp_path, target)?;
    sync_parent_dir(target)
}

fn atomic_target_path(path: &Path) -> io::Result<PathBuf> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(err) => return Err(err),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(path.to_path_buf());
    }

    let target_metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Dangling symlink: `metadata` follows the link and fails; resolve the
            // link text so the write can create the target (matching `fs::write`).
            let link = fs::read_link(path)?;
            return Ok(if link.is_absolute() {
                link
            } else {
                path.parent()
                    .map(|parent| parent.join(&link))
                    .unwrap_or(link)
            });
        }
        Err(err) => return Err(err),
    };
    if target_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "expected file path but found directory",
        ));
    }
    fs::canonicalize(path)
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "relay".into());
    let temp_name = format!(".{file_name}.relay.tmp");
    path.parent()
        .map(|parent| parent.join(&temp_name))
        .unwrap_or_else(|| PathBuf::from(temp_name))
}

fn remove_temp_path_if_exists(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(unix)]
fn open_temp_file(temp_path: &Path, permissions: Option<&fs::Permissions>) -> io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    if let Some(permissions) = permissions {
        options.mode(permissions.mode() & 0o7777);
    }
    options.open(temp_path)
}

#[cfg(not(unix))]
fn open_temp_file(
    temp_path: &Path,
    _permissions: Option<&fs::Permissions>,
) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
}

fn sync_parent_dir(target: &Path) -> io::Result<()> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    let dir = fs::File::open(parent)?;
    dir.sync_all()
}
