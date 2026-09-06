//! Window root view: a tab bar plus the active terminal element.
//!
//! Each window owns its own set of tabs; multiple windows share the app-level
//! keymap defined in `app.rs`.

use std::{cmp::Ordering, path::PathBuf, time::Duration};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Decorations, DismissEvent, Entity,
    FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    PromptLevel, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription,
    WeakEntity, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use settings::Settings as _;
use settings::settings_content::TerminalBell;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::{
    Clear as TerminalClear, Copy as TerminalCopyAction, Paste as TerminalPasteAction,
    PasteText as TerminalPasteTextAction, ScrollLineDown, ScrollLineUp, ScrollPageDown,
    ScrollPageUp, ScrollToBottom, ScrollToTop, SearchTest, SelectAll as TerminalSelectAll,
    ShowCharacterPalette, Terminal, TerminalBuilder,
};
use theme::ActiveTheme as _;
use ui::scrollbars::{ScrollbarVisibility, ShowScrollbar};
use ui::utils::TRAFFIC_LIGHT_PADDING;
use ui::{
    ButtonCommon as _, Clickable as _, Color, ContextMenu, IconButton, IconName, IconSize,
    Indicator, Label, LabelCommon as _, LabelSize, ScrollAxes, Scrollbars, Tab, TabBar,
    TabPosition, Toggleable as _, Tooltip, WithScrollbar,
};
use util::ResultExt;
use util::paths::PathStyle;

