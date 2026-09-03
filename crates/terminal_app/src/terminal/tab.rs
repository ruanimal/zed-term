//! Lightweight view-state host for the terminal element.
//!
//! Replaces Zed's `TerminalView`, which was bound to `Workspace`/`Project`:
//! it owns scroll state and IME state and forwards input to the terminal.

use std::{
    ops::Range as StdRange,
    time::{Duration, Instant},
};

use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Pixels, ScrollWheelEvent, Window};
use settings::Settings as _;
use terminal_core::{
    Event, MaybeNavigationTarget, Search, Terminal, TerminalBounds,
    terminal_settings::TerminalSettings,
};
use util::ResultExt;

use super::TerminalScrollHandle;

fn cursor_is_visible(
    mode: settings::TerminalBlink,
    terminal_blinking_enabled: bool,
    elapsed: Duration,
) -> bool {
    let should_blink = match mode {
        settings::TerminalBlink::Off => false,
        settings::TerminalBlink::On => true,
        settings::TerminalBlink::TerminalControlled => terminal_blinking_enabled,
    };
    !should_blink || (elapsed.as_millis() / 500).is_multiple_of(2)
}

/// Events emitted by a [`TerminalTab`] for the window to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalTabEvent {
    /// The pane's shell exited (typed `exit`, `Ctrl+D`, ...); the pane (and
    /// if it was its tab's only pane, the tab) should be closed.
    CloseTerminal,
    /// The terminal emitted BEL. `newly_notified` distinguishes the first
    /// unread bell from repeats so window attention is requested only once.
    Bell { newly_notified: bool },
}

struct ImeState {
    marked_text: String,
}

/// Per-terminal view state owned by a tab in the standalone terminal app.
pub struct TerminalTab {
    pub terminal: Entity<Terminal>,
    pub focus_handle: FocusHandle,
    /// Focus for the search bar while it is active.
    pub search_focus_handle: FocusHandle,
    /// Scroll offset of a block below the cursor; unused in standalone mode,
    /// kept so the terminal element can read it without branching on it.
    pub scroll_top: Pixels,
    pub scroll_handle: TerminalScrollHandle,
    ime_state: Option<ImeState>,
    /// Active search query and whether the search bar is showing.
    pub search_query: String,
    pub search_active: bool,
    /// Index into `terminal.matches` of the match the user last activated.
    pub active_match: Option<usize>,
    /// Sticky unread bell state, cleared when input is sent to this pane.
    has_bell: bool,
    terminal_blinking_enabled: bool,
    blink_started_at: Instant,
    _subscriptions: Vec<gpui::Subscription>,
}

impl TerminalTab {
    pub fn new(terminal: Entity<Terminal>, cx: &mut Context<Self>) -> Self {
        let scroll_handle = TerminalScrollHandle::new(terminal.read(cx));
        let mut this = Self {
            terminal,
            // Each terminal gets its own focus handle so pane focus is
            // distinguishable in split layouts.
            focus_handle: cx.focus_handle(),
            search_focus_handle: cx.focus_handle(),
            scroll_top: Pixels::ZERO,
            scroll_handle,
            ime_state: None,
            search_query: String::new(),
            search_active: false,
            active_match: None,
            has_bell: false,
            terminal_blinking_enabled: false,
            blink_started_at: Instant::now(),
            _subscriptions: Vec::new(),
        };

        // React to title changes instead of relying on the window heartbeat to
        // pick them up; the element reads `title()` every frame.
        this._subscriptions
            .push(
                cx.subscribe(&this.terminal, |this, terminal, event, cx| match event {
                    // PTY output arrives as a Wakeup event, not an `Entity::notify`;
                    // repaint immediately (this is the no-heartbeat path).
                    Event::Wakeup | Event::TitleChanged | Event::BreadcrumbsChanged => {
                        this.scroll_handle.update(terminal.read(cx));
                        cx.notify();
                    }
                    Event::BlinkChanged(enabled) => {
                        this.terminal_blinking_enabled = *enabled;
                        this.blink_started_at = Instant::now();
                        cx.notify();
                    }
                    Event::Open(target) => match target {
                        // Cmd-click on a hyperlink or path: hand it to the OS.
                        MaybeNavigationTarget::Url(url) => cx.open_url(url),
                        MaybeNavigationTarget::PathLike(target) => {
                            cx.open_url(&format!("file://{}", target.maybe_path));
                        }
                    },
                    Event::Bell => {
                        let newly_notified = !this.has_bell;
                        this.has_bell = true;
                        cx.notify();
                        cx.emit(TerminalTabEvent::Bell { newly_notified });
                    }
                    // The shell exited on its own (`exit`, `Ctrl+D`): tell the
                    // window to tear this pane down rather than leaving a dead
                    // shell accepting no input.
                    Event::CloseTerminal => cx.emit(TerminalTabEvent::CloseTerminal),
                    _ => {}
                }),
            );

        // Content updates notify the window so shell output repaints promptly
        // instead of waiting for the window heartbeat.
        this._subscriptions
            .push(cx.observe(&this.terminal, |this, terminal, cx| {
                this.scroll_handle.update(terminal.read(cx));
                cx.notify();
            }));

        this
    }

