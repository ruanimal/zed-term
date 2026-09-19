//! Lightweight view-state host for the terminal element.
//!
//! Replaces Zed's `TerminalView`, which was bound to `Workspace`/`Project`:
//! it owns scroll state and IME state and forwards input to the terminal.

use std::{
    ops::Range as StdRange,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Pixels, Point,
    ScrollWheelEvent, Task, Window,
};
use settings::Settings as _;
use terminal_core::{
    Event, MaybeNavigationTarget, Range as MatchRange, Search, Terminal, TerminalBounds,
    terminal_settings::TerminalSettings,
};
use url::Url;
use util::ResultExt;
use util::paths::PathWithPosition;

use super::TerminalScrollHandle;
use crate::text_edit::{
    next_utf16_boundary, previous_utf16_boundary, replace_utf16_range, substring_utf16,
};
use crate::transfer_ui::TransferUiState;

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
    /// Whether the query is interpreted as a regular expression.
    pub search_regex: bool,
    /// UTF-16 selection in the search field, plus the anchor/cursor and the
    /// IME marked range that the platform input handler reads back.
    pub(crate) search_selection: StdRange<usize>,
    pub(crate) search_anchor: usize,
    pub(crate) search_cursor: usize,
    pub(crate) search_marked_range: Option<StdRange<usize>>,
    /// Index into `terminal.matches` of the match the user last activated.
    pub active_match: Option<usize>,
    /// Set when regex mode is on and the query does not compile; the bar shows
    /// this instead of "No matches" so a broken pattern is distinguishable from
    /// a pattern that simply has no hits.
    pub(crate) search_invalid_regex: bool,
    /// Bumped for every search so results from a superseded query are dropped.
    search_generation: u64,
    /// In-flight scan; dropping it cancels work the user has already replaced.
    search_task: Option<Task<()>>,
    /// Sticky unread bell state, cleared when input is sent to this pane.
    has_bell: bool,
    terminal_blinking_enabled: bool,
    blink_started_at: Instant,
    /// Transfer overlay state + dialog driving (§6 of docs/TRANSFER_EXTENSION.md).
    pub(crate) transfer_ui: TransferUiState,
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
            search_regex: false,
            search_selection: 0..0,
            search_anchor: 0,
            search_cursor: 0,
            search_marked_range: None,
            active_match: None,
            search_invalid_regex: false,
            search_generation: 0,
            search_task: None,
            has_bell: false,
            terminal_blinking_enabled: false,
            blink_started_at: Instant::now(),
            transfer_ui: TransferUiState::default(),
            _subscriptions: Vec::new(),
        };

        // React to title changes instead of relying on the window heartbeat to
        // pick them up; the element reads `title()` every frame.
        this._subscriptions
            .push(
                cx.subscribe(&this.terminal, |this, terminal, event, cx| match event {
                    // PTY output arrives as a Wakeup event, not an `Entity::notify`;
                    // repaint immediately (this is the no-heartbeat path).
                    Event::Wakeup => {
                        this.scroll_handle.update(terminal.read(cx));
                        // New output invalidates both the match positions and the
                        // active match, so an open search has to rescan.
                        this.refresh_search_for_output(cx);
                        cx.notify();
                    }
                    Event::TitleChanged | Event::BreadcrumbsChanged => {
                        this.scroll_handle.update(terminal.read(cx));
                        cx.notify();
                    }
                    Event::BlinkChanged(enabled) => {
                        this.terminal_blinking_enabled = *enabled;
                        this.blink_started_at = Instant::now();
                        cx.notify();
                    }
                    Event::Open(target) => open_navigation_target(target, cx),
                    // Dropping a hover (modifier release, pointer leaving the
                    // link) only emits this event; the terminal itself is not
                    // notified, so the repaint that clears the pointing-hand
                    // cursor has to be requested here.
                    Event::NewNavigationTarget(_) => cx.notify(),
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
                    Event::Transfer(event) => {
                        this.transfer_ui.handle(event.clone(), &terminal, cx);
                        cx.notify();
                    }
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

    pub(crate) fn transfer_ui_state(&self) -> &TransferUiState {
        &self.transfer_ui
    }

    /// Resolves the link under a pane-local position, so the context menu acts
    /// on the cell that was right-clicked rather than on the focused pane.
    pub(crate) fn navigation_target_at(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<MaybeNavigationTarget> {
        self.terminal
            .update(cx, |terminal, _| terminal.navigation_target_at(position))
    }

    pub(crate) fn cursor_blinking_enabled(&self, cx: &App) -> bool {
        match TerminalSettings::get_global(cx).blinking {
            settings::TerminalBlink::Off => false,
            settings::TerminalBlink::On => true,
            settings::TerminalBlink::TerminalControlled => self.terminal_blinking_enabled,
        }
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

    // Search (§4.4). `terminal_core` owns matching, selection and scrolling;
    // this is the view state for the search bar's text field plus the
    // navigation that Zed's search bar performs over a searchable item.

    pub(crate) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_active {
            // `cmd-f` on an open bar re-focuses and re-selects the query instead
            // of dismissing it, matching Zed's `FocusSearch`.
            self.select_all_search_text(cx);
            self.search_focus_handle.clone().focus(window, cx);
            cx.notify();
            return;
        }
        self.search_active = true;
        // Seed the query with the current selection, like Zed's search does.
        let seed = self
            .terminal
            .read(cx)
            .last_content
            .selection_text
            .clone()
            .unwrap_or_default();
        self.search_query = seed;
        self.select_all_search_text(cx);
        self.run_search(true, cx);
        self.search_focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    /// Closes the search bar, clears the highlights and returns focus to the
    /// terminal (bound to Escape).
    pub(crate) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_active {
            return;
        }
        self.search_active = false;
        self.search_query.clear();
        self.search_selection = 0..0;
        self.search_anchor = 0;
        self.search_cursor = 0;
        self.search_marked_range = None;
        self.active_match = None;
        self.search_invalid_regex = false;
        // Invalidate in-flight results so a scan started before the close
        // cannot repopulate the highlights afterwards.
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_task = None;
        self.terminal.update(cx, |term, _| term.matches.clear());
        self.focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    /// Dismisses a finished-transfer message (or a transient hint) and
    /// returns focus to the terminal. A running transfer is cancelled with
    /// the same button, not dismissed.
    pub(crate) fn dismiss_transfer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_ui.dismiss();
        self.focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    /// Replaces the selected range of the query with `text`, moving the caret
    /// to the end of the insertion. This is the single entry point for both
    /// platform text input and the editing keys.
    pub(crate) fn replace_search_text(
        &mut self,
        replacement_range: Option<StdRange<usize>>,
        text: String,
        new_selected_range: Option<StdRange<usize>>,
        mark_text: bool,
        cx: &mut Context<Self>,
    ) {
        let current_text = self.search_query.clone();
        let range = replacement_range
            .or_else(|| self.search_marked_range.clone())
            .unwrap_or_else(|| self.search_selection.clone());
        let range_start = range.start.min(current_text.encode_utf16().count());
        let (updated_text, cursor) = replace_utf16_range(&current_text, range, &text);
        self.search_query = updated_text;
        self.search_marked_range = mark_text
            .then_some(range_start..cursor)
            .filter(|range| !range.is_empty());
        self.search_selection = new_selected_range
            .map(|range| range_start + range.start..range_start + range.end)
            .unwrap_or(cursor..cursor);
        self.search_anchor = self.search_selection.start;
        self.search_cursor = self.search_selection.end;
        self.run_search(true, cx);
    }

    pub(crate) fn move_search_cursor(
        &mut self,
        move_left: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let target = if !extend_selection && !self.search_selection.is_empty() {
            if move_left {
                self.search_selection.start
            } else {
                self.search_selection.end
            }
        } else if move_left {
            previous_utf16_boundary(&self.search_query, self.search_cursor)
        } else {
            next_utf16_boundary(&self.search_query, self.search_cursor)
        };
        self.update_search_cursor(target, extend_selection, cx);
    }

    pub(crate) fn move_search_to_boundary(
        &mut self,
        move_to_start: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let target = if move_to_start {
            0
        } else {
            self.search_query.encode_utf16().count()
        };
        self.update_search_cursor(target, extend_selection, cx);
    }

    fn update_search_cursor(
        &mut self,
        cursor: usize,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let length = self.search_query.encode_utf16().count();
        let cursor = cursor.min(length);
        if extend_selection {
            self.search_cursor = cursor;
            self.search_selection = if self.search_anchor <= cursor {
                self.search_anchor..cursor
            } else {
                cursor..self.search_anchor
            };
        } else {
            self.search_anchor = cursor;
            self.search_cursor = cursor;
            self.search_selection = cursor..cursor;
        }
        self.search_marked_range = None;
        cx.notify();
    }

    pub(crate) fn delete_search_backward(&mut self, cx: &mut Context<Self>) {
        self.search_marked_range = None;
        if self.search_selection.is_empty() {
            let previous = previous_utf16_boundary(&self.search_query, self.search_cursor);
            if previous == self.search_cursor {
                return;
            }
            self.search_selection = previous..self.search_cursor;
        }
        self.replace_search_text(None, String::new(), None, false, cx);
    }

    pub(crate) fn delete_search_forward(&mut self, cx: &mut Context<Self>) {
        self.search_marked_range = None;
        if self.search_selection.is_empty() {
            let next = next_utf16_boundary(&self.search_query, self.search_cursor);
            if next == self.search_cursor {
                return;
            }
            self.search_selection = self.search_cursor..next;
        }
        self.replace_search_text(None, String::new(), None, false, cx);
    }

    pub(crate) fn select_all_search_text(&mut self, cx: &mut Context<Self>) {
        let length = self.search_query.encode_utf16().count();
        self.search_selection = 0..length;
        self.search_anchor = 0;
        self.search_cursor = length;
        self.search_marked_range = None;
        cx.notify();
    }

    pub(crate) fn copy_search_selection(&self, cx: &mut App) {
        if self.search_selection.is_empty() {
            return;
        }
        let text = substring_utf16(&self.search_query, self.search_selection.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    pub(crate) fn cut_search_selection(&mut self, cx: &mut Context<Self>) {
        if self.search_selection.is_empty() {
            return;
        }
        self.copy_search_selection(cx);
        self.replace_search_text(None, String::new(), None, false, cx);
    }

    pub(crate) fn paste_search_text(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| text.replace(['\n', '\r'], " "))
        else {
            return;
        };
        self.replace_search_text(None, text, None, false, cx);
    }

    pub(crate) fn toggle_search_regex(&mut self, cx: &mut Context<Self>) {
        self.search_regex = !self.search_regex;
        self.run_search(true, cx);
    }

    /// Moves to the next (or previous) match, wrapping around the ends.
    pub(crate) fn search_next(&mut self, reverse: bool, cx: &mut Context<Self>) {
        let match_count = self.terminal.read(cx).matches.len();
        if match_count == 0 {
            return;
        }
        let next = match self.active_match {
            Some(current) if reverse => (current + match_count - 1) % match_count,
            Some(current) => (current + 1) % match_count,
            None if reverse => match_count - 1,
            None => 0,
        };
        self.activate_search_match(next, cx);
    }

    fn activate_search_match(&mut self, index: usize, cx: &mut Context<Self>) {
        self.active_match = Some(index);
        self.terminal
            .update(cx, |term, _| term.activate_match(index));
        cx.notify();
    }

    fn refresh_search_for_output(&mut self, cx: &mut Context<Self>) {
        if self.search_active && !self.search_query.is_empty() {
            // New output repositions the matches but must not pull the viewport
            // around, so the rescan refreshes highlights without re-activating.
            self.run_search(false, cx);
        }
    }

    fn run_search(&mut self, activate: bool, cx: &mut Context<Self>) {
        // Every search supersedes the previous one: bump the generation and
        // drop the earlier task so a slow scan cannot overwrite newer results.
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_task = None;
        let generation = self.search_generation;

        let searcher = if self.search_query.is_empty()
            || is_bare_regex_dot(&self.search_query, self.search_regex)
        {
            self.search_invalid_regex = false;
            None
        } else {
            match search_for_query(&self.search_query, self.search_regex) {
                Some(searcher) => {
                    self.search_invalid_regex = false;
                    Some(searcher)
                }
                // The query is not a valid regular expression.
                None => {
                    self.search_invalid_regex = true;
                    self.active_match = None;
                    self.terminal.update(cx, |term, _| term.matches.clear());
                    cx.notify();
                    return;
                }
            }
        };

        let Some(searcher) = searcher else {
            self.active_match = None;
            self.terminal.update(cx, |term, _| term.matches.clear());
            cx.notify();
            return;
        };

        let terminal = self.terminal.clone();
        let task = terminal.update(cx, |term, cx| term.find_matches(searcher, cx));
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let matches = task.await;
            this.update(cx, |tab, cx| {
                if tab.search_generation != generation {
                    return;
                }
                tab.apply_search_matches(matches, activate, cx);
            })
            .log_err();
        }));
    }

    fn apply_search_matches(
        &mut self,
        matches: Vec<MatchRange>,
        activate: bool,
        cx: &mut Context<Self>,
    ) {
        // Prefer the match nearest where the caret/selection already is, the
        // way Zed's searchable items pick the active match.
        let selection_head = self.terminal.read(cx).selection_head;
        let active_index = active_match_index(selection_head, &matches);
        self.active_match = active_index;
        self.terminal.update(cx, |term, _| {
            term.matches = matches;
            if activate && let Some(index) = active_index {
                term.activate_match(index);
            }
        });
        cx.notify();
    }
}

/// A bare `.` in regex mode would match every character on screen, so Zed
/// treats it as "no query" instead of searching; keep that guard.
fn is_bare_regex_dot(query: &str, regex: bool) -> bool {
    regex && query == "."
}

/// Builds the matcher for a search query.
///
/// Zed's terminal search only exposes the regex option and matches
/// case-insensitively, so case-insensitivity is baked into the pattern and a
/// literal query is escaped before it becomes a regular expression.
fn search_for_query(query: &str, regex: bool) -> Option<Search> {
    let pattern = if regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    Search::new(&format!("(?i){pattern}"))
}

/// Picks the match to activate once a scan finishes, mirroring Zed's
/// `TerminalView::active_match_index`: keep the match that contains or starts
/// after the selection head, falling back to the last match (nearest the
/// cursor) when there is no selection or nothing follows it.
fn active_match_index(
    selection_head: Option<terminal_core::Point>,
    matches: &[MatchRange],
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let Some(selection_head) = selection_head else {
        return Some(matches.len() - 1);
    };
    matches
        .iter()
        .position(|search_match| {
            search_match.contains(selection_head) || search_match.start() > selection_head
        })
        .or(Some(matches.len() - 1))
}

impl EventEmitter<TerminalTabEvent> for TerminalTab {}

/// Opens a terminal link (URL or file path) with the platform default handler.
pub(crate) fn open_navigation_target(target: &MaybeNavigationTarget, cx: &mut App) {
    match target {
        MaybeNavigationTarget::Url(url) => cx.open_url(url),
        MaybeNavigationTarget::PathLike(target) => {
            // `file://` matches are normalized into plain paths before they get
            // here, so `working_directory` is the only thing left to resolve.
            match resolve_path_like_target(&target.maybe_path, target.working_directory.as_deref())
            {
                Some(path) => match Url::from_file_path(&path) {
                    Ok(url) => cx.open_url(url.as_str()),
                    Err(()) => log::warn!("Not opening {path:?}: not an absolute file path"),
                },
                None => log::warn!(
                    "Not opening {:?}: no working directory to resolve it against, or it does not exist",
                    target.maybe_path
                ),
            }
        }
    }
}

/// Link text placed on the clipboard by the context menu's "Copy Link" entry.
pub(crate) fn navigation_target_text(target: &MaybeNavigationTarget) -> String {
    match target {
        MaybeNavigationTarget::Url(url) => url.clone(),
        MaybeNavigationTarget::PathLike(target) => target.maybe_path.clone(),
    }
}

/// Resolves a path-like link into an existing file-system path.
///
/// Terminal output carries `path:line:column` suffixes (the path regex appends
/// the line and column it captured), so the position suffix is stripped first
/// and only the file itself is opened. Relative paths resolve against the
/// working directory recorded for the matched line. A missing path yields
/// `None`, so clicking a stale link does not surface a system error dialog.
fn resolve_path_like_target(maybe_path: &str, working_directory: Option<&Path>) -> Option<PathBuf> {
    let PathWithPosition { path, .. } = PathWithPosition::parse_str(maybe_path);
    let path = if path.is_absolute() {
        path
    } else {
        working_directory?.join(path)
    };

    path.exists().then_some(path)
}

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
    use terminal_core::{PathLikeTarget, TerminalBuilder};
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

    /// Builds a display-only pane, mirroring the terminal launched at startup
    /// but without a PTY.
    fn test_pane(cx: &mut App) -> Entity<TerminalTab> {
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
    }

    #[test]
    fn path_like_links_resolve_against_the_recorded_working_directory() {
        let working_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest_path = working_directory.join("Cargo.toml");

        // The position suffix the path regex appends is not part of the file.
        assert_eq!(
            resolve_path_like_target("Cargo.toml:12:3", Some(working_directory)),
            Some(manifest_path.clone())
        );
        assert_eq!(
            resolve_path_like_target("Cargo.toml(12,3)", Some(working_directory)),
            Some(manifest_path.clone())
        );
        assert_eq!(
            resolve_path_like_target("src/app.rs:1", Some(working_directory)),
            Some(working_directory.join("src/app.rs"))
        );

        // Absolute paths ignore the working directory.
        assert_eq!(
            resolve_path_like_target(
                manifest_path.to_str().expect("manifest path is UTF-8"),
                Some(Path::new("/nonexistent-directory"))
            ),
            Some(manifest_path.clone())
        );

        // Nothing to open when the file is gone or the line has no directory.
        assert_eq!(
            resolve_path_like_target("missing/ghost.rs:1:1", Some(working_directory)),
            None
        );
        assert_eq!(resolve_path_like_target("Cargo.toml:1", None), None);
    }

    #[test]
    fn copied_link_text_matches_what_the_terminal_shows() {
        assert_eq!(
            navigation_target_text(&MaybeNavigationTarget::Url("https://zed.dev/".to_string())),
            "https://zed.dev/"
        );
        assert_eq!(
            navigation_target_text(&MaybeNavigationTarget::PathLike(PathLikeTarget {
                maybe_path: "src/main.rs:12:3".to_string(),
                working_directory: None,
            })),
            "src/main.rs:12:3"
        );
    }

    #[gpui::test]
    async fn opening_links_hands_existing_paths_to_the_os(cx: &mut gpui::TestAppContext) {
        let working_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // A stale match must not reach the OS, which would surface an error.
        cx.update(|cx| {
            open_navigation_target(
                &MaybeNavigationTarget::PathLike(PathLikeTarget {
                    maybe_path: "missing/ghost.rs:1:1".to_string(),
                    working_directory: Some(working_directory.clone()),
                }),
                cx,
            );
        });
        assert_eq!(cx.opened_url(), None);

        cx.update(|cx| {
            open_navigation_target(
                &MaybeNavigationTarget::PathLike(PathLikeTarget {
                    maybe_path: "Cargo.toml:3:1".to_string(),
                    working_directory: Some(working_directory.clone()),
                }),
                cx,
            );
        });

        let expected = Url::from_file_path(working_directory.join("Cargo.toml"))
            .expect("manifest path is absolute");
        assert_eq!(cx.opened_url().as_deref(), Some(expected.as_str()));
    }

    #[gpui::test]
    async fn open_events_reach_the_os_handler(cx: &mut gpui::TestAppContext) {
        let (_pane, terminal) = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            let pane = test_pane(cx);
            let terminal = pane.read(cx).terminal.clone();
            (pane, terminal)
        });

        cx.update(|cx| {
            terminal.update(cx, |_terminal, cx| {
                cx.emit(Event::Open(MaybeNavigationTarget::Url(
                    "https://zed.dev/".to_string(),
                )));
            });
        });
        assert_eq!(cx.opened_url().as_deref(), Some("https://zed.dev/"));

        let working_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        cx.update(|cx| {
            terminal.update(cx, |_terminal, cx| {
                cx.emit(Event::Open(MaybeNavigationTarget::PathLike(
                    PathLikeTarget {
                        maybe_path: "Cargo.toml:3:1".to_string(),
                        working_directory: Some(working_directory.clone()),
                    },
                )));
            });
        });

        let expected = Url::from_file_path(working_directory.join("Cargo.toml"))
            .expect("manifest path is absolute");
        assert_eq!(cx.opened_url().as_deref(), Some(expected.as_str()));
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

    fn match_range(start: (i32, usize), end: (i32, usize)) -> MatchRange {
        MatchRange::new(
            terminal_core::Point::new(start.0, start.1),
            terminal_core::Point::new(end.0, end.1),
        )
    }

    /// Builds a pane whose grid already contains `output`.
    fn pane_with_output(cx: &mut gpui::TestAppContext, output: &str) -> Entity<TerminalTab> {
        let pane = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            test_pane(cx)
        });
        let terminal = cx.update(|cx| pane.read(cx).terminal.clone());
        cx.update(|cx| {
            terminal.update(cx, |terminal, cx| {
                terminal.write_output(output.as_bytes(), cx)
            });
        });
        cx.run_until_parked();
        pane
    }

    /// Sets the query through the input path and waits for the scan to land.
    fn run_query(pane: &Entity<TerminalTab>, query: &str, cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.search_active = true;
                pane.replace_search_text(None, query.to_string(), None, false, cx);
            });
        });
        cx.run_until_parked();
    }

    fn match_count(pane: &Entity<TerminalTab>, cx: &mut gpui::TestAppContext) -> usize {
        cx.update(|cx| pane.read(cx).terminal.read(cx).matches.len())
    }

    #[test]
    fn active_match_index_prefers_the_match_at_or_after_the_selection_head() {
        let matches = vec![
            match_range((0, 0), (0, 3)),
            match_range((1, 0), (1, 3)),
            match_range((2, 0), (2, 3)),
        ];

        // No selection: the last match is the one nearest the cursor.
        assert_eq!(active_match_index(None, &matches), Some(2));
        // A head inside a match keeps that match.
        assert_eq!(
            active_match_index(Some(terminal_core::Point::new(0, 1)), &matches),
            Some(0)
        );
        // The head at a match's end still counts as inside it.
        assert_eq!(
            active_match_index(Some(terminal_core::Point::new(1, 3)), &matches),
            Some(1)
        );
        // Past the end of a match: the next one.
        assert_eq!(
            active_match_index(Some(terminal_core::Point::new(1, 4)), &matches),
            Some(2)
        );
        // Nothing follows: fall back to the last match.
        assert_eq!(
            active_match_index(Some(terminal_core::Point::new(3, 0)), &matches),
            Some(2)
        );
        assert_eq!(
            active_match_index(Some(terminal_core::Point::new(0, 0)), &[]),
            None
        );
    }

    #[test]
    fn literal_queries_are_escaped_and_regex_queries_are_not() {
        // A literal query is escaped, so an unbalanced bracket is still valid.
        assert!(search_for_query("a(b", false).is_some());
        // The same query in regex mode is an invalid pattern, not a panic.
        assert!(search_for_query("a(b", true).is_none());
        assert!(search_for_query("a.b", true).is_some());
    }

    #[test]
    fn a_bare_regex_dot_is_treated_as_no_query() {
        // `.` would otherwise match every cell on screen.
        assert!(is_bare_regex_dot(".", true));
        assert!(!is_bare_regex_dot(".", false));
        assert!(!is_bare_regex_dot("..", true));
    }

    #[gpui::test]
    async fn typed_characters_append_to_the_query(cx: &mut gpui::TestAppContext) {
        // Regression: the old input handler replaced the whole query with each
        // keystroke, so only single-character queries were reachable.
        let pane = pane_with_output(cx, "alpha\r\nbeta\r\n");
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.search_active = true;
                for character in ["b", "e", "t", "a"] {
                    pane.replace_search_text(None, character.to_string(), None, false, cx);
                }
            });
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let pane = pane.read(cx);
            assert_eq!(pane.search_query, "beta");
            assert_eq!(pane.terminal.read(cx).matches.len(), 1);
            assert_eq!(pane.active_match, Some(0));
        });
    }

    #[gpui::test]
    async fn literal_search_matches_case_insensitively(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "Alpha\r\nalpha\r\nALPHA\r\n");
        run_query(&pane, "alpha", cx);
        assert_eq!(match_count(&pane, cx), 3);
    }

    #[gpui::test]
    async fn regex_toggle_changes_how_the_query_is_matched(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "a.b\r\naxb\r\n");
        run_query(&pane, "a.b", cx);
        // A literal query matches only the line that contains the dot.
        assert_eq!(match_count(&pane, cx), 1);

        cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.toggle_search_regex(cx));
        });
        cx.run_until_parked();
        cx.update(|cx| assert!(pane.read(cx).search_regex));
        assert_eq!(match_count(&pane, cx), 2);
    }

    #[gpui::test]
    async fn navigating_matches_wraps_around(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "x1\r\nx2\r\nx3\r\n");
        run_query(&pane, "x", cx);
        let count = match_count(&pane, cx);
        assert_eq!(count, 3);

        // The scan activates the match nearest the cursor, not index 0.
        let first = cx
            .update(|cx| pane.read(cx).active_match)
            .expect("a match is active");
        let second = cx.update(|cx| {
            pane.update(cx, |pane, cx| pane.search_next(false, cx));
            pane.read(cx).active_match
        });
        assert_eq!(second, Some((first + 1) % count));

        // Pressing next on the last match wraps to the first one.
        let wrapped = cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.active_match = Some(count - 1);
                pane.search_next(false, cx);
            });
            pane.read(cx).active_match
        });
        assert_eq!(wrapped, Some(0));

        // ...and previous from the first wraps to the last.
        let wrapped_back = cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.active_match = Some(0);
                pane.search_next(true, cx);
            });
            pane.read(cx).active_match
        });
        assert_eq!(wrapped_back, Some(count - 1));
    }

    #[gpui::test]
    async fn editing_keys_move_the_caret_and_edit_the_query(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "");
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.search_active = true;
                pane.replace_search_text(None, "abc".to_string(), None, false, cx);
                // Caret moves before 'c', so backspace removes 'b'.
                pane.move_search_cursor(true, false, cx);
                pane.delete_search_backward(cx);
                assert_eq!(pane.search_query, "ac");

                pane.move_search_to_boundary(false, false, cx);
                pane.replace_search_text(None, "d".to_string(), None, false, cx);
                assert_eq!(pane.search_query, "acd");

                // Typing over a select-all selection replaces the whole query.
                pane.select_all_search_text(cx);
                pane.replace_search_text(None, "z".to_string(), None, false, cx);
                assert_eq!(pane.search_query, "z");

                pane.delete_search_backward(cx);
                assert_eq!(pane.search_query, "");
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn closing_the_bar_clears_the_query_and_highlights(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "alpha\r\n");
        run_query(&pane, "alpha", cx);
        assert_eq!(match_count(&pane, cx), 1);

        let window = cx.add_empty_window();
        window.update(|window, cx| {
            pane.update(cx, |pane, cx| pane.close_search(window, cx));
        });

        cx.update(|cx| {
            let pane = pane.read(cx);
            assert!(!pane.search_active);
            assert!(pane.search_query.is_empty());
            assert!(pane.active_match.is_none());
            assert!(pane.terminal.read(cx).matches.is_empty());
        });
    }

    #[gpui::test]
    async fn activating_a_match_in_scrollback_scrolls_it_into_view(cx: &mut gpui::TestAppContext) {
        let mut output = String::new();
        for line in 0..80 {
            output.push_str(&format!("line-{line}\r\n"));
        }
        let pane = pane_with_output(cx, &output);
        run_query(&pane, "line-0", cx);
        assert_eq!(match_count(&pane, cx), 1);

        // `activate_match` only queues selection and scrolling; the terminal
        // applies them on the next sync, which a frame normally drives.
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.terminal
                    .update(cx, |terminal, cx| terminal.sync(window, cx));
            });
        });

        cx.update(|cx| {
            let terminal = pane.read(cx).terminal.read(cx);
            assert!(
                terminal.last_content().display_offset > 0,
                "the active match should be scrolled out of the tail"
            );
            assert!(!terminal.last_content().scrolled_to_bottom);
        });
    }

    #[gpui::test]
    async fn new_output_is_rescanned_while_the_bar_is_open(cx: &mut gpui::TestAppContext) {
        let pane = pane_with_output(cx, "alpha\r\n");
        run_query(&pane, "beta", cx);
        assert_eq!(match_count(&pane, cx), 0);

        let terminal = cx.update(|cx| pane.read(cx).terminal.clone());
        cx.update(|cx| {
            terminal.update(cx, |terminal, cx| terminal.write_output(b"beta\r\n", cx));
        });
        cx.run_until_parked();

        assert_eq!(match_count(&pane, cx), 1);
    }
}
