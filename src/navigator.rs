use std::{env, fs, io::Read, path::PathBuf, process::Command};

use log::Level;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    prelude::Buffer,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

use crate::{
    error::Resultx,
    globals::{NF_OCT_FILE_DIRECTORY_FILL, SCROLL_OFF, file_color},
    io::dir::{DirEntry, read_dir},
    log_store::LOG_STORE,
    preview::{image_preview, svg_preview},
};

pub struct Navigator {
    current_dir: PathBuf,
    entries: Vec<DirEntry>,
    selected: usize,
    scroll_offset: usize,
    picker: Picker,
    cached_image: Option<(PathBuf, StatefulProtocol)>,
    log_panel_visible: bool,
    log_scroll_offset: usize,
}

impl Navigator {
    pub fn new(current_dir_path: &str, select: Option<&str>) -> Resultx<Self> {
        let mut current_dir = PathBuf::from(current_dir_path);
        // Calling `parent()` on a relative path returns None, so work with canonical paths.
        if current_dir.is_relative() {
            current_dir = std::fs::canonicalize(current_dir)?;
        }

        let entries = read_dir(&current_dir)?;
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        // Find the index of the file to select
        let selected = select
            .and_then(|name| entries.iter().position(|e| e.name == name))
            .unwrap_or(0);

        Ok(Self {
            current_dir,
            entries,
            selected,
            scroll_offset: 0,
            picker,
            cached_image: None,
            log_panel_visible: false,
            log_scroll_offset: 0,
        })
    }

    /// Returns `Ok(true)` if the navigator should quit (file opened in neovim).
    pub fn enter_selected(&mut self) -> Resultx<bool> {
        if self.entries.is_empty() {
            return Ok(false);
        }

        let entry = &self.entries[self.selected];

        if entry.is_dir {
            if entry.name == "." {
                log::info!(
                    "Staying in the same directory: {}",
                    self.current_dir.display()
                );
                return Ok(false);
            }

            let new_path = self.current_dir.join(&entry.name);

            log::info!("Entering directory: {}", new_path.display());

            let entries = read_dir(&new_path)?;

            self.current_dir = new_path;
            self.entries = entries;
            self.selected = 0;
            self.scroll_offset = 0;
            self.cached_image = None;
            Ok(false)
        } else {
            // File selected - open in neovim
            let path = self.current_dir.join(&entry.name);
            self.open_in_neovim(&path)
        }
    }

    /// Opens a file in the parent neovim instance via the NVIM socket.
    /// Returns `Ok(true)` if successful and navigator should quit.
    fn open_in_neovim(&self, path: &PathBuf) -> Resultx<bool> {
        let nvim_socket = match env::var("NVIM") {
            Ok(socket) => {
                log::info!("NVIM socket: {}", socket);
                socket
            }
            Err(e) => {
                log::warn!(
                    "NVIM env var not set: {} - not running inside neovim terminal",
                    e
                );
                return Ok(false);
            }
        };

        let path_str = path.to_str().unwrap_or("");
        log::info!("Opening in neovim: {}", path_str);
        log::info!("Running: nvim --server {} --remote-expr ...", nvim_socket);

        // Use --remote-expr to:
        // 1. Switch to the previous window (the one behind the floating terminal)
        // 2. Open the file there
        // This way when the floating terminal closes, the file is already visible.
        let cmd = format!("execute('wincmd p | edit {}')", path_str.replace("'", "''"));
        log::info!("Sending command: {}", cmd);

        let output = Command::new("nvim")
            .args(["--server", &nvim_socket, "--remote-expr", &cmd])
            .output()?;

        log::info!("Exit status: {:?}", output.status);

        if !output.stdout.is_empty() {
            log::info!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        }

        if !output.stderr.is_empty() {
            log::error!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        }

        if output.status.success() {
            log::info!("File opened in neovim");
            Ok(true)
        } else {
            log::error!(
                "Failed to open file in neovim (exit code: {:?})",
                output.status.code()
            );
            Ok(false)
        }
    }

