pub mod error;
pub mod log_store;

mod globals;
mod io;
mod marks;
mod memory;
mod navigator;
mod plan;

use clap::Parser;
use ratatui::DefaultTerminal;

use crate::{error::Resultx, navigator::Navigator};

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
    log::info!("Navigator started in: {}", args.path);

    let mut navigator = Navigator::new(&args.path, args.select.as_deref())?;
    loop {
        terminal.draw(|frame| {
            navigator.render(frame.area(), frame.buffer_mut());
        })?;

        let event = ratatui::crossterm::event::read()?;
        if let Some(key) = event.as_key_event()
            && navigator.handle_key(key)?
        {
            return Ok(());
        }
    }
}
