use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::error::{Errx, Resultx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// The absolute, symlink-free form of `path`. Fails if `path` does not exist.
///
/// Navigation needs this: `Path::parent` on a relative path runs out of
/// components immediately, so `.` has no parent to go up to.
pub fn resolve(path: &Path) -> Resultx<PathBuf> {
    fs::canonicalize(path).map_err(|e| Errx::e_io(e, format!("resolving {}", path.display())))
}

/// Directory listing, directories first and alphabetical within each group.
///
/// Entries that cannot be read are skipped rather than failing the whole listing.
pub fn read_dir(path: &Path) -> Resultx<Vec<DirEntry>> {
    log::debug!("Reading directory: {}", path.display());

    let mut entries: Vec<DirEntry> = fs::read_dir(path)
        .map_err(|e| {
            log::error!("Failed to read directory {}: {}", path.display(), e);
            Errx::e_io(e, format!("reading {}", path.display()))
        })?
        .filter_map(Result::ok)
        .map(|entry| DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: is_dir(&entry),
        })
        .collect();

    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

/// [`read_dir`] prefixed with the `.` and `..` navigation entries.
pub fn read_dir_with_dots(path: &Path) -> Resultx<Vec<DirEntry>> {
    let dots = [".", ".."].map(|name| DirEntry {
        name: name.to_string(),
        is_dir: true,
    });

    let mut entries = Vec::from(dots);
    entries.extend(read_dir(path)?);
    Ok(entries)
}

/// `DirEntry::file_type` does not follow symlinks, so a link to a directory would
/// look like a file and refuse to open. Only links pay for the extra `stat`.
fn is_dir(entry: &fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(file_type) if file_type.is_symlink() => entry.path().is_dir(),
        Ok(file_type) => file_type.is_dir(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_directories_before_files() {
        let tmp = TempDir::new();
        tmp.file("zebra.txt", "");
        tmp.dir("apple");
        tmp.file("banana.txt", "");
        tmp.dir("zoo");

        let entries = read_dir(tmp.path()).unwrap();

        assert_eq!(names(&entries), ["apple", "zoo", "banana.txt", "zebra.txt"]);
    }

    #[test]
    fn lists_hidden_files() {
        let tmp = TempDir::new();
        tmp.file(".gitignore", "");
        tmp.dir(".git");
        tmp.file("visible.txt", "");

        let entries = read_dir(tmp.path()).unwrap();

        assert_eq!(names(&entries), [".git", ".gitignore", "visible.txt"]);
    }

    #[test]
    fn lists_an_empty_directory_as_empty() {
        let tmp = TempDir::new();

        assert_eq!(read_dir(tmp.path()).unwrap(), []);
    }

    #[test]
    fn does_not_descend_into_subdirectories() {
        let tmp = TempDir::new();
        tmp.file("nested/deep.txt", "");

        let entries = read_dir(tmp.path()).unwrap();

        assert_eq!(names(&entries), ["nested"]);
    }

    #[test]
    fn reading_a_missing_directory_fails() {
        let tmp = TempDir::new();

        assert!(read_dir(&tmp.path().join("nope")).is_err());
    }

    #[test]
    fn reading_a_file_as_a_directory_fails() {
        let tmp = TempDir::new();
        let file = tmp.file("notadir.txt", "");

        assert!(read_dir(&file).is_err());
    }

    #[test]
    fn dots_come_first_and_are_directories() {
        let tmp = TempDir::new();
        tmp.dir("aaa");

        let entries = read_dir_with_dots(tmp.path()).unwrap();

        assert_eq!(names(&entries), [".", "..", "aaa"]);
        assert!(entries.iter().all(|e| e.is_dir));
    }

    #[test]
    fn dots_are_listed_even_for_an_empty_directory() {
        let tmp = TempDir::new();

        let entries = read_dir_with_dots(tmp.path()).unwrap();

        assert_eq!(names(&entries), [".", ".."]);
    }

    #[test]
    fn resolve_makes_a_relative_path_absolute() {
        let tmp = TempDir::new();
        tmp.dir("sub");

        let resolved = resolve(&tmp.path().join("sub/..")).unwrap();

        assert_eq!(resolved, tmp.path());
        assert!(resolved.is_absolute());
    }

    #[test]
    fn resolving_a_missing_path_fails() {
        let tmp = TempDir::new();

        assert!(resolve(&tmp.path().join("nope")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_directory_counts_as_a_directory() {
        let tmp = TempDir::new();
        let target = tmp.dir("real");
        tmp.file("real/inside.txt", "");
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();

        let entries = read_dir(tmp.path()).unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();

        assert!(link.is_dir, "symlinked directories must be enterable");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_file_is_not_a_directory() {
        let tmp = TempDir::new();
        let target = tmp.file("real.txt", "hi");
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();

        let entries = read_dir(tmp.path()).unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();

        assert!(!link.is_dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_is_not_a_directory() {
        let tmp = TempDir::new();
        std::os::unix::fs::symlink(tmp.path().join("gone"), tmp.path().join("link")).unwrap();

        let entries = read_dir(tmp.path()).unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();

        assert!(!link.is_dir);
    }

    /// Linux only: APFS enforces UTF-8, so such a name cannot even be created on macOS.
    #[cfg(target_os = "linux")]
    #[test]
    fn names_that_are_not_utf8_are_still_listed() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let tmp = TempDir::new();
        // 0xff is never valid UTF-8, so the name has to come back lossily converted.
        let raw = OsStr::from_bytes(b"bad-\xff-name");
        fs::write(tmp.path().join(raw), "").unwrap();

        let entries = read_dir(tmp.path()).unwrap();

        assert_eq!(names(&entries), ["bad-\u{fffd}-name"]);
    }

    /// Assumes the test suite is not run as root, which ignores permission bits.
    #[cfg(unix)]
    #[test]
    fn reading_a_directory_without_permission_fails() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new();
        let locked = tmp.dir("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let result = read_dir(&locked);

        // Restore before asserting, so a failure still leaves a removable temp dir.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
    }
}