use crate::terminal::split::{self, SplitDirection, SplitNode};
use crate::terminal::tab::{ScrollAction, TerminalTabEvent};
use crate::terminal::{TerminalElement, TerminalSearchBar, TerminalTab};
use crate::window_chrome;
use crate::{
    ActivateNextPane, ActivatePreviousPane, CloseAll, CloseLeft, CloseOtherTabs, ClosePane,
    CloseRight, CloseTab, NewTab, NewWindow, NextTab, OpenSettings, PreviousTab, SendKeystroke,
    SendText, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleZoom,
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

#[derive(Default)]
struct TerminalScrollbarSettingsWrapper;

impl ScrollbarVisibility for TerminalScrollbarSettingsWrapper {
    fn visibility(&self, cx: &App) -> ShowScrollbar {
        match TerminalSettings::get_global(cx)
            .scrollbar
            .show
            .unwrap_or(settings::ShowScrollbar::Auto)
        {
            settings::ShowScrollbar::Auto => ShowScrollbar::Auto,
            settings::ShowScrollbar::System => ShowScrollbar::System,
            settings::ShowScrollbar::Always => ShowScrollbar::Always,
            settings::ShowScrollbar::Never => ShowScrollbar::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveTerminalSettings {
    cursor_shape: terminal_core::terminal_settings::CursorShape,
    alternate_scroll: settings::AlternateScroll,
}

impl LiveTerminalSettings {
    fn from_settings(settings: &TerminalSettings) -> Self {
        Self {
            cursor_shape: settings.cursor_shape,
            alternate_scroll: settings.alternate_scroll,
        }
    }
}

/// A window in the standalone terminal app.
pub struct TerminalWindowView {
    pub focus_handle: FocusHandle,
    /// Open tabs, each owning its own split-pane layout (the iTerm2 model:
    /// a tab is the top-level unit; cmd-d splits within the active tab).
    pub(crate) tabs: Vec<WindowTab>,
    pub(crate) active_tab_index: usize,
    /// Right-click context menu for the active terminal, if open.
    context_menu: Option<(Entity<ContextMenu>, gpui::Point<Pixels>, Subscription)>,
    /// Left-button state for the titlebar strip drag gesture.
    titlebar_mouse_down: std::cell::Cell<bool>,
    /// Tracks the tab bar's horizontal scroll so an activated tab can be
    /// scrolled back into view when the tabs overflow.
    tab_bar_scroll_handle: ScrollHandle,
    terminal_error: Option<String>,
    live_settings: LiveTerminalSettings,
    _subscriptions: Vec<Subscription>,
}

/// A single tab: a split-pane group plus which pane currently holds focus.
#[derive(Clone)]
pub(crate) struct WindowTab {
    /// Split-pane tree. `None` means a single terminal (no splits yet);
    /// each leaf holds an `Entity<TerminalTab>`.
    pub(crate) split_root: Option<SplitNode>,
    /// The tab shown in the focused pane of this tab. Anchors pane actions.
    pub(crate) active_pane_tab: Option<Entity<TerminalTab>>,
    /// The pane temporarily occupying the entire content area. The split tree
    /// remains intact so restoring zoom preserves its layout and flexes.
    pub(crate) maximized_pane_tab: Option<Entity<TerminalTab>>,
}

impl WindowTab {
    /// Creates a tab from a single terminal pane (no splits yet).
    fn new(tab: Entity<TerminalTab>) -> Self {
        Self {
            split_root: None,
            active_pane_tab: Some(tab),
            maximized_pane_tab: None,
        }
    }

    /// The pane currently holding focus within this tab.
    fn focused_tab(&self) -> Option<Entity<TerminalTab>> {
        self.active_pane_tab.clone()
    }

    fn contains_pane(&self, pane: &Entity<TerminalTab>) -> bool {
        self.split_root.as_ref().map_or_else(
            || self.active_pane_tab.as_ref() == Some(pane),
            |root| root.contains_tab(pane),
        )
    }

    fn has_bell(&self, cx: &App) -> bool {
        if let Some(root) = self.split_root.as_ref() {
            let mut panes = Vec::new();
            root.collect_tabs(&mut panes);
            panes.into_iter().any(|pane| pane.read(cx).has_bell())
        } else {
            self.active_pane_tab
                .as_ref()
                .is_some_and(|pane| pane.read(cx).has_bell())
        }
    }

    fn is_split_layout(&self) -> bool {
        self.split_root
            .as_ref()
            .is_some_and(|root| root.leaf_count() > 1)
    }

    fn is_pane_maximized(&self) -> bool {
        self.maximized_pane_tab.is_some()
    }
}

#[derive(Clone)]
struct DraggedTerminalTab {
    source_window: gpui::EntityId,
    source_view: WeakEntity<TerminalWindowView>,
    pane: Entity<TerminalTab>,
    title: String,
    is_active: bool,
}

impl DraggedTerminalTab {
    fn ordering_relative_to(
        &self,
        target_pane: &Entity<TerminalTab>,
        cx: &App,
    ) -> Option<Ordering> {
        self.source_view
            .read_with(cx, |source_view, _| {
                let source_index = source_view
                    .tabs
                    .iter()
                    .position(|tab| tab.contains_pane(&self.pane))?;
                let target_index = source_view
                    .tabs
                    .iter()
                    .position(|tab| tab.contains_pane(target_pane))?;
                Some(target_index.cmp(&source_index))
            })
            .ok()
            .flatten()
    }
}

impl Render for DraggedTerminalTab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Tab::new("dragged-terminal-tab")
            .height(window_chrome::TITLE_BAR_HEIGHT)
            .toggle_state(self.is_active)
            .child(
                div().w(TAB_TITLE_WIDTH).child(
                    Label::new(self.title.clone())
                        .single_line()
                        .truncate()
                        .size(LabelSize::Small),
                ),
            )
    }
}

impl TerminalWindowView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let live_settings = LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
        let settings_subscription =
            cx.observe_global::<settings::SettingsStore>(Self::settings_changed);
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab_index: 0,
            context_menu: None,
            titlebar_mouse_down: std::cell::Cell::new(false),
            tab_bar_scroll_handle: ScrollHandle::new(),
            terminal_error: None,
            live_settings,
            _subscriptions: vec![settings_subscription],
        };
        view.spawn_new_tab(cx);

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

    fn all_panes(&self) -> Vec<Entity<TerminalTab>> {
        let mut panes = Vec::new();
        for tab in &self.tabs {
            if let Some(split_root) = tab.split_root.as_ref() {
                let mut split_panes = Vec::new();
                split_root.collect_tabs(&mut split_panes);
                panes.extend(split_panes.into_iter().cloned());
            } else if let Some(pane) = tab.active_pane_tab.as_ref() {
                panes.push(pane.clone());
            }
        }
        panes
    }

    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let live_settings = LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
        let cursor_shape_changed = self.live_settings.cursor_shape != live_settings.cursor_shape;
        let alternate_scroll_changed =
            self.live_settings.alternate_scroll != live_settings.alternate_scroll;

        if cursor_shape_changed || alternate_scroll_changed {
            for pane in self.all_panes() {
                let terminal = pane.read(cx).terminal.clone();
                terminal.update(cx, |terminal, cx| {
                    if cursor_shape_changed {
                        terminal.set_cursor_shape(live_settings.cursor_shape);
                    }
                    if alternate_scroll_changed {
                        terminal.set_alternate_scroll(live_settings.alternate_scroll);
                    }
                    cx.notify();
                });
                pane.update(cx, |_, cx| cx.notify());
            }
            self.live_settings = live_settings;
        }

        cx.notify();
    }

    /// Starts a PTY-backed shell as a brand-new tab (each tab owns its own
    /// split-pane group; cmd-d later splits within it).
    fn spawn_new_tab(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let builder = match build_terminal(cx).await {
                Ok(builder) => builder,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.terminal_error = Some(format!("Could not start terminal: {error:#}"));
                        cx.notify();
                    })
                    .log_err();
                    return;
                }
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|cx| TerminalTab::new(terminal, cx));
            let focus_handle = tab.read_with(cx, |tab, _| tab.focus_handle.clone());
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.terminal_error = None;
                    this.observe_tab(&tab, cx);
                    this.tabs.push(WindowTab::new(tab));
                    this.active_tab_index = this.tabs.len() - 1;
                    Self::focus_handle_after_update(focus_handle, cx);
                    cx.notify();
                })
                .log_err();
            });
        })
        .detach();
    }

    fn focus_handle_after_update(focus_handle: FocusHandle, cx: &mut Context<Self>) {
        let source = cx.entity();
        cx.defer(move |cx| {
            for any_window in cx.windows() {
                let Some(window_handle) = any_window.downcast::<Self>() else {
                    continue;
                };
                let Ok(window_entity) = window_handle.entity(cx) else {
                    continue;
                };
                if window_entity != source {
                    continue;
                }
                window_handle
                    .update(cx, |_, window, cx| {
                        focus_handle.focus(window, cx);
                    })
                    .log_err();
                break;
            }
        });
    }

    /// Registers the window to repaint when the tab notifies (its terminal
    /// updated). Also listens for the pane's shell exiting so the pane (or its
    /// whole tab) can be torn down. Must run on the foreground thread after the
    /// tab exists.
    fn observe_tab(&mut self, tab: &Entity<TerminalTab>, cx: &mut Context<Self>) {
        self._subscriptions.push(cx.observe(tab, |this, _, cx| {
            this.active_tab_index = this.active_tab_index.min(this.tabs.len().saturating_sub(1));
            cx.notify();
        }));
        self._subscriptions
            .push(cx.subscribe(tab, |_this, pane, event, cx| {
                // The callback runs while `TerminalWindowView` is already
                // being updated, so defer window-level handling to avoid a
                // double lease. Resolve the window by its root entity rather
                // than using the active window, because a background terminal
                // window may be the source of the event.
                let event = *event;
                let source = cx.entity();
                let pane = pane.clone();
                cx.defer(move |cx| {
                    for any_window in cx.windows() {
                        let Some(window_handle) = any_window.downcast::<Self>() else {
                            continue;
                        };
                        let Ok(window_entity) = window_handle.entity(cx) else {
                            continue;
                        };
                        if window_entity != source {
                            continue;
                        }
                        window_handle
                            .update(cx, |this, window, cx| match event {
                                TerminalTabEvent::CloseTerminal => {
                                    this.close_exited_pane(&pane, window, cx);
                                }
                                TerminalTabEvent::Bell { newly_notified } => {
                                    this.handle_bell(newly_notified, window, cx);
                                }
                            })
                            .log_err();
                        break;
                    }
                });
            }));
    }

    fn handle_bell(&mut self, newly_notified: bool, window: &mut Window, cx: &mut Context<Self>) {
        if TerminalSettings::get_global(cx).bell == TerminalBell::System {
            window.play_system_bell();
        }
        if newly_notified && !window.is_window_active() {
            window.request_attention();
        }
        cx.notify();
    }

    /// Activates the tab at `index` and keeps it visible in the tab bar even
    /// when the tabs overflow the bar's width (like Zed's `Pane::update_active_tab`).
    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab_index = index;
        self.tab_bar_scroll_handle.scroll_to_item(index);
        // Give the newly active tab's focused pane terminal focus so input
        // routes to it (each terminal tracks its own focus handle).
        if let Some(tab) = self.tabs.get(index)
            && let Some(pane_tab) = tab.focused_tab()
        {
            let focus = pane_tab.read(cx).focus_handle.clone();
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn reorder_tab(
        &mut self,
        dragged: &DraggedTerminalTab,
        target_pane: &Entity<TerminalTab>,
        cx: &mut Context<Self>,
    ) {
        if dragged.source_window != cx.entity_id() {
            return;
        }
        let Some(source_index) = self
            .tabs
            .iter()
            .position(|tab| tab.contains_pane(&dragged.pane))
        else {
            return;
        };
        let Some(target_index) = self
            .tabs
            .iter()
            .position(|tab| tab.contains_pane(target_pane))
        else {
            return;
        };
        if source_index == target_index {
            return;
        }

        let active_pane = self
            .tabs
            .get(self.active_tab_index)
            .and_then(WindowTab::focused_tab);
        let moved_tab = self.tabs.remove(source_index);
        let destination_index = target_index.min(self.tabs.len());
        self.tabs.insert(destination_index, moved_tab);
        self.active_tab_index = active_pane
            .and_then(|active_pane| {
                self.tabs
                    .iter()
                    .position(|tab| tab.contains_pane(&active_pane))
            })
            .unwrap_or_else(|| self.active_tab_index.min(self.tabs.len().saturating_sub(1)));
        self.tab_bar_scroll_handle
            .scroll_to_item(self.active_tab_index);
        cx.notify();
    }

    /// Runs `f` against the active tab's focused pane. Actions like
    /// copy/paste/scroll target whichever pane the user is actually typing in.
    fn with_active_tab(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TerminalTab, &mut Context<TerminalTab>),
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        tab.update(cx, f);
    }

    fn with_focused_tab(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TerminalTab, &mut Context<TerminalTab>),
    ) {
        let Some(tab) = self.focused_tab(window, cx) else {
            return;
        };
        if let Some(active) = self.tabs.get_mut(self.active_tab_index) {
            active.active_pane_tab = Some(tab.clone());
        }
        tab.update(cx, f);
    }

    /// Returns the focused pane's terminal of the active tab.
    pub(crate) fn active_tab(&self) -> Option<Entity<TerminalTab>> {
        self.tabs
            .get(self.active_tab_index)
            .and_then(|t| t.focused_tab())
    }

    /// Returns the terminal whose focus handle actually holds window focus
    /// (within the active tab). Falls back to the active tab's focused pane.
    fn focused_tab(&self, window: &Window, cx: &Context<Self>) -> Option<Entity<TerminalTab>> {
        let focused = window.focused(cx);
        let focused = focused.as_ref();
        let active = self.tabs.get(self.active_tab_index)?;
        let mut leaves = Vec::new();
        if let Some(root) = active.split_root.as_ref() {
            root.collect_tabs(&mut leaves);
        } else if let Some(pane) = active.active_pane_tab.as_ref() {
            leaves.push(pane);
        }
        let by_focus = leaves
            .into_iter()
            .find(|pane| Some(&pane.read(cx).focus_handle) == focused)
            .cloned();
        by_focus.or_else(|| active.focused_tab())
    }

    // Terminal actions (§4.4): forward to the active tab. `Terminal` handles
    // the actual clipboard IO (OSC52 + `InternalEvent::Copy`).

    fn copy(&mut self, _: &TerminalCopyAction, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_active_tab(cx, |tab, cx| tab.copy_selection(cx));
    }

    fn paste(&mut self, _: &TerminalPasteAction, window: &mut Window, cx: &mut Context<Self>) {
        self.with_focused_tab(window, cx, |tab, cx| tab.paste_clipboard(cx));
    }

    fn paste_text(
        &mut self,
        _: &TerminalPasteTextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_focused_tab(window, cx, |tab, cx| tab.paste_clipboard(cx));
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
        let Some(tab) = self.active_tab() else {
            return;
        };
        tab.update(cx, |tab, cx| tab.show_character_palette(window, cx));
    }

    fn new_tab(&mut self, _: &NewTab, _window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_new_tab(cx);
    }

    // Split panes (G1). Each tab owns its own split-pane group; cmd-d splits
    // within the active tab (mirroring Zed's `pane::Split*`).

    /// Returns the terminal of the focused pane in the active tab.
    fn focused_pane_tab(&self) -> Option<Entity<TerminalTab>> {
        self.tabs
            .get(self.active_tab_index)
            .and_then(|t| t.focused_tab())
    }

    fn split_pane(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(anchor_tab) = self.focused_pane_tab() else {
            // No pane yet (first terminal still spawning); a split cannot be
            // placed until a pane exists.
            return;
        };
        // Ensure the active tab has a split root, promoting its single pane
        // if this is the first split within it.
        let active_index = self.active_tab_index;
        if self.tabs[active_index].split_root.is_none() {
            self.tabs[active_index].split_root = Some(SplitNode::leaf(anchor_tab.clone()));
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let builder = match build_terminal(cx).await {
                Ok(builder) => builder,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.terminal_error = Some(format!("Could not start terminal: {error:#}"));
                        cx.notify();
                    })
                    .log_err();
                    return;
                }
            };
            let terminal = cx.new(|cx| builder.subscribe(cx));
            let tab = cx.new(|cx| TerminalTab::new(terminal, cx));
            let focus_handle = tab.read_with(cx, |tab, _| tab.focus_handle.clone());
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    this.terminal_error = None;
                    this.observe_tab(&tab, cx);
                    // The split lives inside the active tab's pane group only;
                    // it never becomes a separate tab in the tab bar.
                    if let Some(active) = this.tabs.get_mut(active_index)
                        && let Some(root) = active.split_root.as_mut()
                        && root.split(&anchor_tab, tab.clone(), direction)
                    {
                        active.active_pane_tab = Some(tab);
                        active.maximized_pane_tab = None;
                    } else {
                        // The active tab or anchor vanished meanwhile; fall
                        // back to a normal tab so the shell is never lost.
                        this.tabs.push(WindowTab::new(tab));
                        this.active_tab_index = this.tabs.len() - 1;
                    }
                    Self::focus_handle_after_update(focus_handle, cx);
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

    /// Cycles pane focus through the active tab's split tree in visual order.
    fn activate_adjacent_pane(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_index = self.active_tab_index;
        let Some(active) = self.tabs.get(active_index) else {
            return;
        };
        let Some(root) = active.split_root.as_ref() else {
            return;
        };
        let mut leaves = Vec::new();
        root.collect_tabs(&mut leaves);
        if leaves.len() < 2 {
            return;
        }
        let current = leaves
            .iter()
            .position(|pane| Some(*pane) == active.active_pane_tab.as_ref());
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
        self.tabs[active_index].active_pane_tab = Some(leaves[next].clone());
        self.tabs[active_index].maximized_pane_tab = None;
        self.focus_pane(window, cx);
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

    fn toggle_zoom(&mut self, _: &ToggleZoom, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.focused_tab(window, cx) else {
            return;
        };
        let Some(active) = self.tabs.get_mut(self.active_tab_index) else {
            return;
        };
        let Some(root) = active.split_root.as_ref() else {
            return;
        };
        if root.leaf_count() < 2 || !root.contains_tab(&pane) {
            return;
        }

        active.active_pane_tab = Some(pane.clone());
        if active.maximized_pane_tab.as_ref() == Some(&pane) {
            active.maximized_pane_tab = None;
        } else {
            active.maximized_pane_tab = Some(pane.clone());
        }

        pane.read(cx).focus_handle.clone().focus(window, cx);
        cx.notify();
    }

    /// Focuses the terminal of the active tab's focused pane.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let focus_handle = tab.read(cx).focus_handle.clone();
        focus_handle.focus(window, cx);
    }

    /// Removes the focused pane from the active tab's split tree, collapsing
    /// the tree like Zed's `PaneAxis::remove`. If it was that tab's last pane,
    /// the tab is closed (and the window closes on the last tab).
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(anchor) = self.active_tab() else {
            return;
        };
        self.remove_pane(&anchor, window, cx);
    }

    /// Removes `anchor` (a specific pane) from whichever tab houses it,
    /// collapsing the tree like Zed's `PaneAxis::remove`. If it was that tab's
    /// last pane, the tab is closed (and the window closes on the last tab).
    /// Used both for `ClosePane` and for a shell that exited on its own.
    fn remove_pane(
        &mut self,
        anchor: &Entity<TerminalTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Locate the tab that contains this pane. A pane is a leaf in a tab's
        // split tree, or the only pane of a single-pane tab.
        let tab_index = self.tabs.iter().position(|tab| {
            if let Some(root) = tab.split_root.as_ref() {
                root.contains_tab(anchor)
            } else {
                tab.active_pane_tab
                    .as_ref()
                    .is_some_and(|pane| pane == anchor)
            }
        });
        let Some(tab_index) = tab_index else {
            return;
        };
        let was_active = tab_index == self.active_tab_index;
        let should_close_tab = {
            let Some(active) = self.tabs.get_mut(tab_index) else {
                return;
            };
            if let Some(root) = active.split_root.as_mut() {
                if let Some(replacement) = root.remove(anchor) {
                    *root = replacement;
                }
                let mut leaves = Vec::new();
                root.collect_tabs(&mut leaves);
                let surviving_panes = leaves.into_iter().cloned().collect::<Vec<_>>();
                if surviving_panes.is_empty() {
                    // This pane was the last one: the whole tab goes away.
                    true
                } else {
                    active.active_pane_tab = active
                        .active_pane_tab
                        .take()
                        .filter(|pane| surviving_panes.contains(pane))
                        .or_else(|| surviving_panes.last().cloned());
                    if active
                        .maximized_pane_tab
                        .as_ref()
                        .is_some_and(|pane| !surviving_panes.contains(pane))
                    {
                        active.maximized_pane_tab = None;
                    }
                    if surviving_panes.len() == 1 {
                        active.split_root = None;
                        active.maximized_pane_tab = None;
                    }
                    false
                }
            } else {
                // Single-pane tab: removing its only pane closes the tab.
                true
            }
        };
        if should_close_tab {
            self.tabs.remove(tab_index);
            if self.tabs.is_empty() {
                window.remove_window();
            } else {
                // If the removed tab sat before the active index, shift it.
                if tab_index < self.active_tab_index {
                    self.active_tab_index -= 1;
                }
                self.active_tab_index = self.active_tab_index.min(self.tabs.len() - 1);
                // Focus the surviving tab's pane so keyboard input lands there
                // instead of a dropped focus handle (only if the active tab
                // moved, so tabs behind the active one keep their focus).
                if was_active {
                    self.focus_pane(window, cx);
                }
            }
        } else if was_active {
            // The pane was removed but the tab lives on (split layout); the
            // closed pane's focus handle is gone. Move focus to the pane the
            // tree chose as the new active pane.
            self.focus_pane(window, cx);
        }
        cx.notify();
    }

    /// Removes the pane whose shell exited on its own, so a dead pane does not
    /// linger as an unresponsive terminal. Falls back to closing the pane (or,
    /// when it was its tab's only pane, the tab) without a confirmation prompt.
    fn close_exited_pane(
        &mut self,
        pane: &Entity<TerminalTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_pane(pane, window, cx);
    }

    /// Sends raw text straight to the PTY (the keymap's escape-sequence
    /// conveniences, e.g. `alt-delete` → ESC d).
    fn send_text(
        &mut self,
        SendText(text): &SendText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        self.with_focused_tab(window, cx, |tab, cx| {
            tab.clear_bell(cx);
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(keystroke) = gpui::Keystroke::parse(key) else {
            log::warn!("Invalid SendKeystroke binding: {key:?}");
            return;
        };
        let option_as_meta = TerminalSettings::get_global(cx).option_as_meta;
        self.with_focused_tab(window, cx, |tab, cx| {
            tab.clear_bell(cx);
            tab.terminal.update(cx, |term, _| {
                term.try_keystroke(&keystroke, option_as_meta);
            });
        });
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        // `CloseTab` (cmd-w), the tab-bar close button and the tab context
        // menu all close the whole tab. When a tab has multiple panes the
        // user confirms first; closing a split tab tears down every pane.
        self.close_tab_entire(window, cx);
    }

    /// Closes the active tab as a whole (all of its panes). If the tab has
    /// multiple panes (a split layout), the user is first asked to confirm;
    /// closing a split tab tears down every pane and the tab itself.
    fn close_tab_entire(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.tabs.get(self.active_tab_index) else {
            return;
        };
        if active.is_split_layout() {
            // Closing a split tab kills every pane in it; confirm first so the
            // user does not lose a whole shell group by accident.
            let pane_count = {
                let mut leaves = Vec::new();
                if let Some(root) = active.split_root.as_ref() {
                    root.collect_tabs(&mut leaves);
                }
                leaves.len()
            };
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                let Some(window) = cx.update(|cx| cx.active_window()) else {
                    return;
                };
                // `window.prompt` returns a oneshot receiver; resolve it and
                // only proceed to tear down the tab when the user confirms.
                let answer = match window.update(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        &format!("Close terminal tab with {pane_count} panes?"),
                        None,
                        &["Close All", "Cancel"],
                        cx,
                    )
                }) {
                    Ok(receiver) => receiver.await,
                    Err(_) => return,
                };
                match answer {
                    Ok(0) => {}
                    _ => return,
                }
                window
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| this.close_tab_entire_inner(window, cx))
                            .log_err();
                    })
                    .log_err();
            })
            .detach();
        } else {
            self.close_tab_entire_inner(window, cx);
        }
    }

    /// Removes the active tab wholesale, regardless of how many panes it has.
    fn close_tab_entire_inner(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            window.remove_window();
            return;
        }
        if self.tabs.len() == 1 {
            self.tabs.clear();
            window.remove_window();
        } else {
            self.tabs.remove(self.active_tab_index);
            self.active_tab_index = self.active_tab_index.min(self.tabs.len() - 1);
            // Refocus the surviving tab's terminal; the closed tab's focus
            // handle is gone and keyboard input must land in a live pane.
            self.focus_pane(window, cx);
            cx.notify();
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let index = (self.active_tab_index + 1) % self.tabs.len();
        self.activate_tab(index, window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let index = (self.active_tab_index + self.tabs.len() - 1) % self.tabs.len();
        self.activate_tab(index, window, cx);
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
        let active_index = self.active_tab_index;
        if self.tabs.len() <= 1 {
            return;
        }
        // Keep only the active tab (drain others). `drain`-based removal
        // keeps the active index valid as index 0 afterward.
        let mut kept = Vec::new();
        if let Some(active) = self.tabs.get(active_index).cloned() {
            kept.push(active);
        }
        self.tabs = kept;
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
        let Some(tab) = self.active_tab() else {
            return;
        };
        let (in_split_layout, is_pane_maximized) = self
            .tabs
            .get(self.active_tab_index)
            .map(|tab| (tab.is_split_layout(), tab.is_pane_maximized()))
            .unwrap_or((false, false));
        let focus_handle = tab.read(cx).focus_handle.clone();
        self.show_context_menu(position, window, cx, |menu, _, _| {
            // Split entries are always shown so a pane can be split again
            // (into a horizontal/vertical pair) from any pane, not just one
            // that already has splits. `Split*` actions target the active
            // pane the right-click selected.
            let menu = menu
                .context(focus_handle)
                .action("New Terminal", Box::new(NewTab))
                .separator()
                .action("Split Right", Box::new(SplitRight))
                .action("Split Down", Box::new(SplitDown))
                .separator();
            let menu = if in_split_layout {
                // Zoom keeps the split tree alive but lets the selected pane
                // temporarily occupy the full terminal content area.
                let zoom_label = if is_pane_maximized {
                    "Restore Panes"
                } else {
                    "Zoom Pane"
                };
                menu.action(zoom_label, Box::new(ToggleZoom))
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
                // Single-pane tab (no split): the only close entry is the tab.
                // In split layout `Close Pane` above is the per-pane close.
                .when(!in_split_layout, |menu| {
                    menu.action("Close Terminal Tab", Box::new(CloseTab))
                })
                .separator()
                .action("Settings", Box::new(OpenSettings))
        });
    }

    /// Right-click menu over a tab in the tab bar.
    fn deploy_tab_context_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let focus_handle = tab.read(cx).focus_handle.clone();
        self.show_context_menu(position, window, cx, |menu, _, _| {
            menu.context(focus_handle)
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
    /// mouse-down, `start_window_move` on drag; interactive children stop
    /// propagation so presses on them don't move the window.
    fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let titlebar_height = window_chrome::TITLE_BAR_HEIGHT;
        let titlebar_background = cx.theme().colors().tab_bar_background;
        let is_server_decorations = matches!(window.window_decorations(), Decorations::Server);
        let close_window = cx.listener(|this, _: &ClickEvent, window, cx| {
            this.close_all(&CloseAll, window, cx);
        });
        let (left_controls, right_controls) =
            window_chrome::render_window_controls(window, cx, close_window);
        let tab_bar = self.render_tab_bar(cx);
        let tab_bar = if is_server_decorations {
            tab_bar
        } else {
            tab_bar.start_child(left_controls).end_child(right_controls)
        };
        div()
            .id("title-bar")
            .h(titlebar_height)
            .flex()
            .flex_row()
            .flex_none()
            .when(is_server_decorations, |this| {
                this.pl(px(TRAFFIC_LIGHT_PADDING))
            })
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
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(tab_bar),
            )
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        TabBar::new("window-tabs")
            .height(window_chrome::TITLE_BAR_HEIGHT)
            .track_scroll(&self.tab_bar_scroll_handle)
            .children(
                // In split layout the tab bar shows a single synthetic tab
                // that follows the focused pane; split panes do not become
                // separate tabs (per product decision).
                self.render_tab_children(cx),
            )
            .child(
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
                                this.spawn_new_tab(cx);
                            })),
                    ),
            )
    }

    /// The tab bar's tab columns, one per open tab. Each tab's title comes
    /// from its focused pane (a tab is a pane group; the focused pane gives
    /// its title).
    fn render_tab_children(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let tab_count = self.tabs.len();
        let source_window = cx.entity_id();
        let source_view = cx.weak_entity();
        self.tabs
            .iter()
            .enumerate()
            .map(|(idx, tab)| {
                let has_bell = tab.has_bell(cx);
                let pane = tab.focused_tab();
                let title = pane
                    .as_ref()
                    .map(|pane| pane.read(cx).title(cx))
                    .unwrap_or_else(|| "Terminal".to_string());
                let title = util::truncate_and_trailoff(&title, TAB_TITLE_MAX_CHARS);
                let dragged_tab = pane.map(|pane| DraggedTerminalTab {
                    source_window,
                    source_view: source_view.clone(),
                    pane,
                    title: title.clone(),
                    is_active: idx == self.active_tab_index,
                });
                let position = if idx == 0 {
                    TabPosition::First
                } else if idx == tab_count - 1 {
                    TabPosition::Last
                } else {
                    TabPosition::Middle(Ordering::Equal)
                };
                let tab_idx = idx;
                Tab::new(idx.to_string())
                    .height(window_chrome::TITLE_BAR_HEIGHT)
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_tab(tab_idx, window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.activate_tab(tab_idx, window, cx);
                            this.deploy_tab_context_menu(event.position, window, cx);
                            cx.notify();
                            // Stop so the window root's right-click
                            // handler does not replace this with the
                            // terminal (copy/paste) menu.
                            cx.stop_propagation();
                        }),
                    )
                    .start_slot::<Indicator>(
                        has_bell.then(|| Indicator::dot().color(Color::Accent)),
                    )
                    .child(self.render_tab_label(title))
                    .end_slot(
                        // Per-tab close button. Clicking a tab's x closes that
                        // whole tab (confirming first when it holds a split).
                        // The wrapping div stops the press from starting the
                        // titlebar-drag gesture and from toggling the tab.
                        div()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                IconButton::new(format!("close-tab-{tab_idx}"), IconName::Close)
                                    .icon_size(IconSize::XSmall)
                                    .on_click(cx.listener(
                                        move |this, _: &gpui::ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.activate_tab(tab_idx, window, cx);
                                            this.close_tab_entire(window, cx);
                                        },
                                    )),
                            ),
                    )
                    .when_some(dragged_tab, |this, dragged_tab| {
                        let target_pane = dragged_tab.pane.clone();
                        let drag_over_target_pane = target_pane.clone();
                        let source_window = dragged_tab.source_window;
                        this.on_drag(dragged_tab, |dragged_tab, _, _, cx| {
                            cx.new(|_| dragged_tab.clone())
                        })
                        .drag_over::<DraggedTerminalTab>(move |style, dragged_tab, _window, cx| {
                            let style = style
                                .bg(cx.theme().colors().drop_target_background)
                                .border_color(cx.theme().colors().drop_target_border)
                                .border_0();
                            match dragged_tab.ordering_relative_to(&drag_over_target_pane, cx) {
                                Some(Ordering::Less) => style.border_l_2(),
                                Some(Ordering::Greater) => style.border_r_2(),
                                Some(Ordering::Equal) | None => style,
                            }
                        })
                        .can_drop(move |value, _window, _cx| {
                            value
                                .downcast_ref::<DraggedTerminalTab>()
                                .is_some_and(|dragged_tab| {
                                    dragged_tab.source_window == source_window
                                })
                        })
                        .on_drop(cx.listener(
                            move |this, dragged_tab: &DraggedTerminalTab, _window, cx| {
                                this.reorder_tab(dragged_tab, &target_pane, cx);
                            },
                        ))
                    })
                    .into_any_element()
            })
            .collect()
    }

    /// A truncated, single-line label for a tab.
    fn render_tab_label(&self, title: String) -> impl IntoElement {
        div().w(TAB_TITLE_WIDTH).child(
            Label::new(title)
                .single_line()
                .truncate()
                .size(LabelSize::Small),
        )
    }
}

