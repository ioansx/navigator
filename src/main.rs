use clap::Parser;
use navigator::{
    Args,
    error::{Kindx, Resultx},
    log_store, run_navigator,
};

fn main() -> Resultx<()> {
    let args = Args::parse();
    let rust_log = std::env::var("RUST_LOG").ok();

    log_store::init_logger(rust_log)?;
    ratatui::run(|terminal| run_navigator(terminal, &args))
        // Logged before it is returned, so the failure that ends the session is
        // in the dump the log store writes when it sees an error.
        .inspect_err(|e| log::error!("{e}"))
        .map_err(|e| e.ctx(Kindx::any("nav encountered an internal error")))?;
    Ok(())
}
