pub mod error;
pub mod log_store;

mod globals;
mod io;
mod marks;
mod memory;
mod navigator;
mod plan;

use std::path::PathBuf;

use clap::Parser;
use ratatui::DefaultTerminal;

use crate::{
    error::Resultx,
    io::file,
    navigator::{Navigator, Outcome},
};

#[derive(Parser, Debug)]
#[command(name = "nav", about = "Terminal file navigator")]
pub struct Args {
    /// Directory to open
    #[arg(default_value = ".")]
    pub path: String,

    /// File to select
    #[arg(short, long)]
    pub select: Option<String>,

    /// Where to record the directory you ended in, for a shell wrapper to cd to
    #[arg(long, value_name = "PATH")]
    pub cwd_file: Option<PathBuf>,
}

/// Runs the navigator until the user quits or opens a file in neovim.
///
/// `Q` is the only quit that moves the shell: it records where the session ended
/// in `--cwd-file`, for the wrapper that `cd`s there.
///
/// # Errors
/// Fails if a directory cannot be read, if the terminal stops delivering events,
/// or if `--cwd-file` cannot be written.
pub fn run_navigator(terminal: &mut DefaultTerminal, args: &Args) -> Resultx<()> {
    log::info!("Navigator started in: {}", args.path);

    let mut navigator = Navigator::new(&args.path, args.select.as_deref())?;
    loop {
        terminal.draw(|frame| {
            navigator.render(frame.area(), frame.buffer_mut());
        })?;

        let event = ratatui::crossterm::event::read()?;
        let Some(key) = event.as_key_event() else {
            continue;
        };

        match navigator.handle_key(key) {
            Outcome::Stay => {}
            Outcome::Quit => return Ok(()),
            Outcome::QuitHere => {
                if let Some(cwd_file) = &args.cwd_file {
                    file::write_cwd(cwd_file, navigator.current_dir())?;
                }
                return Ok(());
            }
        }
    }
}
