use std::path::Path;

use log::Level;
use ratatui::style::Color;

pub const SCROLL_JUMP: usize = 8;
pub const SCROLL_OFF: usize = 8;

// Nerd Font icons
pub const NF_OCT_FILE_DIRECTORY_FILL: &str = "\u{f07b}";

/// Borders, separators, hints — everything that should recede.
pub const DIM: Color = Color::DarkGray;
/// Where you are: the cursor bar, and the prompt you are typing at.
pub const ACCENT: Color = Color::Cyan;
/// Entries you have marked.
pub const MARK: Color = Color::Magenta;
/// Behind the part of a name the search matched. The text on top is drawn in
/// [`SEARCH_TEXT`] rather than the file's own color, which a yellow file would
/// otherwise lose against.
pub const SEARCH: Color = Color::Yellow;
pub const SEARCH_TEXT: Color = Color::Black;

/// The bar drawn beside the row under the cursor.
pub const CURSOR_BAR: &str = "▍";
/// The dot beside a marked entry.
pub const MARK_DOT: &str = "●";

/// Colors come from the terminal's own 16-color palette rather than fixed RGB
/// values, so `nav` follows whatever theme the emulator is set to.
pub fn file_color(name: &str, is_dir: bool) -> Color {
    if is_dir {
        return Color::Blue;
    }

    if name.starts_with('.') {
        return Color::DarkGray;
    }

    // The extension, not the tail after the last dot: a file named `rs` or `c`
    // has no extension at all, and is not source code for having that name.
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        // Archives
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Color::Red,
        // Images and videos
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" | "tiff" | "tif"
        | "mp4" | "mkv" | "webm" | "avi" | "mov" | "flv" | "wmv" => Color::Magenta,
        // Audio
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "wma" => Color::LightRed,
        // Documents
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" => Color::Yellow,
        // Code
        "rs" | "go" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "cpp" | "h" | "hpp" | "java"
        | "rb" | "sh" | "bash" | "zsh" | "lua" | "vim" | "ex" | "exs" => Color::Green,
        // Config and data
        "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "css" | "scss" => Color::Cyan,
        // Anything else keeps the terminal's default foreground.
        _ => Color::Reset,
    }
}

pub const fn level_color(level: Level) -> Color {
    match level {
        Level::Error => Color::Red,
        Level::Warn => Color::Yellow,
        Level::Info => Color::Green,
        Level::Debug => Color::Blue,
        Level::Trace => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_files_keep_the_terminal_foreground() {
        assert_eq!(file_color("notes.txt", false), Color::Reset);
        assert_eq!(file_color("Makefile", false), Color::Reset);
    }

    #[test]
    fn no_color_is_a_fixed_rgb_value() {
        // An Rgb color would ignore the emulator's theme.
        let samples = [
            "dir", ".hidden", "a.zip", "a.png", "a.mp3", "a.pdf", "a.rs", "a.json",
        ];
        for name in samples {
            for is_dir in [true, false] {
                assert!(
                    !matches!(file_color(name, is_dir), Color::Rgb(..) | Color::Indexed(_)),
                    "{name} does not follow the terminal theme"
                );
            }
        }
        for level in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            assert!(!matches!(
                level_color(level),
                Color::Rgb(..) | Color::Indexed(_)
            ));
        }
    }

    #[test]
    fn the_chrome_follows_the_terminal_theme_too() {
        // Borders, the cursor bar, the mark dot and the search highlight are ANSI
        // slots, not fixed values, so they re-colour with the emulator's theme like
        // everything else.
        for color in [DIM, ACCENT, MARK, SEARCH, SEARCH_TEXT] {
            assert!(!matches!(color, Color::Rgb(..) | Color::Indexed(_)));
        }
    }

    #[test]
    fn directories_are_colored_as_directories_whatever_they_are_named() {
        assert_eq!(file_color("photos.png", true), Color::Blue);
    }

    #[test]
    fn hidden_files_are_dimmed() {
        assert_eq!(file_color(".gitignore", false), Color::DarkGray);
    }

    #[test]
    fn extensions_are_matched_case_insensitively() {
        assert_eq!(file_color("MAIN.RS", false), Color::Green);
        assert_eq!(file_color("Photo.PNG", false), Color::Magenta);
    }

    #[test]
    fn a_file_with_no_extension_is_not_miscolored() {
        assert_eq!(file_color("README", false), Color::Reset);
    }

    #[test]
    fn a_name_that_is_only_an_extension_is_not_that_kind_of_file() {
        // `rsplit('.')` on a name with no dot in it hands back the whole name.
        for name in ["rs", "c", "go", "zip", "pdf"] {
            assert_eq!(file_color(name, false), Color::Reset, "{name}");
        }
    }

    #[test]
    fn only_the_last_extension_colors_the_file() {
        assert_eq!(file_color("archive.tar.gz", false), Color::Red);
        assert_eq!(file_color("component.spec.ts", false), Color::Green);
    }
}