    pub fn go_to_parent_directory(&mut self) -> Resultx<()> {
        if let Some(parent) = self.current_dir.parent() {
            // This means the path was relative. Still thinking if I should support relative paths.
            if parent == "" {
                return Ok(());
            }

            log::info!("Going to parent: {}", parent.display());

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
        if self.log_panel_visible {
            // Scroll up in log panel (showing older entries)
            if let Some(store) = LOG_STORE.get() {
                let total = store.len();
                // Since we render from bottom, "up" means scroll to see older (earlier in list).
                self.log_scroll_offset = (self.log_scroll_offset + n).min(total.saturating_sub(1));
            }
        } else {
            self.selected = self.selected.saturating_sub(n);
            self.cached_image = None;
        }
    }

    pub fn move_down_by(&mut self, n: usize) {
        if self.log_panel_visible {
            // Scroll down in log panel (showing newer entries)
            self.log_scroll_offset = self.log_scroll_offset.saturating_sub(n);
        } else if !self.entries.is_empty() {
            self.selected = (self.selected + n).min(self.entries.len() - 1);
            self.cached_image = None;
        }
    }

    pub fn toggle_log_panel(&mut self) {
        self.log_panel_visible = !self.log_panel_visible;
        self.log_scroll_offset = 0;
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected)
            .map(|e| self.current_dir.join(&e.name))
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if self.log_panel_visible {
            // Expanded: file list on top, log panel takes bottom 70%
            let chunks = Layout::vertical([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(area);

            self.render_file_list(chunks[0], buf);
            self.render_log_panel(chunks[1], buf);
        } else {
            // Collapsed: file list + preview on top, status line at bottom
            let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);

            self.render_with_preview(chunks[0], buf);
            self.render_status_line(chunks[1], buf);
        }
    }

    fn render_with_preview(&mut self, area: Rect, buf: &mut Buffer) {
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
            let icon = if entry.is_dir {
                NF_OCT_FILE_DIRECTORY_FILL
            } else {
                " "
            };
            let color = file_color(&entry.name, entry.is_dir);
            let style = if i == self.selected {
                Style::new().fg(color).reversed()
            } else {
                Style::new().fg(color)
            };
            let line = Line::styled(format!("{}  {}", icon, entry.name), style);
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
        } else if svg_preview::is_svg(&path) {
            self.render_svg_preview(&path, area, buf);
        } else if image_preview::is_image(&path) {
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
                            format!("{NF_OCT_FILE_DIRECTORY_FILL}  {}", name)
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
            if let Some(dyn_img) = svg_preview::rasterize_svg(path) {
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

    fn render_status_line(&self, area: Rect, buf: &mut Buffer) {
        let Some(store) = LOG_STORE.get() else {
            return;
        };

        let Some(entry) = store.latest() else {
            return;
        };

        let elapsed = store.time_since_start() - store.elapsed_since(&entry);
        let elapsed_secs = elapsed.as_secs();
        let time_str = if elapsed_secs < 60 {
            format!("({}s)", elapsed_secs)
        } else {
            format!("({}m)", elapsed_secs / 60)
        };

        let level_color = match entry.level {
            Level::Error => Color::Red,
            Level::Warn => Color::Yellow,
            Level::Info => Color::Green,
            Level::Debug => Color::Blue,
            Level::Trace => Color::Gray,
        };

        let block = Block::default().borders(Borders::TOP);
        let inner = block.inner(area);
        block.render(area, buf);

        let line = Line::from(vec![
            Span::styled(
                format!("[{}]", entry.level.as_str()),
                Style::default().fg(level_color),
            ),
            Span::raw(" "),
            Span::raw(&entry.message),
            Span::raw(" "),
            Span::styled(time_str, Style::default().fg(Color::DarkGray)),
        ]);

        line.render(inner, buf);
    }

    fn render_log_panel(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default().borders(Borders::TOP);
        let inner = block.inner(area);
        block.render(area, buf);

        let Some(store) = LOG_STORE.get() else {
            return;
        };

        let entries = store.entries();
        let visible_height = inner.height as usize;
        let total_entries = entries.len();

        // Adjust scroll to keep within bounds
        if total_entries > visible_height {
            let max_offset = total_entries.saturating_sub(visible_height);
            self.log_scroll_offset = self.log_scroll_offset.min(max_offset);
        } else {
            self.log_scroll_offset = 0;
        }

        // Render from bottom (newest at bottom)
        for (i, entry) in entries
            .iter()
            .rev()
            .skip(self.log_scroll_offset)
            .take(visible_height)
            .enumerate()
        {
            let y = (visible_height - 1 - i) as i32;

            let level_color = match entry.level {
                Level::Error => Color::Red,
                Level::Warn => Color::Yellow,
                Level::Info => Color::Green,
                Level::Debug => Color::Blue,
                Level::Trace => Color::Gray,
            };

            let line = Line::from(vec![
                Span::styled(
                    format!("[{}]", entry.level.as_str()),
                    Style::default().fg(level_color),
                ),
                Span::raw(" "),
                Span::raw(&entry.message),
            ]);

            line.render(inner.offset(ratatui::layout::Offset { x: 0, y }), buf);
        }
    }
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
    let null_count = buffer.iter().filter(|b| **b == 0).count();
    if null_count > 0 || buffer.iter().any(|b| *b < 0x09 && *b != 0x00) {
        return "(binary file)".to_string();
    }

    match String::from_utf8(buffer) {
        Ok(text) => text.lines().take(max_lines).collect::<Vec<_>>().join("\n"),
        Err(_) => "(binary file)".to_string(),
    }
}
