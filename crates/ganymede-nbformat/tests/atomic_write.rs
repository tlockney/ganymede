//! `write_file` replaces the file atomically (#74).

use std::fs;
use std::path::Path;

use ganymede_nbformat::{atomic_write, Notebook};

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn write_file_replaces_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("n.ipynb");
    fs::write(&path, "old").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let nb = Notebook::new();
    nb.write_file(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), nb.to_file_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }
    assert_eq!(names(dir.path()), ["n.ipynb"]);
}

#[test]
fn failed_write_keeps_the_old_file() {
    let dir = tempfile::tempdir().unwrap();
    // The parent is missing, so the temp file cannot be created.
    let path = dir.path().join("missing").join("n.ipynb");
    assert!(Notebook::new().write_file(&path).is_err());
    assert_eq!(names(dir.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn writing_through_a_symlink_replaces_the_target() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    fs::create_dir(&real_dir).unwrap();
    let real = real_dir.join("n.ipynb");
    fs::write(&real, "old").unwrap();
    let link = dir.path().join("link.ipynb");
    symlink("real/n.ipynb", &link).unwrap();
    // A link to a link, too.
    let link2 = dir.path().join("link2.ipynb");
    symlink(&link, &link2).unwrap();

    atomic_write(&link2, b"new").unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::symlink_metadata(&link2)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    assert_eq!(names(&real_dir), ["n.ipynb"]);
}

#[test]
fn long_file_names_get_a_short_enough_temp_name() {
    let dir = tempfile::tempdir().unwrap();
    // 250 bytes plus `.ipynb`: legal, but `.~<name>-XXXXXX.tmp` would not be.
    let name = format!("{}.ipynb", "a".repeat(249));
    assert_eq!(name.len(), 255);
    let path = dir.path().join(&name);
    atomic_write(&path, b"x").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"x");
    assert_eq!(names(dir.path()), [name]);
}
