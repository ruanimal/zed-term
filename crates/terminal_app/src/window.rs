//! Window root view: a tab bar plus the active terminal element.
//!
//! Each window owns its own set of tabs; multiple windows share the app-level
//! keymap defined in `app.rs`.

use std::cmp::Ordering;
use std::time::Duration;

use collections::HashMap;
use gpui::{
    AppContext as _, Context, DismissEvent, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, anchored,
    deferred, div, prelude::FluentBuilder, px,
};
use settings::Settings as _;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::{
    Clear as TerminalClear, Copy as TerminalCopyAction, Paste as TerminalPasteAction,
    PasteText as TerminalPasteTextAction, ScrollLineDown, ScrollLineUp, ScrollPageDown,
    ScrollPageUp, ScrollToBottom, ScrollToTop, SearchTest, SelectAll as TerminalSelectAll,
    ShowCharacterPalette, Terminal, TerminalBuilder,
};
use theme::ActiveTheme as _;
use ui::utils::{TRAFFIC_LIGHT_PADDING, platform_title_bar_height};
use ui::{
    Clickable as _, ContextMenu, IconButton, IconName, IconSize, Label, LabelCommon as _,
    LabelSize, Tab, TabBar, TabPosition, Toggleable as _,
};
use util::ResultExt;
use util::paths::PathStyle;

use crate::terminal::split::{self, SplitDirection, SplitNode};
use crate::terminal::tab::ScrollAction;
use crate::terminal::{TerminalElement, TerminalSearchBar, TerminalTab};
use crate::{
    ActivateNextPane, ActivatePreviousPane, CloseAll, CloseLeft, CloseOtherTabs, ClosePane,
    CloseRight, CloseTab, NewTab, NewWindow, NextTab, OpenSettings, PreviousTab, ReopenClosedTab,
    SendKeystroke, SendText, SplitDown, SplitLeft, SplitRight, SplitUp,
};

/// Fixed content width for every tab so the tab bar does not reflow while
/// the title follows the foreground process (e.g. `zsh` → `ls -al` → `zsh`).
/// Titles longer than this are cut off with an ellipsis, like editor tabs
/// (`MAX_TAB_TITLE_LEN` in the editor crate).
const TAB_TITLE_WIDTH: gpui::Pixels = px(140.);
/// Character cap applied to the title string itself, mirroring the editor's
/// `MAX_TAB_TITLE_LEN`; keeps tooltips and copy-paste from carrying absurd
/// titles even though the layout already truncates visually.
const TAB_TITLE_MAX_CHARS: usize = 24;
/// How many recently closed tabs' working directories to remember for
/// `ReopenClosedTab`.
const MAX_REMEMBERED_CLOSED_TABS: usize = 10;