fn render_terminal_pane(
    terminal: Entity<Terminal>,
    tab: Entity<TerminalTab>,
    focus: FocusHandle,
    window: &mut Window,
    cx: &mut Context<TerminalWindowView>,
) -> AnyElement {
    let cursor_visible = tab.read(cx).cursor_visible(cx);
    let scroll_handle = tab.read(cx).scroll_handle.clone();
    let scrollbar_id = tab.entity_id();
    let colors = cx.theme().colors();
    div()
        .id(("terminal-pane", tab.entity_id()))
        .relative()
        .size_full()
        .bg(colors.terminal_background)
        .child(TerminalElement::new(
            terminal,
            tab,
            focus.clone(),
            focus.is_focused(window),
            cursor_visible,
        ))
        .custom_scrollbars(
            Scrollbars::for_settings::<TerminalScrollbarSettingsWrapper>()
                .id(("terminal-scrollbar", scrollbar_id))
                .show_along(ScrollAxes::Vertical)
                .tracked_scroll_handle(&scroll_handle),
            window,
            cx,
        )
        .into_any_element()
}

impl Render for TerminalWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Content area: the active tab's split tree, a zoomed pane, or a
        // single terminal when that tab has no splits yet. Zoom does not
        // mutate the tree, so restoring it preserves divider positions.
        let active_tab = self.active_tab();
        let active_window_tab = self.tabs.get(self.active_tab_index);
        let in_split_layout = active_window_tab
            .map(WindowTab::is_split_layout)
            .unwrap_or(false);
        let maximized_pane = active_window_tab.and_then(|tab| {
            tab.maximized_pane_tab.clone().filter(|pane| {
                tab.split_root
                    .as_ref()
                    .is_some_and(|root| root.contains_tab(pane))
            })
        });
        let search_active = active_tab
            .as_ref()
            .map(|pane| pane.read(cx).search_active)
            .unwrap_or(false);
        let content = if let Some(tab) = maximized_pane {
            let terminal = tab.read(cx).terminal.clone();
            let focus = tab.read(cx).focus_handle.clone();
            div()
                .relative()
                .size_full()
                .child(render_terminal_pane(terminal, tab, focus, window, cx))
                .child(
                    div().absolute().top_2().right_2().child(
                        IconButton::new("restore-zoomed-pane", IconName::Minimize)
                            .icon_size(IconSize::Small)
                            .tooltip(|_window, cx| {
                                Tooltip::for_action("Restore Panes", &ToggleZoom, cx)
                            })
                            .on_click(cx.listener(|this, _: &gpui::ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.toggle_zoom(&ToggleZoom, window, cx);
                            })),
                    ),
                )
                .into_any_element()
        } else if self
            .tabs
            .get(self.active_tab_index)
            .and_then(|tab| tab.split_root.as_ref())
            .is_some()
        {
            let mut pane_tabs: Vec<Entity<TerminalTab>> = Vec::new();
            if let Some(root) = self
                .tabs
                .get(self.active_tab_index)
                .and_then(|tab| tab.split_root.as_ref())
            {
                let mut pane_out: Vec<&Entity<TerminalTab>> = Vec::new();
                root.collect_tabs(&mut pane_out);
                pane_tabs.extend(pane_out.into_iter().cloned());
            }

            let pane_elements: Vec<_> = pane_tabs
                .into_iter()
                .map(|tab| {
                    let terminal = tab.read(cx).terminal.clone();
                    let focus = tab.read(cx).focus_handle.clone();
                    render_terminal_pane(terminal, tab, focus, window, cx)
                })
                .collect();
            let mut pane_elements = pane_elements.into_iter();
            self.tabs
                .get_mut(self.active_tab_index)
                .and_then(|tab| tab.split_root.as_mut())
                .map(|root| {
                    root.render(&[], window, cx, &mut |_pane| {
                        pane_elements
                            .next()
                            .unwrap_or_else(|| div().into_any_element())
                    })
                })
                .unwrap_or_else(|| div().into_any_element())
        } else {
            active_tab
                .clone()
                .map(|tab| {
                    let terminal = tab.read(cx).terminal.clone();
                    let focus = tab.read(cx).focus_handle.clone();
                    render_terminal_pane(terminal, tab, focus, window, cx)
                })
                .unwrap_or_else(|| {
                    div()
                        .size_full()
                        .child("Starting terminal…")
                        .into_any_element()
                })
        };

        window_chrome::client_side_decorations(
            div()
                .id("terminal-window")
                .key_context("TerminalWindow")
                .track_focus(&self.focus_handle)
                .size_full()
                .flex()
                .flex_col()
                .bg(cx.theme().colors().terminal_background)
                .child(self.render_title_bar(window, cx))
                .when_some(self.terminal_error.clone(), |this, error| {
                    this.child(
                        div()
                            .px_3()
                            .py_2()
                            .bg(cx.theme().colors().element_background)
                            .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                    )
                })
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
                .on_action(cx.listener(Self::toggle_zoom))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    // Titlebar drag gesture first (it owns the flag-based latch).
                    if this.titlebar_mouse_down.get() {
                        this.titlebar_mouse_down.set(false);
                        window.start_window_move();
                        return;
                    }
                    // Divider drag keeps the selected divider and its local pointer
                    // anchor until the button is released.
                    if let (Some(anchor), Some(divider)) =
                        (split::drag_anchor(), split::drag_divider())
                    {
                        let is_horizontal = divider.axis() == gpui::Axis::Horizontal;
                        let position = if is_horizontal {
                            event.position.x
                        } else {
                            event.position.y
                        };
                        let viewport = window.viewport_size();
                        let root_size = gpui::Size::new(
                            viewport.width,
                            (viewport.height - window_chrome::TITLE_BAR_HEIGHT).max(px(0.)),
                        );
                        if let Some(root) = this
                            .tabs
                            .get_mut(this.active_tab_index)
                            .and_then(|tab| tab.split_root.as_mut())
                        {
                            if root.resize_divider(&divider, root_size, position - anchor) {
                                split::update_drag_anchor(position);
                                window.refresh();
                                cx.notify();
                            } else {
                                split::take_drag_anchor();
                            }
                        }
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseUpEvent, _window, _cx| {
                        split::take_drag_anchor();
                    }),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseUpEvent, _window, _cx| {
                        split::take_drag_anchor();
                    }),
                )
                .on_mouse_exit(cx.listener(|_, _: &MouseExitEvent, _window, _cx| {
                    split::take_drag_anchor();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // In split layout the right-click may land on any pane;
                        // make the clicked leaf the active tab's focused pane so
                        // the menu (and its split/closing actions) act on it.
                        if let Some(active) = this.tabs.get_mut(this.active_tab_index)
                            && active.is_split_layout()
                            && let Some(clicked) = split::take_clicked_leaf()
                        {
                            active.active_pane_tab = Some(clicked.clone());
                            if let Some(pane) = active.active_pane_tab.as_ref() {
                                let focus = pane.read(cx).focus_handle.clone();
                                focus.focus(window, cx);
                            }
                        }
                        let Some(tab) = this.active_tab() else {
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
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    // Keys that no keymap binding consumed (Enter, Tab, arrows,
                    // Ctrl+C, ...) are translated to ANSI and sent to the pty,
                    // mirroring Zed's `TerminalView::key_down`. Targets the pane
                    // actually holding focus so input lands where the user typed.
                    let Some(tab) = this.focused_tab(window, cx) else {
                        return;
                    };
                    let handled = tab.update(cx, |tab, cx| {
                        tab.clear_bell(cx);
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
                })),
            window,
            cx,
        )
    }
}

