pub mod error;

use std::path::PathBuf;

use ratatui::{DefaultTerminal, widgets::Widget};

use crate::error::Resultx;

pub struct Navigator {
    wd: PathBuf,
}

impl Navigator {
    pub fn new() -> Self {
        Self {
            wd: PathBuf::from("."),
        }
    }
}

impl Widget for &Navigator {
    fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer)
    where
        Self: Sized,
    {
        format!("Navigator: {}", self.wd.display()).render(area, buf);
    }
}

pub fn run_navigator(terminal: &mut DefaultTerminal) -> Resultx<()> {
    let mut navigator = Navigator::new();
    loop {
        terminal.draw(|frame| {
            // Widget is rendered by reference, can be reused.
            frame.render_widget(&navigator, frame.area());
        })?;

        let event = crossterm::event::read()?;
        if let Some(key_event) = event.as_key_event() {
            if key_event.code == crossterm::event::KeyCode::Char('q') {
                break Ok(());
            }
        }
    }
}
