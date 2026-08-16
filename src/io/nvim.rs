use std::{env, path::Path, process::Command};

use crate::error::{Errx, Resultx};

/// Opens `path` in the neovim instance whose terminal `nav` is running in.
///
/// Returns `false` when there is no such instance, or when neovim refused the
/// edit — either way the navigator stays open.
pub fn open(path: &Path) -> Resultx<bool> {
    let Ok(socket) = env::var("NVIM") else {
        log::warn!("NVIM is not set - not running inside a neovim terminal");
        return Ok(false);
    };

    let expr = edit_expr(path);
    log::info!("Opening {} via {socket}", path.display());

    let output = Command::new("nvim")
        .args(["--server", &socket, "--remote-expr", &expr])
        .output()
        .map_err(|e| Errx::e_io(e, "running nvim --server"))?;

    if !output.stderr.is_empty() {
        log::error!("nvim: {}", String::from_utf8_lossy(&output.stderr).trim());
    }

    if !output.status.success() {
        log::error!("nvim exited with {:?}", output.status.code());
        return Ok(false);
    }

    log::info!("File opened in neovim");
    Ok(true)
}

/// Vimscript that opens `path` in the window `nav`'s terminal occupies, so the file
/// takes that window over the way a file explorer buffer would.
fn edit_expr(path: &Path) -> String {
    // Doubling is how a single quote is escaped inside a Vimscript literal string,
    // and `fnameescape` handles the spaces and the `|`, `%`, `#` that `:edit` would
    // otherwise read as syntax.
    let quoted = path.to_string_lossy().replace('\'', "''");
    format!("execute('edit ' . fnameescape('{quoted}'))")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn expr_for(path: &str) -> String {
        edit_expr(&PathBuf::from(path))
    }

    #[test]
    fn edits_the_path_in_the_current_window() {
        let expr = expr_for("/home/me/main.rs");

        assert_eq!(expr, "execute('edit ' . fnameescape('/home/me/main.rs'))");
    }

    #[test]
    fn passes_the_path_through_fnameescape() {
        // Spaces, `|`, `%` and `#` are all syntax to `:edit`; vim must escape them.
        for path in ["/a b/c.rs", "/a|b.rs", "/a%b.rs", "/a#b.rs"] {
            let expr = expr_for(path);
            assert!(
                expr.contains(&format!("fnameescape('{path}')")),
                "{path} was not handed to fnameescape: {expr}"
            );
        }
    }

    #[test]
    fn doubles_single_quotes_so_they_do_not_end_the_string() {
        let expr = expr_for("/tmp/it's here.rs");

        assert_eq!(expr, "execute('edit ' . fnameescape('/tmp/it''s here.rs'))");
    }

    #[test]
    fn a_quote_only_path_stays_balanced() {
        let expr = expr_for("'");

        // Every quote in the result must pair up, or vim sees an unterminated string.
        assert_eq!(expr.matches('\'').count() % 2, 0, "{expr}");
    }
}
