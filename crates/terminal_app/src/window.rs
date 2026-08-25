//! Window root view for the standalone terminal app.
//!
//! Hosts a single terminal tab for now; WP3 replaces this with a tab bar and
//! multi-window management.

use collections::HashMap;
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, IntoElement, ParentElement as _, Render,
    Styled as _, WeakEntity, Window, div, rgb,
};
use settings::Settings as _;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::TerminalBuilder;
use util::paths::PathStyle;
use util::ResultExt;

use crate::terminal::{TerminalElement, TerminalTab};

pub struct TerminalWindowView {
    focus_handle: FocusHandle,
    terminal_tab: Option<Entity<TerminalTab>>,
}

impl TerminalWindowView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let view = Self {
            focus_handle,
            terminal_tab: None,
        };

        // Build the PTY-backed terminal on the foreground executor, then
        // attach it to the window once the shell is up.
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
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
            let builder = builder.await;

            if let Ok(builder) = builder {
                let terminal = cx.new(|cx| builder.subscribe(cx));
                let tab = cx.new(|cx| TerminalTab::new(terminal, cx.focus_handle()));
                cx.update(|cx| {
                    this.update(cx, |this, cx| {
                        this.terminal_tab = Some(tab);
                        cx.notify();
                    })
                })
                .log_err();
            }
        })
        .detach();

        view
    }
}

impl Render for TerminalWindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tab) = &self.terminal_tab {
            let terminal = tab.read(cx).terminal.clone();
            TerminalElement::new(
                terminal,
                tab.clone(),
                self.focus_handle.clone(),
                true,
                true,
            )
            .into_any_element()
        } else {
            div()
                .size_full()
                .bg(rgb(0x14151a))
                .child("Starting terminal…")
                .into_any_element()
        }
    }
}