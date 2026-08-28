//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gpui::{
    Action, App, AppContext as _, KeyBinding, UpdateGlobal, WindowOptions, actions, px, size,
};
use settings::Settings as _;
use settings::SettingsStore;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::{
    Clear, Copy, Paste, PasteText, ScrollLineDown, ScrollLineUp, ScrollPageDown, ScrollPageUp,
    ScrollToBottom, ScrollToTop, SearchTest, SelectAll, ShowCharacterPalette,
};
use util::ResultExt;

pub mod persistence;
pub mod settings_ui;
pub mod terminal;
pub mod window;

/// Product/program name injected into child processes (`TERM_PROGRAM`,
/// `ZED_TERM`) and used as the window `app_id`.
pub const TERM_PROGRAM: &str = "zedterm";

actions!(
    terminal_app,
    [
        NewTab,
        CloseTab,
        CloseOtherTabs,
        CloseLeft,
        CloseRight,
        CloseAll,
        NextTab,
        PreviousTab,
        NewWindow,
        OpenSettings
    ]
);

/// Sends raw text (escape sequences included) directly to the PTY, mirroring
/// Zed's `terminal::SendText`.
#[derive(Action, Clone, Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[action(namespace = terminal_app)]
pub struct SendText(pub String);

/// Sends the given keystroke through the terminal's keystroke translation
/// (`Terminal::try_keystroke`), so e.g. `cmd-backspace` can send `ctrl-u`,
/// mirroring Zed's `terminal::SendKeystroke`.
#[derive(Action, Clone, Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[action(namespace = terminal_app)]
pub struct SendKeystroke(pub String);

/// Window options shared by every window the app opens. The system titlebar
/// is transparent: the tab bar itself occupies the titlebar strip (as in
/// Zed), with the traffic lights floating over its left edge.
pub fn window_options(bounds: gpui::Bounds<gpui::Pixels>) -> WindowOptions {
    use gpui::{
        TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind,
        point,
    };

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(9.0), px(9.0))),
        }),
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        // We draw our own titlebar (the tab bar) and move the window via
        // `Window::start_window_move`, so AppKit should not own titlebar
        // dragging. No-op off macOS.
        app_owns_titlebar_drag: true,
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

/// Registers the embedded Zed theme families (One/ayu/gruvbox from the asset
/// source) so settings.json's `theme` selection resolves against them.
fn load_embedded_themes(cx: &mut App) {
    let asset_source = cx.asset_source();
    let Ok(theme_paths) = asset_source.list("themes") else {
        return;
    };
    let registry = theme::ThemeRegistry::global(cx);
    for theme_path in theme_paths {
        if !theme_path.ends_with(".json") {
            continue;
        }
        let Some(bytes) = asset_source.load(&theme_path).unwrap_or_else(|_| {
            log::warn!("Failed to load embedded theme asset {theme_path:?}");
            None
        }) else {
            continue;
        };
        theme_settings::load_user_theme(&registry, &bytes).log_err();
    }
}

/// Loads user themes from the themes dir (`paths::themes_dir()`, shared with
/// Zed), so any Zed theme JSON on disk is available to ZedTerm too.
fn load_user_themes_in_background(cx: &mut App) {
    let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    cx.spawn(async move |cx| {
        let themes_dir = paths::themes_dir().clone();
        if !fs.is_dir(&themes_dir).await {
            return;
        }
        let mut theme_paths = match fs.read_dir(&themes_dir).await {
            Ok(paths) => paths,
            Err(error) => {
                log::warn!("Failed to read themes dir {themes_dir:?}: {error}");
                return;
            }
        };
        let registry = cx.update(|cx| theme::ThemeRegistry::global(cx));
        while let Some(Ok(theme_path)) = theme_paths.next().await {
            let Some(bytes) = fs.load_bytes(&theme_path).await.log_err() else {
                continue;
            };
            theme_settings::load_user_theme(&registry, &bytes).log_err();
        }
        cx.update(theme_settings::reload_theme);
    })
    .detach();
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
    open_window_with_bounds(bounds, cx)
}

/// Opens a main window at the given bounds and schedules its activation.
fn open_window_with_bounds(bounds: gpui::Bounds<gpui::Pixels>, cx: &mut App) {
    let handle = cx
        .open_window(window_options(bounds), |window, cx| {
            let view = cx.new(window::TerminalWindowView::new);
            view.read(cx).focus_handle.clone().focus(window, cx);
            view
        })
        .log_err();

    if let Some(handle) = handle {
        let task = cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
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
/// registry, fonts, keymap, persistence, then the first window.
pub fn run(cx: &mut App) {
    settings::init(cx);
    theme_settings::init(theme::LoadThemes::JustBase, cx);
    load_embedded_themes(cx);
    load_user_themes_in_background(cx);
    load_fonts(cx);
    watch_user_settings(cx);
    persist_window_geometry_on_quit(cx);

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
        KeyBinding::new(
            "ctrl-cmd-space",
            ShowCharacterPalette,
            Some("TerminalWindow"),
        ),
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
        KeyBinding::new(
            "cmd-shift-up",
            terminal_core::ScrollHalfPageUp,
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "cmd-shift-down",
            terminal_core::ScrollHalfPageDown,
            Some("TerminalWindow"),
        ),
        // Shell-line-editing conveniences translated through SendKeystroke /
        // SendText, mirroring Zed's "Terminal" context (alt-b/f/left/right are
        // word jumps; cmd-backspace/delete clear to start/end of line).
        KeyBinding::new(
            "cmd-backspace",
            SendKeystroke("ctrl-u".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "cmd-delete",
            SendKeystroke("ctrl-k".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "cmd-right",
            SendKeystroke("ctrl-e".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "cmd-left",
            SendKeystroke("ctrl-a".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "ctrl-backspace",
            SendKeystroke("ctrl-w".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "ctrl-delete",
            SendText("\x1b[3;5~".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new(
            "alt-delete",
            SendText("\x1bd".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new("alt-left", SendText("\x1bb".into()), Some("TerminalWindow")),
        KeyBinding::new(
            "alt-right",
            SendText("\x1bf".into()),
            Some("TerminalWindow"),
        ),
        KeyBinding::new("alt-b", SendText("\x1bb".into()), Some("TerminalWindow")),
        KeyBinding::new("alt-f", SendText("\x1bf".into()), Some("TerminalWindow")),
    ]);

    open_first_window(cx);
}

/// Opens the first window, restoring its geometry from the previous session
/// when possible and falling back to a default-sized window otherwise.
fn open_first_window(cx: &mut App) {
    let settings = TerminalSettings::get_global(cx);
    let default_size = size(settings.default_width, settings.default_height);
    let bounds = persistence::first_window_bounds(cx)
        .unwrap_or_else(|| persistence::default_first_window_bounds(cx, default_size));
    open_window_with_bounds(bounds, cx);
}

/// Saves each main window's geometry when the app quits so the next launch
/// can restore it. The settings floating window is excluded; tabs and shell
/// state are not persisted.
fn persist_window_geometry_on_quit(cx: &mut App) {
    cx.on_app_quit(|cx| {
        let mut bounds: Vec<gpui::Bounds<gpui::Pixels>> = Vec::new();
        for handle in cx.windows() {
            if let Some(main_window) = handle.downcast::<window::TerminalWindowView>() {
                main_window
                    .update(cx, |_, window, _| bounds.push(window.bounds()))
                    .log_err();
            }
        }
        persistence::save_window_geometries(&bounds);
        async {}
    })
    .detach();
}
