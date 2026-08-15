use std::path::{Path, PathBuf};

use ratatui::{
    layout::{Constraint, Layout, Offset, Rect},
    prelude::Buffer,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

use crate::{
    error::Resultx,
    globals::{NF_OCT_FILE_DIRECTORY_FILL, SCROLL_OFF, file_color, level_color},
    io::{
        FileKind,
        dir::{self, DirEntry},
        file, nvim, raster,
    },
    log_store::LOG_STORE,
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
        let current_dir = dir::resolve(Path::new(current_dir_path))?;
        let entries = dir::read_dir_with_dots(&current_dir)?;
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
        let name = entry.name.clone();

        if !entry.is_dir {
            return nvim::open(&self.current_dir.join(&name));
        }

        match name.as_str() {
            "." => log::info!(
                "Staying in the same directory: {}",
                self.current_dir.display()
            ),
            ".." => self.go_to_parent_directory()?,
            name => {
                let new_path = self.current_dir.join(name);
                log::info!("Entering directory: {}", new_path.display());
                self.go_to(new_path)?;
            }
        }
        Ok(false)
    }

    /// Goes up one level. At the filesystem root there is nowhere to go, so this does nothing.
    pub fn go_to_parent_directory(&mut self) -> Resultx<()> {
        if let Some(parent) = self.current_dir.parent() {
            log::info!("Going to parent: {}", parent.display());
            self.go_to(parent.to_path_buf())?;
        }
        Ok(())
    }

    /// Switches to `path`, resetting the selection and any cached preview.
    fn go_to(&mut self, path: PathBuf) -> Resultx<()> {
        self.entries = dir::read_dir_with_dots(&path)?;
        self.current_dir = path;
        self.selected = 0;
        self.scroll_offset = 0;
        self.cached_image = None;
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

    pub const fn toggle_log_panel(&mut self) {
        self.log_panel_visible = !self.log_panel_visible;
        self.log_scroll_offset = 0;
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
            render_status_line(chunks[1], buf);
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
            let color = file_color(&entry.name, entry.is_dir);
            let style = if i == self.selected {
                Style::new().fg(color).reversed()
            } else {
                Style::new().fg(color)
            };
            let line = Line::styled(format!("{}  {}", icon_for(entry), entry.name), style);
            line.render(row(inner, i - self.scroll_offset), buf);
        }
    }

    fn render_preview(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };

        let path = self.current_dir.join(&entry.name);
        match FileKind::of(&path, entry.is_dir) {
            FileKind::Dir => render_directory_preview(&path, area, buf),
            FileKind::Image => self.render_image_preview(&path, area, buf),
            FileKind::Text => render_text_preview(&path, area, buf),
        }
    }

    fn render_image_preview(&mut self, path: &Path, area: Rect, buf: &mut Buffer) {
        let cached = self
            .cached_image
            .as_ref()
            .is_some_and(|(cached, _)| cached == path);

        if !cached {
            match raster::load(path) {
                Ok(img) => {
                    let protocol = self.picker.new_resize_protocol(img);
                    self.cached_image = Some((path.to_path_buf(), protocol));
                }
                Err(e) => {
                    log::warn!("{e}");
                    Paragraph::new("(cannot load image)").render(area, buf);
                    return;
                }
            }
        }

        if let Some((_, protocol)) = &mut self.cached_image {
            StatefulImage::default().render(area, buf, protocol);
        }
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

        // Keep the scroll within bounds as the panel resizes or entries expire.
        self.log_scroll_offset = self
            .log_scroll_offset
            .min(entries.len().saturating_sub(visible_height));

        // Newest at the bottom, so the first entry drawn goes on the last row.
        for (i, entry) in entries
            .iter()
            .rev()
            .skip(self.log_scroll_offset)
            .take(visible_height)
            .enumerate()
        {
            let line = Line::from(vec![
                Span::styled(
                    format!("[{}]", entry.level.as_str()),
                    Style::default().fg(level_color(entry.level)),
                ),
                Span::raw(" "),
                Span::raw(&entry.message),
            ]);

            line.render(row(inner, visible_height - 1 - i), buf);
        }
    }
}

/// Row `n` of `area`, counted from its top.
fn row(area: Rect, n: usize) -> Rect {
    let y = i32::try_from(n).unwrap_or(i32::MAX);
    area.offset(Offset { x: 0, y })
}

const fn icon_for(entry: &DirEntry) -> &'static str {
    if entry.is_dir {
        NF_OCT_FILE_DIRECTORY_FILL
    } else {
        " "
    }
}

fn render_directory_preview(path: &Path, area: Rect, buf: &mut Buffer) {
    let content = match dir::read_dir(path) {
        Ok(entries) if entries.is_empty() => "(empty directory)".to_string(),
        Ok(entries) => entries
            .iter()
            .take(area.height as usize)
            .map(|entry| format!("{}  {}", icon_for(entry), entry.name))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => "(cannot read directory)".to_string(),
    };

    Paragraph::new(content).render(area, buf);
}

fn render_text_preview(path: &Path, area: Rect, buf: &mut Buffer) {
    let content = file::read_text_preview(path, area.height as usize);
    Paragraph::new(content).render(area, buf);
}

