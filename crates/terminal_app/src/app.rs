//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use gpui::{
    actions, App, AppContext as _, KeyBinding, WindowKind, WindowOptions, px, size,
};
use util::ResultExt;

pub mod terminal;
pub mod window;

/// Product/program name injected into child processes (`TERM_PROGRAM`,
/// `ZED_TERM`) and used as the window `app_id`.
pub const TERM_PROGRAM: &str = "zedterm";

actions!(terminal_app, [NewTab, CloseTab, NextTab, PreviousTab, NewWindow]);

/// Window options shared by every window the app opens.
pub fn window_options() -> WindowOptions {
    use gpui::{TitlebarOptions, WindowBackgroundAppearance, point};

    WindowOptions {
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
    }
}

/// Opens a fresh window with its own tab set.
pub fn open_new_window(cx: &mut App) {
    cx.open_window(window_options(), |_window, cx| {
        cx.new(|cx| window::TerminalWindowView::new(cx))
    })
    .log_err();
}

/// Bootstrap the application inside GPUI's launch callback: settings, theme
/// registry, keymap, then the first window.
pub fn run(cx: &mut App) {
    settings::init(cx);
    theme_settings::init(theme::LoadThemes::JustBase, cx);

    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-w", CloseTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-tab", NextTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-n", NewWindow, Some("TerminalWindow")),
    ]);

    open_new_window(cx);
}