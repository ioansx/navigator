pub mod error;
pub mod logging;

mod globals;
mod io;
mod navigator;

use ratatui::DefaultTerminal;

use crate::{error::Resultx, globals::SCROLL_JUMP, navigator::Navigator};

pub fn run_navigator(terminal: &mut DefaultTerminal) -> Resultx<()> {
    use crossterm::event::{KeyCode, KeyModifiers};

    logging::init_logger().ok();
    log::info!("Navigator started");

    let mut navigator = Navigator::new(".")?;
    loop {
        terminal.draw(|frame| {
            navigator.render(frame.area(), frame.buffer_mut());
        })?;

        let event = crossterm::event::read()?;
        if let Some(key_event) = event.as_key_event() {
            let ctrl = key_event.modifiers.contains(KeyModifiers::CONTROL);

            match key_event.code {
                KeyCode::Char('q') => {
                    log::info!("Navigator quit");
                    return Ok(());
                }
                KeyCode::Char('d') if ctrl => {
                    navigator.move_down_by(SCROLL_JUMP);
                }
                KeyCode::Char('u') if ctrl => {
                    navigator.move_up_by(SCROLL_JUMP);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    navigator.move_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    navigator.move_up();
                }
                KeyCode::Enter => {
                    navigator.enter_selected_directory()?;
                }
                KeyCode::Char('-') => {
                    navigator.go_to_parent_directory()?;
                }
                KeyCode::Char('L') => {
                    navigator.toggle_log_panel();
                }
                _ => {}
            }
        }
    }
}
