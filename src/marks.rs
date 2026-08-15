//! The set of entries the user has selected for bulk operations.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Paths selected across every directory visited this session.
///
/// Paths are absolute so a mark survives navigating elsewhere, and the set is
/// ordered so bulk operations are staged in a predictable order.
#[derive(Default)]
pub struct Marks {
    paths: BTreeSet<PathBuf>,
}

impl Marks {
    /// Adds `path` if it is not marked, removes it if it is. Returns the new state.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.paths.remove(path) {
            return false;
        }
        self.paths.insert(path.to_path_buf());
        true
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &PathBuf> {
        self.paths.iter()
    }

    /// How many marks live outside `dir`, and so cannot be seen from it.
    pub fn count_outside(&self, dir: &Path) -> usize {
        self.paths
            .iter()
            .filter(|path| path.parent() != Some(dir))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    #[test]
    fn nothing_is_marked_to_begin_with() {
        let marks = Marks::default();

        assert!(marks.is_empty());
        assert!(!marks.contains(&p("/a/x.txt")));
    }

    #[test]
    fn toggling_marks_then_unmarks() {
        let mut marks = Marks::default();

        assert!(marks.toggle(&p("/a/x.txt")));
        assert!(marks.contains(&p("/a/x.txt")));

        assert!(!marks.toggle(&p("/a/x.txt")));
        assert!(!marks.contains(&p("/a/x.txt")));
        assert!(marks.is_empty());
    }

    #[test]
    fn the_same_path_is_never_marked_twice() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/x.txt"));
        marks.toggle(&p("/a/x.txt"));
        marks.toggle(&p("/a/x.txt"));

        assert_eq!(marks.len(), 1);
    }

    #[test]
    fn marks_are_iterated_in_a_predictable_order() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/zebra.txt"));
        marks.toggle(&p("/a/apple.txt"));
        marks.toggle(&p("/b/middle.txt"));

        let ordered: Vec<_> = marks.iter().map(|path| path.to_str().unwrap()).collect();

        assert_eq!(
            ordered,
            ["/a/apple.txt", "/a/zebra.txt", "/b/middle.txt"],
            "bulk operations must stage in a deterministic order"
        );
    }

    #[test]
    fn marks_span_directories() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/x.txt"));
        marks.toggle(&p("/b/y.txt"));

        assert_eq!(marks.len(), 2);
        assert!(marks.contains(&p("/a/x.txt")));
        assert!(marks.contains(&p("/b/y.txt")));
    }

    #[test]
    fn counts_the_marks_that_cannot_be_seen_from_here() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/one.txt"));
        marks.toggle(&p("/b/two.txt"));
        marks.toggle(&p("/b/three.txt"));

        assert_eq!(marks.count_outside(&p("/a")), 2);
        assert_eq!(marks.count_outside(&p("/b")), 1);
        assert_eq!(marks.count_outside(&p("/elsewhere")), 3);
    }

    #[test]
    fn a_mark_in_a_nested_directory_counts_as_outside() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/deep/x.txt"));

        assert_eq!(marks.count_outside(&p("/a")), 1);
    }

    #[test]
    fn clearing_removes_everything() {
        let mut marks = Marks::default();
        marks.toggle(&p("/a/x.txt"));
        marks.toggle(&p("/b/y.txt"));
        marks.clear();

        assert!(marks.is_empty());
    }
}
