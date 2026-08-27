//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    actions, App, AppContext as _, KeyBinding, UpdateGlobal, WindowOptions, px, size,
};
use settings::Settings as _;
use settings::SettingsStore;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::{
    Clear, Copy, Paste, PasteText, ScrollLineDown, ScrollLineUp, ScrollPageDown, ScrollPageUp,
    ScrollToBottom, ScrollToTop, SearchTest, SelectAll, ShowCharacterPalette,
};
use util::ResultExt;

pub mod settings_ui;
pub mod terminal;
pub mod window;

/// Product/program name injected into child processes (`TERM_PROGRAM`,
/// `ZED_TERM`) and used as the window `app_id`.
pub const TERM_PROGRAM: &str = "zedterm";

actions!(
    terminal_app,
    [NewTab, CloseTab, NextTab, PreviousTab, NewWindow, OpenSettings]
);

/// Window options shared by every window the app opens.
pub fn window_options(bounds: gpui::Bounds<gpui::Pixels>) -> WindowOptions {
    use gpui::{WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind};

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
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

/// Watches the user settings file (`~/Library/Application Support/Zed/
/// settings.json`, same path Zed uses) and applies changes live.
fn watch_user_settings(cx: &mut App) {
    let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    SettingsStore::update_global(cx, |store, cx| {
        store.watch_settings_files(fs, cx, |settings_file, result, cx| {
            if matches!(settings_file, settings::SettingsFile::User)
                && let settings::ParseStatus::Failed { error } = &result.parse_status
            {
                log::error!("Failed to load user settings: {error}");
            }
            cx.refresh_windows();
        });
    });
}

/// Opens a fresh window with its own tab set. Activates the window shortly
/// after opening so the macOS display link (which drives redraws) starts.
pub fn open_new_window(cx: &mut App) {
    let settings = TerminalSettings::get_global(cx);
    let bounds = gpui::Bounds::centered(
        None,
        size(settings.default_width, settings.default_height),
        cx,
    );
    let handle = cx
        .open_window(window_options(bounds), |window, cx| {
            let view = cx.new(|cx| window::TerminalWindowView::new(cx));
            view.read(cx).focus_handle.clone().focus(window, cx);
            view
        })
        .log_err();

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
    watch_user_settings(cx);

    cx.bind_keys([
        // Window / tab management.
        KeyBinding::new("cmd-t", NewTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-w", CloseTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-tab", NextTab, Some("TerminalWindow")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("TerminalWindow")),
        KeyBinding::new("cmd-n", NewWindow, Some("TerminalWindow")),
        KeyBinding::new("cmd-,", OpenSettings, Some("TerminalWindow")),
        // Terminal actions, mirroring Zed's `"terminal"` keymap context.
        KeyBinding::new("cmd-c", Copy, Some("TerminalWindow")),
        KeyBinding::new("cmd-v", Paste, Some("TerminalWindow")),
        KeyBinding::new("ctrl-cmd-v", PasteText, Some("TerminalWindow")),
        KeyBinding::new("cmd-a", SelectAll, Some("TerminalWindow")),
        KeyBinding::new("cmd-k", Clear, Some("TerminalWindow")),
        KeyBinding::new("cmd-f", SearchTest, Some("TerminalWindow")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TerminalWindow")),
        KeyBinding::new("shift-up", ScrollLineUp, Some("TerminalWindow")),
        KeyBinding::new("shift-down", ScrollLineDown, Some("TerminalWindow")),
        KeyBinding::new("shift-pageup", ScrollPageUp, Some("TerminalWindow")),
        KeyBinding::new("shift-pagedown", ScrollPageDown, Some("TerminalWindow")),
        KeyBinding::new("cmd-up", ScrollPageUp, Some("TerminalWindow")),
        KeyBinding::new("cmd-down", ScrollPageDown, Some("TerminalWindow")),
        KeyBinding::new("shift-home", ScrollToTop, Some("TerminalWindow")),
        KeyBinding::new("shift-end", ScrollToBottom, Some("TerminalWindow")),
        KeyBinding::new("cmd-home", ScrollToTop, Some("TerminalWindow")),
        KeyBinding::new("cmd-end", ScrollToBottom, Some("TerminalWindow")),
    ]);

    open_new_window(cx);
}