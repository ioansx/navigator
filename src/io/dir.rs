use std::path::PathBuf;

use crate::error::{Errx, Resultx};

pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

pub fn read_dir(path: &PathBuf) -> Resultx<Vec<DirEntry>> {
    let mut entries: Vec<DirEntry> = std::fs::read_dir(path)
        .map_err(|e| Errx::e_io(e, format!("reading {}", path.display())))?
        // TODO: do not ignore these errors
        .filter_map(|e| e.ok())
        .map(|e| DirEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
        })
        .collect();

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });

    let all = [
        DirEntry {
            name: ".".to_string(),
            is_dir: true,
        },
        DirEntry {
            name: "..".to_string(),
            is_dir: true,
        },
    ]
    .into_iter()
    .chain(entries.into_iter());

    Ok(all.collect())
}
