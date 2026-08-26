//! ZedTerm — a standalone terminal application, forked out of Zed's terminal.
//!
//! Owns the `Application` bootstrap; the per-`App` initialization lives in
//! `terminal_app::run`.

use anyhow::Result;
use gpui::Application;

fn build_application() -> Application {
    gpui_platform::application()
}

fn main() -> Result<()> {
    let app = build_application();
    app.run(|cx| terminal_app::run(cx));
    Ok(())
}