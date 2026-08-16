//! Jumping to a directory by name, through the database `zoxide` keeps.
//!
//! Read-only: `nav` asks zoxide where a name points and never records anything.
//! The database is the shell's, built up by `z` as you work, so a jump here lands
//! where the same query would land in the terminal.

use std::{path::PathBuf, process::Command};

use crate::error::{Errx, Resultx};

/// The directory zoxide ranks highest for `terms`.
///
/// # Errors
/// Fails if zoxide is not installed, or if it knows no directory by that name.
pub fn query(terms: &str) -> Resultx<PathBuf> {
    let output = Command::new("zoxide")
        .arg("query")
        // Past `--` every word is a search term, never a flag, whatever was typed.
        .arg("--")
        .args(terms.split_whitespace())
        .output()
        .map_err(|e| Errx::e_io(e, "running zoxide (is it installed?)"))?;

    if !output.stderr.is_empty() {
        log::debug!("zoxide: {}", String::from_utf8_lossy(&output.stderr).trim());
    }

    best_match(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| Errx::any(format!("zoxide knows nowhere matching {terms:?}")))
}

/// zoxide prints the winning path on a line of its own, and nothing at all when
/// it has no match.
fn best_match(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().next()?.trim_end();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_path_off_the_line_zoxide_printed() {
        assert_eq!(
            best_match("/home/me/dev/navigator\n"),
            Some(PathBuf::from("/home/me/dev/navigator"))
        );
    }

    #[test]
    fn no_match_is_no_path() {
        for stdout in ["", "\n", "   \n"] {
            assert_eq!(best_match(stdout), None, "{stdout:?}");
        }
    }

    #[test]
    fn only_the_top_ranked_line_is_taken() {
        assert_eq!(
            best_match("/first\n/second\n"),
            Some(PathBuf::from("/first"))
        );
    }

    #[test]
    fn a_path_with_spaces_in_it_survives_intact() {
        assert_eq!(
            best_match("/home/me/my notes\n"),
            Some(PathBuf::from("/home/me/my notes"))
        );
    }
}
