use std::{fs, io::Read, path::Path};

use crate::error::{Errx, Resultx};

/// How much of a file is read to build its preview.
const PREVIEW_BYTES: usize = 8192;

/// The head of `path` as displayable text, or a short message saying why not.
pub fn read_text_preview(path: &Path, max_lines: usize) -> String {
    match read_head(path, PREVIEW_BYTES) {
        Ok(head) => text_preview(&head, max_lines).unwrap_or_else(|| "(binary file)".to_string()),
        Err(e) => {
            log::warn!("{e}");
            "(cannot read file)".to_string()
        }
    }
}

/// The first `max_lines` of `head` as text, or `None` if `head` is not text.
///
/// `head` is only the start of a file, so its last character may be cut in half.
/// That trailing fragment is dropped — it does not make the file binary.
pub fn text_preview(head: &[u8], max_lines: usize) -> Option<String> {
    if !head.iter().all(|byte| is_text_byte(*byte)) {
        return None;
    }

    let text = match std::str::from_utf8(head) {
        Ok(text) => text,
        // `error_len() == None` means the bytes ran out mid-character.
        Err(e) if e.error_len().is_none() => std::str::from_utf8(&head[..e.valid_up_to()]).ok()?,
        Err(_) => return None,
    };

    Some(text.lines().take(max_lines).collect::<Vec<_>>().join("\n"))
}

/// The first `max_bytes` of `path`, or the whole file if it is shorter.
fn read_head(path: &Path, max_bytes: usize) -> Resultx<Vec<u8>> {
    let file =
        fs::File::open(path).map_err(|e| Errx::e_io(e, format!("opening {}", path.display())))?;

    let mut head = Vec::new();
    file.take(max_bytes as u64)
        .read_to_end(&mut head)
        .map_err(|e| Errx::e_io(e, format!("reading {}", path.display())))?;

    Ok(head)
}

/// Tab, newline, carriage return, or anything printable. Other control bytes mean binary.
fn is_text_byte(byte: u8) -> bool {
    byte >= 0x20 || matches!(byte, b'\t' | b'\n' | b'\r')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn preview(bytes: &[u8]) -> Option<String> {
        text_preview(bytes, usize::MAX)
    }

    #[test]
    fn reads_plain_text() {
        assert_eq!(preview(b"hello\nworld").unwrap(), "hello\nworld");
    }

    #[test]
    fn keeps_at_most_max_lines() {
        let text = preview_lines(b"one\ntwo\nthree\nfour", 2);

        assert_eq!(text, "one\ntwo");
    }

    fn preview_lines(bytes: &[u8], max_lines: usize) -> String {
        text_preview(bytes, max_lines).unwrap()
    }

    #[test]
    fn zero_lines_previews_nothing() {
        assert_eq!(preview_lines(b"one\ntwo", 0), "");
    }

    #[test]
    fn an_empty_file_is_text_not_binary() {
        assert_eq!(preview(b"").unwrap(), "");
    }

    #[test]
    fn a_trailing_newline_does_not_add_a_line() {
        assert_eq!(preview(b"one\ntwo\n").unwrap(), "one\ntwo");
    }

    #[test]
    fn carriage_returns_are_stripped() {
        assert_eq!(preview(b"one\r\ntwo\r\n").unwrap(), "one\ntwo");
    }

    #[test]
    fn tabs_survive() {
        assert_eq!(preview(b"a\tb").unwrap(), "a\tb");
    }

    #[test]
    fn a_null_byte_means_binary() {
        assert_eq!(preview(b"text\0more"), None);
    }

    #[test]
    fn a_control_byte_means_binary() {
        assert_eq!(preview(b"text\x07bell"), None);
        assert_eq!(preview(b"text\x1bescape"), None);
    }

    #[test]
    fn multibyte_characters_survive() {
        assert_eq!(preview("héllo → 🦀".as_bytes()).unwrap(), "héllo → 🦀");
    }

    #[test]
    fn a_character_cut_in_half_at_the_end_is_dropped_not_treated_as_binary() {
        // "ab🦀" with the crab's last byte missing, as an 8KB read of a longer file would give.
        let full = "ab🦀".as_bytes();
        let truncated = &full[..full.len() - 1];

        assert_eq!(preview(truncated).unwrap(), "ab");
    }

    #[test]
    fn genuinely_invalid_utf8_means_binary() {
        // 0xc3 starts a two-byte character, but `(` cannot continue it.
        assert_eq!(preview(b"text\xc3("), None);
    }

    #[test]
    fn reads_a_file_from_disk() {
        let tmp = TempDir::new();
        let path = tmp.file("notes.txt", "first\nsecond\nthird");

        assert_eq!(read_text_preview(&path, 2), "first\nsecond");
    }

    #[test]
    fn a_missing_file_says_so() {
        let tmp = TempDir::new();

        assert_eq!(
            read_text_preview(&tmp.path().join("nope.txt"), 10),
            "(cannot read file)"
        );
    }

    #[test]
    fn a_directory_says_it_cannot_be_read() {
        let tmp = TempDir::new();

        assert_eq!(read_text_preview(tmp.path(), 10), "(cannot read file)");
    }

    #[test]
    fn a_binary_file_says_so() {
        let tmp = TempDir::new();
        let path = tmp.file("app.bin", [0x7f, 0x45, 0x4c, 0x46, 0x00, 0x01]);

        assert_eq!(read_text_preview(&path, 10), "(binary file)");
    }

    #[test]
    fn only_the_head_of_a_large_file_is_read() {
        let tmp = TempDir::new();
        // Far more than PREVIEW_BYTES, with the marker well past the cutoff.
        let mut content = "filler\n".repeat(4000);
        content.push_str("MARKER\n");
        let path = tmp.file("big.log", &content);

        let text = read_text_preview(&path, usize::MAX);

        assert!(!text.contains("MARKER"), "read past the preview window");
        assert_eq!(text.len(), PREVIEW_BYTES);
    }

    #[test]
    fn a_large_file_cut_mid_character_still_previews() {
        let tmp = TempDir::new();
        // 3 bytes per `→`, so the 8192-byte cutoff lands inside a character.
        let path = tmp.file("unicode.txt", "→".repeat(4000));

        let text = read_text_preview(&path, usize::MAX);

        assert!(text.starts_with('→'), "expected text, got {text}");
    }
}
