//! Search bar for the terminal.
//!
//! Zed implements terminal search through `BufferSearchBar`, which is built on
//! the `editor` crate and cannot be reused here: this app deliberately has no
//! editor/workspace dependency. The terminal half of Zed's search *is* reused —
//! `terminal_core` owns match finding, match activation and scrolling — so this
//! module only supplies the trimmed equivalent of the bar itself: an editable
//! query field, the match counter, previous/next navigation, the regex option
//! that Zed's terminal search exposes, and dismissal.
//!
//! Text editing goes through GPUI's `InputHandler` protocol (the same one the
//! settings page uses) so IME composition, selection and caret state stay in
//! sync with the platform.

use std::ops::Range;

use gpui::{
    App, Bounds, CursorStyle, Edges, Element, Entity, GlobalElementId, InputHandler,
    InspectorElementId, KeyDownEvent, LayoutId, MouseButton, Pixels, Position, RenderOnce, Style,
    Styled as _, UTF16Selection, Window, div, px,
};
use ui::prelude::*;
use ui::{IconButtonShape, Tooltip};

use crate::terminal::TerminalTab;
use crate::text_edit::substring_utf16;

#[derive(IntoElement)]
pub struct TerminalSearchBar {
    tab: Entity<TerminalTab>,
}

impl TerminalSearchBar {
    pub fn new(tab: Entity<TerminalTab>) -> Self {
        Self { tab }
    }
}

impl RenderOnce for TerminalSearchBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tab = self.tab.clone();
        let (query, selection, regex, match_count, active_match, invalid_regex) = {
            let tab = tab.read(cx);
            (
                tab.search_query.clone(),
                tab.search_selection.clone(),
                tab.search_regex,
                tab.terminal.read(cx).matches.len(),
                tab.active_match,
                tab.search_invalid_regex,
            )
        };
        let search_focus = tab.read(cx).search_focus_handle.clone();
        let focused = search_focus.is_focused(window);
        let colors = cx.theme().colors();

        let status = if query.is_empty() {
            None
        } else if invalid_regex {
            Some(
                Label::new("Invalid pattern")
                    .size(LabelSize::Small)
                    .color(Color::Error)
                    .into_any_element(),
            )
        } else if match_count == 0 {
            Some(
                Label::new("No matches")
                    .size(LabelSize::Small)
                    .color(Color::Error)
                    .into_any_element(),
            )
        } else {
            let index = active_match.map_or(0, |index| index + 1);
            Some(
                Label::new(format!("{index}/{match_count}"))
                    .size(LabelSize::Small)
                    .into_any_element(),
            )
        };

        let nav_button =
            |id: &'static str, icon: IconName, reverse: bool, tooltip: &'static str| {
                let tab = tab.clone();
                IconButton::new(id, icon)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .disabled(match_count == 0)
                    .tooltip(Tooltip::text(tooltip))
                    .on_click(move |_, _window, cx| {
                        tab.update(cx, |tab, cx| tab.search_next(reverse, cx));
                    })
            };

        let field_border = if invalid_regex {
            Color::Error.color(cx)
        } else {
            colors.border
        };

        div()
            .id("terminal-search-bar")
            .debug_selector(|| "terminal-search-bar".to_string())
            .track_focus(&search_focus)
            .h_9()
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .bg(colors.toolbar_background)
            .border_b_1()
            .border_color(colors.border)
            .child(
                div().mr_1().flex_none().child(
                    Icon::new(IconName::MagnifyingGlass)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div()
                    .id("terminal-search-query")
                    .debug_selector(|| "terminal-search-query".to_string())
                    .relative()
                    .flex_grow_1()
                    .min_w_0()
                    .h_6()
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .border_1()
                    .border_color(field_border)
                    .bg(colors.editor_background)
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, {
                        let search_focus = search_focus.clone();
                        move |_, window, cx| {
                            search_focus.focus(window, cx);
                        }
                    })
                    .child(search_field_content(&query, &selection, focused, cx))
                    .child(SearchFieldInput { tab: tab.clone() }),
            )
            .when_some(status, |this, status| this.child(status))
            .child(nav_button(
                "terminal-search-previous",
                IconName::ChevronLeft,
                true,
                "Select Previous Match (shift-enter)",
            ))
            .child(nav_button(
                "terminal-search-next",
                IconName::ChevronRight,
                false,
                "Select Next Match (enter)",
            ))
            .child({
                let tab = tab.clone();
                IconButton::new("terminal-search-regex", IconName::Regex)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .toggle_state(regex)
                    .tooltip(Tooltip::text("Use Regular Expressions"))
                    .on_click(move |_, _window, cx| {
                        tab.update(cx, |tab, cx| tab.toggle_search_regex(cx));
                    })
            })
            .child({
                let tab = tab.clone();
                IconButton::new("terminal-search-close", IconName::Close)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Close Search (escape)"))
                    .on_click(move |_, window, cx| {
                        tab.update(cx, |tab, cx| tab.close_search(window, cx));
                    })
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_ref();
                let shift = event.keystroke.modifiers.shift;
                let secondary = event.keystroke.modifiers.secondary();
                tab.update(cx, |tab, cx| {
                    let handled = match key {
                        "escape" => {
                            tab.close_search(window, cx);
                            true
                        }
                        "enter" => {
                            tab.search_next(shift, cx);
                            true
                        }
                        "left" => {
                            tab.move_search_cursor(true, shift, cx);
                            true
                        }
                        "right" => {
                            tab.move_search_cursor(false, shift, cx);
                            true
                        }
                        "home" => {
                            tab.move_search_to_boundary(true, shift, cx);
                            true
                        }
                        "end" => {
                            tab.move_search_to_boundary(false, shift, cx);
                            true
                        }
                        "backspace" => {
                            tab.delete_search_backward(cx);
                            true
                        }
                        "delete" => {
                            tab.delete_search_forward(cx);
                            true
                        }
                        "a" if secondary => {
                            tab.select_all_search_text(cx);
                            true
                        }
                        "c" if secondary => {
                            tab.copy_search_selection(cx);
                            true
                        }
                        "x" if secondary => {
                            tab.cut_search_selection(cx);
                            true
                        }
                        "v" if secondary => {
                            tab.paste_search_text(cx);
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        cx.stop_propagation();
                    }
                });
            })
    }
}

