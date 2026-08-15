//! Everything that touches the filesystem or spawns a process.
//!
//! Each function takes a path and returns plain data, so the rest of the crate
//! can render without knowing how anything was read.

pub mod dir;
pub mod file;
pub mod nvim;
pub mod raster;

#[cfg(test)]
pub mod testdir;

use std::path::Path;

/// Which preview an entry gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    /// Anything [`raster::load`] turns into pixels: bitmaps and SVGs alike.
    Image,
    Text,
}

impl FileKind {
    pub fn of(path: &Path, is_dir: bool) -> FileKind {
        if is_dir {
            return FileKind::Dir;
        }

        let ext = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif" | "svg" => {
                FileKind::Image
            }
            _ => FileKind::Text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn kind_of(name: &str) -> FileKind {
        FileKind::of(&PathBuf::from(name), false)
    }

    #[test]
    fn a_directory_wins_over_its_name() {
        assert_eq!(
            FileKind::of(&PathBuf::from("assets.png"), true),
            FileKind::Dir
        );
    }

    #[test]
    fn bitmaps_and_svgs_are_both_images() {
        for name in [
            "a.png", "a.jpg", "a.jpeg", "a.gif", "a.bmp", "a.webp", "a.ico", "a.tiff", "a.tif",
            "a.svg",
        ] {
            assert_eq!(kind_of(name), FileKind::Image, "{name}");
        }
    }

    #[test]
    fn extensions_are_matched_case_insensitively() {
        assert_eq!(kind_of("PHOTO.PNG"), FileKind::Image);
        assert_eq!(kind_of("Logo.SvG"), FileKind::Image);
    }

    #[test]
    fn anything_else_is_text() {
        for name in ["main.rs", "README", "Makefile", "notes.txt", "a.pngx"] {
            assert_eq!(kind_of(name), FileKind::Text, "{name}");
        }
    }

    #[test]
    fn a_dotfile_extension_does_not_count_as_an_extension() {
        // `Path::extension` returns None here, so `.png` is the whole file name.
        assert_eq!(kind_of(".png"), FileKind::Text);
    }

    #[test]
    fn only_the_last_extension_matters() {
        assert_eq!(kind_of("archive.png.gz"), FileKind::Text);
        assert_eq!(kind_of("sprite.tar.png"), FileKind::Image);
    }
}
