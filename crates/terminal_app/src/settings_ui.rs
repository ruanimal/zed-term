//! Settings page (WP4 §4.3): a small floating window that edits the
//! "terminal" section of the user settings file through the standard
//! `SettingsStore` write path, which preserves comments and formatting.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, UpdateGlobal, Window,
    WindowBounds, WindowKind, div, px, rgb, size,
};
use settings::Settings as _;
use settings::SettingsStore;
use terminal_core::terminal_settings::{CursorShape, TerminalSettings};
use util::ResultExt;

use crate::window_options;

pub struct SettingsPage {
    focus_handle: FocusHandle,
}

/// Opens the settings page in a floating window.
pub fn open_settings_window(cx: &mut App) {
    let mut options = window_options(gpui::Bounds::centered(None, size(px(480.), px(420.)), cx));
    options.kind = WindowKind::Floating;
    options.window_bounds = Some(WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        size(px(480.), px(420.)),
        cx,
    )));
    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| SettingsPage {
            focus_handle: cx.focus_handle(),
        });
        view.read(cx).focus_handle.clone().focus(window, cx);
        view
    })
    .log_err();
}

/// Applies an edit to the "terminal" section of user settings.json.
fn write_setting(
    cx: &mut App,
    update: impl FnOnce(&mut settings::TerminalSettingsContent) + Send + 'static,
) {
    let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    SettingsStore::update_global(cx, |store, _| {
        store.update_settings_file(fs, move |content, _| {
            let terminal = content
                .terminal
                .get_or_insert_with(settings::TerminalSettingsContent::default);
            update(terminal);
        });
    });
    // The file watcher will refresh windows once the write lands; refresh
    // immediately too so the page reflects the new value without delay.
    cx.refresh_windows();
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot the values up front so the settings borrow does not outlive
        // the `cx` borrow passed into each row.
        let (font_size, cursor_shape, blinking, option_as_meta, copy_on_select) = {
            let settings = TerminalSettings::get_global(cx);
            (
                settings.font_size.map(f32::from).unwrap_or(15.0),
                format!("{:?}", settings.cursor_shape).to_lowercase(),
                match settings.blinking {
                    settings::TerminalBlink::On => "on",
                    settings::TerminalBlink::Off => "off",
                    settings::TerminalBlink::TerminalControlled => "terminal-controlled",
                }
                .to_string(),
                settings.option_as_meta,
                settings.copy_on_select,
            )
        };

        div()
            .id("settings-page")
            .key_context("SettingsPage")
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .bg(rgb(0x14151a))
            .child(div().text_color(rgb(0xdcddde)).child("Terminal Settings"))
            .child(self.number_row(
                cx,
                "font-size",
                "Font size",
                format!("{font_size:.1} px"),
                |cx, delta| {
                    let current = TerminalSettings::get_global(cx)
                        .font_size
                        .map(f32::from)
                        .unwrap_or(15.0);
                    let next = (current + delta).max(1.0);
                    write_setting(cx, move |content| {
                        content.font_size = Some(settings::FontSize(next));
                    });
                },
            ))
            .child(
                self.cycle_row(cx, "cursor-shape", "Cursor shape", cursor_shape, |cx| {
                    let next = match TerminalSettings::get_global(cx).cursor_shape {
                        CursorShape::Block => settings::CursorShapeContent::Underline,
                        CursorShape::Underline => settings::CursorShapeContent::Bar,
                        CursorShape::Bar => settings::CursorShapeContent::Hollow,
                        CursorShape::Hollow => settings::CursorShapeContent::Block,
                    };
                    write_setting(cx, move |content| content.cursor_shape = Some(next));
                }),
            )
            .child(
                self.cycle_row(cx, "cursor-blink", "Cursor blinking", blinking, |cx| {
                    let next = match TerminalSettings::get_global(cx).blinking {
                        settings::TerminalBlink::Off => settings::TerminalBlink::TerminalControlled,
                        settings::TerminalBlink::TerminalControlled => settings::TerminalBlink::On,
                        settings::TerminalBlink::On => settings::TerminalBlink::Off,
                    };
                    write_setting(cx, move |content| content.blinking = Some(next));
                }),
            )
            .child(self.toggle_row(
                cx,
                "option-as-meta",
                "Option as meta",
                option_as_meta,
                |cx| {
                    let next = !TerminalSettings::get_global(cx).option_as_meta;
                    write_setting(cx, move |content| content.option_as_meta = Some(next));
                },
            ))
            .child(self.toggle_row(
                cx,
                "copy-on-select",
                "Copy on select",
                copy_on_select,
                |cx| {
                    let next = !TerminalSettings::get_global(cx).copy_on_select;
                    write_setting(cx, move |content| content.copy_on_select = Some(next));
                },
            ))
    }
}

impl SettingsPage {
    /// A labeled row with a `-`/`+` stepper that calls `apply(cx, delta)`.
    fn number_row(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        value: String,
        apply: impl Fn(&mut App, f32) + 'static + Clone,
    ) -> impl IntoElement {
        let minus = apply.clone();
        let plus = apply;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_grow_1().text_color(rgb(0x9aa4b2)).child(label))
            .child(div().text_color(rgb(0xdcddde)).child(value))
            .child(
                div()
                    .id(format!("{id}-minus"))
                    .px_1()
                    .border_1()
                    .border_color(rgb(0x2e333d))
                    .child("-")
                    .on_click(cx.listener(move |_, _, _, cx| minus(cx, -1.0))),
            )
            .child(
                div()
                    .id(format!("{id}-plus"))
                    .px_1()
                    .border_1()
                    .border_color(rgb(0x2e333d))
                    .child("+")
                    .on_click(cx.listener(move |_, _, _, cx| plus(cx, 1.0))),
            )
    }

    /// A labeled row whose button cycles `apply(cx)` through choices.
    fn cycle_row(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        value: String,
        apply: impl Fn(&mut App) + 'static + Clone,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_grow_1().text_color(rgb(0x9aa4b2)).child(label))
            .child(div().text_color(rgb(0xdcddde)).child(value))
            .child(
                div()
                    .id(format!("{id}-button"))
                    .px_1()
                    .border_1()
                    .border_color(rgb(0x2e333d))
                    .child("Cycle")
                    .on_click(cx.listener(move |_, _, _, cx| apply(cx))),
            )
    }

    /// A labeled row with an on/off toggle.
    fn toggle_row(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        apply: impl Fn(&mut App) + 'static + Clone,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_grow_1().text_color(rgb(0x9aa4b2)).child(label))
            .child(
                div()
                    .text_color(if enabled {
                        rgb(0x73d0ff)
                    } else {
                        rgb(0x9aa4b2)
                    })
                    .child(if enabled { "On" } else { "Off" }),
            )
            .child(
                div()
                    .id(format!("{id}-button"))
                    .px_1()
                    .border_1()
                    .border_color(rgb(0x2e333d))
                    .child("Toggle")
                    .on_click(cx.listener(move |_, _, _, cx| apply(cx))),
            )
    }
}
