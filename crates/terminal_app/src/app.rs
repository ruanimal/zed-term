//! ZedTerm — a standalone terminal application forked out of Zed's terminal.
//!
//! Keeps only traditional terminal capabilities and terminal-related settings
//! (including a settings page). See `terminal-app/PLAN.md` for the split plan.

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _,
    TitlebarOptions, Window, WindowKind, WindowOptions, div, point, px, rgb, size, white,
};

/// Product/program name injected into child processes (`TERM_PROGRAM`,
/// `ZED_TERM`) and used as the window `app_id`.
pub const TERM_PROGRAM: &str = "zedterm";

/// Bootstrap the application inside GPUI's launch callback: settings, theme
/// registry, then a window. WP2/WP3 replace the placeholder view with the real
/// terminal element and tab bar; WP4 adds the settings page.
pub fn run(cx: &mut App) {
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
        window_background: gpui::WindowBackgroundAppearance::Opaque,
        app_id: Some(TERM_PROGRAM.to_string()),
        window_min_size: Some(size(px(640.0), px(400.0))),
        ..Default::default()
    };

    cx.open_window(window_options, |_window, cx| cx.new(|_| PlaceholderWindow))
        .ok();
}

/// Bootstrap placeholder root view, rendered until WP2/WP3 land the terminal
/// element and tab bar.
struct PlaceholderWindow;

impl Render for PlaceholderWindow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x14151a))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_color(white())
                    .child(format!("{TERM_PROGRAM} — bootstrap window")),
            )
    }
}