use navigator::{
    error::{Errx, Resultx},
    run_navigator,
};

fn main() -> Resultx<()> {
    ratatui::run(run_navigator).map_err(|e| Errx::e_any(e, "nav failed"))?;
    Ok(())
}