/// Renders the query with its caret and selection so the user can see what is
/// being matched. The IME marked range is part of the query text; the platform
/// owns the composition state and the caret already tracks it.
fn search_field_content(
    query: &str,
    selection: &Range<usize>,
    focused: bool,
    cx: &App,
) -> AnyElement {
    let text_color = cx.theme().colors().text;
    // A 2px solid bar in the text color stays legible against every theme; the
    // caret is only drawn while the field actually owns focus.
    let caret = || {
        div()
            .id("terminal-search-caret")
            .debug_selector(|| "terminal-search-caret".to_string())
            .flex_none()
            .w(px(2.))
            .h_4()
            .bg(text_color)
    };

    let mut content = h_flex()
        .items_center()
        .min_w_0()
        .flex_grow_1()
        .overflow_hidden();

    if query.is_empty() {
        // The placeholder alone gives no clue where typing lands, so the caret
        // leads it while the field is focused.
        if focused {
            content = content.child(caret());
        }
        return content
            .child(Label::new("Search…").color(Color::Muted))
            .into_any_element();
    }

    let text_length = query.encode_utf16().count();
    let selection_start = selection.start.min(text_length);
    let selection_end = selection.end.min(text_length).max(selection_start);

    if selection_start == selection_end {
        let before = substring_utf16(query, 0..selection_start);
        let after = substring_utf16(query, selection_start..text_length);
        content = content.child(Label::new(before));
        if focused {
            content = content.child(caret());
        }
        content = content.child(Label::new(after));
    } else {
        let before = substring_utf16(query, 0..selection_start);
        let selected = substring_utf16(query, selection_start..selection_end);
        let after = substring_utf16(query, selection_end..text_length);
        content = content
            .child(Label::new(before))
            .child(
                div()
                    .bg(cx.theme().colors().ghost_element_selected)
                    .child(Label::new(selected)),
            )
            .child(Label::new(after));
    }
    content.into_any_element()
}

/// Registers the input handler for the query field during paint, so the IME has
/// the field's real bounds to position its candidate window against.
struct SearchFieldInput {
    tab: Entity<TerminalTab>,
}

impl IntoElement for SearchFieldInput {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchFieldInput {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            position: Position::Absolute,
            inset: Edges::all(px(0.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let focus = self.tab.read(cx).search_focus_handle.clone();
        window.set_focus_handle(&focus, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.tab.read(cx).search_focus_handle.clone();
        window.handle_input(
            &focus,
            TerminalSearchBarInput {
                tab: self.tab.clone(),
                bounds,
            },
            cx,
        );
    }
}

struct TerminalSearchBarInput {
    tab: Entity<TerminalTab>,
    bounds: Bounds<Pixels>,
}

impl InputHandler for TerminalSearchBarInput {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let tab = self.tab.read(cx);
        let length = tab.search_query.encode_utf16().count();
        let start = tab.search_selection.start.min(length);
        let end = tab.search_selection.end.min(length).max(start);
        Some(UTF16Selection {
            range: start..end,
            reversed: tab.search_cursor < tab.search_anchor,
        })
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.tab.read(cx).search_marked_range.clone()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        Some(substring_utf16(
            &self.tab.read(cx).search_query,
            range_utf16,
        ))
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.tab.update(cx, |tab, cx| {
            tab.replace_search_text(replacement_range, text.to_string(), None, false, cx)
        });
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.tab.update(cx, |tab, cx| {
            tab.replace_search_text(
                range_utf16,
                new_text.to_string(),
                new_selected_range,
                true,
                cx,
            )
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.tab.update(cx, |tab, cx| {
            tab.search_marked_range = None;
            cx.notify();
        });
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.tab.update(cx, |tab, cx| {
            let length = tab.search_query.encode_utf16().count();
            let start = range_utf16.start.min(length);
            let end = range_utf16.end.min(length).max(start);
            tab.search_selection = start..end;
            tab.search_anchor = start;
            tab.search_cursor = end;
            tab.search_marked_range = None;
            cx.notify();
        });
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        Some(self.bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<usize> {
        None
    }

    fn element_bounds(&mut self, _window: &mut Window, _cx: &mut App) -> Option<Bounds<Pixels>> {
        Some(self.bounds)
    }

    fn text_length_utf16(&mut self, _window: &mut Window, cx: &mut App) -> Option<usize> {
        Some(self.tab.read(cx).search_query.encode_utf16().count())
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        true
    }
}