    pub fn entity(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    pub fn title(&self, cx: &App) -> String {
        self.terminal.read(cx).title(false)
    }

    pub(crate) fn has_bell(&self) -> bool {
        self.has_bell
    }

    pub(crate) fn cursor_visible(&self, cx: &App) -> bool {
        cursor_is_visible(
            TerminalSettings::get_global(cx).blinking,
            self.terminal_blinking_enabled,
            self.blink_started_at.elapsed(),
        )
    }

    pub(crate) fn apply_pending_scrollbar_offset(&mut self, cx: &mut Context<Self>) {
        let Some(target_offset) = self.scroll_handle.future_display_offset.take() else {
            return;
        };
        let current_offset = self.terminal.read(cx).last_content().display_offset;
        self.terminal.update(cx, |terminal, _| {
            if target_offset > current_offset {
                terminal.scroll_up_by(target_offset - current_offset);
            } else if target_offset < current_offset {
                terminal.scroll_down_by(current_offset - target_offset);
            }
        });
    }

    pub(crate) fn update_scrollbar(&self, cx: &App) {
        self.scroll_handle.update(self.terminal.read(cx));
    }

    pub(crate) fn clear_bell(&mut self, cx: &mut Context<Self>) {
        if self.has_bell {
            self.has_bell = false;
            cx.notify();
        }
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
            self.clear_bell(cx);
            self.terminal.update(cx, |term, _| {
                term.input(text.to_string().into_bytes());
            });
        }
    }

    // Terminal actions (§4.4). These forward to `Terminal`; copy and the
    // terminal's own paste path already talk to the system clipboard inside
    // `terminal_core` (OSC52 + `InternalEvent::Copy`).

