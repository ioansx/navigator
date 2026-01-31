use std::{fs, io::Read, path::PathBuf};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    prelude::Buffer,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

use crate::{
    error::Resultx,
    globals::SCROLL_OFF,
    io::dir::{DirEntry, read_dir},
};

pub struct Navigator {
    current_dir: PathBuf,
    entries: Vec<DirEntry>,
    selected: usize,
    scroll_offset: usize,
    picker: Picker,
    cached_image: Option<(PathBuf, StatefulProtocol)>,
}

impl Navigator {
    pub fn new(current_dir_path: &str) -> Resultx<Self> {
        let mut current_dir = PathBuf::from(current_dir_path);
        // Calling `parent()` on a relative path returns None, so work with canonical paths.
        if current_dir.is_relative() {
            current_dir = std::fs::canonicalize(current_dir)?;
        }

        let entries = read_dir(&current_dir)?;
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        Ok(Self {
            current_dir,
            entries,
            selected: 0,
            scroll_offset: 0,
            picker,
            cached_image: None,
        })
    }

    pub fn enter_selected_directory(&mut self) -> Resultx<()> {
        if self.entries.is_empty() {
            return Ok(());
        }

        let selected_entry = &self.entries[self.selected];
        if selected_entry.is_dir {
            let new_path = self.current_dir.join(&selected_entry.name);

            // FS
            let entries = read_dir(&new_path)?;

            self.current_dir = new_path;
            self.entries = entries;
            self.selected = 0;
            self.scroll_offset = 0;
            self.cached_image = None;
        }

        Ok(())
    }

    pub fn go_to_parent_directory(&mut self) -> Resultx<()> {
        if let Some(parent) = self.current_dir.parent() {
            // This means the path was relative. Still thinking if I should support relative paths.
            if parent == "" {
                return Ok(());
            }

            let entries = read_dir(&parent.to_path_buf())?;
            self.current_dir = parent.to_path_buf();
            self.entries = entries;
            self.selected = 0;
            self.scroll_offset = 0;
            self.cached_image = None;
        }
        Ok(())
    }

    pub fn move_up(&mut self) {
        self.move_up_by(1);
    }

    pub fn move_down(&mut self) {
        self.move_down_by(1);
    }

    pub fn move_up_by(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
        self.cached_image = None;
    }

