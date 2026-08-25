//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use std::time::Duration;

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
    use gpui::{WindowBackgroundAppearance, WindowDecorations};

    WindowOptions {
        window_bounds: None,
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        app_owns_titlebar_drag: false,
        display_id: None,
        window_background: WindowBackgroundAppearance::Opaque,
        window_decorations: Some(WindowDecorations::Server),
        app_id: Some(TERM_PROGRAM.to_string()),
        window_min_size: Some(size(px(640.0), px(400.0))),
        ..Default::default()
    }
}

/// Loads the embedded fonts (Lilex/IBM Plex Sans) that `.ZedMono`/`.ZedSans`
/// resolve to; without them terminal text fails to shape.
fn load_fonts(cx: &mut App) {
    let asset_source = cx.asset_source();
    let font_paths = asset_source.list("fonts").unwrap();
    let mut embedded_fonts = Vec::new();
    for font_path in &font_paths {
        if !font_path.ends_with(".ttf") {
            continue;
        }
        if let Some(font_bytes) = asset_source.load(font_path).unwrap() {
            embedded_fonts.push(font_bytes);
        }
    }
    cx.text_system().add_fonts(embedded_fonts).unwrap();
}

/// Opens a fresh window with its own tab set. Activates the window and moves
/// keyboard focus into it so window-level keybindings take effect.
pub fn open_new_window(cx: &mut App) {
    let handle = cx
        .open_window(window_options(), |window, cx| {
            let view = cx.new(|cx| window::TerminalWindowView::new(cx));
            view.read(cx).focus_handle.clone().focus(window, cx);
            view
        })
        .log_err();

    // Give AppKit a moment to attach the window to a screen before asking for
    // key status; the display link (which drives redraws) is started when the
    // window becomes visible/key.
    if let Some(handle) = handle {
        let task = cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            handle
                .update(cx, |_, window, _| {
                    window.activate_window();
                })
                .log_err();
        });
        task.detach();
    }
}

/// Bootstrap the application inside GPUI's launch callback: settings, theme
/// registry, fonts, keymap, then the first window.
pub fn run(cx: &mut App) {
    settings::init(cx);
    theme_settings::init(theme::LoadThemes::JustBase, cx);
    load_fonts(cx);

    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-w", CloseTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-tab", NextTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-n", NewWindow, Some("TerminalWindow")),
    ]);

    open_new_window(cx);
}