    pub(crate) fn copy_selection(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |term, _| term.copy(None));
        cx.notify();
    }

    pub(crate) fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if !text.is_empty() {
            self.clear_bell(cx);
            self.terminal.update(cx, |term, _| term.paste(&text));
            cx.notify();
        }
    }

    pub(crate) fn clear_screen(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |term, _| term.clear());
        cx.notify();
    }

    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |term, _| term.select_all());
        cx.notify();
    }

    pub(crate) fn scroll(&mut self, scroll: ScrollAction, cx: &mut Context<Self>) {
        self.terminal.update(cx, |term, _| match scroll {
            ScrollAction::LineUp => term.scroll_line_up(),
            ScrollAction::LineDown => term.scroll_line_down(),
            ScrollAction::PageUp => term.scroll_page_up(),
            ScrollAction::PageDown => term.scroll_page_down(),
            // Half-page scrolls compose from the line-based delta API; the
            // alacritty grid clamps deltas at the top/bottom (and to zero on
            // the alt screen), so no extra mode handling is needed here.
            ScrollAction::HalfPageUp => term.scroll_up_by((term.viewport_lines() / 2).max(1)),
            ScrollAction::HalfPageDown => term.scroll_down_by((term.viewport_lines() / 2).max(1)),
            ScrollAction::Top => term.scroll_to_top(),
            ScrollAction::Bottom => term.scroll_to_bottom(),
        });
        cx.notify();
    }

    pub(crate) fn show_character_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.show_character_palette();
        cx.notify();
    }

    // Search (§4.4): a minimal search bar backed by `Terminal::find_matches`;
    // the element already renders `terminal.matches` as highlights.

    pub(crate) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_active = !self.search_active;
        if self.search_active {
            // Seed the query with the current selection, like Zed's search does
            let seed = self
                .terminal
                .read(cx)
                .last_content
                .selection_text
                .clone()
                .unwrap_or_default();
            self.search_query = seed.clone();
            self.run_search(seed, cx);
            self.search_focus_handle.clone().focus(window, cx);
        } else {
            self.search_query.clear();
            self.active_match = None;
            self.terminal.update(cx, |term, _| term.matches.clear());
            self.focus_handle.clone().focus(window, cx);
        }
        cx.notify();
    }

    /// Entry point for the search bar's input handler: replaces the whole
    /// query and re-runs the search.
    pub(crate) fn update_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_query = query.clone();
        self.active_match = None;
        self.run_search(query, cx);
        cx.notify();
    }

    /// Closes the search bar and clears highlights (bound to Escape). The
    /// caller moves focus back to the terminal.
    pub(crate) fn close_search(&mut self, cx: &mut Context<Self>) {
        if !self.search_active {
            return;
        }
        self.search_active = false;
        self.search_query.clear();
        self.active_match = None;
        self.terminal.update(cx, |term, _| term.matches.clear());
        cx.notify();
    }

    pub(crate) fn search_next(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let match_count = self.terminal.read(cx).matches.len();
        if match_count == 0 {
            return;
        }
        let current = self.active_match.unwrap_or(0);
        let next = if reverse {
            (current + match_count - 1) % match_count
        } else {
            (current + 1) % match_count
        };
        self.active_match = Some(next);
        self.terminal
            .update(cx, |term, _| term.activate_match(next));
        cx.notify();
    }

    fn run_search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            self.active_match = None;
            self.terminal.update(cx, |term, _| term.matches.clear());
            return;
        }
        let Some(searcher) = Search::new(&regex::escape(&query)) else {
            return;
        };
        let terminal = self.terminal.clone();
        let task = terminal.update(cx, |term, cx| term.find_matches(searcher, cx));
        cx.spawn(async move |this, cx| {
            let matches = task.await;
            this.update(cx, |tab, cx| {
                tab.active_match = (!matches.is_empty()).then_some(0);
                tab.terminal.update(cx, |term, _| term.matches = matches);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

impl EventEmitter<TerminalTabEvent> for TerminalTab {}

/// Which direction the `scroll` action family should scroll.
#[derive(Clone, Copy)]
pub(crate) enum ScrollAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, UpdateGlobal as _};
    use proptest::strategy::{Just, Strategy as _};
    use settings::SettingsStore;
    use terminal_core::TerminalBuilder;
    use util::paths::PathStyle;

    fn terminal_blink_strategy()
    -> impl proptest::strategy::Strategy<Value = settings::TerminalBlink> {
        proptest::prop_oneof![
            Just(settings::TerminalBlink::Off),
            Just(settings::TerminalBlink::On),
            Just(settings::TerminalBlink::TerminalControlled),
        ]
    }

    fn elapsed_duration_strategy() -> impl proptest::strategy::Strategy<Value = Duration> {
        proptest::prop_oneof![
            Just(Duration::ZERO),
            Just(Duration::from_nanos(499_999_999)),
            Just(Duration::from_millis(500)),
            Just(Duration::from_nanos(999_999_999)),
            Just(Duration::from_secs(1)),
            (0_u64..20_000, 0_u32..1_000_000).prop_map(
                |(milliseconds, submillisecond_nanoseconds)| {
                    Duration::from_millis(milliseconds)
                        + Duration::from_nanos(u64::from(submillisecond_nanoseconds))
                }
            ),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 128,
            failure_persistence: None,
            ..proptest::test_runner::Config::default()
        })]

        /// Feature: terminal-settings-completion, Property 8: Cursor visibility follows mode and 500ms phase
        /// **Validates: Requirements 3.2, 3.3, 3.4, 8.4**
        #[test]
        fn cursor_visibility_follows_mode_and_500_millisecond_phase(
            mode in terminal_blink_strategy(),
            terminal_blinking_enabled in proptest::bool::ANY,
            elapsed in elapsed_duration_strategy(),
        ) {
            let phase_is_visible =
                (elapsed.as_nanos() / Duration::from_millis(500).as_nanos()).is_multiple_of(2);
            let expected = match mode {
                settings::TerminalBlink::Off => true,
                settings::TerminalBlink::On => phase_is_visible,
                settings::TerminalBlink::TerminalControlled => {
                    !terminal_blinking_enabled || phase_is_visible
                }
            };

            proptest::prop_assert_eq!(
                cursor_is_visible(mode, terminal_blinking_enabled, elapsed),
                expected,
                "mode={:?}, terminal_blinking_enabled={}, elapsed={:?}",
                mode,
                terminal_blinking_enabled,
                elapsed,
            );
        }
    }

    #[gpui::test]
    async fn blink_changed_resets_phase_on_existing_pane(cx: &mut gpui::TestAppContext) {
        let pane = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_with(settings::TerminalSettingsContent::default)
                        .blinking = Some(settings::TerminalBlink::TerminalControlled);
                });
            });

            let settings = TerminalSettings::get_global(cx);
            let builder = TerminalBuilder::new_display_only(
                settings.cursor_shape,
                settings.alternate_scroll,
                settings.max_scroll_history_lines,
                1,
                cx.background_executor(),
                PathStyle::local(),
            );
            let terminal = cx.new(|cx| builder.subscribe(cx));
            cx.new(|cx| TerminalTab::new(terminal, cx))
        });

        let terminal = cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.blink_started_at = Instant::now() - Duration::from_millis(500);
            });
            assert!(pane.read(cx).cursor_visible(cx));
            pane.read(cx).terminal.clone()
        });

        let timer = cx.update(|cx| cx.background_executor().timer(Duration::from_millis(500)));
        cx.background_executor
            .advance_clock(Duration::from_millis(500));
        timer.await;

        cx.update(|cx| {
            terminal.update(cx, |_terminal, cx| {
                cx.emit(Event::BlinkChanged(true));
            });
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(pane.terminal_blinking_enabled);
            assert!(pane.cursor_visible(cx));
            assert!(pane.blink_started_at.elapsed() < Duration::from_millis(500));
        });
    }
}