/// Builds a PTY-backed terminal on the foreground executor.
#[cfg(test)]
struct TestTerminalLaunchFailure {
    reason: String,
}

#[cfg(test)]
impl gpui::Global for TestTerminalLaunchFailure {}

async fn build_terminal(cx: &gpui::AsyncApp) -> anyhow::Result<TerminalBuilder> {
    let settings = cx.update(|cx| TerminalSettings::get_global(cx).clone());
    let builder = cx.update(|cx| {
        launch_terminal_with(settings, cx, |launch, cx| {
            #[cfg(test)]
            if let Some(failure) = cx.try_global::<TestTerminalLaunchFailure>() {
                return gpui::Task::ready(Err(anyhow::anyhow!(failure.reason.clone())));
            }

            TerminalBuilder::new(
                launch.working_directory,
                launch.shell,
                launch.environment,
                launch.cursor_shape,
                launch.alternate_scroll,
                launch.max_scroll_history_lines,
                launch.path_hyperlink_regexes,
                launch.path_hyperlink_timeout,
                false,
                0,
                None,
                cx,
                Vec::new(),
                PathStyle::local(),
            )
        })
    });
    builder.await
}

struct TerminalLaunchArguments {
    working_directory: Option<PathBuf>,
    shell: util::shell::Shell,
    environment: collections::HashMap<String, String>,
    cursor_shape: terminal_core::terminal_settings::CursorShape,
    alternate_scroll: settings::AlternateScroll,
    max_scroll_history_lines: Option<usize>,
    path_hyperlink_regexes: Vec<String>,
    path_hyperlink_timeout: Duration,
}

