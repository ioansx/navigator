use ratatui::style::Color;

pub const SCROLL_JUMP: usize = 8;
pub const SCROLL_OFF: usize = 8;

// Nerd Font icons
pub const NF_OCT_FILE_DIRECTORY: &str = "\u{f07b}";
pub const NF_OCT_FILE_SYMLINK_DIRECTORY: &str = "\u{f482}";
pub const NF_OCT_FILE_DIRECTORY_FILL: &str = "\u{f07b}";
pub const NF_OCT_FILE_DIRECTORY_OPEN_FILL: &str = "\u{f07c}";
pub const NF_FILE: &str = "\u{f15b}";

// Gruvbox colors
pub const GRUVBOX_BLUE: Color = Color::Rgb(131, 165, 152);    // #83a598 - directories
pub const GRUVBOX_GREEN: Color = Color::Rgb(184, 187, 38);    // #b8bb26 - executables, code
pub const GRUVBOX_AQUA: Color = Color::Rgb(142, 192, 124);    // #8ec07c - symlinks
pub const GRUVBOX_RED: Color = Color::Rgb(251, 73, 52);       // #fb4934 - archives
pub const GRUVBOX_PURPLE: Color = Color::Rgb(211, 134, 155);  // #d3869b - images, videos
pub const GRUVBOX_ORANGE: Color = Color::Rgb(254, 128, 25);   // #fe8019 - audio
pub const GRUVBOX_YELLOW: Color = Color::Rgb(250, 189, 47);   // #fabd2f - documents
pub const GRUVBOX_GRAY: Color = Color::Rgb(146, 131, 116);    // #928374 - hidden files
pub const GRUVBOX_FG: Color = Color::Rgb(235, 219, 178);      // #ebdbb2 - default

pub fn file_color(name: &str, is_dir: bool) -> Color {
    if is_dir {
        return GRUVBOX_BLUE;
    }

    if name.starts_with('.') {
        return GRUVBOX_GRAY;
    }

    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        // Archives
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" => GRUVBOX_RED,
        // Images
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" | "tiff" => GRUVBOX_PURPLE,
        // Videos
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "flv" | "wmv" => GRUVBOX_PURPLE,
        // Audio
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "wma" => GRUVBOX_ORANGE,
        // Documents
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" => GRUVBOX_YELLOW,
        // Code
        "rs" | "go" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "cpp" | "h" | "hpp"
        | "java" | "rb" | "sh" | "bash" | "zsh" | "lua" | "vim" | "ex" | "exs" => GRUVBOX_GREEN,
        // Config/data
        "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "css" | "scss" => GRUVBOX_AQUA,
        // Default
        _ => GRUVBOX_FG,
    }
}