/// A window in the standalone terminal app.
pub struct TerminalWindowView {
    pub focus_handle: FocusHandle,
    pub(crate) tabs: Vec<Entity<TerminalTab>>,
    pub(crate) active_tab_index: usize,
    /// Split-tree layout of panes; leaves reference tabs from `tabs`.
    /// `None` means the single-pane (tabs-only) legacy layout.
    pub(crate) split_root: Option<crate::terminal::split::SplitNode>,
    /// The tab shown in the focused pane. Kept as a handle so pane actions
    /// (split/close/focus cycling) have an anchor even while iterating.
    pub(crate) active_pane_tab: Option<Entity<TerminalTab>>,
    /// Working directories of recently closed tabs, most recent first, for
    /// `ReopenClosedTab` (G4). Zed keeps the whole item around; keeping just
    /// the cwd matches this app's no-history-policy while restoring the
    /// "continue where I was" experience.
    closed_tab_cwds: Vec<std::path::PathBuf>,
    /// Right-click context menu for the active terminal, if open.
    context_menu: Option<(Entity<ContextMenu>, gpui::Point<Pixels>, Subscription)>,
    /// Left-button state for the titlebar strip drag gesture.
    titlebar_mouse_down: std::cell::Cell<bool>,
    /// Tracks the tab bar's horizontal scroll so an activated tab can be
    /// scrolled back into view when the tabs overflow.
    tab_bar_scroll_handle: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl TerminalWindowView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab_index: 0,
            split_root: None,
            active_pane_tab: None,
            closed_tab_cwds: Vec::new(),
            context_menu: None,
            titlebar_mouse_down: std::cell::Cell::new(false),
            tab_bar_scroll_handle: ScrollHandle::new(),
            _subscriptions: Vec::new(),
        };
        view.spawn_new_terminal(cx);

        // The macOS display link only redraws while invalidated. Shell output now
        // repaints through the observe chain (Terminal -> TerminalTab ->
        // TerminalWindowView) plus the TitleChanged subscription; the heartbeat
        // remains as a fallback for cursor blink and any path that does not
        // notify.
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                cx.update(|cx| {
                    for handle in cx.windows() {
                        handle.update(cx, |_, window, _| window.refresh()).log_err();
                    }
                });
            }
        })
        .detach();

        view
    }

    /// Starts a PTY-backed shell; in split layout the new terminal takes over
    /// the active pane, otherwise it appears as a new tab.
    fn spawn_new_terminal(&mut self, cx: &mut Context<Self>) {
        let focus = self.focus_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let Some(builder) = build_terminal(cx).await else {
                return;
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|cx| TerminalTab::new(terminal, focus.clone(), cx));
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.observe_tab(&tab, cx);
                    this.tabs.push(tab.clone());
                    if this.split_root.is_some() {
                        this.replace_active_pane_tab(&tab, cx);
                    }
                    this.active_tab_index = this.tabs.len() - 1;
                    cx.notify();
                })
                .log_err();
            });
        })
        .detach();
    }

    /// Replaces the tab in the focused pane with `tab` (used by split
    /// actions), keeping the split tree shape intact.
    fn replace_active_pane_tab(&mut self, tab: &Entity<TerminalTab>, _cx: &mut Context<Self>) {
        Self::replace_tab_in_node(self.split_root.as_mut(), self.active_pane_tab.as_ref(), tab);
        self.active_pane_tab = Some(tab.clone());
    }

    fn replace_tab_in_node(
        node: Option<&mut crate::terminal::split::SplitNode>,
        old_tab: Option<&Entity<TerminalTab>>,
        new_tab: &Entity<TerminalTab>,
    ) {
        let (Some(node), Some(old_tab)) = (node, old_tab) else {
            return;
        };
        match node {
            crate::terminal::split::SplitNode::Leaf { tab } => {
                if tab == old_tab {
                    *tab = new_tab.clone();
                }
            }
            crate::terminal::split::SplitNode::Axis { children, .. } => {
                for child in children {
                    Self::replace_tab_in_node(Some(child), Some(old_tab), new_tab);
                }
            }
        }
    }

    /// Registers the window to repaint when the tab notifies (its terminal
    /// updated). Must run on the foreground thread after the tab exists.
    fn observe_tab(&mut self, tab: &Entity<TerminalTab>, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.observe(tab, |this, _, cx| {
            this.active_tab_index = this.active_tab_index.min(this.tabs.len().saturating_sub(1));
            cx.notify();
        }));
    }

    /// Activates the tab at `index` and keeps it visible in the tab bar even
    /// when the tabs overflow the bar's width (like Zed's `Pane::update_active_tab`).
    fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.active_tab_index = index;
        self.tab_bar_scroll_handle.scroll_to_item(index);
        cx.notify();
    }

    /// Runs `f` against the active tab, if there is one.
    fn with_active_tab(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TerminalTab, &mut Context<TerminalTab>),
    ) {
        let Some(tab) = self.tabs.get(self.active_tab_index).cloned() else {
            return;
        };
        tab.update(cx, f);
    }

    // Terminal actions (§4.4): forward to the active tab. `Terminal` handles
    // the actual clipboard IO (OSC52 + `InternalEvent::Copy`).

    fn copy(&mut self, _: &TerminalCopyAction, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.copy_selection(cx));
    }

    fn paste(&mut self, _: &TerminalPasteAction, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.paste_clipboard(cx));
    }

    fn paste_text(
        &mut self,
        _: &TerminalPasteTextAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.paste_clipboard(cx));
    }

    fn clear(&mut self, _: &TerminalClear, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.clear_screen(cx));
    }

    fn select_all(&mut self, _: &TerminalSelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.select_all(cx));
    }

    fn scroll_line_up(&mut self, _: &ScrollLineUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::LineUp, cx));
    }

    fn scroll_line_down(
        &mut self,
        _: &ScrollLineDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::LineDown, cx));
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::PageUp, cx));
    }

    fn scroll_page_down(
        &mut self,
        _: &ScrollPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::PageDown, cx));
    }

    fn scroll_half_page_up(
        &mut self,
        _: &terminal_core::ScrollHalfPageUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::HalfPageUp, cx));
    }

    fn scroll_half_page_down(
        &mut self,
        _: &terminal_core::ScrollHalfPageDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::HalfPageDown, cx));
    }

    fn scroll_to_top(&mut self, _: &ScrollToTop, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::Top, cx));
    }

    fn scroll_to_bottom(
        &mut self,
        _: &ScrollToBottom,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_active_tab(cx, |tab, cx| tab.scroll(ScrollAction::Bottom, cx));
    }

    fn toggle_search(&mut self, _: &SearchTest, window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.toggle_search(window, cx));
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab_index).cloned() else {
            return;
        };
        tab.update(cx, |tab, cx| tab.show_character_palette(window, cx));
    }

    fn new_tab(&mut self, _: &NewTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_new_terminal(cx);
    }

    // Split panes (G1). The split tree lives in `split_root`; each split
    // spawns a fresh terminal into a new leaf next to the focused pane's leaf
    // (mirroring Zed's `pane::Split*` + `workspace::split_pane`).

    /// Returns the tab currently filling the focused pane (in split layout)
    /// or the active tab (plain tab layout).
    fn focused_pane_tab(&self) -> Option<Entity<TerminalTab>> {
        if self.split_root.is_some() {
            self.active_pane_tab.clone()
        } else {
            self.tabs.get(self.active_tab_index).cloned()
        }
    }

    fn split_pane(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(anchor_tab) = self.focused_pane_tab() else {
            // No pane yet (first terminal still spawning): fall back to the
            // legacy new-tab flow; once it lands, splitting is available.
            self.spawn_new_terminal(cx);
            return;
        };
        // First split: promote the current single-pane layout into a split
        // tree whose first leaf keeps the anchor tab.
        if self.split_root.is_none() {
            let Some(anchor) = self.tabs.get(self.active_tab_index).cloned() else {
                return;
            };
            self.split_root = Some(SplitNode::leaf(anchor));
            self.active_pane_tab = self.tabs.get(self.active_tab_index).cloned();
        }
        let focus = self.focus_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let Some(builder) = build_terminal(cx).await else {
                return;
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|cx| TerminalTab::new(terminal, focus.clone(), cx));
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.observe_tab(&tab, cx);
                    this.tabs.push(tab.clone());
                    if let Some(root) = this.split_root.as_mut()
                        && root.split(&anchor_tab, tab.clone(), direction)
                    {
                        this.active_pane_tab = Some(tab);
                        let index = this.tabs.len() - 1;
                        this.tab_bar_scroll_handle.scroll_to_item(index);
                    }
                    cx.notify();
                })
                .log_err();
            });
        })
        .detach();
    }

    fn split_right(&mut self, _: &SplitRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Right, cx);
    }

    fn split_left(&mut self, _: &SplitLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Left, cx);
    }

    fn split_up(&mut self, _: &SplitUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Up, cx);
    }

    fn split_down(&mut self, _: &SplitDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.split_pane(SplitDirection::Down, cx);
    }

    /// Cycles pane focus through the split tree's leaves in visual order.
    fn activate_adjacent_pane(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.active_pane_tab.clone() else {
            return;
        };
        let Some(root) = self.split_root.as_ref() else {
            return;
        };
        let mut leaves = Vec::new();
        root.collect_tabs(&mut leaves);
        if leaves.len() < 2 {
            return;
        }
        let current = leaves.iter().position(|tab| **tab == anchor);
        let next = match current {
            Some(index) => {
                if forward {
                    (index + 1) % leaves.len()
                } else {
                    (index + leaves.len() - 1) % leaves.len()
                }
            }
            // The anchor vanished (e.g. removed elsewhere); pick an edge.
            None => {
                if forward {
                    0
                } else {
                    leaves.len() - 1
                }
            }
        };
        self.active_pane_tab = Some(leaves[next].clone());
        self.focus_pane(window, cx);
        if let Some(index) = self.tabs.iter().position(|tab| *tab == *leaves[next]) {
            self.active_tab_index = index;
            self.tab_bar_scroll_handle.scroll_to_item(index);
        }
        cx.notify();
    }

    fn activate_next_pane(
        &mut self,
        _: &ActivateNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_adjacent_pane(true, window, cx);
    }

    fn activate_previous_pane(
        &mut self,
        _: &ActivatePreviousPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_adjacent_pane(false, window, cx);
    }

    /// Focuses the terminal inside the active pane.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_pane_tab.as_ref() {
            let focus_handle = tab.read(cx).focus_handle.clone();
            focus_handle.focus(window, cx);
        }
    }

    /// Removes the focused pane's leaf from the split tree, collapsing the
    /// tree like Zed's `PaneAxis::remove`. The pane's tab is also closed
    /// (removed from the tab bar); closing the last pane closes the window.
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(anchor) = self.active_pane_tab.clone() else {
            return;
        };
        if let Some(root) = self.split_root.as_mut() {
            if let Some(replacement) = root.remove(&anchor) {
                *root = replacement;
            }
            // Drop the closed tab from the tab bar model.
            if let Some(index) = self.tabs.iter().position(|tab| *tab == anchor) {
                self.tabs.remove(index);
            }
            // Pick a new active pane tab.
            let mut leaves = Vec::new();
            root.collect_tabs(&mut leaves);
            if leaves.is_empty() || self.tabs.is_empty() {
                window.remove_window();
                return;
            }
            let next_tab = leaves[leaves.len() - 1].clone();
            self.active_pane_tab = Some(next_tab.clone());
            if let Some(index) = self.tabs.iter().position(|tab| *tab == next_tab) {
                self.active_tab_index = index;
            }
        }
        cx.notify();
    }

    /// Sends raw text straight to the PTY (the keymap's escape-sequence
    /// conveniences, e.g. `alt-delete` → ESC d).
    fn send_text(
        &mut self,
        SendText(text): &SendText,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        self.with_active_tab(cx, |tab, cx| {
            tab.terminal.update(cx, |term, _| {
                term.input(text.clone().into_bytes());
            });
        });
    }

    /// Translates the action's keystroke string (e.g. "ctrl-u") via
    /// `Terminal::try_keystroke`, like Zed's TerminalView::send_keystroke.
    /// Lets the keymap map mac-friendly shortcuts onto shell-line-editing
    /// control codes (`cmd-backspace` → ctrl-u, `cmd-right` → ctrl-e, ...).
    fn send_keystroke(
        &mut self,
        SendKeystroke(key): &SendKeystroke,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(keystroke) = gpui::Keystroke::parse(key) else {
            log::warn!("Invalid SendKeystroke binding: {key:?}");
            return;
        };
        let option_as_meta = TerminalSettings::get_global(cx).option_as_meta;
        self.with_active_tab(cx, |tab, cx| {
            tab.terminal.update(cx, |term, _| {
                term.try_keystroke(&keystroke, option_as_meta);
            });
        });
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        // Remember the cwd so `ReopenClosedTab` can restore it (G4).
        if let Some(tab) = self.tabs.get(self.active_tab_index)
            && let Some(cwd) = tab.read(cx).terminal.read(cx).working_directory()
        {
            self.closed_tab_cwds.insert(0, cwd);
            self.closed_tab_cwds.truncate(MAX_REMEMBERED_CLOSED_TABS);
        }
        self.tabs.remove(self.active_tab_index);
        if self.tabs.is_empty() {
            window.remove_window();
        } else {
            self.active_tab_index = self.active_tab_index.min(self.tabs.len() - 1);
            cx.notify();
        }
    }

    /// Reopens the most recently closed tab, starting its shell in that tab's
    /// former working directory (mirrors Zed's `pane::ReopenClosedItem`).
    fn reopen_closed_tab(
        &mut self,
        _: &ReopenClosedTab,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.closed_tab_cwds.first().cloned() else {
            return;
        };
        self.closed_tab_cwds.remove(0);
        let focus = self.focus_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let Some(builder) = build_terminal_in(Some(cwd), cx).await else {
                return;
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|cx| TerminalTab::new(terminal, focus.clone(), cx));
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.observe_tab(&tab, cx);
                    this.tabs.push(tab);
                    this.active_tab_index = this.tabs.len() - 1;
                    cx.notify();
                })
                .log_err();
            });
        })
        .detach();
    }

    fn next_tab(&mut self, _: &NextTab, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let index = (self.active_tab_index + 1) % self.tabs.len();
        self.activate_tab(index, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, _window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let index = (self.active_tab_index + self.tabs.len() - 1) % self.tabs.len();
        self.activate_tab(index, cx);
    }

    fn new_window(&mut self, _: &NewWindow, _window: &mut Window, cx: &mut Context<Self>) {
        crate::open_new_window(cx);
    }

    fn close_other_tabs(
        &mut self,
        _: &CloseOtherTabs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active) = self.tabs.get(self.active_tab_index) else {
            return;
        };
        let active_id = active.entity_id();
        self.tabs.retain(|tab| tab.entity_id() == active_id);
        self.active_tab_index = 0;
        cx.notify();
    }

    fn close_left(&mut self, _: &CloseLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.drain(..self.active_tab_index);
        self.active_tab_index = 0;
        cx.notify();
    }

    fn close_right(&mut self, _: &CloseRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.truncate(self.active_tab_index + 1);
        cx.notify();
    }

    fn close_all(&mut self, _: &CloseAll, window: &mut Window, _cx: &mut Context<Self>) {
        self.tabs.clear();
        window.remove_window();
    }

    fn open_settings(&mut self, _: &OpenSettings, _window: &mut Window, cx: &mut Context<Self>) {
        crate::settings_ui::open_settings_window(cx);
    }

    /// Builds and shows a right-click context menu, keeping it alive in
    /// `self.context_menu` until dismissed. The menu is rendered as an
    /// anchored popover in `render`.
    fn show_context_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(ContextMenu, &mut Window, &mut Context<ContextMenu>) -> ContextMenu,
    ) {
        let context_menu = ContextMenu::build(window, cx, build);
        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe_in(
            &context_menu,
            window,
            |this, _, _: &DismissEvent, _window, cx| {
                this.context_menu.take();
                cx.notify();
            },
        );
        self.context_menu = Some((context_menu, position, subscription));
    }

    /// Right-click menu over the terminal surface (mirrors Zed's terminal
    /// context menu minus the workspace/assistant entries).
    fn deploy_terminal_context_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab_index).cloned() else {
            return;
        };
        let in_split_layout = self.split_root.is_some();
        self.show_context_menu(position, window, cx, |menu, _, cx| {
            let menu = menu
                .context(tab.read(cx).focus_handle.clone())
                .action("New Terminal", Box::new(NewTab))
                .separator();
            let menu = if in_split_layout {
                // Split actions apply to the pane the right-click landed on
                // (now the active pane).
                menu.action("Split Right", Box::new(SplitRight))
                    .action("Split Down", Box::new(SplitDown))
                    .separator()
                    .action("Close Pane", Box::new(ClosePane))
                    .separator()
            } else {
                menu
            };
            menu.action("Copy", Box::new(terminal_core::Copy))
                .action("Paste", Box::new(terminal_core::Paste))
                .action("Paste Text", Box::new(terminal_core::PasteText))
                .action("Select All", Box::new(terminal_core::SelectAll))
                .action("Clear", Box::new(terminal_core::Clear))
                .separator()
                .action("Close Terminal Tab", Box::new(CloseTab))
        });
    }

    /// Right-click menu over a tab in the tab bar.
    fn deploy_tab_context_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab_index).cloned() else {
            return;
        };
        self.show_context_menu(position, window, cx, |menu, _, cx| {
            menu.context(tab.read(cx).focus_handle.clone())
                .action("Reopen Closed Tab", Box::new(ReopenClosedTab))
                .separator()
                .action("Close", Box::new(CloseTab))
                .action("Close Others", Box::new(CloseOtherTabs))
                .action("Close Left", Box::new(CloseLeft))
                .action("Close Right", Box::new(CloseRight))
                .action("Close All", Box::new(CloseAll))
        });
    }

    /// Renders the titlebar strip: the tab bar itself acts as the window
    /// titlebar (as in Zed). The traffic lights float over its left edge, so
    /// that area is reserved as a drag handle; the remaining empty area of
    /// the strip drags the window and double-click zooms it, like a native
    /// titlebar. Dragging follows Zed's PlatformTitleBar pattern: flag on
    /// mouse-down, `start_window_move` on drag; interactive children (tabs,
    /// buttons) stop propagation so presses on them don't move the window.
    fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let titlebar_height = platform_title_bar_height(window);
        let titlebar_background = cx.theme().colors().tab_bar_background;
        div()
            .id("title-bar")
            .h(titlebar_height)
            .flex_none()
            .pl(px(TRAFFIC_LIGHT_PADDING))
            .bg(titlebar_background)
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
            .on_click(cx.listener(|_, event: &gpui::ClickEvent, window, _| {
                if event.click_count() >= 2 {
                    window.zoom_window();
                }
            }))
            .child(self.render_tab_bar(cx))
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let tab_count = self.tabs.len();
        TabBar::new("window-tabs")
            .track_scroll(&self.tab_bar_scroll_handle)
            .end_child(
                div()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| {
                            // Keep button presses out of the titlebar drag
                            // gesture (IconButton has no mouse-down hook).
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        IconButton::new("new-tab-button", IconName::Plus)
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(|this, _: &gpui::ClickEvent, _window, cx| {
                                this.spawn_new_terminal(cx);
                            })),
                    ),
            )
            .end_child(
                div()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        IconButton::new("settings-button", IconName::Settings)
                            .icon_size(IconSize::XSmall)
                            .on_click(cx.listener(|_, _: &gpui::ClickEvent, _window, cx| {
                                crate::settings_ui::open_settings_window(cx);
                            })),
                    ),
            )
            .children(
                self.tabs
                    .iter()
                    .enumerate()
                    .map(|(idx, tab)| {
                        let title = tab.read(cx).title(cx);
                        let title = util::truncate_and_trailoff(&title, TAB_TITLE_MAX_CHARS);
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                    // Keep tab presses out of the titlebar
                                    // drag gesture.
                                    cx.stop_propagation();
                                }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_tab(idx, cx);
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.activate_tab(idx, cx);
                                    this.deploy_tab_context_menu(event.position, window, cx);
                                    cx.notify();
                                    // Stop so the window root's right-click
                                    // handler does not replace this with the
                                    // terminal (copy/paste) menu.
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                div().w(TAB_TITLE_WIDTH).child(
                                    Label::new(title)
                                        .single_line()
                                        .truncate()
                                        .size(LabelSize::Small),
                                ),
                            )
                    })
                    .collect::<Vec<_>>(),
            )
    }
}