fn launch_terminal_with<T>(
    settings: TerminalSettings,
    cx: &App,
    launcher: impl FnOnce(TerminalLaunchArguments, &App) -> T,
) -> T {
    let launch = TerminalLaunchArguments {
        working_directory: standalone_working_directory(&settings.working_directory),
        shell: settings.shell,
        environment: settings.env,
        cursor_shape: settings.cursor_shape,
        alternate_scroll: settings.alternate_scroll,
        max_scroll_history_lines: settings.max_scroll_history_lines,
        path_hyperlink_regexes: settings.path_hyperlink_regexes,
        path_hyperlink_timeout: Duration::from_millis(settings.path_hyperlink_timeout_ms),
    };
    launcher(launch, cx)
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FakeLaunchSnapshot {
    pub(crate) shell: util::shell::Shell,
    pub(crate) environment: collections::HashMap<String, String>,
    pub(crate) working_directory: Option<PathBuf>,
    pub(crate) max_scroll_history_lines: Option<usize>,
    pub(crate) cursor_shape: terminal_core::terminal_settings::CursorShape,
    pub(crate) alternate_scroll: settings::AlternateScroll,
}

#[cfg(test)]
pub(crate) fn fake_launch_snapshot(cx: &App) -> FakeLaunchSnapshot {
    let settings = TerminalSettings::get_global(cx).clone();
    launch_terminal_with(settings, cx, |launch, _| FakeLaunchSnapshot {
        shell: launch.shell,
        environment: launch.environment,
        working_directory: launch.working_directory,
        max_scroll_history_lines: launch.max_scroll_history_lines,
        cursor_shape: launch.cursor_shape,
        alternate_scroll: launch.alternate_scroll,
    })
}

#[cfg(test)]
pub(crate) fn fake_launch_shell_snapshot(cx: &App) -> util::shell::Shell {
    fake_launch_snapshot(cx).shell
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EffectiveLiveSettingsSnapshot {
    pub(crate) font_family: Option<String>,
    pub(crate) font_weight: Option<f32>,
    pub(crate) line_height: settings::TerminalLineHeight,
    pub(crate) minimum_contrast: f32,
    pub(crate) blinking: settings::TerminalBlink,
    pub(crate) scrollbar: Option<settings::ShowScrollbar>,
    pub(crate) option_as_meta: bool,
    pub(crate) copy_on_select: bool,
    pub(crate) keep_selection_on_copy: bool,
    pub(crate) open_links_in_mouse_mode: bool,
    pub(crate) scroll_multiplier: f32,
    pub(crate) bell: settings::TerminalBell,
    pub(crate) cursor_shape: terminal_core::terminal_settings::CursorShape,
    pub(crate) alternate_scroll: settings::AlternateScroll,
}

#[cfg(test)]
impl EffectiveLiveSettingsSnapshot {
    pub(crate) fn capture(cx: &App) -> Self {
        let settings = TerminalSettings::get_global(cx);
        Self {
            font_family: settings
                .font_family
                .as_ref()
                .map(|font_family| font_family.0.to_string()),
            font_weight: settings.font_weight.map(|font_weight| font_weight.0),
            line_height: settings.line_height.clone(),
            minimum_contrast: settings.minimum_contrast,
            blinking: settings.blinking,
            scrollbar: settings.scrollbar.show,
            option_as_meta: settings.option_as_meta,
            copy_on_select: settings.copy_on_select,
            keep_selection_on_copy: settings.keep_selection_on_copy,
            open_links_in_mouse_mode: settings.open_links_in_mouse_mode,
            scroll_multiplier: settings.scroll_multiplier,
            bell: settings.bell,
            cursor_shape: settings.cursor_shape,
            alternate_scroll: settings.alternate_scroll,
        }
    }
}

fn standalone_working_directory(working_directory: &settings::WorkingDirectory) -> Option<PathBuf> {
    match working_directory {
        settings::WorkingDirectory::Always { directory } => {
            let directory = expand_home_directory(directory);
            directory
                .is_dir()
                .then_some(directory)
                .or_else(|| Some(paths::home_dir().clone()))
        }
        settings::WorkingDirectory::AlwaysHome
        | settings::WorkingDirectory::CurrentFileDirectory
        | settings::WorkingDirectory::CurrentProjectDirectory
        | settings::WorkingDirectory::FirstProjectDirectory => Some(paths::home_dir().clone()),
    }
}

fn expand_home_directory(directory: &str) -> PathBuf {
    if directory == "~" {
        return paths::home_dir().clone();
    }
    if let Some(path) = directory.strip_prefix("~/") {
        return paths::home_dir().join(path);
    }
    PathBuf::from(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::UpdateGlobal as _;
    use proptest::strategy::Strategy as _;
    use settings::SettingsStore;

    const LAUNCH_FAILURE_REASON: &str = "injected launcher failure: executable unavailable";
    const VISIBLE_LAUNCH_FAILURE: &str =
        "Could not start terminal: injected launcher failure: executable unavailable";

    fn test_window_without_initial_launch(
        tabs: Vec<WindowTab>,
        active_tab_index: usize,
        cx: &mut App,
    ) -> Entity<TerminalWindowView> {
        cx.new(|cx| {
            let live_settings =
                LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
            let settings_subscription =
                cx.observe_global::<settings::SettingsStore>(TerminalWindowView::settings_changed);
            TerminalWindowView {
                focus_handle: cx.focus_handle(),
                tabs,
                active_tab_index,
                context_menu: None,
                titlebar_mouse_down: std::cell::Cell::new(false),
                tab_bar_scroll_handle: ScrollHandle::new(),
                terminal_error: None,
                live_settings,
                _subscriptions: vec![settings_subscription],
            }
        })
    }

    fn pane_entity_ids(window_view: &Entity<TerminalWindowView>, cx: &App) -> Vec<gpui::EntityId> {
        window_view
            .read(cx)
            .all_panes()
            .into_iter()
            .map(|pane| pane.entity_id())
            .collect()
    }

    /// **Validates: Requirements 4.10, 8.10**
    #[gpui::test]
    fn new_tab_launch_failure_is_visible_and_preserves_existing_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let window_view = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(TestTerminalLaunchFailure {
                reason: LAUNCH_FAILURE_REASON.to_string(),
            });
            let settings = TerminalSettings::get_global(cx).clone();
            let existing_pane = new_test_pane(&settings, 40_000, cx);
            test_window_without_initial_launch(vec![WindowTab::new(existing_pane)], 0, cx)
        });
        let (tab_count_before, panes_before) = cx.update(|cx| {
            (
                window_view.read(cx).tabs.len(),
                pane_entity_ids(&window_view, cx),
            )
        });

        cx.update(|cx| {
            window_view.update(cx, |window_view, cx| window_view.spawn_new_tab(cx));
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let state = window_view.read(cx);
            assert_eq!(
                state.terminal_error.as_deref(),
                Some(VISIBLE_LAUNCH_FAILURE)
            );
            assert_eq!(state.tabs.len(), tab_count_before);
            assert_eq!(pane_entity_ids(&window_view, cx), panes_before);
        });
    }

    /// **Validates: Requirements 4.10, 8.10**
    #[gpui::test]
    fn split_pane_launch_failure_is_visible_and_preserves_existing_split(
        cx: &mut gpui::TestAppContext,
    ) {
        let window_view = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            cx.set_global(TestTerminalLaunchFailure {
                reason: LAUNCH_FAILURE_REASON.to_string(),
            });
            let settings = TerminalSettings::get_global(cx).clone();
            let first_pane = new_test_pane(&settings, 50_000, cx);
            let second_pane = new_test_pane(&settings, 50_001, cx);
            let mut split_root = SplitNode::leaf(first_pane.clone());
            assert!(split_root.split(&first_pane, second_pane.clone(), SplitDirection::Right,));
            test_window_without_initial_launch(
                vec![WindowTab {
                    split_root: Some(split_root),
                    active_pane_tab: Some(second_pane),
                    maximized_pane_tab: None,
                }],
                0,
                cx,
            )
        });
        let (tab_count_before, panes_before, active_pane_before) = cx.update(|cx| {
            let state = window_view.read(cx);
            (
                state.tabs.len(),
                pane_entity_ids(&window_view, cx),
                state.active_tab().map(|pane| pane.entity_id()),
            )
        });

        cx.update(|cx| {
            window_view.update(cx, |window_view, cx| {
                window_view.split_pane(SplitDirection::Down, cx);
            });
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let state = window_view.read(cx);
            assert_eq!(
                state.terminal_error.as_deref(),
                Some(VISIBLE_LAUNCH_FAILURE)
            );
            assert_eq!(state.tabs.len(), tab_count_before);
            assert_eq!(pane_entity_ids(&window_view, cx), panes_before);
            assert_eq!(
                state.active_tab().map(|pane| pane.entity_id()),
                active_pane_before,
            );
            assert_eq!(
                state
                    .tabs
                    .first()
                    .and_then(|tab| tab.split_root.as_ref())
                    .map(SplitNode::leaf_count),
                Some(2),
            );
        });
    }

    #[derive(Clone, Debug)]
    struct GeneratedConstructionSettings {
        shell_seed: u8,
        environment_seed: u8,
        working_directory_variant: u8,
        max_scroll_history_lines: Option<usize>,
    }

    impl GeneratedConstructionSettings {
        fn for_revision(&self, revision: usize) -> Self {
            Self {
                shell_seed: self.shell_seed.wrapping_add(revision as u8),
                environment_seed: self.environment_seed.wrapping_add(revision as u8),
                working_directory_variant: self.working_directory_variant,
                max_scroll_history_lines: self.max_scroll_history_lines,
            }
        }

        fn shell(&self, revision: usize) -> settings::Shell {
            settings::Shell::WithArguments {
                program: format!("audit-shell-{}-{revision}", self.shell_seed),
                args: vec![
                    format!("--environment-seed={}", self.environment_seed),
                    format!("--revision={revision}"),
                ],
                title_override: Some(format!("Audit {revision}")),
            }
        }

        fn environment(&self, revision: usize) -> collections::HashMap<String, String> {
            collections::HashMap::from_iter([
                (
                    "AUDIT_ENVIRONMENT_SEED".to_string(),
                    self.environment_seed.to_string(),
                ),
                ("AUDIT_REVISION".to_string(), revision.to_string()),
                (
                    "VALUE_WITH_EQUALS".to_string(),
                    "left=middle=right".to_string(),
                ),
            ])
        }

        fn working_directory(&self) -> settings::WorkingDirectory {
            match self.working_directory_variant % 3 {
                0 => settings::WorkingDirectory::AlwaysHome,
                1 => settings::WorkingDirectory::Always {
                    directory: paths::home_dir().to_string_lossy().into_owned(),
                },
                _ => {
                    let temporary_directory = std::env::temp_dir();
                    let directory = if temporary_directory.is_dir() {
                        temporary_directory
                    } else {
                        paths::home_dir().clone()
                    };
                    settings::WorkingDirectory::Always {
                        directory: directory.to_string_lossy().into_owned(),
                    }
                }
            }
        }

        fn cursor_shape(revision: usize) -> terminal_core::terminal_settings::CursorShape {
            match revision % 4 {
                0 => terminal_core::terminal_settings::CursorShape::Block,
                1 => terminal_core::terminal_settings::CursorShape::Underline,
                2 => terminal_core::terminal_settings::CursorShape::Bar,
                _ => terminal_core::terminal_settings::CursorShape::Hollow,
            }
        }

        fn alternate_scroll(revision: usize) -> settings::AlternateScroll {
            if revision.is_multiple_of(2) {
                settings::AlternateScroll::On
            } else {
                settings::AlternateScroll::Off
            }
        }
    }

    fn construction_settings_strategy()
    -> impl proptest::strategy::Strategy<Value = GeneratedConstructionSettings> {
        (
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::option::of(0_usize..=terminal_core::MAX_SCROLL_HISTORY_LINES),
        )
            .prop_map(
                |(
                    shell_seed,
                    environment_seed,
                    working_directory_variant,
                    max_scroll_history_lines,
                )| GeneratedConstructionSettings {
                    shell_seed,
                    environment_seed,
                    working_directory_variant,
                    max_scroll_history_lines,
                },
            )
    }

    fn update_construction_settings(
        cx: &mut App,
        values: &GeneratedConstructionSettings,
        revision: usize,
        cursor_shape: terminal_core::terminal_settings::CursorShape,
        alternate_scroll: settings::AlternateScroll,
    ) {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |content| {
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                terminal.project.shell = Some(values.shell(revision));
                terminal.project.env = Some(values.environment(revision));
                terminal.project.working_directory = Some(values.working_directory());
                terminal.max_scroll_history_lines = values.max_scroll_history_lines;
                terminal.cursor_shape = Some(cursor_shape_content(cursor_shape));
                terminal.alternate_scroll = Some(alternate_scroll);
            });
        });
    }

    fn assert_existing_panes_have_live_values(
        window_view: &Entity<TerminalWindowView>,
        cursor_shape: terminal_core::terminal_settings::CursorShape,
        alternate_scroll: settings::AlternateScroll,
        cx: &App,
    ) {
        for pane in window_view.read(cx).all_panes() {
            let terminal = pane.read(cx).terminal.clone();
            assert_eq!(
                terminal.read(cx).live_settings_snapshot_for_test(),
                (Some(cursor_shape), alternate_scroll),
                "existing pane {:?} did not receive the live projection",
                pane.entity_id(),
            );
        }
    }

    fn effective_construction_projection(cx: &App) -> FakeLaunchSnapshot {
        let settings = TerminalSettings::get_global(cx);
        FakeLaunchSnapshot {
            shell: settings.shell.clone(),
            environment: settings.env.clone(),
            working_directory: standalone_working_directory(&settings.working_directory),
            max_scroll_history_lines: settings.max_scroll_history_lines,
            cursor_shape: settings.cursor_shape,
            alternate_scroll: settings.alternate_scroll,
        }
    }

    /// Feature: terminal-settings-completion, Property 10: Construction settings affect only subsequent panes
    /// **Validates: Requirements 4.6–4.9, 5.9, 8.3**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn construction_settings_affect_only_subsequent_panes(
        cx: &mut gpui::TestAppContext,
        #[strategy = proptest::collection::vec(construction_settings_strategy(), 2..7)]
        generated_sequence: Vec<GeneratedConstructionSettings>,
    ) {
        let Some(initial_source) = generated_sequence.first() else {
            return;
        };
        let (default_launch, window_view, initial_pane_construction, mut launched_panes) = cx
            .update(|cx| {
                let settings_store = SettingsStore::test(cx);
                cx.set_global(settings_store);
                let default_launch = fake_launch_snapshot(cx);
                assert_eq!(default_launch, effective_construction_projection(cx));

                let initial = initial_source.for_revision(0);
                update_construction_settings(
                    cx,
                    &initial,
                    0,
                    GeneratedConstructionSettings::cursor_shape(0),
                    GeneratedConstructionSettings::alternate_scroll(0),
                );
                let window_view = test_window_with_panes(1, 2, 30_000, cx);
                let panes = window_view.read(cx).all_panes();
                let initial_pane_construction = pane_invariant_snapshots(&panes, cx);
                let initial_launch = fake_launch_snapshot(cx);
                assert_eq!(initial_launch, effective_construction_projection(cx));
                (
                    default_launch,
                    window_view,
                    initial_pane_construction,
                    vec![initial_launch],
                )
            });

        for (revision, generated) in generated_sequence.iter().enumerate().skip(1) {
            let generated = generated.for_revision(revision);
            let existing_panes_before_save = launched_panes.clone();
            let cursor_shape = GeneratedConstructionSettings::cursor_shape(revision);
            let alternate_scroll = GeneratedConstructionSettings::alternate_scroll(revision);
            cx.update(|cx| {
                update_construction_settings(
                    cx,
                    &generated,
                    revision,
                    cursor_shape,
                    alternate_scroll,
                );
            });
            let new_pane = cx.update(|cx| {
                assert_existing_panes_have_live_values(
                    &window_view,
                    cursor_shape,
                    alternate_scroll,
                    cx,
                );
                let new_pane = fake_launch_snapshot(cx);
                assert_eq!(new_pane, effective_construction_projection(cx));
                new_pane
            });

            assert_eq!(
                launched_panes, existing_panes_before_save,
                "save at revision {revision} changed an existing pane's immutable launch snapshot",
            );
            assert_eq!(new_pane.cursor_shape, cursor_shape);
            assert_eq!(new_pane.alternate_scroll, alternate_scroll);
            assert_eq!(
                new_pane.environment.get("AUDIT_REVISION"),
                Some(&revision.to_string()),
            );
            launched_panes.push(new_pane);
        }

        let pre_reset_revision = generated_sequence.len();
        let Some(pre_reset_source) = generated_sequence.last() else {
            return;
        };
        let pre_reset = pre_reset_source.for_revision(pre_reset_revision);
        let pre_reset_cursor_shape = different_cursor_shape(default_launch.cursor_shape);
        let pre_reset_alternate_scroll =
            different_alternate_scroll(default_launch.alternate_scroll);
        let existing_panes_before_reset = launched_panes.clone();
        cx.update(|cx| {
            update_construction_settings(
                cx,
                &pre_reset,
                pre_reset_revision,
                pre_reset_cursor_shape,
                pre_reset_alternate_scroll,
            );
        });
        let pre_reset_launch = cx.update(|cx| {
            assert_existing_panes_have_live_values(
                &window_view,
                pre_reset_cursor_shape,
                pre_reset_alternate_scroll,
                cx,
            );
            let pre_reset_launch = fake_launch_snapshot(cx);
            assert_eq!(pre_reset_launch, effective_construction_projection(cx));
            pre_reset_launch
        });
        launched_panes.push(pre_reset_launch);

        cx.update(reset_terminal_settings);
        let reset_launch = cx.update(|cx| {
            assert_existing_panes_have_live_values(
                &window_view,
                default_launch.cursor_shape,
                default_launch.alternate_scroll,
                cx,
            );
            let reset_launch = fake_launch_snapshot(cx);
            assert_eq!(reset_launch, effective_construction_projection(cx));
            reset_launch
        });

        assert!(
            launched_panes
                .iter()
                .take(existing_panes_before_reset.len())
                .eq(existing_panes_before_reset.iter()),
            "Reset changed an existing pane's immutable launch snapshot",
        );
        assert_eq!(
            reset_launch, default_launch,
            "the first pane after Reset did not use Default_Settings_Source construction values",
        );
        cx.update(|cx| {
            let panes = window_view.read(cx).all_panes();
            assert_eq!(
                pane_invariant_snapshots(&panes, cx),
                initial_pane_construction,
                "construction or identity of an existing pane changed after save/reset",
            );
        });
    }

    const GLOBAL_LIVE_SETTING_PATHS: [(&str, &str); 12] = [
        ("font_family", "TerminalElement::prepaint"),
        ("font_weight", "TerminalElement::prepaint"),
        ("line_height", "TerminalElement::prepaint"),
        ("minimum_contrast", "TerminalElement::prepaint"),
        ("cursor_blink", "TerminalTab::cursor_visible"),
        ("scrollbar", "TerminalScrollbarSettingsWrapper::visibility"),
        ("option_as_meta", "TerminalWindowView::render::on_key_down"),
        ("copy_on_select", "Terminal::mouse_up"),
        ("keep_selection_on_copy", "Terminal::process_terminal_event"),
        ("open_links_in_mouse_mode", "Terminal::mouse_down"),
        ("scroll_multiplier", "TerminalTab::scroll_wheel"),
        ("bell", "TerminalWindowView::handle_bell"),
    ];

    #[derive(Clone, Debug)]
    struct GeneratedLiveSettings {
        font_family: String,
        font_weight: f32,
        line_height: settings::TerminalLineHeight,
        minimum_contrast: f32,
        blinking: settings::TerminalBlink,
        scrollbar: settings::ShowScrollbar,
        option_as_meta: bool,
        copy_on_select: bool,
        keep_selection_on_copy: bool,
        open_links_in_mouse_mode: bool,
        scroll_multiplier: f32,
        bell: settings::TerminalBell,
        cursor_shape: terminal_core::terminal_settings::CursorShape,
        alternate_scroll: settings::AlternateScroll,
    }

    #[derive(Debug, PartialEq)]
    struct PaneInvariantSnapshot {
        pane_entity_id: gpui::EntityId,
        terminal_entity_id: gpui::EntityId,
        is_pty: bool,
        process_id: String,
        construction: String,
    }

    fn cursor_shape_content(
        cursor_shape: terminal_core::terminal_settings::CursorShape,
    ) -> settings::CursorShapeContent {
        match cursor_shape {
            terminal_core::terminal_settings::CursorShape::Block => {
                settings::CursorShapeContent::Block
            }
            terminal_core::terminal_settings::CursorShape::Underline => {
                settings::CursorShapeContent::Underline
            }
            terminal_core::terminal_settings::CursorShape::Bar => settings::CursorShapeContent::Bar,
            terminal_core::terminal_settings::CursorShape::Hollow => {
                settings::CursorShapeContent::Hollow
            }
        }
    }

    fn different_cursor_shape(
        cursor_shape: terminal_core::terminal_settings::CursorShape,
    ) -> terminal_core::terminal_settings::CursorShape {
        match cursor_shape {
            terminal_core::terminal_settings::CursorShape::Block => {
                terminal_core::terminal_settings::CursorShape::Underline
            }
            _ => terminal_core::terminal_settings::CursorShape::Block,
        }
    }

    fn different_alternate_scroll(
        alternate_scroll: settings::AlternateScroll,
    ) -> settings::AlternateScroll {
        match alternate_scroll {
            settings::AlternateScroll::On => settings::AlternateScroll::Off,
            settings::AlternateScroll::Off => settings::AlternateScroll::On,
        }
    }

    fn update_live_settings(cx: &mut App, values: &GeneratedLiveSettings) {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |content| {
                let terminal = content
                    .terminal
                    .get_or_insert_with(settings::TerminalSettingsContent::default);
                terminal.font_family =
                    Some(settings::FontFamilyName(values.font_family.clone().into()));
                terminal.font_weight = Some(settings::FontWeightContent(values.font_weight));
                terminal.line_height = Some(values.line_height.clone());
                terminal.minimum_contrast = Some(values.minimum_contrast);
                terminal.blinking = Some(values.blinking);
                terminal.scrollbar.get_or_insert_default().show = Some(values.scrollbar);
                terminal.option_as_meta = Some(values.option_as_meta);
                terminal.copy_on_select = Some(values.copy_on_select);
                terminal.keep_selection_on_copy = Some(values.keep_selection_on_copy);
                terminal.open_links_in_mouse_mode = Some(values.open_links_in_mouse_mode);
                terminal.scroll_multiplier = Some(values.scroll_multiplier);
                terminal.bell = Some(values.bell);
                terminal.cursor_shape = Some(cursor_shape_content(values.cursor_shape));
                terminal.alternate_scroll = Some(values.alternate_scroll);
            });
        });
    }

    fn reset_terminal_settings(cx: &mut App) {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |content| content.terminal = None);
        });
    }

    fn new_test_pane(
        settings: &TerminalSettings,
        window_id: u64,
        cx: &mut App,
    ) -> Entity<TerminalTab> {
        let builder = TerminalBuilder::new_display_only(
            settings.cursor_shape,
            settings.alternate_scroll,
            settings.max_scroll_history_lines,
            window_id,
            cx.background_executor(),
            PathStyle::local(),
        );
        let terminal = cx.new(|cx| builder.subscribe(cx));
        cx.new(|cx| TerminalTab::new(terminal, cx))
    }

    fn test_window_with_panes(
        non_active_tab_count: usize,
        split_leaf_count: usize,
        window_id_seed: u64,
        cx: &mut App,
    ) -> Entity<TerminalWindowView> {
        let settings = TerminalSettings::get_global(cx).clone();
        let mut tabs = Vec::with_capacity(non_active_tab_count + 1);
        for index in 0..non_active_tab_count {
            tabs.push(WindowTab::new(new_test_pane(
                &settings,
                window_id_seed + index as u64,
                cx,
            )));
        }

        let first_split_pane =
            new_test_pane(&settings, window_id_seed + non_active_tab_count as u64, cx);
        let mut split_root = SplitNode::leaf(first_split_pane.clone());
        let mut active_pane = first_split_pane.clone();
        for index in 1..split_leaf_count {
            let split_pane = new_test_pane(
                &settings,
                window_id_seed + non_active_tab_count as u64 + index as u64,
                cx,
            );
            assert!(
                split_root.split(&first_split_pane, split_pane.clone(), SplitDirection::Right,)
            );
            active_pane = split_pane;
        }
        tabs.push(WindowTab {
            split_root: Some(split_root),
            active_pane_tab: Some(active_pane),
            maximized_pane_tab: None,
        });
        let active_tab_index = tabs.len() - 1;

        let window_view = cx.new(TerminalWindowView::new);
        window_view.update(cx, |window_view, _| {
            window_view.tabs = tabs;
            window_view.active_tab_index = active_tab_index;
        });
        window_view
    }

    fn pane_invariant_snapshots(
        panes: &[Entity<TerminalTab>],
        cx: &App,
    ) -> Vec<PaneInvariantSnapshot> {
        panes
            .iter()
            .map(|pane| {
                let terminal = pane.read(cx).terminal.clone();
                let terminal = terminal.read(cx);
                PaneInvariantSnapshot {
                    pane_entity_id: pane.entity_id(),
                    terminal_entity_id: pane.read(cx).terminal.entity_id(),
                    is_pty: terminal.is_pty(),
                    process_id: format!("{:?}", terminal.pid()),
                    construction: format!("{:?}", terminal.construction_snapshot_for_test()),
                }
            })
            .collect()
    }

    fn scrollbar_visibility(show: Option<settings::ShowScrollbar>) -> ShowScrollbar {
        match show.unwrap_or(settings::ShowScrollbar::Auto) {
            settings::ShowScrollbar::Auto => ShowScrollbar::Auto,
            settings::ShowScrollbar::System => ShowScrollbar::System,
            settings::ShowScrollbar::Always => ShowScrollbar::Always,
            settings::ShowScrollbar::Never => ShowScrollbar::Never,
        }
    }

    fn audit_stage(
        stage: &str,
        window_view: &Entity<TerminalWindowView>,
        expected: &EffectiveLiveSettingsSnapshot,
        cx: &mut App,
    ) -> Vec<String> {
        let panes = window_view.read(cx).all_panes();
        let before = pane_invariant_snapshots(&panes, cx);
        let mut gaps = Vec::new();

        assert_eq!(EffectiveLiveSettingsSnapshot::capture(cx), *expected);
        assert_eq!(
            TerminalScrollbarSettingsWrapper.visibility(cx),
            scrollbar_visibility(expected.scrollbar),
        );
        for pane in &panes {
            assert_eq!(EffectiveLiveSettingsSnapshot::capture(cx), *expected);
            pane.read(cx).cursor_visible(cx);

            let terminal = pane.read(cx).terminal.clone();
            let terminal = terminal.read(cx);
            let observed = terminal.live_settings_snapshot_for_test();
            if observed.0 != Some(expected.cursor_shape) {
                gaps.push(format!(
                    "{stage}: pane {:?} cursor_shape remained {:?}, expected {:?}",
                    pane.entity_id(),
                    observed.0,
                    expected.cursor_shape,
                ));
            }
            if observed.1 != expected.alternate_scroll {
                gaps.push(format!(
                    "{stage}: pane {:?} alternate_scroll remained {:?}, expected {:?}",
                    pane.entity_id(),
                    observed.1,
                    expected.alternate_scroll,
                ));
            }
            assert_eq!(
                terminal.live_settings_application_counts_for_test(),
                (1, 1),
                "{stage}: pane {:?} must receive each imperative live setting exactly once",
                pane.entity_id(),
            );
        }

        let after = pane_invariant_snapshots(&panes, cx);
        assert_eq!(
            before, after,
            "{stage}: live update recreated pane, terminal, PTY, or construction state"
        );
        gaps
    }

    struct GeneratedLiveSettingsParameters {
        font_seed: u8,
        font_weight_step: u8,
        line_height_step: u8,
        minimum_contrast: u8,
        blink_variant: u8,
        scrollbar_variant: u8,
        option_as_meta: bool,
        copy_on_select: bool,
        keep_selection_on_copy: bool,
        open_links_in_mouse_mode: bool,
        scroll_multiplier_step: u8,
        bell_enabled: bool,
        cursor_shape_variant: u8,
        alternate_scroll_enabled: bool,
    }

    fn generated_live_settings(
        parameters: GeneratedLiveSettingsParameters,
    ) -> GeneratedLiveSettings {
        let GeneratedLiveSettingsParameters {
            font_seed,
            font_weight_step,
            line_height_step,
            minimum_contrast,
            blink_variant,
            scrollbar_variant,
            option_as_meta,
            copy_on_select,
            keep_selection_on_copy,
            open_links_in_mouse_mode,
            scroll_multiplier_step,
            bell_enabled,
            cursor_shape_variant,
            alternate_scroll_enabled,
        } = parameters;
        let blinking = match blink_variant % 3 {
            0 => settings::TerminalBlink::Off,
            1 => settings::TerminalBlink::On,
            _ => settings::TerminalBlink::TerminalControlled,
        };
        let scrollbar = match scrollbar_variant % 4 {
            0 => settings::ShowScrollbar::Auto,
            1 => settings::ShowScrollbar::System,
            2 => settings::ShowScrollbar::Always,
            _ => settings::ShowScrollbar::Never,
        };
        let cursor_shape = match cursor_shape_variant % 4 {
            0 => terminal_core::terminal_settings::CursorShape::Block,
            1 => terminal_core::terminal_settings::CursorShape::Underline,
            2 => terminal_core::terminal_settings::CursorShape::Bar,
            _ => terminal_core::terminal_settings::CursorShape::Hollow,
        };
        GeneratedLiveSettings {
            font_family: format!("Audit Font {font_seed}"),
            font_weight: 100.0 + f32::from(font_weight_step % 9) * 100.0,
            line_height: settings::TerminalLineHeight::Custom(
                1.0 + f32::from(line_height_step % 21) / 10.0,
            ),
            minimum_contrast: f32::from(minimum_contrast % 107),
            blinking,
            scrollbar,
            option_as_meta,
            copy_on_select,
            keep_selection_on_copy,
            open_links_in_mouse_mode,
            scroll_multiplier: 0.1 + f32::from(scroll_multiplier_step % 100) / 10.0,
            bell: if bell_enabled {
                settings::TerminalBell::System
            } else {
                settings::TerminalBell::Off
            },
            cursor_shape,
            alternate_scroll: if alternate_scroll_enabled {
                settings::AlternateScroll::On
            } else {
                settings::AlternateScroll::Off
            },
        }
    }

    fn generated_live_settings_strategy()
    -> impl proptest::strategy::Strategy<Value = GeneratedLiveSettings> {
        (
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            proptest::arbitrary::any::<u8>(),
            (
                proptest::arbitrary::any::<bool>(),
                proptest::arbitrary::any::<bool>(),
                proptest::arbitrary::any::<bool>(),
                proptest::arbitrary::any::<bool>(),
                proptest::arbitrary::any::<bool>(),
                proptest::arbitrary::any::<bool>(),
            ),
        )
            .prop_map(
                |(
                    font_seed,
                    font_weight_step,
                    line_height_step,
                    minimum_contrast,
                    blink_variant,
                    scrollbar_variant,
                    scroll_multiplier_step,
                    cursor_shape_variant,
                    (
                        option_as_meta,
                        copy_on_select,
                        keep_selection_on_copy,
                        open_links_in_mouse_mode,
                        bell_enabled,
                        alternate_scroll_enabled,
                    ),
                )| {
                    generated_live_settings(GeneratedLiveSettingsParameters {
                        font_seed,
                        font_weight_step,
                        line_height_step,
                        minimum_contrast,
                        blink_variant,
                        scrollbar_variant,
                        option_as_meta,
                        copy_on_select,
                        keep_selection_on_copy,
                        open_links_in_mouse_mode,
                        scroll_multiplier_step,
                        bell_enabled,
                        cursor_shape_variant,
                        alternate_scroll_enabled,
                    })
                },
            )
    }

    /// Feature: terminal-settings-completion, Property 7: Live settings reach every existing pane without recreation
    /// **Validates: Requirements 3.1–3.7, 5.8, 8.2**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn live_settings_reach_every_existing_pane_without_recreation(
        cx: &mut gpui::TestAppContext,
        #[strategy = 1_usize..4] non_active_tab_count: usize,
        #[strategy = 2_usize..6] split_leaf_count: usize,
        #[strategy = generated_live_settings_strategy()] custom: GeneratedLiveSettings,
    ) {
        let custom_window = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            assert_eq!(GLOBAL_LIVE_SETTING_PATHS.len(), 12);

            let mut initial_custom = custom.clone();
            initial_custom.cursor_shape = different_cursor_shape(custom.cursor_shape);
            initial_custom.alternate_scroll = different_alternate_scroll(custom.alternate_scroll);
            update_live_settings(cx, &initial_custom);
            test_window_with_panes(non_active_tab_count, split_leaf_count, 10_000, cx)
        });

        cx.update(|cx| update_live_settings(cx, &custom));
        let mut gaps = cx.update(|cx| {
            let custom_expected = EffectiveLiveSettingsSnapshot::capture(cx);
            audit_stage("saved live values", &custom_window, &custom_expected, cx)
        });

        let defaults = cx.update(|cx| {
            reset_terminal_settings(cx);
            EffectiveLiveSettingsSnapshot::capture(cx)
        });
        let reset_initial = GeneratedLiveSettings {
            font_family: "Reset audit override".to_string(),
            font_weight: 900.0,
            line_height: settings::TerminalLineHeight::Custom(2.0),
            minimum_contrast: 106.0,
            blinking: settings::TerminalBlink::On,
            scrollbar: settings::ShowScrollbar::Always,
            option_as_meta: !defaults.option_as_meta,
            copy_on_select: !defaults.copy_on_select,
            keep_selection_on_copy: !defaults.keep_selection_on_copy,
            open_links_in_mouse_mode: !defaults.open_links_in_mouse_mode,
            scroll_multiplier: defaults.scroll_multiplier + 1.0,
            bell: match defaults.bell {
                settings::TerminalBell::System => settings::TerminalBell::Off,
                settings::TerminalBell::Off => settings::TerminalBell::System,
            },
            cursor_shape: different_cursor_shape(defaults.cursor_shape),
            alternate_scroll: different_alternate_scroll(defaults.alternate_scroll),
        };
        let reset_window = cx.update(|cx| {
            update_live_settings(cx, &reset_initial);
            test_window_with_panes(non_active_tab_count, split_leaf_count, 20_000, cx)
        });

        cx.update(reset_terminal_settings);
        cx.update(|cx| {
            let reset_expected = EffectiveLiveSettingsSnapshot::capture(cx);
            assert_eq!(reset_expected, defaults);
            gaps.extend(audit_stage(
                "Reset Terminal Defaults",
                &reset_window,
                &reset_expected,
                cx,
            ));
        });

        assert!(
            gaps.is_empty(),
            "Property 7 live-setting audit found gaps:\n{}",
            gaps.join("\n"),
        );
    }

    /// Feature: terminal-settings-completion, Property 9: Scrollbar visibility mapping is total and exact
    /// **Validates: Requirements 3.5, 8.5**
    #[gpui::property_test(config = proptest::test_runner::Config {
        cases: 128,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]
    fn scrollbar_visibility_mapping_is_total_and_exact_without_recreating_panes(
        cx: &mut gpui::TestAppContext,
        #[strategy = 0_usize..4] non_active_tab_count: usize,
        #[strategy = 1_usize..6] split_leaf_count: usize,
        #[strategy = proptest::arbitrary::any::<u8>()] rotation: u8,
    ) {
        let window_view = cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            test_window_with_panes(
                non_active_tab_count,
                split_leaf_count,
                30_000 + u64::from(rotation),
                cx,
            )
        });
        let panes_before = cx.update(|cx| {
            let panes = window_view.read(cx).all_panes();
            (
                pane_entity_ids(&window_view, cx),
                pane_invariant_snapshots(&panes, cx),
            )
        });
        let mappings = [
            (settings::ShowScrollbar::Auto, ShowScrollbar::Auto),
            (settings::ShowScrollbar::System, ShowScrollbar::System),
            (settings::ShowScrollbar::Always, ShowScrollbar::Always),
            (settings::ShowScrollbar::Never, ShowScrollbar::Never),
        ];

        for mapping_index in 0..mappings.len() {
            let (configured, expected) =
                mappings[(mapping_index + usize::from(rotation)) % mappings.len()];
            cx.update(|cx| {
                SettingsStore::update_global(cx, |store, cx| {
                    store.update_user_settings(cx, |content| {
                        content
                            .terminal
                            .get_or_insert_default()
                            .scrollbar
                            .get_or_insert_default()
                            .show = Some(configured);
                    });
                });

                assert_eq!(
                    TerminalScrollbarSettingsWrapper.visibility(cx),
                    expected,
                    "scrollbar mode {configured:?} did not map exactly",
                );
                let panes = window_view.read(cx).all_panes();
                assert_eq!(
                    pane_entity_ids(&window_view, cx),
                    panes_before.0,
                    "switching scrollbar mode {configured:?} recreated a pane",
                );
                assert_eq!(
                    pane_invariant_snapshots(&panes, cx),
                    panes_before.1,
                    "switching scrollbar mode {configured:?} changed pane or terminal identity",
                );
            });
        }
    }
}
