//! ZedTerm — a standalone terminal application, forked out of Zed's terminal.
//!
//! Owns the `Application` bootstrap; the per-`App` initialization lives in
//! `terminal_app::run`.

use std::{
    env,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use anyhow::{Context as _, Result, bail};
use gpui::Application;

struct LaunchOptions {
    user_data_dir: Option<PathBuf>,
}

impl LaunchOptions {
    fn parse() -> Result<Self> {
        let mut arguments = env::args_os().skip(1);
        let mut user_data_dir = None;

        while let Some(argument) = arguments.next() {
            if argument == OsStr::new("--user-data-dir") {
                let path = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--user-data-dir requires a path"))?;
                if path.is_empty() {
                    bail!("--user-data-dir requires a non-empty path");
                }
                if user_data_dir.replace(PathBuf::from(path)).is_some() {
                    bail!("--user-data-dir may only be specified once");
                }
            } else if let Some(argument) = argument.to_str()
                && let Some(path) = argument.strip_prefix("--user-data-dir=")
            {
                if path.is_empty() {
                    bail!("--user-data-dir requires a non-empty path");
                }
                if user_data_dir.replace(PathBuf::from(path)).is_some() {
                    bail!("--user-data-dir may only be specified once");
                }
            } else {
                bail!("unrecognized argument: {}", argument.to_string_lossy());
            }
        }

        Ok(Self { user_data_dir })
    }

    fn restart_arguments(&self) -> Vec<OsString> {
        let Some(directory) = self.user_data_dir.as_deref() else {
            return Vec::new();
        };

        let directory = paths::set_custom_data_dir(directory);
        vec![
            OsString::from("--user-data-dir"),
            directory.as_os_str().to_owned(),
        ]
    }
}

fn init_paths() -> Result<()> {
    for path in [
        paths::config_dir(),
        paths::data_dir(),
        paths::state_dir(),
        paths::temp_dir(),
        paths::logs_dir(),
    ] {
        std::fs::create_dir_all(path)
            .with_context(|| format!("Failed to create ZedTerm directory {path:?}"))?;
    }
    Ok(())
}

fn build_application() -> Application {
    // Without an explicit asset source gpui defaults to an empty one, which
    // silently breaks embedded fonts and icon SVGs (the "+" / settings
    // buttons render blank). `assets::Assets` embeds Zed's theme/font/icon set.
    gpui_platform::application().with_assets(assets::Assets)
}

fn main() -> Result<()> {
    let launch_options = LaunchOptions::parse()?;
    let restart_arguments = launch_options.restart_arguments();
    init_paths()?;
    let app = build_application().with_restart_arguments(restart_arguments);
    #[cfg(target_os = "macos")]
    app.on_reopen(terminal_app::reopen_window_if_needed);
    app.run(terminal_app::run);
    Ok(())
}