impl Render for TerminalWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_tab = self.focused_pane_tab();
        let terminal = active_tab.as_ref().map(|tab| tab.read(cx).terminal.clone());
        let search_active = active_tab
            .as_ref()
            .map(|tab| tab.read(cx).search_active)
            .unwrap_or(false);
        let in_split_layout = self.split_root.is_some();

        // Content area: split tree when present, single terminal otherwise.
        // Pre-clone the tab handles so the recursive render borrows the tree
        // exclusively and the closure only touches already-cloned state.
        let content = if let Some(root) = self.split_root.as_mut() {
            let focus_handle = self.focus_handle.clone();
            let mut leaf_tabs: Vec<(Entity<Terminal>, Entity<TerminalTab>)> = Vec::new();
            {
                let mut tabs_out: Vec<&Entity<TerminalTab>> = Vec::new();
                root.collect_tabs(&mut tabs_out);
                for tab in tabs_out {
                    let terminal = tab.read(cx).terminal.clone();
                    leaf_tabs.push((terminal, tab.clone()));
                }
            }
            let mut tab_iter = leaf_tabs.into_iter();
            root.render(window, cx, &mut |_tab| {
                // Leaves are visited in the same visual order as
                // `collect_tabs`, so popping front-to-back stays aligned.
                if let Some((terminal, tab)) = tab_iter.next() {
                    TerminalElement::new(terminal, tab, focus_handle.clone(), true, true)
                        .into_any_element()
                } else {
                    div().into_any_element()
                }
            })
        } else {
            terminal
                .zip(active_tab.clone())
                .map(|(terminal, tab)| {
                    TerminalElement::new(terminal, tab, self.focus_handle.clone(), true, true)
                        .into_any_element()
                })
                .unwrap_or_else(|| {
                    div()
                        .size_full()
                        .child("Starting terminal…")
                        .into_any_element()
                })
        };

        div()
            .id("terminal-window")
            .key_context("TerminalWindow")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().colors().terminal_background)
            .child(self.render_title_bar(window, cx))
            .when_some(active_tab.clone().filter(|_| search_active), |this, tab| {
                this.child(TerminalSearchBar::new(tab, cx.weak_entity()).into_any_element())
            })
            .child(div().flex_1().min_h_0().child(content))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::paste_text))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::scroll_line_up))
            .on_action(cx.listener(Self::scroll_line_down))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_half_page_up))
            .on_action(cx.listener(Self::scroll_half_page_down))
            .on_action(cx.listener(Self::scroll_to_top))
            .on_action(cx.listener(Self::scroll_to_bottom))
            .on_action(cx.listener(Self::toggle_search))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::reopen_closed_tab))
            .on_action(cx.listener(Self::send_text))
            .on_action(cx.listener(Self::send_keystroke))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::close_other_tabs))
            .on_action(cx.listener(Self::close_left))
            .on_action(cx.listener(Self::close_right))
            .on_action(cx.listener(Self::close_all))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::new_window))
            .on_action(cx.listener(Self::open_settings))
            .when(in_split_layout, |this| {
                this.on_action(cx.listener(Self::activate_next_pane))
                    .on_action(cx.listener(Self::activate_previous_pane))
                    .on_action(cx.listener(Self::close_pane))
            })
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_left))
            .on_action(cx.listener(Self::split_up))
            .on_action(cx.listener(Self::split_down))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                // Titlebar drag gesture first (it owns the flag-based latch).
                if this.titlebar_mouse_down.get() {
                    this.titlebar_mouse_down.set(false);
                    window.start_window_move();
                    return;
                }
                // Divider drag: while an anchor is latched, every move
                // re-distributes the first flex pair of the dragged axis.
                if let (Some(anchor), Some(axis)) = (split::drag_anchor(), split::drag_axis()) {
                    let is_horizontal = axis == gpui::Axis::Horizontal;
                    let position = if is_horizontal {
                        event.position.x
                    } else {
                        event.position.y
                    };
                    // The split container is the window content minus the
                    // titlebar strip; take its axis length from the viewport.
                    let viewport = window.viewport_size();
                    let container_length = if is_horizontal {
                        viewport.width
                    } else {
                        viewport.height - platform_title_bar_height(window)
                    };
                    if container_length > px(0.)
                        && let Some(root) = this.split_root.as_mut()
                    {
                        root.resize_flexes(axis, container_length, position - anchor);
                        split::update_drag_anchor(position);
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|_, _: &MouseUpEvent, _window, _cx| {
                    split::take_drag_anchor();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // In split layout the right-click may land on any pane;
                    // make the clicked leaf the active pane so the menu (and
                    // its split/closing actions) act on it.
                    if this.split_root.is_some()
                        && let Some(clicked) = split::take_clicked_leaf()
                    {
                        this.active_pane_tab = Some(clicked.clone());
                        if let Some(index) = this.tabs.iter().position(|tab| *tab == clicked) {
                            this.active_tab_index = index;
                        }
                    }
                    let Some(tab) = this.tabs.get(this.active_tab_index) else {
                        return;
                    };
                    let in_mouse_mode = tab
                        .read(cx)
                        .terminal
                        .read(cx)
                        .mouse_mode(event.modifiers.shift);
                    if !in_mouse_mode {
                        // Mirrors Zed: right-click selects the word under the
                        // cursor before showing the context menu.
                        let has_selection = tab
                            .read(cx)
                            .terminal
                            .read(cx)
                            .last_content
                            .selection
                            .is_some();
                        if !has_selection {
                            tab.update(cx, |tab, cx| {
                                tab.terminal.update(cx, |term, _| {
                                    term.select_word_at_event_position(event);
                                });
                            });
                        }
                        this.deploy_terminal_context_menu(event.position, window, cx);
                        cx.notify();
                    }
                }),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(gpui::Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                // Keys that no keymap binding consumed (Enter, Tab, arrows,
                // Ctrl+C, ...) are translated to ANSI and sent to the pty,
                // mirroring Zed's `TerminalView::key_down`.
                let Some(tab) = this.tabs.get(this.active_tab_index) else {
                    return;
                };
                let handled = tab.update(cx, |tab, cx| {
                    tab.terminal.update(cx, |term, cx| {
                        term.try_keystroke(
                            &event.keystroke,
                            TerminalSettings::get_global(cx).option_as_meta,
                        )
                    })
                });
                if handled {
                    cx.stop_propagation();
                }
            }))
    }
}

/// Builds a PTY-backed terminal on the foreground executor.
async fn build_terminal(cx: &gpui::AsyncApp) -> Option<TerminalBuilder> {
    build_terminal_in(None, cx).await
}

/// Builds a PTY-backed terminal, optionally rooted at `cwd` (used by
/// `ReopenClosedTab` to restore the closed tab's working directory).
async fn build_terminal_in(
    cwd: Option<std::path::PathBuf>,
    cx: &gpui::AsyncApp,
) -> Option<TerminalBuilder> {
    let settings = cx.update(|cx| TerminalSettings::get_global(cx).clone());
    let builder = cx.update(|cx| {
        TerminalBuilder::new(
            cwd,
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
