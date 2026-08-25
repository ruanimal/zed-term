//! Lightweight view-state host for the terminal element.
//!
//! Replaces Zed's `TerminalView`, which was bound to `Workspace`/`Project`:
//! it owns scroll state and IME state and forwards input to the terminal.

use std::ops::Range as StdRange;

use gpui::{App, Context, Entity, FocusHandle, Pixels, ScrollWheelEvent};
use settings::Settings as _;
use terminal_core::{Terminal, TerminalBounds, terminal_settings::TerminalSettings};

struct ImeState {
    marked_text: String,
}

/// Per-terminal view state owned by a tab in the standalone terminal app.
pub struct TerminalTab {
    pub terminal: Entity<Terminal>,
    pub focus_handle: FocusHandle,
    /// Scroll offset of a block below the cursor; unused in standalone mode,
    /// kept so the terminal element can read it without branching on it.
    pub scroll_top: Pixels,
    ime_state: Option<ImeState>,
}

impl TerminalTab {
    pub fn new(terminal: Entity<Terminal>, focus_handle: FocusHandle) -> Self {
        Self {
            terminal,
            focus_handle,
            scroll_top: Pixels::ZERO,
            ime_state: None,
        }
    }

    pub fn entity(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    pub fn title(&self, cx: &App) -> String {
        self.terminal.read(cx).title(false)
    }

    pub(crate) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        self.terminal.update(cx, |term, cx| {
            term.scroll_wheel(
                event,
                TerminalSettings::get_global(cx).scroll_multiplier.max(0.01),
            )
        });
    }

    pub(crate) fn terminal_bounds(&self, cx: &App) -> TerminalBounds {
        self.terminal.read(cx).last_content().terminal_bounds
    }

    // IME

    pub(crate) fn set_marked_text(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return self.clear_marked_text(cx);
        }
        self.ime_state = Some(ImeState { marked_text: text });
        cx.notify();
    }

    /// Gets the current marked range (UTF-16).
    pub(crate) fn marked_text_range(&self) -> Option<StdRange<usize>> {
        self.ime_state
            .as_ref()
            .map(|state| 0..state.marked_text.encode_utf16().count())
    }

    pub(crate) fn marked_text(&self) -> Option<&str> {
        self.ime_state
            .as_ref()
            .map(|state| state.marked_text.as_str())
    }

    pub(crate) fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        if self.ime_state.is_some() {
            self.ime_state = None;
            cx.notify();
        }
    }

    /// Commits (sends) the given text to the PTY. Called by `InputHandler::replace_text_in_range`.
    pub(crate) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !text.is_empty() {
            self.terminal.update(cx, |term, _| {
                term.input(text.to_string().into_bytes());
            });
        }
    }
}