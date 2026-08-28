//! ZedTerm — a standalone terminal application, forked out of Zed's terminal.
//!
//! Owns the `Application` bootstrap; the per-`App` initialization lives in
//! `terminal_app::run`.

use anyhow::Result;
use gpui::Application;

fn build_application() -> Application {
    // Without an explicit asset source gpui defaults to an empty one, which
    // silently breaks embedded fonts and icon SVGs (the "+" / settings
    // buttons render blank). `assets::Assets` embeds Zed's theme/font/icon set.
    gpui_platform::application().with_assets(assets::Assets)
}

fn main() -> Result<()> {
    let app = build_application();
    app.run(terminal_app::run);
    Ok(())
}
