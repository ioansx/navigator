//! Carrying out staged operations, and the checks that need to ask the disk.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{
    error::{Errx, Resultx},
    plan::{Op, Problem},
};

/// `EXDEV`, the same value on Linux and macOS. `io::ErrorKind::CrossesDevices`
/// would say this properly but is still unstable.
const CROSS_DEVICE: i32 = 18;

/// Why `op` cannot run, asking the filesystem about anything the pure checks
/// in [`Op::problem`] could not know.
pub fn problem(op: &Op) -> Option<Problem> {
    if let Some(problem) = op.problem() {
        return Some(problem);
    }

    if let Some(source) = op.source()
        && !exists(source)
    {
        return Some(Problem::SourceMissing);
    }

    // Trash always has somewhere to put things; everything else writes a new path.
    if !matches!(op, Op::Trash(_)) && exists(op.destination()) {
        return Some(Problem::DestinationExists);
    }

    None
}

/// Runs `op`. Callers check [`problem`] first; this still refuses to clobber.
pub fn apply(op: &Op) -> Resultx<()> {
    match op {
        Op::CreateFile(path) => fs::File::create_new(path)
            .map(drop)
            .map_err(|e| Errx::e_io(e, format!("creating {}", path.display()))),

        Op::CreateDir(path) => {
            fs::create_dir(path).map_err(|e| Errx::e_io(e, format!("creating {}", path.display())))
        }

        Op::Trash(path) => trash(path).map(drop),

        Op::Copy { from, to } => copy_tree(from, to),

        Op::Move { from, to } => rename_or_move(from, to),
    }
}

/// Where deleted entries go. One directory per run, under a shared root.
pub fn trash_root() -> PathBuf {
    std::env::temp_dir()
        .join("nav-trash")
        .join(std::process::id().to_string())
}

/// Moves `path` into the trash, returning where it landed.
fn trash(path: &Path) -> Resultx<PathBuf> {
    let destination = trash_root().join(mirrored(path));

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Errx::e_io(e, format!("preparing {}", parent.display())))?;
    }

    rename_or_move(path, &destination)?;
    log::info!("Trashed {} to {}", path.display(), destination.display());
    Ok(destination)
}

/// `path` with its root stripped, so it can be nested under the trash root.
///
/// Mirroring the original tree means two files with the same name from different
/// directories cannot collide, and the trashed path says where it came from.
fn mirrored(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, Component::RootDir | Component::Prefix(_)))
        .collect()
}

/// Renames `from` to `to`, falling back to copy-then-remove across filesystems.
fn rename_or_move(from: &Path, to: &Path) -> Resultx<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(CROSS_DEVICE) => {
            log::info!(
                "{} is on another filesystem, copying instead",
                from.display()
            );
            copy_tree(from, to)?;
            remove_tree(from)
        }
        Err(e) => Err(Errx::e_io(
            e,
            format!("moving {} to {}", from.display(), to.display()),
        )),
    }
}

/// Copies `from` to `to`, recursing into directories.
///
/// Symlinks are recreated rather than followed: following them would loop on a
/// cycle and silently duplicate whatever they point at.
fn copy_tree(from: &Path, to: &Path) -> Resultx<()> {
    let metadata = fs::symlink_metadata(from)
        .map_err(|e| Errx::e_io(e, format!("reading {}", from.display())))?;

    if metadata.is_symlink() {
        return copy_symlink(from, to);
    }

    if !metadata.is_dir() {
        return fs::copy(from, to)
            .map(drop)
            .map_err(|e| Errx::e_io(e, format!("copying {} to {}", from.display(), to.display())));
    }

    fs::create_dir(to).map_err(|e| Errx::e_io(e, format!("creating {}", to.display())))?;

    let entries =
        fs::read_dir(from).map_err(|e| Errx::e_io(e, format!("reading {}", from.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Errx::e_io(e, format!("reading {}", from.display())))?;
        copy_tree(&entry.path(), &to.join(entry.file_name()))?;
    }

    // Set the mode last: a read-only directory cannot be filled after the fact.
    fs::set_permissions(to, metadata.permissions())
        .map_err(|e| Errx::e_io(e, format!("setting permissions on {}", to.display())))
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Resultx<()> {
    let target =
        fs::read_link(from).map_err(|e| Errx::e_io(e, format!("reading {}", from.display())))?;
    std::os::unix::fs::symlink(target, to)
        .map_err(|e| Errx::e_io(e, format!("linking {}", to.display())))
}

#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Resultx<()> {
    fs::copy(from, to)
        .map(drop)
        .map_err(|e| Errx::e_io(e, format!("copying {} to {}", from.display(), to.display())))
}

fn remove_tree(path: &Path) -> Resultx<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Errx::e_io(e, format!("reading {}", path.display())))?;

    // A symlink to a directory is removed as a link, not followed and emptied.
    let removed = if metadata.is_dir() && !metadata.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    removed.map_err(|e| Errx::e_io(e, format!("removing {}", path.display())))
}

