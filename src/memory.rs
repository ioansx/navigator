//! What the navigator remembers about places it has already been.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Where the cursor was left in each directory visited this session.
///
/// Positions are stored as entry names rather than row numbers: a directory's
/// contents can change between two visits, and a row number would then quietly
/// point at the wrong file.
///
/// Nothing here outlives the process. `nav` is launched per-invocation from
/// neovim and exits when a file is opened, so remembering across runs would be a
/// separate feature with its own storage and staleness questions.
#[derive(Default)]
pub struct Memory {
    cursor: HashMap<PathBuf, String>,
}

impl Memory {
    /// Records that `entry` was under the cursor in `dir`.
    pub fn remember(&mut self, dir: &Path, entry: &str) {
        self.cursor.insert(dir.to_path_buf(), entry.to_string());
    }

    /// The entry last under the cursor in `dir`, if it has been visited.
    pub fn recall(&self, dir: &Path) -> Option<&str> {
        self.cursor.get(dir).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn an_unvisited_directory_is_not_remembered() {
        let memory = Memory::default();

        assert_eq!(memory.recall(&dir("/a")), None);
    }

    #[test]
    fn recalls_what_was_remembered() {
        let mut memory = Memory::default();
        memory.remember(&dir("/a"), "notes.txt");

        assert_eq!(memory.recall(&dir("/a")), Some("notes.txt"));
    }

    #[test]
    fn the_latest_position_replaces_the_previous_one() {
        let mut memory = Memory::default();
        memory.remember(&dir("/a"), "first.txt");
        memory.remember(&dir("/a"), "second.txt");

        assert_eq!(memory.recall(&dir("/a")), Some("second.txt"));
    }

    #[test]
    fn directories_are_remembered_independently() {
        let mut memory = Memory::default();
        memory.remember(&dir("/a"), "in-a.txt");
        memory.remember(&dir("/a/b"), "in-b.txt");

        assert_eq!(memory.recall(&dir("/a")), Some("in-a.txt"));
        assert_eq!(memory.recall(&dir("/a/b")), Some("in-b.txt"));
    }

    #[test]
    fn a_nested_path_is_not_confused_with_its_parent() {
        let mut memory = Memory::default();
        memory.remember(&dir("/a/b"), "deep.txt");

        assert_eq!(memory.recall(&dir("/a")), None);
    }
}
