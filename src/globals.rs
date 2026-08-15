use log::Level;
use ratatui::style::Color;

pub const SCROLL_JUMP: usize = 8;
pub const SCROLL_OFF: usize = 8;

// Nerd Font icons
pub const NF_OCT_FILE_DIRECTORY_FILL: &str = "\u{f07b}";

/// Colors come from the terminal's own 16-color palette rather than fixed RGB
/// values, so `nav` follows whatever theme the emulator is set to.
pub fn file_color(name: &str, is_dir: bool) -> Color {
    if is_dir {
        return Color::Blue;
    }

    if name.starts_with('.') {
        return Color::DarkGray;
    }

    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
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
}