/// Whether anything is at `path`, counting broken symlinks — which still occupy
/// the name, and which you must be allowed to delete.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn creating_a_file_makes_an_empty_one() {
        let tmp = TempDir::new();
        let path = tmp.path().join("new.txt");

        apply(&Op::CreateFile(path.clone())).unwrap();

        assert_eq!(read(&path), "");
    }

    #[test]
    fn creating_a_file_that_exists_fails() {
        let tmp = TempDir::new();
        let path = tmp.file("taken.txt", "keep me");

        assert!(apply(&Op::CreateFile(path.clone())).is_err());
        assert_eq!(read(&path), "keep me", "must not truncate what is there");
    }

    #[test]
    fn creating_a_directory_makes_one() {
        let tmp = TempDir::new();
        let path = tmp.path().join("new");

        apply(&Op::CreateDir(path.clone())).unwrap();

        assert!(path.is_dir());
    }

    #[test]
    fn copying_a_file_leaves_the_original() {
        let tmp = TempDir::new();
        let from = tmp.file("source.txt", "contents");
        let to = tmp.path().join("copy.txt");

        apply(&Op::Copy {
            from: from.clone(),
            to: to.clone(),
        })
        .unwrap();

        assert_eq!(read(&to), "contents");
        assert_eq!(read(&from), "contents");
    }

    #[test]
    fn copying_a_directory_brings_the_whole_tree() {
        let tmp = TempDir::new();
        tmp.file("tree/top.txt", "top");
        tmp.file("tree/deep/nested/leaf.txt", "leaf");
        tmp.dir("tree/empty");
        let to = tmp.path().join("clone");

        apply(&Op::Copy {
            from: tmp.path().join("tree"),
            to: to.clone(),
        })
        .unwrap();

        assert_eq!(read(&to.join("top.txt")), "top");
        assert_eq!(read(&to.join("deep/nested/leaf.txt")), "leaf");
        assert!(to.join("empty").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn copying_recreates_symlinks_instead_of_following_them() {
        let tmp = TempDir::new();
        let target = tmp.file("tree/real.txt", "real");
        std::os::unix::fs::symlink(&target, tmp.path().join("tree/link.txt")).unwrap();
        let to = tmp.path().join("clone");

        apply(&Op::Copy {
            from: tmp.path().join("tree"),
            to: to.clone(),
        })
        .unwrap();

        let copied = to.join("link.txt");
        assert!(
            fs::symlink_metadata(&copied).unwrap().is_symlink(),
            "the link should still be a link"
        );
        assert_eq!(fs::read_link(&copied).unwrap(), target);
    }

    #[cfg(unix)]
    #[test]
    fn copying_a_directory_containing_a_link_to_itself_terminates() {
        let tmp = TempDir::new();
        let tree = tmp.dir("tree");
        tmp.file("tree/file.txt", "x");
        // Following this would recurse forever.
        std::os::unix::fs::symlink(&tree, tmp.path().join("tree/loop")).unwrap();

        apply(&Op::Copy {
            from: tree,
            to: tmp.path().join("clone"),
        })
        .unwrap();

        assert!(tmp.path().join("clone/loop").is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn copying_preserves_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new();
        let from = tmp.file("run.sh", "#!/bin/sh\n");
        fs::set_permissions(&from, fs::Permissions::from_mode(0o755)).unwrap();
        let to = tmp.path().join("run-copy.sh");

        apply(&Op::Copy {
            from,
            to: to.clone(),
        })
        .unwrap();

        let mode = fs::metadata(&to).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable bit was lost");
    }

    #[test]
    fn moving_a_file_removes_the_original() {
        let tmp = TempDir::new();
        let from = tmp.file("source.txt", "contents");
        let to = tmp.path().join("moved.txt");

        apply(&Op::Move {
            from: from.clone(),
            to: to.clone(),
        })
        .unwrap();

        assert_eq!(read(&to), "contents");
        assert!(!from.exists());
    }

    #[test]
    fn renaming_is_a_move_within_one_directory() {
        let tmp = TempDir::new();
        let from = tmp.file("old.txt", "same file");

        apply(&Op::Move {
            from: from.clone(),
            to: tmp.path().join("new.txt"),
        })
        .unwrap();

        assert!(!from.exists());
        assert_eq!(read(&tmp.path().join("new.txt")), "same file");
    }

    #[test]
    fn trashing_moves_the_file_out_of_the_way() {
        let tmp = TempDir::new();
        let path = tmp.file("doomed.txt", "goodbye");

        apply(&Op::Trash(path.clone())).unwrap();

        assert!(!path.exists());
        assert_eq!(read(&trash_root().join(mirrored(&path))), "goodbye");
    }

    #[test]
    fn trashing_takes_whole_directories() {
        let tmp = TempDir::new();
        tmp.file("doomed/inside/deep.txt", "still here");
        let doomed = tmp.path().join("doomed");

        apply(&Op::Trash(doomed.clone())).unwrap();

        assert!(!doomed.exists());
        let landed = trash_root().join(mirrored(&doomed));
        assert_eq!(read(&landed.join("inside/deep.txt")), "still here");
    }

    #[test]
    fn two_files_with_one_name_do_not_collide_in_the_trash() {
        let tmp = TempDir::new();
        let first = tmp.file("one/notes.txt", "from one");
        let second = tmp.file("two/notes.txt", "from two");

        apply(&Op::Trash(first.clone())).unwrap();
        apply(&Op::Trash(second.clone())).unwrap();

        assert_eq!(read(&trash_root().join(mirrored(&first))), "from one");
        assert_eq!(read(&trash_root().join(mirrored(&second))), "from two");
    }

    #[test]
    fn the_trash_path_mirrors_the_original() {
        let path = Path::new("/Users/ioan/project/old.rs");

        assert_eq!(mirrored(path), PathBuf::from("Users/ioan/project/old.rs"));
        assert!(
            !mirrored(path).is_absolute(),
            "must be joinable under the trash root"
        );
    }

    #[test]
    fn a_missing_source_is_a_problem() {
        let tmp = TempDir::new();
        let gone = tmp.path().join("never-existed.txt");

        let op = Op::Copy {
            from: gone,
            to: tmp.path().join("x.txt"),
        };

        assert_eq!(problem(&op), Some(Problem::SourceMissing));
    }

    #[test]
    fn an_occupied_destination_is_a_problem() {
        let tmp = TempDir::new();
        let from = tmp.file("source.txt", "a");
        let to = tmp.file("taken.txt", "b");

        assert_eq!(
            problem(&Op::Copy { from, to }),
            Some(Problem::DestinationExists)
        );
    }

    #[test]
    fn creating_over_something_is_a_problem() {
        let tmp = TempDir::new();
        let taken = tmp.file("taken.txt", "");

        assert_eq!(
            problem(&Op::CreateFile(taken.clone())),
            Some(Problem::DestinationExists)
        );
        assert_eq!(
            problem(&Op::CreateDir(taken)),
            Some(Problem::DestinationExists)
        );
    }

    #[test]
    fn trashing_never_reports_a_destination_conflict() {
        let tmp = TempDir::new();
        let path = tmp.file("doomed.txt", "");

        assert_eq!(problem(&Op::Trash(path)), None);
    }

    #[test]
    fn the_pure_checks_run_before_the_filesystem_is_asked() {
        let tmp = TempDir::new();
        let inner = tmp.dir("outer/inner");

        // The destination does not exist, so only the pure check can catch this.
        let op = Op::Move {
            from: tmp.path().join("outer"),
            to: inner.join("deeper"),
        };

        assert_eq!(problem(&op), Some(Problem::DirectoryIntoItself));
    }

    #[test]
    fn a_clean_operation_has_no_problem() {
        let tmp = TempDir::new();
        let from = tmp.file("source.txt", "");

        let op = Op::Copy {
            from,
            to: tmp.path().join("destination.txt"),
        };

        assert_eq!(problem(&op), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_counts_as_present_so_it_can_be_trashed() {
        let tmp = TempDir::new();
        let link = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("nothing-here"), &link).unwrap();

        assert_eq!(problem(&Op::Trash(link.clone())), None);

        apply(&Op::Trash(link.clone())).unwrap();
        assert!(fs::symlink_metadata(&link).is_err());
    }
}
