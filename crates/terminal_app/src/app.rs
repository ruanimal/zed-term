//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use gpui::{App, AppContext as _, WindowKind, WindowOptions, px, size};

pub mod terminal;
pub mod window;

/// Product/program name injected into child processes (`TERM_PROGRAM`,
/// `ZED_TERM`) and used as the window `app_id`.
pub const TERM_PROGRAM: &str = "zedterm";

/// Bootstrap the application inside GPUI's launch callback: settings, theme
/// registry, then a window. WP3 adds the tab bar and multi-window management,
/// WP4 the settings page.
pub fn run(cx: &mut App) {
    use crate::window::TerminalWindowView;
    use gpui::{TitlebarOptions, WindowBackgroundAppearance, point};

    settings::init(cx);
    theme_settings::init(theme::LoadThemes::JustBase, cx);

    let window_options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(9.0), px(9.0))),
        }),
        window_bounds: None,
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        app_owns_titlebar_drag: true,
        display_id: None,
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some(TERM_PROGRAM.to_string()),
        window_min_size: Some(size(px(640.0), px(400.0))),
        ..Default::default()
    };

    cx.open_window(window_options, |_window, cx| {
        cx.new(|cx| TerminalWindowView::new(cx))
    })
    .ok();
}