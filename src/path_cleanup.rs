use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn remove_with_owner_access(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if !metadata.is_dir() {
        return fs::remove_file(path);
    }

    make_directory_tree_owner_accessible(path)?;
    fs::remove_dir_all(path)
}

fn make_directory_tree_owner_accessible(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    make_directory_owner_accessible(path, &metadata)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_directory_tree_owner_accessible(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_directory_owner_accessible(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = metadata.permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700))
}

#[cfg(not(unix))]
fn make_directory_owner_accessible(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn recursive_cleanup_does_not_follow_directory_symlinks() -> io::Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let tmp = TempDir::new()?;
        let doomed = tmp.path().join("doomed");
        let preserved = tmp.path().join("preserved");
        fs::create_dir_all(doomed.join("nested"))?;
        fs::create_dir_all(&preserved)?;
        fs::write(preserved.join("keep.txt"), "keep")?;
        symlink(&preserved, doomed.join("nested/link"))?;
        fs::set_permissions(&preserved, fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(doomed.join("nested"), fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&doomed, fs::Permissions::from_mode(0o500))?;

        remove_with_owner_access(&doomed)?;

        assert!(!doomed.exists());
        assert_eq!(fs::read_to_string(preserved.join("keep.txt"))?, "keep");
        assert_eq!(
            fs::metadata(&preserved)?.permissions().mode() & 0o777,
            0o500
        );
        fs::set_permissions(&preserved, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}
