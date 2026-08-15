pub mod error;
pub mod log_store;

mod globals;
mod io;
mod memory;
mod navigator;

use clap::Parser;
use ratatui::DefaultTerminal;

use crate::{error::Resultx, globals::SCROLL_JUMP, navigator::Navigator};

#[derive(Parser, Debug)]
#[command(name = "nav", about = "Terminal file navigator")]
pub struct Args {
    /// Directory to open
    #[arg(default_value = ".")]
    pub path: String,

    /// File to select
    #[arg(short, long)]
    pub select: Option<String>,
}

/// Runs the navigator until the user quits or opens a file in neovim.
///
/// # Errors
/// Fails if a directory cannot be read, or if the terminal stops delivering events.
pub fn run_navigator(terminal: &mut DefaultTerminal, args: &Args) -> Resultx<()> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    log::info!("Navigator started in: {}", args.path);

    let mut navigator = Navigator::new(&args.path, args.select.as_deref())?;
    loop {
        terminal.draw(|frame| {
            navigator.render(frame.area(), frame.buffer_mut());
        })?;

        let event = ratatui::crossterm::event::read()?;
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
                KeyCode::Enter | KeyCode::Char('l') => {
                    if navigator.enter_selected()? {
                        return Ok(()); // File opened in neovim, quit navigator
                    }
                }
                KeyCode::Char('-' | 'h') => {
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
