//! Putting a file's contents on the system clipboard with OSC 52.
//!
//! The terminal emulator does the work, so this reaches the real clipboard even
//! when `nav` is running on the far side of an SSH connection.

use std::{io::Write, path::Path};

use crate::{
    error::{Errx, Resultx},
    io::file,
};

/// Terminals cap how much OSC 52 they accept and truncate silently past it —
/// tmux at roughly 75 KB. This leaves room once base64 has grown it by a third.
const MAX_BYTES: usize = 48 * 1024;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Copies `path`'s contents to the system clipboard, returning how many bytes went.
///
/// Refuses anything that is not text, or is too large to survive the escape
/// sequence intact: a corrupted paste is worse than none.
pub fn copy_file(path: &Path) -> Resultx<usize> {
    let contents =
        std::fs::read(path).map_err(|e| Errx::e_io(e, format!("reading {}", path.display())))?;

    if contents.len() > MAX_BYTES {
        return Err(Errx::any(format!(
            "{} is {} KiB, over the {} KiB clipboard limit",
            name_of(path),
            contents.len() / 1024,
            MAX_BYTES / 1024
        )));
    }

    if !file::looks_like_text(&contents) {
        return Err(Errx::any(format!("{} is not a text file", name_of(path))));
    }

    // Written straight to the terminal rather than through ratatui: this is a
    // control sequence, not something to draw, and it must not land mid-frame.
    let mut stdout = std::io::stdout();
    stdout
        .write_all(osc52(&contents, std::env::var_os("TMUX").is_some()).as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Errx::e_io(e, "writing to the terminal"))?;

    Ok(contents.len())
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The escape sequence that hands `data` to the terminal's clipboard.
///
/// tmux intercepts escape sequences from the program it runs, so inside tmux the
/// whole thing is wrapped in a passthrough that tells it to forward this one on.
fn osc52(data: &[u8], inside_tmux: bool) -> String {
    let sequence = format!("\x1b]52;c;{}\x07", base64(data));

    if inside_tmux {
        // tmux ends its passthrough at the first ESC, so any ESC inside is doubled.
        return format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
    }

    sequence
}

/// Standard base64, as in RFC 4648.
fn base64(data: &[u8]) -> String {
    let mut encoded = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let padded = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let bits = u32::from(padded[0]) << 16 | u32::from(padded[1]) << 8 | u32::from(padded[2]);

        let sextet = |shift: u32| ALPHABET[(bits >> shift & 0b11_1111) as usize] as char;

        encoded.push(sextet(18));
        encoded.push(sextet(12));
        encoded.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        encoded.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    /// The test vectors from RFC 4648 section 10.
    #[test]
    fn encodes_the_rfc_4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];

        for (input, expected) in vectors {
            assert_eq!(base64(input.as_bytes()), expected, "{input:?}");
        }
    }

    #[test]
    fn encodes_bytes_that_are_not_ascii() {
        // Every bit pattern in the top half, where a sloppy encoder sign-extends.
        assert_eq!(base64(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64(&[0x00, 0x00, 0x00]), "AAAA");
        assert_eq!(base64(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn the_encoded_length_is_always_a_multiple_of_four() {
        for length in 0..32 {
            let encoded = base64(&vec![b'x'; length]);
            assert_eq!(encoded.len() % 4, 0, "length {length} encoded to {encoded}");
        }
    }

    #[test]
    fn the_escape_sequence_carries_the_encoded_payload() {
        assert_eq!(osc52(b"foo", false), "\x1b]52;c;Zm9v\x07");
    }

    #[test]
    fn inside_tmux_the_sequence_is_wrapped_for_passthrough() {
        let wrapped = osc52(b"foo", true);

        assert!(wrapped.starts_with("\x1bPtmux;"), "{wrapped:?}");
        assert!(wrapped.ends_with("\x1b\\"), "{wrapped:?}");
        assert!(
            wrapped.contains("\x1b\x1b]52;c;Zm9v"),
            "the inner escape must be doubled: {wrapped:?}"
        );
    }

    #[test]
    fn a_binary_file_is_refused() {
        let tmp = TempDir::new();
        let path = tmp.file("app.bin", [0x7f, 0x45, 0x4c, 0x46, 0x00]);

        let refusal = copy_file(&path).unwrap_err().to_string();

        assert!(refusal.contains("not a text file"), "{refusal}");
    }

    #[test]
    fn a_file_over_the_limit_is_refused() {
        let tmp = TempDir::new();
        let path = tmp.file("huge.txt", "x".repeat(MAX_BYTES + 1));

        let refusal = copy_file(&path).unwrap_err().to_string();

        assert!(refusal.contains("clipboard limit"), "{refusal}");
    }

    #[test]
    fn a_file_at_exactly_the_limit_is_allowed() {
        let tmp = TempDir::new();
        let path = tmp.file("big.txt", "x".repeat(MAX_BYTES));

        assert_eq!(copy_file(&path).unwrap(), MAX_BYTES);
    }

    #[test]
    fn a_missing_file_is_refused() {
        let tmp = TempDir::new();

        assert!(copy_file(&tmp.path().join("gone.txt")).is_err());
    }
}