    pub fn move_down_by(&mut self, n: usize) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + n).min(self.entries.len() - 1);
            self.cached_image = None;
        }
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected)
            .map(|e| self.current_dir.join(&e.name))
    }

    pub fn render_with_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_file_list(chunks[0], buf);
        self.render_preview(chunks[1], buf);
    }

    fn render_file_list(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .title(format!(" {} ", self.current_dir.display()));

        let inner = block.inner(area);
        block.render(area, buf);

        let visible_height = inner.height as usize;
        let scrolloff = SCROLL_OFF.min(visible_height / 2);

        // Adjust scroll offset to keep selection visible with scrolloff context
        if self.selected < self.scroll_offset + scrolloff {
            self.scroll_offset = self.selected.saturating_sub(scrolloff);
        } else if self.selected + scrolloff >= self.scroll_offset + visible_height {
            self.scroll_offset = (self.selected + scrolloff + 1).saturating_sub(visible_height);
        }

        for (i, entry) in self
            .entries
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(visible_height)
        {
            let y = (i - self.scroll_offset) as i32;
            let prefix = if entry.is_dir { "📁 " } else { "   " };
            let style = if i == self.selected {
                Style::new().reversed()
            } else {
                Style::default()
            };
            let line = Line::styled(format!("{}{}", prefix, entry.name), style);
            line.render(inner.offset(ratatui::layout::Offset { x: 0, y }), buf);
        }
    }

    fn render_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(path) = self.selected_path() else {
            return;
        };

        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };

        if entry.is_dir {
            self.render_directory_preview(&path, area, buf);
        } else if is_svg(&path) {
            self.render_svg_preview(&path, area, buf);
        } else if is_image(&path) {
            self.render_image_preview(&path, area, buf);
        } else {
            self.render_text_preview(&path, area, buf);
        }
    }

    fn render_directory_preview(&self, path: &PathBuf, area: Rect, buf: &mut Buffer) {
        let content = match fs::read_dir(path) {
            Ok(entries) => {
                let items: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .take(area.height as usize)
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        if is_dir {
                            format!("📁 {}", name)
                        } else {
                            format!("   {}", name)
                        }
                    })
                    .collect();
                if items.is_empty() {
                    "(empty directory)".to_string()
                } else {
                    items.join("\n")
                }
            }
            Err(_) => "(cannot read directory)".to_string(),
        };

        Paragraph::new(content).render(area, buf);
    }

    fn render_svg_preview(&mut self, path: &PathBuf, area: Rect, buf: &mut Buffer) {
        let needs_reload = match &self.cached_image {
            Some((cached_path, _)) => cached_path != path,
            None => true,
        };

        if needs_reload {
            if let Some(dyn_img) = rasterize_svg(path) {
                let protocol = self.picker.new_resize_protocol(dyn_img);
                self.cached_image = Some((path.clone(), protocol));
            } else {
                Paragraph::new("(cannot load svg)").render(area, buf);
                return;
            }
        }

        if let Some((_, ref mut protocol)) = self.cached_image {
            StatefulImage::default().render(area, buf, protocol);
        }
    }

    fn render_image_preview(&mut self, path: &PathBuf, area: Rect, buf: &mut Buffer) {
        let needs_reload = match &self.cached_image {
            Some((cached_path, _)) => cached_path != path,
            None => true,
        };

        if needs_reload {
            if let Ok(dyn_img) = image::open(path) {
                let protocol = self.picker.new_resize_protocol(dyn_img);
                self.cached_image = Some((path.clone(), protocol));
            } else {
                Paragraph::new("(cannot load image)").render(area, buf);
                return;
            }
        }

        if let Some((_, ref mut protocol)) = self.cached_image {
            StatefulImage::default().render(area, buf, protocol);
        }
    }

    fn render_text_preview(&self, path: &PathBuf, area: Rect, buf: &mut Buffer) {
        let content = read_file_preview(path, area.height as usize);
        Paragraph::new(content).render(area, buf);
    }
}

fn is_svg(path: &PathBuf) -> bool {
    path.extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("svg"))
}

fn rasterize_svg(path: &PathBuf) -> Option<image::DynamicImage> {
    let tree = resvg::usvg::Tree::from_data(&fs::read(path).ok()?, &Default::default()).ok()?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())?;
    resvg::render(&tree, Default::default(), &mut pixmap.as_mut());
    let img = image::RgbaImage::from_raw(size.width(), size.height(), pixmap.take())?;
    Some(image::DynamicImage::ImageRgba8(img))
}

fn is_image(path: &PathBuf) -> bool {
    let Some(ext) = path.extension() else {
        return false;
    };
    let ext = ext.to_string_lossy().to_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif"
    )
}

fn read_file_preview(path: &PathBuf, max_lines: usize) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return "(cannot read file)".to_string();
    };

    let mut buffer = vec![0u8; 8192];
    let bytes_read = match file.read(&mut buffer) {
        Ok(n) => n,
        Err(_) => return "(cannot read file)".to_string(),
    };

    buffer.truncate(bytes_read);

    // Check if content appears to be binary
    let null_count = buffer.iter().filter(|&&b| b == 0).count();
    if null_count > 0 || buffer.iter().any(|&b| b < 0x09 && b != 0x00) {
        return "(binary file)".to_string();
    }

    match String::from_utf8(buffer) {
        Ok(text) => text.lines().take(max_lines).collect::<Vec<_>>().join("\n"),
        Err(_) => "(binary file)".to_string(),
    }
}
