use navigator::{
    error::{Kindx, Resultx},
    logging, run_navigator,
};

fn main() -> Resultx<()> {
    let rust_log = std::env::var("RUST_LOG").ok();

    logging::init_logger(rust_log)?;
    ratatui::run(run_navigator)
        .map_err(|e| e.ctx(Kindx::any("nav encountered an internal error")))?;
    Ok(())
}
