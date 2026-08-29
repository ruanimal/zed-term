//! Settings page (WP4 §4.3): a floating window that edits the "terminal"
//! section of the user settings file through the standard `SettingsStore`
//! write path, which preserves comments and formatting.
//!
//! Zed's full `settings_ui` crate drags in editor/picker/project/workspace
//! and can't be reused here, so this page borrows its control design and
//! visual structure (section headers, labeled setting rows, dropdown /
//! toggle rows using theme colors and `ui` primitives) so it looks like Zed.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Render,
    SharedString, Styled as _, UpdateGlobal, Window, WindowBounds, WindowKind, size,
};
use settings::Settings as _;
use settings::SettingsStore;
use terminal_core::terminal_settings::TerminalSettings;
use theme::{ActiveTheme as _, ThemeRegistry};
use ui::prelude::*;
use ui::utils::{TRAFFIC_LIGHT_PADDING, platform_title_bar_height};
use ui::{
    Color, ContextMenu, Divider, DividerColor, DropdownMenu, IconName, IconSize, Label, LabelSize,
    Switch, ToggleState,
};
use util::ResultExt;

use crate::window_options;

/// A dropdown option: its label and the action to run when selected.
type OptionAction = Box<dyn Fn(&mut App) + 'static>;
type DropdownOptions = Vec<(&'static str, OptionAction)>;

/// Builds a stable element id from a setting title.
fn element_id_for(title: &'static str) -> gpui::ElementId {
    format!("setting-{title}").into()
}

/// Builds a `(&'static str, Box<dyn Fn(&mut App)>)` dropdown option. The
/// boxed closure coalesces to the `OptionAction` trait object so differing
/// closures can live in one `Vec`.
fn opt(label: &'static str, apply: impl Fn(&mut App) + 'static) -> (&'static str, OptionAction) {
    (label, Box::new(apply))
}

/// Settings window view.
pub struct SettingsPage {
    focus_handle: FocusHandle,
    /// Left-button state for the titlebar strip drag gesture.
    titlebar_mouse_down: std::cell::Cell<bool>,
}

/// Opens the settings page in a floating window.
pub fn open_settings_window(cx: &mut App) {
    let mut options = window_options(gpui::Bounds::centered(None, size(px(560.), px(560.)), cx));
    options.kind = WindowKind::Floating;
    options.window_bounds = Some(WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        size(px(560.), px(560.)),
        cx,
    )));
    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| SettingsPage {
            focus_handle: cx.focus_handle(),
            titlebar_mouse_down: std::cell::Cell::new(false),
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

/// Applies an edit to the top-level "theme" section of user settings.json,
/// mirroring Zed's theme picker: `theme_settings::set_theme` keeps the
/// static/dynamic selection intact.
fn write_theme(cx: &mut App, theme_name: SharedString, appearance: theme::Appearance) {
    let fs: Arc<dyn fs::Fs> = Arc::new(fs::RealFs::new(None, cx.background_executor().clone()));
    SettingsStore::update_global(cx, |store, _| {
        store.update_settings_file(fs, move |content, _| {
            theme_settings::set_theme(content, theme_name.to_string(), appearance, appearance);
        });
    });
    cx.refresh_windows();
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot the values up front so the settings borrow does not outlive
        // the `cx` borrow passed into each row.
        let (font_size, cursor_shape, blinking, option_as_meta, copy_on_select, alternate_scroll, scroll_multiplier, max_scroll_history, bell) = {
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
                format!("{:?}", settings.alternate_scroll).to_lowercase(),
                settings.scroll_multiplier,
                settings.max_scroll_history_lines.unwrap_or(10_000),
                format!("{:?}", settings.bell).to_lowercase(),
            )
        };
        let current_theme = theme_settings::ThemeSettings::get_global(cx)
            .theme
            .name(theme::SystemAppearance::global(cx).0)
            .0
            .to_string();
        let theme_names = ThemeRegistry::global(cx).list_names();
        let colors = cx.theme().colors();

        v_flex()
            .id("settings-page")
            .key_context("SettingsPage")
            .size_full()
            .bg(colors.panel_background)
            .child(self.render_title_bar(window, cx))
            .child(self.section_header(window, cx, "Appearance"))
            .child(self.theme_row(window, cx, current_theme, theme_names))
            .child(self.number_row(
                cx,
                "Font size",
                "The size of the terminal font.",
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
            .child(self.section_header(window, cx, "Cursor"))
            .child(self.enum_row(
                window,
                cx,
                "Cursor shape",
                "The shape of the terminal cursor.",
                cursor_shape,
                vec![
                    opt("block", |cx| write_setting(cx, |c| c.cursor_shape = Some(settings::CursorShapeContent::Block))),
                    opt("underline", |cx| write_setting(cx, |c| c.cursor_shape = Some(settings::CursorShapeContent::Underline))),
                    opt("bar", |cx| write_setting(cx, |c| c.cursor_shape = Some(settings::CursorShapeContent::Bar))),
                    opt("hollow", |cx| write_setting(cx, |c| c.cursor_shape = Some(settings::CursorShapeContent::Hollow))),
                ],
            ))
            .child(self.enum_row(
                window,
                cx,
                "Cursor blinking",
                "Whether the cursor blinks when the terminal is idle.",
                blinking,
                vec![
                    opt("on", |cx| write_setting(cx, |c| c.blinking = Some(settings::TerminalBlink::On))),
                    opt("off", |cx| write_setting(cx, |c| c.blinking = Some(settings::TerminalBlink::Off))),
                    opt("terminal-controlled", |cx| write_setting(cx, |c| c.blinking = Some(settings::TerminalBlink::TerminalControlled))),
                ],
            ))
            .child(self.section_header(window, cx, "Behavior"))
            .child(self.toggle_row(
                "Option as meta",
                "Use the option key as the meta key.",
                option_as_meta,
                |cx| {
                    let next = !TerminalSettings::get_global(cx).option_as_meta;
                    write_setting(cx, move |content| content.option_as_meta = Some(next));
                },
            ))
            .child(self.toggle_row(
                "Copy on select",
                "Automatically copy selected text to the clipboard.",
                copy_on_select,
                |cx| {
                    let next = !TerminalSettings::get_global(cx).copy_on_select;
                    write_setting(cx, move |content| content.copy_on_select = Some(next));
                },
            ))
            .child(self.enum_row(
                window,
                cx,
                "Alternate scroll",
                "Convert scroll events to key presses in the alternate screen.",
                alternate_scroll,
                vec![
                    opt("On", |cx| write_setting(cx, |c| c.alternate_scroll = Some(settings::AlternateScroll::On))),
                    opt("Off", |cx| write_setting(cx, |c| c.alternate_scroll = Some(settings::AlternateScroll::Off))),
                ],
            ))
            .child(self.enum_row(
                window,
                cx,
                "Bell",
                "What to do when the BEL character is printed.",
                bell,
                vec![
                    opt("system", |cx| write_setting(cx, |c| c.bell = Some(settings::TerminalBell::System))),
                    opt("off", |cx| write_setting(cx, |c| c.bell = Some(settings::TerminalBell::Off))),
                ],
            ))
            .child(self.section_header(window, cx, "Input & Scroll"))
            .child(self.number_row(
                cx,
                "Scroll multiplier",
                "The multiplier for scrolling with the mouse wheel.",
                format!("{scroll_multiplier:.2}x"),
                |cx, delta| {
                    let current = TerminalSettings::get_global(cx).scroll_multiplier;
                    let next = (current + delta).max(0.1);
                    write_setting(cx, move |content| content.scroll_multiplier = Some(next));
                },
            ))
            .child(self.number_row(
                cx,
                "Max scrollback lines",
                "The maximum number of lines to keep in scrollback.",
                format!("{max_scroll_history}"),
                |cx, delta| {
                    let current = TerminalSettings::get_global(cx)
                        .max_scroll_history_lines
                        .unwrap_or(10_000);
                    let next = (current as i64 + delta as i64).clamp(0, 100_000) as usize;
                    write_setting(cx, move |content| {
                        content.max_scroll_history_lines = Some(next);
                    });
                },
            ))
    }
}

impl SettingsPage {
    /// The settings window's self-drawn title bar, mirroring the main window:
    /// the traffic lights float over the strip's left reserved area, and the
    /// window title sits to their right so it is never obscured. Pressing and
    /// dragging the strip moves the window (the window uses a transparent
    /// system title bar via `window_options`).
    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let titlebar_height = platform_title_bar_height(window);
        div()
            .id("settings-title-bar")
            .h(titlebar_height)
            .flex_none()
            .pl(px(TRAFFIC_LIGHT_PADDING))
            .bg(cx.theme().colors().tab_bar_background)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, _| {
                    this.titlebar_mouse_down.set(true);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    this.titlebar_mouse_down.set(false);
                }),
            )
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                if this.titlebar_mouse_down.get() {
                    this.titlebar_mouse_down.set(false);
                    window.start_window_move();
                }
            }))
            .child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(
                        Label::new("ZedTerm — Settings")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
    }

    /// A section heading, mirroring Zed's `SettingsSectionHeader`.
    fn section_header(&self, _window: &mut Window, cx: &mut Context<Self>, label: &'static str) -> impl IntoElement {
        v_flex()
            .id(format!("section-{label}"))
            .w_full()
            .px_8()
            .gap_1p5()
            .child(
                Label::new(SharedString::new_static(label))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .buffer_font(cx),
            )
            .child(Divider::horizontal().color(DividerColor::BorderFaded))
    }

    /// A labeled setting row: title + description on the left, control on the
    /// right, with a divider below — Zed's `render_settings_item_layout`.
    fn row(
        &self,
        title: &'static str,
        description: &'static str,
        control: impl IntoElement,
    ) -> impl IntoElement {
        let id = element_id_for(title);
        v_flex()
            .id(id.clone())
            .group(format!("setting-item-{id}"))
            .w_full()
            .px_8()
            .py_2()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w_2_3()
                            .min_w_0()
                            .child(Label::new(SharedString::new_static(title)))
                            .child(
                                Label::new(SharedString::new_static(description))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(control),
            )
            .child(Divider::horizontal().color(DividerColor::BorderFaded))
    }

    /// A dropdown row with a `DropdownMenu` of `options` (label + apply).
    fn enum_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        value: String,
        options: DropdownOptions,
    ) -> impl IntoElement {
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for (option_label, apply) in options {
                menu = menu.entry(option_label, None, move |_window, cx| apply(cx));
            }
            menu
        });
        self.row(
            title,
            description,
            DropdownMenu::new(element_id_for(title), value, menu),
        )
    }

    /// A labeled row with a `-`/`+` stepper that calls `apply(cx, delta)`.
    fn number_row(
        &self,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        value: String,
        apply: impl Fn(&mut App, f32) + 'static + Clone,
    ) -> impl IntoElement {
        let id = element_id_for(title);
        let minus = apply.clone();
        let plus = apply;
        let control = h_flex()
            .items_center()
            .gap_2()
            .child(Label::new(value).color(Color::Muted))
            .child(
                IconButton::new(format!("{id}-minus"), IconName::SquareMinus)
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .on_click(cx.listener(move |_, _, _, cx| minus(cx, -1.0))),
            )
            .child(
                IconButton::new(format!("{id}-plus"), IconName::SquarePlus)
                    .icon_color(Color::Muted)
                    .icon_size(IconSize::Small)
                    .on_click(cx.listener(move |_, _, _, cx| plus(cx, 1.0))),
            );
        self.row(title, description, control)
    }

    /// A labeled row with a Zed-style `Switch`.
    fn toggle_row(
        &self,
        title: &'static str,
        description: &'static str,
        enabled: bool,
        apply: impl Fn(&mut App) + 'static + Clone,
    ) -> impl IntoElement {
        let id = element_id_for(title);
        let control = Switch::new(
            format!("{id}-switch"),
            if enabled {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            },
        )
        .on_click(move |_, _window, cx| apply(cx));
        self.row(title, description, control)
    }

    /// A dropdown row that lists the available themes and writes the selected
    /// one to settings.json via `theme_settings::set_theme`.
    fn theme_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        current: String,
        names: Vec<SharedString>,
    ) -> impl IntoElement {
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for name in names.iter() {
                let name = name.clone();
                menu = menu.entry(name.to_string(), None, move |_window, cx| {
                    let appearance = theme::SystemAppearance::global(cx).0;
                    write_theme(cx, name.clone(), appearance);
                });
            }
            menu
        });
        self.row(
            "Color theme",
            "Choose the color theme for the terminal and UI.",
            DropdownMenu::new(element_id_for("Color theme"), current, menu),
        )
    }
}
