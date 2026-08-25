//! ZedTerm — a standalone terminal application, forked out of Zed's terminal.
//!
//! Owns the `Application` bootstrap; the per-`App` initialization lives in
//! `terminal_app::run`.

use anyhow::Result;
use gpui::Application;

fn build_application() -> Application {
    let platform = gpui_platform::current_platform(false);
    Application::new_inaccessible(platform)
}

fn main() -> Result<()> {
    let app = build_application().with_assets(assets::Assets);
    app.run(|cx| terminal_app::run(cx));
    Ok(())
}