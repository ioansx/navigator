use navigator::{
    error::{Kindx, Resultx},
    run_navigator,
};

fn main() -> Resultx<()> {
    ratatui::run(run_navigator).map_err(|e| e.ctx(Kindx::any("nav failed")))?;
    Ok(())
}
