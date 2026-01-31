use std::{io::Read, path::PathBuf};

pub fn read_text_file_preview(path: &PathBuf, max_lines: usize) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return "(cannot read file)".to_string();
    };

    let mut buffer = vec![0u8; 8192];
    let bytes_read = match file.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return "(cannot read file)".to_string(),
    };

    buffer.truncate(bytes_read);

    // Check if content appears to be binary
    let null_count = buffer.iter().filter(|b| **b == 0).count();
    if null_count > 0 || buffer.iter().any(|b| *b < 0x09 && *b != 0x00) {
        return "(binary file)".to_string();
    }

    match String::from_utf8(buffer) {
        Ok(text) => text.lines().take(max_lines).collect::<Vec<_>>().join("\n"),
        Err(_) => "(binary file)".to_string(),
    }
}
