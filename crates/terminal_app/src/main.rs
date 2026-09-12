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
use util::paths::{PathStyle, UrlExt as _};

struct LaunchOptions {
    user_data_dir: Option<PathBuf>,
    initial_directory: Option<PathBuf>,
}

impl LaunchOptions {
    fn parse() -> Result<Self> {
        let mut arguments = env::args_os().skip(1);
        let mut user_data_dir = None;
        let mut initial_directory = None;
        let mut parse_options = true;

        while let Some(argument) = arguments.next() {
            if parse_options && argument == OsStr::new("--") {
                parse_options = false;
            } else if parse_options && argument == OsStr::new("--user-data-dir") {
                let path = arguments
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--user-data-dir requires a path"))?;
                if path.is_empty() {
                    bail!("--user-data-dir requires a non-empty path");
                }
                if user_data_dir.replace(PathBuf::from(path)).is_some() {
                    bail!("--user-data-dir may only be specified once");
                }
            } else if parse_options
                && let Some(argument) = argument.to_str()
                && let Some(path) = argument.strip_prefix("--user-data-dir=")
            {
                if path.is_empty() {
                    bail!("--user-data-dir requires a non-empty path");
                }
                if user_data_dir.replace(PathBuf::from(path)).is_some() {
                    bail!("--user-data-dir may only be specified once");
                }
            } else if parse_options && argument.to_string_lossy().starts_with('-') {
                bail!("unrecognized argument: {}", argument.to_string_lossy());
            } else if initial_directory
                .replace(parse_initial_directory(argument)?)
                .is_some()
            {
                bail!("only one initial directory may be specified");
            }
        }

        Ok(Self {
            user_data_dir,
            initial_directory,
        })
    }

    fn restart_arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        if let Some(directory) = self.user_data_dir.as_deref() {
            let directory = paths::set_custom_data_dir(directory);
            arguments.extend([
                OsString::from("--user-data-dir"),
                directory.as_os_str().to_owned(),
            ]);
        }
        if let Some(directory) = self.initial_directory.as_deref() {
            arguments.push(OsString::from("--"));
            arguments.push(directory.as_os_str().to_owned());
        }
        arguments
    }
}

fn parse_initial_directory(argument: OsString) -> Result<PathBuf> {
    let directory = if argument == OsStr::new("%u") || argument == OsStr::new("%U") {
        // KDE's terminal launcher passes desktop field codes literally and sets the child CWD.
        env::current_dir().context("could not access current working directory")?
    } else if let Some(argument) = argument.to_str()
        && (argument.contains("://") || argument.starts_with("file:"))
    {
        let url = url::Url::parse(argument)
            .with_context(|| format!("invalid directory URI: {argument}"))?;
        if url.scheme() != "file" {
            bail!("unsupported directory URI: {argument}");
        }
        url.to_file_path_ext(PathStyle::local())
            .map_err(|_| anyhow::anyhow!("directory URI is not a local file URI: {argument}"))?
    } else {
        PathBuf::from(argument)
    };

    let metadata = std::fs::metadata(&directory)
        .with_context(|| format!("could not access directory {directory:?}"))?;
    if !metadata.is_dir() {
        bail!("initial path is not a directory: {directory:?}");
    }
    Ok(directory)
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

fn build_application() -> Result<Application> {
    // Without an explicit asset source gpui defaults to an empty one, which
    // silently breaks embedded fonts and icon SVGs (the "+" / settings
    // buttons render blank). `assets::Assets` embeds Zed's theme/font/icon set.
    let app = gpui_platform::application().with_assets(assets::Assets);

    // gpui's default desktop client rejects every request, so the theme
    // extension store needs a real one installed before the app starts.
    let user_agent = format!("ZedTerm/{}", env!("CARGO_PKG_VERSION"));
    let http_client = reqwest_client::ReqwestClient::user_agent(&user_agent)
        .context("could not start the HTTP client")?;
    Ok(app.with_http_client(std::sync::Arc::new(http_client)))
}

fn main() -> Result<()> {
    let launch_options = LaunchOptions::parse()?;
    let restart_arguments = launch_options.restart_arguments();
    let initial_directory = launch_options.initial_directory;
    init_paths()?;
    let app = build_application()?.with_restart_arguments(restart_arguments);
    #[cfg(target_os = "macos")]
    app.on_reopen(terminal_app::reopen_window_if_needed);
    app.run(move |cx| terminal_app::run(cx, initial_directory));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_url_field_codes_use_process_working_directory() -> Result<()> {
        let current_directory = env::current_dir()?;
        for field_code in ["%u", "%U"] {
            assert_eq!(
                parse_initial_directory(OsString::from(field_code))?,
                current_directory
            );
        }
        Ok(())
    }
}
