pub mod error;

use std::{fs, path::PathBuf};

use ratatui::{DefaultTerminal, widgets::Widget};

use crate::error::Resultx;

pub fn run_navigator(terminal: &mut DefaultTerminal) -> Resultx<()> {
    use crossterm::event::KeyCode;

    let mut navigator = Navigator::new()?;
    loop {
        terminal.draw(|frame| {
            frame.render_widget(&navigator, frame.area());
        })?;

        let event = crossterm::event::read()?;
        if let Some(key_event) = event.as_key_event() {
            match key_event.code {
                KeyCode::Char('q') => break Ok(()),
                KeyCode::Char('j') | KeyCode::Down => navigator.move_down(),
                KeyCode::Char('k') | KeyCode::Up => navigator.move_up(),
                _ => {}
            }
        }
    }
}

pub struct Navigator {
    current_directory: PathBuf,
    entries: Vec<DirEntry>,
    selected: usize,
}

pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

impl Navigator {
    pub fn new() -> Resultx<Self> {
        let current_directory = PathBuf::from(".");
        let entries = Self::read_dir(&current_directory)?;
        Ok(Self {
            current_directory,
            entries,
            selected: 0,
        })
    }

    fn read_dir(path: &PathBuf) -> Resultx<Vec<DirEntry>> {
        let mut entries: Vec<DirEntry> = fs::read_dir(path)?
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

        Ok(entries)
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len() - 1);
        }
    }
}

impl Widget for &Navigator {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer)
    where
        Self: Sized,
    {
        use ratatui::style::Style;
        use ratatui::text::Line;

        for (i, entry) in self.entries.iter().enumerate() {
            if i as u16 >= area.height {
                break;
            }
            let prefix = if entry.is_dir { "📁 " } else { "   " };
            let style = if i == self.selected {
                Style::new().reversed()
            } else {
                Style::default()
            };
            let line = Line::styled(format!("{}{}", prefix, entry.name), style);
            line.render(
                area.offset(ratatui::layout::Offset { x: 0, y: i as i32 }),
                buf,
            );
        }
    }
}
