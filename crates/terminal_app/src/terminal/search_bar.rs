//! Minimal search bar for the terminal window.
//!
//! A plain input-handler-backed text field: typed characters update the
//! terminal's match highlights, Enter moves to the next match, and Escape
//! closes the bar. IME composition is not supported in this first pass.

use gpui::{
    App, Bounds, Entity, Focusable as _, InputHandler, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Pixels, RenderOnce, Styled as _, UTF16Selection,
    WeakEntity, Window, div,
};
use util::ResultExt;

use crate::terminal::TerminalTab;
use crate::window::TerminalWindowView;

#[derive(IntoElement)]
pub struct TerminalSearchBar {
    tab: Entity<TerminalTab>,
    window_view: WeakEntity<TerminalWindowView>,
    bounds: Bounds<Pixels>,
}

impl TerminalSearchBar {
    pub fn new(tab: Entity<TerminalTab>, window_view: WeakEntity<TerminalWindowView>) -> Self {
        Self {
            tab,
            window_view,
            bounds: Bounds::default(),
        }
    }
}

impl RenderOnce for TerminalSearchBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tab = self.tab.clone();
        let search_focus = tab.read(cx).search_focus_handle.clone();

        window.handle_input(
            &search_focus,
            TerminalSearchBarInput {
                tab: self.tab.clone(),
                bounds: self.bounds,
            },
            cx,
        );

        let (search_query, match_count) = {
            let tab = tab.read(cx);
            (
                tab.search_query.clone(),
                tab.terminal.read(cx).matches.len(),
            )
        };

        div()
            .id("terminal-search-bar")
            .track_focus(&search_focus)
            .h_8()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .bg(gpui::rgb(0x1c1f26))
            .border_b_1()
            .border_color(gpui::rgb(0x2e333d))
            .child(
                div()
                    .flex_none()
                    .text_color(gpui::rgb(0x9aa4b2))
                    .child("Search:"),
            )
            .child(
                div()
                    .flex_grow_1()
                    .text_color(gpui::rgb(0xdcddde))
                    .child(if search_query.is_empty() {
                        "Type to search the terminal buffer…".to_string()
                    } else {
                        format!("{match_count} matches")
                    }),
            )
            .on_key_down({
                let window_view = self.window_view.clone();
                move |event: &KeyDownEvent, window, cx| {
                    window_view
                        .update(cx, |this, cx| {
                            let Some(tab) = this.tabs.get(this.active_tab_index) else {
                                return;
                            };
                            match event.keystroke.key.as_ref() {
                                "escape" => {
                                    tab.update(cx, |tab, cx| {
                                        tab.close_search(cx);
                                    });
                                    this.focus_handle.clone().focus(window, cx);
                                }
                                "enter" => {
                                    let reverse = event.keystroke.modifiers.shift;
                                    tab.update(cx, |tab, cx| {
                                        tab.search_next(reverse, cx);
                                    });
                                }
                                _ => {}
                            }
                        })
                        .log_err();
                }
            })
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
        let length = self.tab.read(cx).search_query.encode_utf16().count();
        Some(UTF16Selection {
            range: 0..length,
            reversed: false,
        })
    }

    fn marked_text_range(
        &mut self,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn text_for_range(
        &mut self,
        range_utf16: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        let query = self.tab.read(cx).search_query.clone();
        let char_count = query.chars().count();
        let safe_range = range_utf16.start.min(char_count)..range_utf16.end.min(char_count);
        Some(
            query
                .chars()
                .skip(safe_range.start)
                .take(safe_range.end.saturating_sub(safe_range.start))
                .collect(),
        )
    }

    fn replace_text_in_range(
        &mut self,
        _replacement_range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.tab
            .update(cx, |tab, cx| tab.update_search_query(text.to_string(), cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<std::ops::Range<usize>>,
        _new_text: &str,
        _new_marked_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut App) {}

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
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
}