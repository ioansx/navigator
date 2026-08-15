//! Staged file operations, and the rules that decide whether they may run.
//!
//! Everything here is pure. The checks that need the filesystem live beside the
//! syscalls in [`crate::io::fs_ops`].

use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

/// One staged change. Nothing here has touched the disk yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    CreateFile(PathBuf),
    CreateDir(PathBuf),
    Trash(PathBuf),
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    /// Also covers renaming: a move whose two parents are equal.
    Move {
        from: PathBuf,
        to: PathBuf,
    },
}

impl Op {
    /// The problems that can be seen without asking the filesystem anything.
    pub fn problem(&self) -> Option<Problem> {
        let (Self::Copy { from, to } | Self::Move { from, to }) = self else {
            return None;
        };

        if from == to {
            return Some(Problem::SameSourceAndDestination);
        }

        // Catches `mv /a /a/b`, which would otherwise recurse until the disk fills.
        if to.starts_with(from) {
            return Some(Problem::DirectoryIntoItself);
        }

        None
    }

    /// How this operation reads, in parts a row can shorten independently.
    pub fn describe(&self) -> Described {
        let (verb, subject, destination) = match self {
            Self::CreateFile(path) => ("create", name_of(path), None),
            Self::CreateDir(path) => ("mkdir", name_of(path), None),
            Self::Trash(path) => ("trash", name_of(path), None),
            Self::Move { from, to } if from.parent() == to.parent() => {
                ("rename", name_of(from), Some(name_of(to)))
            }
            Self::Copy { from, to } => ("copy", name_of(from), Some(to.display().to_string())),
            Self::Move { from, to } => ("move", name_of(from), Some(to.display().to_string())),
        };

        Described {
            verb,
            subject,
            destination,
        }
    }

    /// The path this operation writes to, which is what a conflict is about.
    pub fn destination(&self) -> &Path {
        match self {
            Self::CreateFile(path) | Self::CreateDir(path) | Self::Trash(path) => path,
            Self::Copy { to, .. } | Self::Move { to, .. } => to,
        }
    }

    /// The path this operation reads from, if it has one.
    pub const fn source(&self) -> Option<&PathBuf> {
        match self {
            Self::CreateFile(_) | Self::CreateDir(_) => None,
            Self::Trash(path) => Some(path),
            Self::Copy { from, .. } | Self::Move { from, .. } => Some(from),
        }
    }

    /// Renames the destination in place, keeping the directory it points into.
    pub fn retarget(&mut self, name: &str) {
        let retargeted = match self {
            Self::CreateFile(path)
            | Self::CreateDir(path)
            | Self::Trash(path)
            | Self::Copy { to: path, .. }
            | Self::Move { to: path, .. } => path,
        };

        let parent = retargeted
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        *retargeted = parent.join(name);
    }
}

/// A staged operation split up, so a narrow row can shorten the long path
/// without losing the name of the thing being acted on.
pub struct Described {
    pub verb: &'static str,
    pub subject: String,
    pub destination: Option<String>,
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Why a staged operation may not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    DirectoryIntoItself,
    SameSourceAndDestination,
    InvalidName,
    SourceMissing,
    DestinationExists,
}

impl Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::DirectoryIntoItself => "would go inside itself",
            Self::SameSourceAndDestination => "source and destination are the same",
            Self::InvalidName => "not a usable name",
            Self::SourceMissing => "source no longer exists",
            Self::DestinationExists => "destination already exists",
        };
        f.write_str(text)
    }
}

/// Checks a name typed at a prompt, before it ever becomes a path.
pub fn validate_name(name: &str) -> Option<Problem> {
    let trimmed = name.trim();

    let usable = !trimmed.is_empty()
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains(['/', '\\'])
        && !trimmed.contains('\0');

    (!usable).then_some(Problem::InvalidName)
}

/// Staged work, in the order it was staged, with any failure from the last apply.
#[derive(Default)]
pub struct Plan {
    staged: Vec<Staged>,
}

pub struct Staged {
    pub op: Op,
    /// Set when this operation was attempted and failed, so it can be retried.
    pub failure: Option<String>,
}

impl Plan {
    pub fn push(&mut self, op: Op) {
        self.staged.push(Staged { op, failure: None });
    }

    pub const fn is_empty(&self) -> bool {
        self.staged.is_empty()
    }

