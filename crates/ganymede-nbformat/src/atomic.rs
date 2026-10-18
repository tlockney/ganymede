//! Atomic file replacement, shared by notebook writes and the contents layer.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Longest file name most filesystems allow, in bytes.
const NAME_MAX: usize = 255;
/// Bytes of the temp name that are not the original name: the `.~` prefix,
/// `-`, the random part `tempfile` adds (6 bytes) and the `.tmp` suffix.
const TEMP_OVERHEAD: usize = 2 + 1 + 6 + 4;
/// Symlink hops followed before giving up, as Linux's `ELOOP` limit.
const MAX_SYMLINK_HOPS: usize = 40;

/// Write `bytes` to `path` atomically: a temp file in the same directory,
/// fsync, rename over the target. A reader sees the old contents or the new,
/// never a truncated file, and a failed write leaves the old file alone.
///
/// The mode of an existing file is kept; a new file gets `0666 & !umask`
/// like `creat(2)`. When `path` is a symlink, the file it points to is
/// replaced and the link is left in place.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let target = resolve_symlinks(path.as_ref())?;
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        Some(_) => Path::new("."),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} has no parent directory", target.display()),
            ))
        }
    };
    let name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name", target.display()),
        )
    })?;
    let existing_permissions = fs::metadata(&target)
        .ok()
        .filter(fs::Metadata::is_file)
        .map(|m| m.permissions());

    let prefix = temp_prefix(name);
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix).suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Some(permissions) = existing_permissions {
        tmp.as_file().set_permissions(permissions)?;
    }
    tmp.persist(&target).map_err(|e| e.error)?;
    // Make the rename durable too. Not every filesystem allows fsync on a
    // directory handle, so a failure here is not an error.
    if let Ok(dir_handle) = fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

/// Follow `path` through any symlinks to the file they name. A dangling
/// link resolves to its (missing) target, which the write then creates.
fn resolve_symlinks(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&current)?;
                current = match current.parent() {
                    Some(parent) if link.is_relative() => parent.join(link),
                    _ => link,
                };
            }
            Ok(_) => return Ok(current),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

/// `.~<name>-`, with `<name>` cut short so the whole temp name fits in
/// [`NAME_MAX`] bytes.
fn temp_prefix(name: &std::ffi::OsStr) -> OsString {
    let budget = NAME_MAX - TEMP_OVERHEAD;
    let lossy = name.to_string_lossy();
    let mut end = lossy.len().min(budget);
    while !lossy.is_char_boundary(end) {
        end -= 1;
    }
    OsString::from(format!(".~{}-", &lossy[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_prefix_fits_name_max() {
        let long = "é".repeat(200); // 400 bytes
        let prefix = temp_prefix(std::ffi::OsStr::new(&long));
        assert!(prefix.len() + 6 + 4 <= NAME_MAX);
        assert_eq!(temp_prefix(std::ffi::OsStr::new("a.ipynb")), ".~a.ipynb-");
    }
}
