//! Window root view: a tab bar plus the active terminal element.
//!
//! Each window owns its own set of tabs; multiple windows share the app-level
//! keymap defined in `app.rs`.

use std::cmp::Ordering;
use std::time::Duration;

use collections::HashMap;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window,
    div, rgb,
};
use settings::Settings as _;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::TerminalBuilder;
use ui::{Tab, TabBar, TabPosition, Toggleable as _};
use util::paths::PathStyle;
use util::ResultExt;

use crate::{CloseTab, NewTab, NewWindow, NextTab, PreviousTab};
use crate::terminal::{TerminalElement, TerminalTab};

/// A window in the standalone terminal app.
pub struct TerminalWindowView {
    pub focus_handle: FocusHandle,
    tabs: Vec<Entity<TerminalTab>>,
    active_tab_index: usize,
}

impl TerminalWindowView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab_index: 0,
        };
        view.spawn_new_terminal(cx);
        view
    }

    /// Starts a PTY-backed shell in a new tab; the tab appears once the shell
    /// is up.
    fn spawn_new_terminal(&self, cx: &mut Context<Self>) {
        let focus = self.focus_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let Some(builder) = build_terminal(&cx).await else {
                return;
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|_| TerminalTab::new(terminal, focus.clone()));
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.tabs.push(tab);
                    this.active_tab_index = this.tabs.len() - 1;
                    cx.notify();
                })
                .log_err();
            });
        })
        .detach();
    }

    fn new_tab(&mut self, _: &NewTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_new_terminal(cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        self.tabs.remove(self.active_tab_index);
        if self.tabs.is_empty() {
            window.remove_window();
        } else {
            self.active_tab_index = self.active_tab_index.min(self.tabs.len() - 1);
            cx.notify();
        }
    }

    fn next_tab(&mut self, _: &NextTab, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab_index = (self.active_tab_index + 1) % self.tabs.len();
        cx.notify();
    }

    fn previous_tab(&mut self, _: &PreviousTab, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab_index =
            (self.active_tab_index + self.tabs.len() - 1) % self.tabs.len();
        cx.notify();
    }

    fn new_window(&mut self, _: &NewWindow, _window: &mut Window, cx: &mut Context<Self>) {
        crate::open_new_window(cx);
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let tab_count = self.tabs.len();
        TabBar::new("window-tabs").children(
            self.tabs
                .iter()
                .enumerate()
                .map(|(idx, tab)| {
                    let title = tab.read(cx).title(cx);
                    let position = if idx == 0 {
                        TabPosition::First
                    } else if idx == tab_count - 1 {
                        TabPosition::Last
                    } else {
                        TabPosition::Middle(Ordering::Equal)
                    };
                    Tab::new(idx.to_string())
                        .position(position)
                        .toggle_state(idx == self.active_tab_index)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active_tab_index = idx;
                            cx.notify();
                        }))
                        .child(title)
                })
                .collect::<Vec<_>>(),
        )
    }
}

impl Render for TerminalWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_tab = self.tabs.get(self.active_tab_index).cloned();
        let terminal = active_tab
            .as_ref()
            .map(|tab| tab.read(cx).terminal.clone());

        div()
            .id("terminal-window")
            .key_context("TerminalWindow")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x14151a))
            .child(self.render_tab_bar(cx))
            .child(
                terminal
                    .zip(active_tab)
                    .map(|(terminal, tab)| {
                        TerminalElement::new(
                            terminal,
                            tab,
                            self.focus_handle.clone(),
                            true,
                            true,
                        )
                        .into_any_element()
                    })
                    .unwrap_or_else(|| {
                        div()
                            .size_full()
                            .child("Starting terminal…")
                            .into_any_element()
                    }),
            )
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::new_window))
    }
}

/// Builds a PTY-backed terminal on the foreground executor.
async fn build_terminal(cx: &gpui::AsyncApp) -> Option<TerminalBuilder> {
    let settings = cx.update(|cx| TerminalSettings::get_global(cx).clone());
    let builder = cx.update(|cx| {
        TerminalBuilder::new(
            None,
            settings.shell.clone(),
            HashMap::<String, String>::default(),
            settings.cursor_shape,
            settings.alternate_scroll,
            settings.max_scroll_history_lines,
            settings.path_hyperlink_regexes.clone(),
            Duration::from_millis(settings.path_hyperlink_timeout_ms),
            false,
            0,
            None,
            cx,
            Vec::new(),
            PathStyle::local(),
        )
    });
    builder.await.ok()
}