    pub const fn len(&self) -> usize {
        self.staged.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Staged> {
        self.staged.iter()
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Staged> {
        self.staged.get_mut(index)
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.staged.len() {
            self.staged.remove(index);
        }
    }

    /// Drops every operation for which `blocked` reports a problem.
    pub fn drop_blocked(&mut self, blocked: impl Fn(&Op) -> bool) {
        self.staged.retain(|staged| !blocked(&staged.op));
    }

    /// Replaces the plan with the operations that failed, keeping their errors.
    pub fn keep_failures(&mut self, failures: Vec<(Op, String)>) {
        self.staged = failures
            .into_iter()
            .map(|(op, failure)| Staged {
                op,
                failure: Some(failure),
            })
            .collect();
    }

    pub fn ops(&self) -> impl Iterator<Item = &Op> {
        self.staged.iter().map(|staged| &staged.op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    fn copy(from: &str, to: &str) -> Op {
        Op::Copy {
            from: p(from),
            to: p(to),
        }
    }

    fn mv(from: &str, to: &str) -> Op {
        Op::Move {
            from: p(from),
            to: p(to),
        }
    }

    #[test]
    fn a_plain_copy_has_no_problem() {
        assert_eq!(copy("/a/x.txt", "/b/x.txt").problem(), None);
    }

    #[test]
    fn copying_a_directory_inside_itself_is_refused() {
        assert_eq!(
            copy("/a/src", "/a/src/backup").problem(),
            Some(Problem::DirectoryIntoItself)
        );
    }

    #[test]
    fn moving_a_directory_deep_inside_itself_is_refused() {
        assert_eq!(
            mv("/a", "/a/b/c/d").problem(),
            Some(Problem::DirectoryIntoItself)
        );
    }

    #[test]
    fn a_shared_name_prefix_is_not_containment() {
        // "/a/bb" is not inside "/a/b", even though the strings share a prefix.
        assert_eq!(copy("/a/b", "/a/bb").problem(), None);
    }

    #[test]
    fn copying_onto_itself_is_refused() {
        assert_eq!(
            copy("/a/x.txt", "/a/x.txt").problem(),
            Some(Problem::SameSourceAndDestination)
        );
    }

    #[test]
    fn operations_without_a_source_have_no_pure_problem() {
        assert_eq!(Op::CreateFile(p("/a/new.txt")).problem(), None);
        assert_eq!(Op::CreateDir(p("/a/new")).problem(), None);
        assert_eq!(Op::Trash(p("/a/old.txt")).problem(), None);
    }

    #[test]
    fn a_move_within_one_directory_reads_as_a_rename() {
        let described = mv("/a/old.txt", "/a/new.txt").describe();

        assert_eq!(described.verb, "rename");
        assert_eq!(described.subject, "old.txt");
        assert_eq!(described.destination.as_deref(), Some("new.txt"));
    }

    #[test]
    fn a_move_between_directories_reads_as_a_move() {
        let described = mv("/a/x.txt", "/b/x.txt").describe();

        assert_eq!(described.verb, "move");
        assert_eq!(described.subject, "x.txt");
        assert_eq!(described.destination.as_deref(), Some("/b/x.txt"));
    }

    #[test]
    fn every_operation_describes_itself() {
        let verbs: Vec<_> = [
            Op::CreateFile(p("/a/n.txt")),
            Op::CreateDir(p("/a/n")),
            Op::Trash(p("/a/o.txt")),
            copy("/a/x", "/b/x"),
        ]
        .iter()
        .map(|op| op.describe().verb)
        .collect();

        assert_eq!(verbs, ["create", "mkdir", "trash", "copy"]);
    }

    #[test]
    fn the_destination_is_what_gets_written() {
        assert_eq!(copy("/a/x", "/b/x").destination(), Path::new("/b/x"));
        assert_eq!(mv("/a/x", "/b/x").destination(), Path::new("/b/x"));
        assert_eq!(
            Op::CreateFile(p("/a/n.txt")).destination(),
            Path::new("/a/n.txt")
        );
    }

    #[test]
    fn retargeting_keeps_the_destination_directory() {
        let mut op = copy("/a/x.txt", "/b/x.txt");
        op.retarget("renamed.txt");

        assert_eq!(op.destination(), Path::new("/b/renamed.txt"));
        assert_eq!(op.source().unwrap(), &p("/a/x.txt"));
    }

    #[test]
    fn retargeting_a_create_keeps_its_directory() {
        let mut op = Op::CreateDir(p("/a/wrong"));
        op.retarget("right");

        assert_eq!(op.destination(), Path::new("/a/right"));
    }

    #[test]
    fn usable_names_are_accepted() {
        for name in ["notes.txt", "src", ".gitignore", "a b c.rs", "..hidden"] {
            assert_eq!(validate_name(name), None, "{name}");
        }
    }

    #[test]
    fn unusable_names_are_refused() {
        for name in ["", "   ", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert_eq!(
                validate_name(name),
                Some(Problem::InvalidName),
                "{name:?} should not be accepted"
            );
        }
    }

    #[test]
    fn a_plan_stages_in_order() {
        let mut plan = Plan::default();
        plan.push(Op::Trash(p("/a/first")));
        plan.push(Op::Trash(p("/a/second")));

        let names: Vec<_> = plan.ops().map(|op| op.describe().subject).collect();

        assert_eq!(names, ["first", "second"]);
        assert_eq!(plan.len(), 2);
    }

    #[test]
    fn dropping_by_index_removes_only_that_operation() {
        let mut plan = Plan::default();
        plan.push(Op::Trash(p("/a/first")));
        plan.push(Op::Trash(p("/a/second")));
        plan.remove(0);

        let names: Vec<_> = plan.ops().map(|op| op.describe().subject).collect();

        assert_eq!(names, ["second"]);
    }

    #[test]
    fn dropping_out_of_range_is_harmless() {
        let mut plan = Plan::default();
        plan.push(Op::Trash(p("/a/only")));
        plan.remove(9);

        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn dropping_blocked_keeps_the_rest() {
        let mut plan = Plan::default();
        plan.push(copy("/a/good", "/b/good"));
        plan.push(copy("/a/bad", "/a/bad/inside"));
        plan.push(copy("/a/also-good", "/b/also-good"));

        plan.drop_blocked(|op| op.problem().is_some());

        assert_eq!(plan.len(), 2);
        assert!(plan.ops().all(|op| op.problem().is_none()));
    }

    #[test]
    fn failures_replace_the_plan_and_keep_their_reason() {
        let mut plan = Plan::default();
        plan.push(Op::Trash(p("/a/one")));
        plan.push(Op::Trash(p("/a/two")));

        plan.keep_failures(vec![(Op::Trash(p("/a/two")), "permission denied".into())]);

        assert_eq!(plan.len(), 1);
        let staged = plan.iter().next().unwrap();
        assert_eq!(staged.failure.as_deref(), Some("permission denied"));
    }
}
