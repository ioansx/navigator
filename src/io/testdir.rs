//! A throwaway directory tree for tests. Test-only, never compiled into `nav`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A directory under the OS temp dir, deleted when this value drops.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nav-test-{}-{unique}", std::process::id()));

        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("creating the temp dir");

        // The OS temp dir is a symlink on macOS, and tests compare canonical paths.
        let path = fs::canonicalize(&path).expect("resolving the temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates `name` (which may contain `/`) holding `contents`, and returns its path.
    pub fn file(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("creating the parent dir");
        }
        fs::write(&path, contents).expect("writing the file");
        path
    }

    /// Creates directory `name` (which may contain `/`), and returns its path.
    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir_all(&path).expect("creating the dir");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