fn render_status_line(area: Rect, buf: &mut Buffer) {
    let Some(store) = LOG_STORE.get() else {
        return;
    };

    let Some(entry) = store.latest() else {
        return;
    };

    let elapsed = store
        .time_since_start()
        .saturating_sub(store.elapsed_since(&entry));
    let elapsed_secs = elapsed.as_secs();
    let time_str = if elapsed_secs < 60 {
        format!("({elapsed_secs}s)")
    } else {
        format!("({}m)", elapsed_secs / 60)
    };

    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    block.render(area, buf);

    let line = Line::from(vec![
        Span::styled(
            format!("[{}]", entry.level.as_str()),
            Style::default().fg(level_color(entry.level)),
        ),
        Span::raw(" "),
        Span::raw(&entry.message),
        Span::raw(" "),
        Span::styled(time_str, Style::default().fg(Color::DarkGray)),
    ]);

    line.render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    fn open(path: &Path, select: Option<&str>) -> Navigator {
        Navigator::new(path.to_str().unwrap(), select).unwrap()
    }

    fn render_to_string(nav: &mut Navigator, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        nav.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The name under the cursor.
    fn selected(nav: &Navigator) -> &str {
        &nav.entries[nav.selected].name
    }

    #[test]
    fn renders_the_listing_and_the_preview_side_by_side() {
        let tmp = TempDir::new();
        tmp.dir("subdir");
        tmp.file("notes.txt", "the file contents");

        let mut nav = open(tmp.path(), Some("notes.txt"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("subdir"), "missing directory in:\n{out}");
        assert!(out.contains("notes.txt"), "missing file in:\n{out}");
        assert!(
            out.contains("the file contents"),
            "missing preview in:\n{out}"
        );
    }

    #[test]
    fn starts_on_the_selected_file() {
        let tmp = TempDir::new();
        tmp.file("aaa.txt", "");
        tmp.file("zzz.txt", "");

        let nav = open(tmp.path(), Some("zzz.txt"));

        assert_eq!(selected(&nav), "zzz.txt");
    }

    #[test]
    fn starts_at_the_top_when_the_selection_is_missing() {
        let tmp = TempDir::new();
        tmp.file("aaa.txt", "");

        let nav = open(tmp.path(), Some("not-here.txt"));

        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn opening_a_missing_directory_fails() {
        let tmp = TempDir::new();

        assert!(Navigator::new(tmp.path().join("nope").to_str().unwrap(), None).is_err());
    }

    #[test]
    fn entering_a_subdirectory_descends_into_it() {
        let tmp = TempDir::new();
        tmp.file("sub/inside.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path().join("sub"));
        assert_eq!(selected(&nav), ".");
        assert!(nav.entries.iter().any(|e| e.name == "inside.txt"));
    }

    #[test]
    fn entering_dot_dot_goes_up_instead_of_appending_to_the_path() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        tmp.file("marker.txt", "");

        let mut nav = open(&tmp.path().join("sub"), None);
        nav.selected = 1;
        assert_eq!(selected(&nav), "..");
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path());
        assert!(nav.entries.iter().any(|e| e.name == "marker.txt"));
    }

    #[test]
    fn entering_dot_stays_put() {
        let tmp = TempDir::new();
        tmp.dir("sub");

        let mut nav = open(tmp.path(), None);
        assert_eq!(selected(&nav), ".");
        nav.enter_selected().unwrap();

        assert_eq!(nav.current_dir, tmp.path());
    }

    #[cfg(unix)]
    #[test]
    fn entering_a_symlinked_directory_descends_into_it() {
        let tmp = TempDir::new();
        let target = tmp.dir("real");
        tmp.file("real/inside.txt", "");
        std::os::unix::fs::symlink(&target, tmp.path().join("link")).unwrap();

        let mut nav = open(tmp.path(), Some("link"));
        nav.enter_selected().unwrap();

        assert!(nav.entries.iter().any(|e| e.name == "inside.txt"));
    }

    #[test]
    fn moving_stops_at_both_ends_of_the_list() {
        let tmp = TempDir::new();
        tmp.file("a.txt", "");
        tmp.file("b.txt", "");

        let mut nav = open(tmp.path(), None);

        nav.move_down_by(999);
        assert_eq!(selected(&nav), "b.txt");

        nav.move_up_by(999);
        assert_eq!(selected(&nav), ".");
    }

    #[test]
    fn navigating_away_drops_the_cached_preview() {
        let tmp = TempDir::new();
        tmp.dir("sub");
        tmp.file("a.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        nav.cached_image = None;
        nav.enter_selected().unwrap();

        assert!(nav.cached_image.is_none());
        assert_eq!(nav.scroll_offset, 0);
    }

    #[test]
    fn an_empty_directory_still_lists_the_dots() {
        let tmp = TempDir::new();

        let mut nav = open(tmp.path(), None);
        let out = render_to_string(&mut nav, 40, 10);

        assert_eq!(nav.entries.len(), 2);
        assert!(out.contains('.'), "missing dot entries in:\n{out}");
    }

    #[test]
    fn a_directory_preview_lists_its_contents() {
        let tmp = TempDir::new();
        tmp.file("sub/nested.txt", "");

        let mut nav = open(tmp.path(), Some("sub"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("nested.txt"), "missing preview in:\n{out}");
    }

    #[test]
    fn a_binary_file_preview_says_so() {
        let tmp = TempDir::new();
        tmp.file("app.bin", [0x00, 0x01, 0x02, 0x03]);

        let mut nav = open(tmp.path(), Some("app.bin"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("(binary file)"), "in:\n{out}");
    }

    #[test]
    fn a_broken_image_preview_says_so() {
        let tmp = TempDir::new();
        tmp.file("broken.png", "not actually a png");

        let mut nav = open(tmp.path(), Some("broken.png"));
        let out = render_to_string(&mut nav, 80, 24);

        assert!(out.contains("(cannot load image)"), "in:\n{out}");
    }
}
