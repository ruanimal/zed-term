//! Window root view: a tab bar plus the active terminal element.
//!
//! Each window owns its own set of tabs; multiple windows share the app-level
//! keymap defined in `app.rs`.

use std::{
    cmp::Ordering,
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

use collections::HashMap;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, ClipboardItem, Context, Decorations,
    DismissEvent, Edges, Element, ElementId, Entity, FocusHandle, Focusable as _, GlobalElementId,
    InputHandler, InspectorElementId, InteractiveElement as _, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Position, PromptLevel, Render, ScrollHandle, StatefulInteractiveElement as _, Style,
    Styled as _, Subscription, UTF16Selection, WeakEntity, Window, WindowHandle, anchored,
    deferred, div, prelude::FluentBuilder, px,
};
use settings::Settings as _;
use settings::settings_content::TerminalBell;
use terminal_core::terminal_settings::TerminalSettings;
use terminal_core::transfer_mux::TransferSetup;
use terminal_core::{
    Clear as TerminalClear, Copy as TerminalCopyAction, MaybeNavigationTarget,
    Paste as TerminalPasteAction, PasteText as TerminalPasteTextAction, ScrollLineDown,
    ScrollLineUp, ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop, SearchTest,
    SelectAll as TerminalSelectAll, ShowCharacterPalette, Terminal, TerminalBuilder,
};
use theme::ActiveTheme as _;
use ui::scrollbars::{ScrollbarVisibility, ShowScrollbar};
use ui::utils::TRAFFIC_LIGHT_PADDING;
use ui::{
    Button, ButtonCommon as _, ButtonSize, ButtonStyle, Clickable as _, Color, ContextMenu,
    Disableable as _, DynamicSpacing, IconButton, IconButtonShape, IconName, IconSize, Indicator,
    Label, LabelCommon as _, LabelSize, ScrollAxes, Scrollbars, Tab, TabBar, TabPosition,
    Toggleable as _, Tooltip, WithScrollbar,
};
use util::ResultExt;
use util::paths::PathStyle;

use crate::terminal::split::{self, SplitDirection, SplitNode};
use crate::terminal::tab::{
    ScrollAction, TerminalTabEvent, is_context_menu_navigation_target, navigation_target_text,
    open_navigation_target,
};
use crate::terminal::{TerminalElement, TerminalSearchBar, TerminalTab};
use crate::text_edit::{
    next_utf16_boundary, previous_utf16_boundary, replace_utf16_range, substring_utf16,
};
use crate::transfer_ui::{TransferBar, transfer_setup};
use crate::window_chrome;
use crate::{
    ActivateNextPane, ActivatePreviousPane, CloseAll, CloseLeft, CloseOtherTabs, ClosePane,
    CloseRight, CloseTab, CopyLink, NewTab, NewWindow, NextTab, OpenLink, OpenSettings,
    PreviousTab, SendKeystroke, SendText, SplitDown, SplitLeft, SplitRight, SplitUp, ToggleZoom,
};

/// Fixed content width for every tab so the tab bar does not reflow while
/// the title follows the foreground process (e.g. `zsh` → `ls -al` → `zsh`).
/// Titles longer than this are cut off with an ellipsis, like editor tabs
/// (`MAX_TAB_TITLE_LEN` in the editor crate).
const TAB_TITLE_WIDTH: gpui::Pixels = px(100.);
/// Character cap applied to the tab label and drag title, mirroring the editor's
/// `MAX_TAB_TITLE_LEN`. Hover tooltips retain the complete title.
const TAB_TITLE_MAX_CHARS: usize = 24;

#[derive(Clone, Copy)]
enum TabBarScrollDirection {
    Left,
    Right,
}

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
    /// Link under the last right-click, if that cell held one. Its menu entries
    /// act on the clicked cell rather than on the focused pane.
    context_navigation_target: Option<MaybeNavigationTarget>,
    tab_rename: Option<TabRenameState>,
    /// Left-button state for the titlebar strip drag gesture.
    titlebar_mouse_down: std::cell::Cell<bool>,
    /// Tracks the tab bar's horizontal scroll so an activated tab can be
    /// scrolled back into view when the tabs overflow.
    tab_bar_scroll_handle: ScrollHandle,
    tab_bar_has_overflow: bool,
    tab_bar_can_scroll_left: bool,
    tab_bar_can_scroll_right: bool,
    terminal_error: Option<String>,
    live_settings: LiveTerminalSettings,
    pane_subscriptions: HashMap<gpui::EntityId, PaneSubscriptions>,
    _subscriptions: Vec<Subscription>,
}

struct PaneSubscriptions {
    _observation: Subscription,
    event_subscription: Subscription,
}

struct TabMoveDestination {
    window: WindowHandle<TerminalWindowView>,
    position: Option<gpui::Point<Pixels>>,
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
    title_override: Option<String>,
}

struct TabRenameState {
    panes: Vec<Entity<TerminalTab>>,
    name: String,
    selection: Range<usize>,
    anchor: usize,
    cursor: usize,
    marked_range: Option<Range<usize>>,
    focus_handle: FocusHandle,
}

impl WindowTab {
    /// Creates a tab from a single terminal pane (no splits yet).
    fn new(tab: Entity<TerminalTab>) -> Self {
        Self {
            split_root: None,
            active_pane_tab: Some(tab),
            maximized_pane_tab: None,
            title_override: None,
        }
    }

    /// The pane currently holding focus within this tab.
    fn focused_tab(&self) -> Option<Entity<TerminalTab>> {
        self.active_pane_tab.clone()
    }

    fn title(&self, cx: &App) -> String {
        self.title_override
            .clone()
            .or_else(|| self.focused_tab().map(|pane| pane.read(cx).title(cx)))
            .unwrap_or_else(|| "Terminal".to_string())
    }

    fn set_title_override(&mut self, title: String) {
        let title = title.trim().to_string();
        self.title_override = (!title.is_empty()).then_some(title);
    }

    fn panes(&self) -> Vec<Entity<TerminalTab>> {
        if let Some(root) = self.split_root.as_ref() {
            let mut panes = Vec::new();
            root.collect_tabs(&mut panes);
            panes.into_iter().cloned().collect()
        } else {
            self.active_pane_tab.iter().cloned().collect()
        }
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

struct TabRenameInput {
    view: WeakEntity<TerminalWindowView>,
    focus_handle: FocusHandle,
}

impl IntoElement for TabRenameInput {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TabRenameInput {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
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
        window.set_focus_handle(&self.focus_handle, cx);
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
        window.handle_input(
            &self.focus_handle,
            TabRenameInputHandler {
                view: self.view.clone(),
                bounds,
            },
            cx,
        );
    }
}

struct TabRenameInputHandler {
    view: WeakEntity<TerminalWindowView>,
    bounds: Bounds<Pixels>,
}

impl TabRenameInputHandler {
    fn update(&self, cx: &mut App, update: impl FnOnce(&mut TabRenameState)) {
        self.view
            .update(cx, |view, cx| {
                if let Some(state) = view.tab_rename.as_mut() {
                    update(state);
                    cx.notify();
                }
            })
            .log_err();
    }
}

impl InputHandler for TabRenameInputHandler {
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.view
            .read_with(cx, |view, _| {
                view.tab_rename.as_ref().map(|state| UTF16Selection {
                    range: state.selection.clone(),
                    reversed: state.cursor < state.anchor,
                })
            })
            .log_err()
            .flatten()
    }

    fn marked_text_range(&mut self, _window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .read_with(cx, |view, _| {
                view.tab_rename
                    .as_ref()
                    .and_then(|state| state.marked_range.clone())
            })
            .log_err()
            .flatten()
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.view
            .read_with(cx, |view, _| {
                view.tab_rename
                    .as_ref()
                    .map(|state| substring_utf16(&state.name, range_utf16))
            })
            .log_err()
            .flatten()
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.update(cx, |state| {
            replace_tab_rename_text(state, replacement_range, text, None, false);
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
        self.update(cx, |state| {
            replace_tab_rename_text(state, range_utf16, new_text, new_selected_range, true);
        });
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut App) {
        self.update(cx, |state| state.marked_range = None);
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) {
        self.update(cx, |state| {
            let length = state.name.encode_utf16().count();
            let start = range_utf16.start.min(length);
            let end = range_utf16.end.min(length).max(start);
            state.selection = start..end;
            state.anchor = start;
            state.cursor = end;
            state.marked_range = None;
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
        self.view
            .read_with(cx, |view, _| {
                view.tab_rename
                    .as_ref()
                    .map(|state| state.name.encode_utf16().count())
            })
            .log_err()
            .flatten()
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        true
    }
}

fn replace_tab_rename_text(
    state: &mut TabRenameState,
    replacement_range: Option<Range<usize>>,
    text: &str,
    new_selected_range: Option<Range<usize>>,
    mark_text: bool,
) {
    let current_text = state.name.clone();
    let range = replacement_range
        .or_else(|| state.marked_range.clone())
        .unwrap_or_else(|| state.selection.clone());
    let range_start = range.start.min(current_text.encode_utf16().count());
    let (updated_text, cursor) = replace_utf16_range(&current_text, range, text);
    state.name = updated_text;
    state.marked_range = mark_text
        .then_some(range_start..cursor)
        .filter(|range| !range.is_empty());
    let inserted_length = text.encode_utf16().count();
    state.selection = new_selected_range
        .map(|range| {
            let start = range.start.min(inserted_length);
            let end = range.end.min(inserted_length).max(start);
            range_start + start..range_start + end
        })
        .unwrap_or(cursor..cursor);
    state.anchor = state.selection.start;
    state.cursor = state.selection.end;
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
    pub fn new(cx: &mut Context<Self>, initial_directory: Option<PathBuf>) -> Self {
        let mut view = Self::new_empty(cx);
        view.spawn_new_tab(cx, initial_directory);
        view
    }

    pub(crate) fn new_empty(cx: &mut Context<Self>) -> Self {
        let live_settings = LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
        let settings_subscription =
            cx.observe_global::<settings::SettingsStore>(Self::settings_changed);
        let view = Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active_tab_index: 0,
            context_menu: None,
            context_navigation_target: None,
            tab_rename: None,
            titlebar_mouse_down: std::cell::Cell::new(false),
            tab_bar_scroll_handle: ScrollHandle::new(),
            tab_bar_has_overflow: false,
            tab_bar_can_scroll_left: false,
            tab_bar_can_scroll_right: false,
            terminal_error: None,
            live_settings,
            pane_subscriptions: HashMap::default(),
            _subscriptions: vec![settings_subscription],
        };

        // The macOS display link only redraws while invalidated. Shell output now
        // repaints through the observe chain (Terminal -> TerminalTab ->
        // TerminalWindowView) plus the TitleChanged subscription. Keep a local
        // heartbeat only for cursor blink, rather than refreshing every window.
        let entity_id = cx.entity_id();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;

                let should_refresh = match this.update(cx, |view, cx| {
                    view.all_panes()
                        .iter()
                        .any(|pane| pane.read(cx).cursor_blinking_enabled(cx))
                }) {
                    Ok(should_refresh) => should_refresh,
                    Err(_) => break,
                };

                if should_refresh
                    && cx
                        .with_window(entity_id, |window, _| window.refresh())
                        .is_none()
                {
                    break;
                }
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
    fn initial_directory_for_new_tab(&self, cx: &App) -> Option<PathBuf> {
        if !matches!(
            &TerminalSettings::get_global(cx).working_directory,
            settings::WorkingDirectory::PreviousTab
        ) {
            return None;
        }

        self.active_tab().and_then(|tab| {
            tab.read(cx)
                .terminal
                .read(cx)
                .working_directory_for_new_terminal()
        })
    }

    pub(crate) fn spawn_new_tab(
        &mut self,
        cx: &mut Context<Self>,
        initial_directory: Option<PathBuf>,
    ) {
        let initial_directory =
            initial_directory.or_else(|| self.initial_directory_for_new_tab(cx));
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let builder = match build_terminal(cx, initial_directory).await {
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
                    this.tab_bar_scroll_handle
                        .scroll_to_item(this.active_tab_index);
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
        self.observe_tab_with_subscription(tab, None, cx);
    }

    fn observe_tab_with_subscription(
        &mut self,
        tab: &Entity<TerminalTab>,
        event_subscription: Option<Subscription>,
        cx: &mut Context<Self>,
    ) {
        let observation = cx.observe(tab, |this, _, cx| {
            this.active_tab_index = this.active_tab_index.min(this.tabs.len().saturating_sub(1));
            cx.notify();
        });
        let event_subscription = event_subscription.unwrap_or_else(|| {
            App::subscribe(cx, tab, |pane, event: &TerminalTabEvent, cx| {
                // Resolve the current owner after pending updates because the pane
                // may move between event emission and handling.
                let event = *event;
                let pane = pane.clone();
                cx.defer(move |cx| {
                    for any_window in cx.windows() {
                        let Some(window_handle) = any_window.downcast::<Self>() else {
                            continue;
                        };
                        let Ok(window_entity) = window_handle.entity(cx) else {
                            continue;
                        };
                        if !window_entity
                            .read(cx)
                            .tabs
                            .iter()
                            .any(|tab| tab.contains_pane(&pane))
                        {
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
            })
        });
        self.pane_subscriptions.insert(
            tab.entity_id(),
            PaneSubscriptions {
                _observation: observation,
                event_subscription,
            },
        );
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

    fn move_tab_to_window(
        &mut self,
        pane: &Entity<TerminalTab>,
        destination: Option<WindowHandle<Self>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if destination.is_some_and(|destination| {
            destination.window_id() == window.window_handle().window_id()
        }) {
            return Ok(());
        }
        let Some((source_index, tab)) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.contains_pane(pane))
            .map(|(index, tab)| (index, tab.clone()))
        else {
            anyhow::bail!("The terminal tab is no longer in this window");
        };
        let panes = tab.panes();
        let is_new_window = destination.is_none();
        let destination = match destination {
            Some(destination) => destination,
            None => crate::open_empty_terminal_window(cx)?,
        };

        // Keep the source intact until the destination accepts the live entities;
        // a window selected from the menu may have closed in the meantime.
        if let Err(error) = destination.update(cx, |target, window, cx| {
            for pane in &panes {
                // Carry the existing listener across so queued events cannot
                // fall between cancellation and activation of subscriptions.
                let event_subscription = self
                    .pane_subscriptions
                    .remove(&pane.entity_id())
                    .map(|subscriptions| subscriptions.event_subscription);
                target.observe_tab_with_subscription(pane, event_subscription, cx);
            }
            target.tabs.push(tab);
            target.terminal_error = None;
            target.activate_tab(target.tabs.len() - 1, window, cx);
            window.activate_window();
        }) {
            if is_new_window {
                destination
                    .update(cx, |_, window, _| window.remove_window())
                    .log_err();
            }
            return Err(error);
        }

        self.tabs.remove(source_index);
        self.tab_rename = None;
        self.terminal_error = None;
        if self.tabs.is_empty() {
            self.active_tab_index = 0;
            window.remove_window();
        } else {
            let was_active = source_index == self.active_tab_index;
            if source_index < self.active_tab_index {
                self.active_tab_index -= 1;
            }
            self.active_tab_index = self.active_tab_index.min(self.tabs.len() - 1);
            if was_active {
                self.focus_pane(window, cx);
            }
            self.tab_bar_scroll_handle
                .scroll_to_item(self.active_tab_index);
        }
        cx.notify();
        Ok(())
    }

    fn request_tab_move(
        source: &WeakEntity<Self>,
        pane: &Entity<TerminalTab>,
        destination: Option<WindowHandle<Self>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        source
            .update(cx, |this, cx| {
                if let Err(error) = this.move_tab_to_window(pane, destination, window, cx) {
                    log::error!("Could not move terminal tab: {error:#}");
                    this.terminal_error = Some(format!("Could not move terminal tab: {error:#}"));
                    cx.notify();
                }
            })
            .log_err();
    }

    fn tab_move_destinations(
        source_window: gpui::WindowId,
        cx: &mut App,
    ) -> Vec<TabMoveDestination> {
        let positions_available = cx.compositor_name() != "Wayland";
        let mut destinations = cx
            .windows()
            .into_iter()
            .filter_map(|handle| handle.downcast::<Self>())
            .filter(|handle| handle.window_id() != source_window)
            .filter_map(|handle| {
                let position = if positions_available {
                    Some(
                        gpui::AnyWindowHandle::from(handle)
                            .update(cx, |_, window, _| window.bounds().origin)
                            .log_err()?,
                    )
                } else {
                    None
                };
                Some(TabMoveDestination {
                    window: handle,
                    position,
                })
            })
            .collect::<Vec<_>>();
        destinations.sort_by_key(|destination| {
            (
                destination
                    .position
                    .map(|position| (position.y, position.x)),
                destination.window.window_id().as_u64(),
            )
        });
        destinations
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
        let Some(tab) = self.focused_tab_updated(window, cx) else {
            return;
        };
        tab.update(cx, f);
    }

    /// Resolves the pane that actually holds window focus and records it as the
    /// active pane, so later actions target the same pane.
    fn focused_tab_updated(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Entity<TerminalTab>> {
        let tab = self.focused_tab(window, cx)?;
        if let Some(active) = self.tabs.get_mut(self.active_tab_index) {
            active.active_pane_tab = Some(tab.clone());
        }
        Some(tab)
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

    /// Opens the link that was under the context-menu right-click.
    fn open_link(&mut self, _: &OpenLink, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.context_navigation_target.clone() else {
            return;
        };
        open_navigation_target(&target, cx);
    }

    /// Copies the link that was under the context-menu right-click.
    fn copy_link(&mut self, _: &CopyLink, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.context_navigation_target.as_ref() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(navigation_target_text(target)));
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
        // Search the pane the user is actually in: after a left-click the
        // tracked active pane can lag behind real focus.
        let Some(tab) = self.focused_tab_updated(window, cx) else {
            return;
        };
        tab.update(cx, |tab, cx| tab.toggle_search(window, cx));
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
        self.spawn_new_tab(cx, None);
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
        let initial_directory = anchor_tab
            .read(cx)
            .terminal
            .read(cx)
            .working_directory_for_new_terminal();
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let builder = match build_terminal(cx, initial_directory).await {
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
        if !self.tabs.is_empty() {
            self.tab_bar_scroll_handle
                .scroll_to_item(self.active_tab_index);
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
            self.tab_bar_scroll_handle
                .scroll_to_item(self.active_tab_index);
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
        self.tab_bar_scroll_handle
            .scroll_to_item(self.active_tab_index);
        cx.notify();
    }

    fn close_left(&mut self, _: &CloseLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.drain(..self.active_tab_index);
        self.active_tab_index = 0;
        self.tab_bar_scroll_handle
            .scroll_to_item(self.active_tab_index);
        cx.notify();
    }

    fn close_right(&mut self, _: &CloseRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.truncate(self.active_tab_index + 1);
        self.tab_bar_scroll_handle
            .scroll_to_item(self.active_tab_index);
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

    fn begin_tab_rename(&mut self, tab_index: usize, cx: &mut Context<Self>) -> FocusHandle {
        let Some(tab) = self.tabs.get(tab_index) else {
            return cx.focus_handle();
        };
        let panes = tab.panes();
        let name = tab.title(cx);
        let cursor = name.encode_utf16().count();
        let focus_handle = cx.focus_handle();
        self.tab_rename = Some(TabRenameState {
            panes,
            name,
            selection: 0..cursor,
            anchor: 0,
            cursor,
            marked_range: None,
            focus_handle: focus_handle.clone(),
        });
        cx.notify();
        focus_handle
    }

    fn commit_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.tab_rename.take() else {
            return;
        };
        let name = rename.name;
        if let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| rename.panes.iter().any(|pane| tab.contains_pane(pane)))
        {
            tab.set_title_override(name);
        }
        self.focus_pane(window, cx);
        cx.notify();
    }

    fn cancel_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_rename.take().is_some() {
            self.focus_pane(window, cx);
            cx.notify();
        }
    }

    fn tab_rename_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_rename.is_none() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let secondary = event.keystroke.modifiers.secondary();
        let shift = event.keystroke.modifiers.shift;
        let handled = match key {
            "escape" => {
                self.cancel_tab_rename(window, cx);
                true
            }
            "enter" => {
                self.commit_tab_rename(window, cx);
                true
            }
            "left" => {
                self.move_tab_rename_cursor(true, shift, cx);
                true
            }
            "right" => {
                self.move_tab_rename_cursor(false, shift, cx);
                true
            }
            "home" => {
                self.move_tab_rename_to_boundary(true, shift, cx);
                true
            }
            "end" => {
                self.move_tab_rename_to_boundary(false, shift, cx);
                true
            }
            "backspace" => {
                self.delete_tab_rename_text(true, cx);
                true
            }
            "delete" => {
                self.delete_tab_rename_text(false, cx);
                true
            }
            "a" if secondary => {
                self.select_all_tab_rename_text(cx);
                true
            }
            "c" if secondary => {
                self.copy_tab_rename_text(cx);
                true
            }
            "x" if secondary => {
                self.cut_tab_rename_text(cx);
                true
            }
            "v" if secondary => {
                self.paste_tab_rename_text(cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn move_tab_rename_cursor(
        &mut self,
        move_left: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.tab_rename.as_mut() else {
            return;
        };
        let target = if !extend_selection && !state.selection.is_empty() {
            if move_left {
                state.selection.start
            } else {
                state.selection.end
            }
        } else if move_left {
            previous_utf16_boundary(&state.name, state.cursor)
        } else {
            next_utf16_boundary(&state.name, state.cursor)
        };
        Self::update_tab_rename_selection(state, target, extend_selection);
        cx.notify();
    }

    fn move_tab_rename_to_boundary(
        &mut self,
        move_to_start: bool,
        extend_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.tab_rename.as_mut() else {
            return;
        };
        let target = if move_to_start {
            0
        } else {
            state.name.encode_utf16().count()
        };
        Self::update_tab_rename_selection(state, target, extend_selection);
        cx.notify();
    }

    fn update_tab_rename_selection(
        state: &mut TabRenameState,
        cursor: usize,
        extend_selection: bool,
    ) {
        let cursor = cursor.min(state.name.encode_utf16().count());
        if extend_selection {
            state.cursor = cursor;
            state.selection = if state.anchor <= cursor {
                state.anchor..cursor
            } else {
                cursor..state.anchor
            };
        } else {
            state.anchor = cursor;
            state.cursor = cursor;
            state.selection = cursor..cursor;
        }
        state.marked_range = None;
    }

    fn delete_tab_rename_text(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(state) = self.tab_rename.as_mut() else {
            return;
        };
        state.marked_range = None;
        if state.selection.is_empty() {
            let boundary = if backwards {
                previous_utf16_boundary(&state.name, state.cursor)
            } else {
                next_utf16_boundary(&state.name, state.cursor)
            };
            if boundary == state.cursor {
                return;
            }
            state.selection = if backwards {
                boundary..state.cursor
            } else {
                state.cursor..boundary
            };
        }
        replace_tab_rename_text(state, None, "", None, false);
        cx.notify();
    }

    fn select_all_tab_rename_text(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.tab_rename.as_mut() else {
            return;
        };
        let length = state.name.encode_utf16().count();
        state.selection = 0..length;
        state.anchor = 0;
        state.cursor = length;
        state.marked_range = None;
        cx.notify();
    }

    fn copy_tab_rename_text(&self, cx: &mut App) {
        if let Some(state) = self.tab_rename.as_ref()
            && !state.selection.is_empty()
        {
            let text = substring_utf16(&state.name, state.selection.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut_tab_rename_text(&mut self, cx: &mut Context<Self>) {
        if self
            .tab_rename
            .as_ref()
            .is_some_and(|state| !state.selection.is_empty())
        {
            self.copy_tab_rename_text(cx);
            self.delete_tab_rename_text(false, cx);
        }
    }

    fn paste_tab_rename_text(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| text.replace(['\n', '\r'], " "))
        else {
            return;
        };
        if let Some(state) = self.tab_rename.as_mut() {
            replace_tab_rename_text(state, None, &text, None, false);
            cx.notify();
        }
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
        let has_link = self.context_navigation_target.is_some();
        self.show_context_menu(position, window, cx, |menu, _, _| {
            let menu = menu.context(focus_handle);
            // Link entries only exist when the click landed on a link, and they
            // act on that link rather than on the pane's selection.
            let menu = if has_link {
                menu.action("Open Link", Box::new(OpenLink))
                    .action("Copy Link", Box::new(CopyLink))
                    .separator()
            } else {
                menu
            };
            // Split entries are always shown so a pane can be split again
            // (into a horizontal/vertical pair) from any pane, not just one
            // that already has splits. `Split*` actions target the active
            // pane the right-click selected.
            let menu = menu
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
                .action("Search", Box::new(SearchTest))
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
        let tab_index = self.active_tab_index;
        let view = cx.weak_entity();
        let rename_view = view.clone();
        let pane = tab;
        self.show_context_menu(position, window, cx, move |menu, _, _| {
            menu.context(focus_handle)
                .entry("Rename Tab", None, move |window, cx| {
                    if let Some(focus_handle) = rename_view
                        .update(cx, |this, cx| this.begin_tab_rename(tab_index, cx))
                        .log_err()
                    {
                        window.focus(&focus_handle, cx);
                    }
                })
                .separator()
                .submenu("Move Tab to Window", move |menu, window, cx| {
                    let target_windows =
                        Self::tab_move_destinations(window.window_handle().window_id(), cx);
                    let new_window_view = view.clone();
                    let new_window_pane = pane.clone();
                    let mut menu = menu.entry("New Window", None, move |window, cx| {
                        Self::request_tab_move(
                            &new_window_view,
                            &new_window_pane,
                            None,
                            window,
                            cx,
                        );
                    });
                    for (index, destination) in target_windows.iter().enumerate() {
                        let source = view.clone();
                        let pane = pane.clone();
                        let label = match destination.position {
                            Some(position) => format!(
                                "Window {} ({:.0}, {:.0})",
                                index + 1,
                                position.x.as_f32(),
                                position.y.as_f32(),
                            ),
                            None => format!("Window {} (position unavailable)", index + 1),
                        };
                        let destination = destination.window;
                        menu = menu.entry(label, None, move |window, cx| {
                            Self::request_tab_move(&source, &pane, Some(destination), window, cx);
                        });
                    }
                    menu
                })
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
    /// mouse-down, `start_window_move` on drag; interactive children stop
    /// propagation so presses on them don't move the window.
    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let titlebar_height = window_chrome::TITLE_BAR_HEIGHT;
        let titlebar_background = cx.theme().colors().tab_bar_background;
        let is_server_decorations = matches!(window.window_decorations(), Decorations::Server);
        let close_window = cx.listener(|this, _: &ClickEvent, window, cx| {
            this.close_all(&CloseAll, window, cx);
        });
        let (left_controls, right_controls) =
            window_chrome::render_window_controls(window, cx, close_window);
        let tab_bar = self.render_tab_bar(window, cx);
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

    fn scroll_tab_bar(&mut self, direction: TabBarScrollDirection, cx: &mut Context<Self>) {
        let max_offset = self.tab_bar_scroll_handle.max_offset().x;
        if max_offset <= px(0.) {
            return;
        }

        let offset = self.tab_bar_scroll_handle.offset();
        let viewport_width = self.tab_bar_scroll_handle.bounds().size.width;
        let scroll_distance = (viewport_width * 0.8).max(px(96.));
        let target_x = match direction {
            TabBarScrollDirection::Left => offset.x + scroll_distance,
            TabBarScrollDirection::Right => offset.x - scroll_distance,
        }
        .clamp(-max_offset, px(0.));

        if target_x == offset.x {
            return;
        }

        self.tab_bar_scroll_handle
            .set_offset(gpui::Point::new(target_x, offset.y));
        cx.notify();
    }

    fn render_tab_scroll_button(
        &self,
        direction: TabBarScrollDirection,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (button_id, icon, label) = match direction {
            TabBarScrollDirection::Left => (
                "scroll-tabs-left",
                IconName::ChevronLeft,
                "Scroll tabs left",
            ),
            TabBarScrollDirection::Right => (
                "scroll-tabs-right",
                IconName::ChevronRight,
                "Scroll tabs right",
            ),
        };
        div()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                }),
            )
            .child(
                IconButton::new(button_id, icon)
                    .icon_size(IconSize::XSmall)
                    .shape(IconButtonShape::Square)
                    .style(ButtonStyle::Subtle)
                    .disabled(!enabled)
                    .aria_label(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.scroll_tab_bar(direction, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_tab_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> TabBar {
        // max_offset and offset are updated during prepaint, so refresh the view after the
        // frame to keep both indicator buttons in sync with the actual scroll position.
        cx.on_next_frame(window, |view, _window, cx| {
            let max_offset = view.tab_bar_scroll_handle.max_offset().x;
            let offset = view.tab_bar_scroll_handle.offset().x;
            let epsilon = px(0.5);
            let has_overflow = max_offset > epsilon;
            let can_scroll_left = offset < -epsilon;
            let can_scroll_right = offset > -max_offset + epsilon;
            if view.tab_bar_has_overflow != has_overflow
                || view.tab_bar_can_scroll_left != can_scroll_left
                || view.tab_bar_can_scroll_right != can_scroll_right
            {
                view.tab_bar_has_overflow = has_overflow;
                view.tab_bar_can_scroll_left = can_scroll_left;
                view.tab_bar_can_scroll_right = can_scroll_right;
                cx.notify();
            }
        });
        let mut tab_bar = TabBar::new("window-tabs")
            .height(window_chrome::TITLE_BAR_HEIGHT)
            .track_scroll(&self.tab_bar_scroll_handle)
            .children(
                // In split layout the tab bar shows a single synthetic tab
                // that follows the focused pane; split panes do not become
                // separate tabs (per product decision).
                self.render_tab_children(cx),
            );
        let new_tab_button = self.render_new_tab_button(cx);
        if self.tab_bar_has_overflow {
            tab_bar = tab_bar
                .end_child(self.render_tab_scroll_button(
                    TabBarScrollDirection::Left,
                    self.tab_bar_can_scroll_left,
                    cx,
                ))
                .end_child(self.render_tab_scroll_button(
                    TabBarScrollDirection::Right,
                    self.tab_bar_can_scroll_right,
                    cx,
                ))
                .end_child(new_tab_button);
        } else {
            tab_bar = tab_bar.child(new_tab_button);
        }
        tab_bar
    }

    fn render_new_tab_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .ml(DynamicSpacing::Base02.rems(cx))
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
                        this.spawn_new_tab(cx, None);
                    })),
            )
            .into_any_element()
    }

    /// The tab title follows the focused pane unless the user has renamed it,
    /// in which case the custom title stays fixed.
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
                let full_title = tab.title(cx);
                let title = util::truncate_and_trailoff(&full_title, TAB_TITLE_MAX_CHARS);
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
                    TabPosition::Middle(idx.cmp(&self.active_tab_index))
                };
                let tab_idx = idx;
                Tab::new(idx.to_string())
                    .height(window_chrome::TITLE_BAR_HEIGHT)
                    .position(position)
                    .toggle_state(idx == self.active_tab_index)
                    .tooltip(Tooltip::text_with_background_opacity(full_title, 0.8))
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
                            .mt(px(1.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                }),
                            )
                            .child(
                                IconButton::new(format!("close-tab-{tab_idx}"), IconName::Close)
                                    .icon_size(IconSize::XSmall)
                                    .size(ButtonSize::None)
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

    fn render_tab_rename_dialog(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.tab_rename.as_ref()?;
        let name = state.name.clone();
        let selection = state.selection.clone();
        let focus_handle = state.focus_handle.clone();
        let input_focused = focus_handle.is_focused(window);
        let view = cx.weak_entity();
        let dialog = div()
            .id("rename-tab-dialog")
            .key_context("TabRename")
            .w(px(360.))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(Label::new("Rename Tab").size(LabelSize::Large))
            .child(
                div()
                    .id("rename-tab-name-input")
                    .relative()
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(36.))
                    .px_2()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().background)
                    .track_focus(&focus_handle)
                    .on_key_down(cx.listener(Self::tab_rename_key_down))
                    .child(render_tab_rename_input_content(
                        &name,
                        &selection,
                        input_focused,
                        cx,
                    ))
                    .child(TabRenameInput { view, focus_handle }),
            )
            .child(
                Label::new("Enter to rename · Esc to cancel")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("rename-tab-cancel", "Cancel")
                            .style(ButtonStyle::Outlined)
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.cancel_tab_rename(window, cx);
                            })),
                    )
                    .child(
                        Button::new("rename-tab-confirm", "Rename")
                            .style(ButtonStyle::Filled)
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.commit_tab_rename(window, cx);
                            })),
                    ),
            );
        Some(
            deferred(
                div()
                    .id("rename-tab-overlay")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::black().opacity(0.4))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            this.cancel_tab_rename(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(cx.listener(Self::tab_rename_key_down))
                    .child(dialog),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

fn render_tab_rename_input_content(
    name: &str,
    selection: &Range<usize>,
    focused: bool,
    cx: &App,
) -> AnyElement {
    let text_length = name.encode_utf16().count();
    let selection_start = selection.start.min(text_length);
    let selection_end = selection.end.min(text_length).max(selection_start);
    let mut content = div()
        .flex()
        .items_center()
        .min_w_0()
        .flex_grow_1()
        .overflow_hidden();
    let caret = || {
        div()
            .flex_none()
            .w(px(1.))
            .h(px(16.))
            .bg(cx.theme().colors().text)
    };

    if name.is_empty() {
        if focused {
            content = content.child(caret());
        }
        return content
            .child(
                Label::new("Tab name")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element();
    }

    if selection_start == selection_end {
        content = content.child(Label::new(substring_utf16(name, 0..selection_start)));
        if focused {
            content = content.child(caret());
        }
        content = content.child(Label::new(substring_utf16(
            name,
            selection_start..text_length,
        )));
    } else {
        content = content
            .child(Label::new(substring_utf16(name, 0..selection_start)))
            .child(
                div()
                    .bg(cx.theme().colors().ghost_element_selected)
                    .child(Label::new(substring_utf16(
                        name,
                        selection_start..selection_end,
                    ))),
            )
            .child(Label::new(substring_utf16(
                name,
                selection_end..text_length,
            )));
    }
    content.into_any_element()
}
fn render_terminal_pane(
    terminal: Entity<Terminal>,
    tab: Entity<TerminalTab>,
    focus: FocusHandle,
    active: bool,
    window: &mut Window,
    cx: &mut Context<TerminalWindowView>,
) -> AnyElement {
    let cursor_visible = tab.read(cx).cursor_visible(cx);
    let scroll_handle = tab.read(cx).scroll_handle.clone();
    let scrollbar_id = tab.entity_id();
    let pane_id = tab.entity_id();
    let colors = cx.theme().colors();
    let focused = focus.is_focused(window);
    let search_active = tab.read(cx).search_active;
    // The pane stays visually active while its own search bar holds focus; only
    // the terminal cursor falls back to its unfocused shape.
    let search_focused = tab.read(cx).search_focus_handle.is_focused(window);
    let pane_focused = focused || (search_active && search_focused);
    let content_opacity = if pane_focused {
        1.0
    } else {
        crate::terminal::INACTIVE_PANE_OPACITY
    };
    let background_color = if pane_focused {
        colors.terminal_background
    } else {
        colors.terminal_ansi_black
    };
    // The bar lives at the top of the pane it searches and is only shown for
    // the pane the user is actually in, so a split never displays a bar bound
    // to a different terminal.
    let search_bar = (search_active && active).then(|| TerminalSearchBar::new(tab.clone()));
    // Transfers are per-pane too, but unlike the search bar they are shown
    // even when the pane is not focused: an ongoing transfer must stay
    // visible (§6).
    let transfer_bar =
        (!tab.read(cx).transfer_ui_state().is_empty()).then(|| TransferBar::new(tab.clone()));

    div()
        .id(("terminal-pane", pane_id))
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(background_color)
        .when_some(search_bar, |this, search_bar| this.child(search_bar))
        .when_some(transfer_bar, |this, transfer_bar| this.child(transfer_bar))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .size_full()
                        .opacity(content_opacity)
                        .child(TerminalElement::new(
                            terminal,
                            tab,
                            focus,
                            focused,
                            cursor_visible,
                        )),
                )
                .custom_scrollbars(
                    Scrollbars::for_settings::<TerminalScrollbarSettingsWrapper>()
                        .id(("terminal-scrollbar", scrollbar_id))
                        .show_along(ScrollAxes::Vertical)
                        .tracked_scroll_handle(&scroll_handle),
                    window,
                    cx,
                ),
        )
        .into_any_element()
}

impl Render for TerminalWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Left-clicking a pane focuses its terminal without going through an
        // action, so refresh the tab's active pane from real window focus
        // before anything reads it. Without this, actions (and the search bar)
        // keep targeting the pane that was active before the click.
        if let Some(focused) = self.focused_tab(window, cx)
            && let Some(active) = self.tabs.get_mut(self.active_tab_index)
        {
            active.active_pane_tab = Some(focused);
        }

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
        let active_pane = active_window_tab.and_then(|tab| tab.active_pane_tab.clone());
        let content = if let Some(tab) = maximized_pane {
            let terminal = tab.read(cx).terminal.clone();
            let focus = tab.read(cx).focus_handle.clone();
            div()
                .relative()
                .size_full()
                .child(render_terminal_pane(terminal, tab, focus, true, window, cx))
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
                    let active = active_pane.as_ref() == Some(&tab);
                    render_terminal_pane(terminal, tab, focus, active, window, cx)
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
                    render_terminal_pane(terminal, tab, focus, true, window, cx)
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
                .child(div().flex_1().min_h_0().child(content))
                .on_action(cx.listener(Self::copy))
                .on_action(cx.listener(Self::copy_link))
                .on_action(cx.listener(Self::open_link))
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
                            // Resolve the link under the clicked cell: the menu's
                            // link entries must act on that cell, and a click that
                            // misses a link must not offer them at all.
                            let scroll_top = tab.read(cx).scroll_top;
                            let mut position = event.position;
                            if scroll_top > Pixels::ZERO {
                                position.y += scroll_top;
                            }
                            this.context_navigation_target = tab
                                .update(cx, |tab, cx| tab.navigation_target_at(position, cx))
                                .filter(is_context_menu_navigation_target);
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
                .children(self.render_tab_rename_dialog(window, cx))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if this.tab_rename.is_some() {
                        return;
                    }
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

async fn build_terminal(
    cx: &gpui::AsyncApp,
    initial_directory: Option<PathBuf>,
) -> anyhow::Result<TerminalBuilder> {
    let settings = cx.update(|cx| TerminalSettings::get_global(cx).clone());
    let builder = cx.update(|cx| {
        launch_terminal_with(settings, initial_directory.as_deref(), cx, |launch, cx| {
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
                launch.transfer,
                cx,
                Vec::new(),
                PathStyle::local(),
            )
        })
    });
    builder.await
}

struct TerminalLaunchArguments {
    transfer: Option<TransferSetup>,
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
    initial_directory: Option<&Path>,
    cx: &App,
    launcher: impl FnOnce(TerminalLaunchArguments, &App) -> T,
) -> T {
    let launch = TerminalLaunchArguments {
        // Transfers default to on (trzsz enabled, §8.3): an unconfigured
        // `transfer` section must still arm the tap, otherwise detection can
        // never fire.
        transfer: transfer_setup(&settings.transfer.clone().unwrap_or_default()),
        working_directory: initial_directory
            .map(Path::to_path_buf)
            .or_else(|| standalone_working_directory(&settings.working_directory)),
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
    launch_terminal_with(settings, None, cx, |launch, _| FakeLaunchSnapshot {
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
        | settings::WorkingDirectory::FirstProjectDirectory
        | settings::WorkingDirectory::PreviousTab => Some(paths::home_dir().clone()),
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
                context_navigation_target: None,
                tab_rename: None,
                titlebar_mouse_down: std::cell::Cell::new(false),
                tab_bar_scroll_handle: ScrollHandle::new(),
                tab_bar_has_overflow: false,
                tab_bar_can_scroll_left: false,
                tab_bar_can_scroll_right: false,
                terminal_error: None,
                live_settings,
                pane_subscriptions: HashMap::default(),
                _subscriptions: vec![settings_subscription],
            }
        })
    }

    #[gpui::test]
    fn fixed_tab_title_overrides_generated_title_until_cleared(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            let pane = new_test_pane(&settings, 50_002, cx);
            let mut tab = WindowTab::new(pane);
            let generated_title = tab.title(cx);

            tab.set_title_override("Pinned".to_string());
            assert_eq!(tab.title(cx), "Pinned");

            tab.set_title_override("  ".to_string());
            assert_eq!(tab.title(cx), generated_title);
        });
    }

    fn pane_entity_ids(window_view: &Entity<TerminalWindowView>, cx: &App) -> Vec<gpui::EntityId> {
        window_view
            .read(cx)
            .all_panes()
            .into_iter()
            .map(|pane| pane.entity_id())
            .collect()
    }

    fn open_test_window_with_tabs(
        tabs: Vec<WindowTab>,
        active_tab_index: usize,
        cx: &mut App,
    ) -> WindowHandle<TerminalWindowView> {
        let handle = crate::open_empty_terminal_window(cx).expect("window should open");
        handle
            .update(cx, |view, window, cx| {
                for tab in &tabs {
                    for pane in tab.panes() {
                        view.observe_tab(&pane, cx);
                    }
                }
                view.tabs = tabs;
                view.activate_tab(active_tab_index, window, cx);
            })
            .expect("window should exist");
        handle
    }

    fn dispatch_test_menu_actions(
        source: WindowHandle<TerminalWindowView>,
        actions: &[&str],
        cx: &mut gpui::TestAppContext,
    ) {
        for action in actions {
            cx.update(|cx| {
                gpui::AnyWindowHandle::from(source)
                    .update(cx, |_, window, cx| {
                        let action = cx
                            .build_action(action, None)
                            .expect("menu action should exist");
                        window.dispatch_action(action, cx);
                    })
                    .expect("source should exist during menu action");
            });
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn move_tab_preserves_split_session_and_rebinds_events(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let (source, destination, first, second, before) = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            let remaining = new_test_pane(&settings, 51_000, cx);
            let first = new_test_pane(&settings, 51_001, cx);
            let second = new_test_pane(&settings, 51_002, cx);
            first.update(cx, |pane, cx| {
                pane.search_query = "preserved query".into();
                pane.search_active = true;
                pane.terminal.update(cx, |terminal, cx| {
                    terminal.write_output(b"retained terminal output\r\n", cx);
                });
            });
            let mut split_root = SplitNode::leaf(first.clone());
            assert!(split_root.split(&first, second.clone(), SplitDirection::Right));
            if let SplitNode::Axis { flexes, .. } = &mut split_root {
                *flexes = vec![0.3, 0.7];
            }
            let source = open_test_window_with_tabs(
                vec![
                    WindowTab::new(remaining),
                    WindowTab {
                        split_root: Some(split_root),
                        active_pane_tab: Some(second.clone()),
                        maximized_pane_tab: Some(second.clone()),
                        title_override: Some("Running work".into()),
                    },
                ],
                1,
                cx,
            );
            let destination_pane = new_test_pane(&settings, 51_003, cx);
            let destination =
                open_test_window_with_tabs(vec![WindowTab::new(destination_pane)], 0, cx);
            let before = pane_invariant_snapshots(&[first.clone(), second.clone()], cx);
            source
                .update(cx, |view, window, cx| {
                    view.move_tab_to_window(&second, Some(destination), window, cx)
                        .expect("tab should move");
                    assert_eq!(view.tabs.len(), 1);
                    assert_eq!(view.active_tab_index, 0);
                    assert_eq!(view.pane_subscriptions.len(), 1);
                    assert!(
                        view.active_tab()
                            .expect("remaining tab")
                            .read(cx)
                            .focus_handle
                            .is_focused(window)
                    );
                })
                .expect("source should remain open");
            destination
                .update(cx, |view, window, cx| {
                    assert_eq!(view.tabs.len(), 2);
                    assert_eq!(view.active_tab_index, 1);
                    assert_eq!(view.pane_subscriptions.len(), 3);
                    let tab = view.tabs.last().expect("moved tab");
                    assert_eq!(tab.title(cx), "Running work");
                    assert_eq!(tab.focused_tab(), Some(second.clone()));
                    assert_eq!(tab.maximized_pane_tab, Some(second.clone()));
                    match tab.split_root.as_ref().expect("split layout") {
                        SplitNode::Axis { flexes, .. } => assert_eq!(flexes, &[0.3, 0.7]),
                        SplitNode::Leaf { .. } => panic!("split layout should survive"),
                    }
                    assert!(second.read(cx).focus_handle.is_focused(window));
                })
                .expect("destination should exist");
            assert_eq!(first.read(cx).search_query, "preserved query");
            assert!(first.read(cx).search_active);
            (source, destination, first, second, before)
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                pane_invariant_snapshots(&[first.clone(), second.clone()], cx),
                before
            );
            first.update(cx, |_, cx| cx.emit(TerminalTabEvent::CloseTerminal));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            source
                .update(cx, |view, _, _| assert_eq!(view.tabs.len(), 1))
                .expect("source should remain open");
            destination
                .update(cx, |view, _, _| {
                    let tab = view.tabs.last().expect("moved tab");
                    assert_eq!(tab.panes(), vec![second.clone()]);
                    assert!(tab.split_root.is_none());
                })
                .expect("destination should remain open");
            second.update(cx, |_, cx| cx.emit(TerminalTabEvent::CloseTerminal));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            destination
                .update(cx, |view, _, _| assert_eq!(view.tabs.len(), 1))
                .expect("destination's original tab should survive");
        });
    }

    #[gpui::test]
    fn tab_move_destinations_follow_screen_position(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        cx.update(|cx| {
            let open_window = |position, cx: &mut App| {
                cx.open_window(
                    crate::window_options(Bounds::new(position, gpui::size(px(600.), px(400.)))),
                    |_, cx| cx.new(TerminalWindowView::new_empty),
                )
                .expect("terminal window should open")
            };
            let source = open_window(gpui::point(px(-900.), px(-900.)), cx);
            let lower = open_window(gpui::point(px(350.), px(200.)), cx);
            let top_left = open_window(gpui::point(px(-50.), px(-100.)), cx);
            let top_right = open_window(gpui::point(px(400.), px(-100.)), cx);
            let overlapping = open_window(gpui::point(px(-50.), px(-100.)), cx);
            let bottom_left = open_window(gpui::point(px(-600.), px(300.)), cx);
            let destinations = source
                .update(cx, |_, window, cx| {
                    TerminalWindowView::tab_move_destinations(
                        window.window_handle().window_id(),
                        cx,
                    )
                })
                .expect("source should exist");
            assert_eq!(
                destinations
                    .iter()
                    .map(|destination| destination.window)
                    .collect::<Vec<_>>(),
                vec![top_left, overlapping, top_right, lower, bottom_left],
            );
            assert_eq!(
                destinations
                    .iter()
                    .map(|destination| destination.position)
                    .collect::<Vec<_>>(),
                vec![
                    Some(gpui::point(px(-50.), px(-100.))),
                    Some(gpui::point(px(-50.), px(-100.))),
                    Some(gpui::point(px(400.), px(-100.))),
                    Some(gpui::point(px(350.), px(200.))),
                    Some(gpui::point(px(-600.), px(300.))),
                ],
            );
        });
    }

    #[gpui::test]
    fn move_tab_from_context_menu_to_existing_window(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let (source, destination, moving) = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            let remaining = new_test_pane(&settings, 55_000, cx);
            let moving = new_test_pane(&settings, 55_001, cx);
            let destination_pane = new_test_pane(&settings, 55_002, cx);
            let source = open_test_window_with_tabs(
                vec![WindowTab::new(remaining), WindowTab::new(moving.clone())],
                1,
                cx,
            );
            let destination =
                open_test_window_with_tabs(vec![WindowTab::new(destination_pane)], 0, cx);
            source
                .update(cx, |view, window, cx| {
                    view.deploy_tab_context_menu(gpui::point(px(30.), px(20.)), window, cx);
                })
                .expect("source menu should open");
            (source, destination, moving)
        });
        cx.run_until_parked();
        dispatch_test_menu_actions(
            source,
            &[
                "menu::SelectFirst",
                "menu::SelectNext",
                "menu::Confirm",
                "menu::SelectNext",
                "menu::Confirm",
            ],
            cx,
        );
        cx.update(|cx| {
            source
                .update(cx, |view, _, _| {
                    assert_eq!(view.tabs.len(), 1);
                    assert!(view.context_menu.is_none());
                })
                .expect("source should keep its other tab");
            destination
                .update(cx, |view, window, cx| {
                    assert_eq!(view.tabs.len(), 2);
                    assert_eq!(view.active_tab(), Some(moving.clone()));
                    assert!(moving.read(cx).focus_handle.is_focused(window));
                })
                .expect("destination should receive the selected tab");
        });
    }

    #[gpui::test]
    fn move_tab_to_new_window_closes_empty_source_without_launching_shell(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test_theme(cx);
        let (source, pane) = cx.update(|cx| {
            cx.set_global(TestTerminalLaunchFailure {
                reason: LAUNCH_FAILURE_REASON.into(),
            });
            let settings = TerminalSettings::get_global(cx).clone();
            let pane = new_test_pane(&settings, 52_000, cx);
            let source = open_test_window_with_tabs(vec![WindowTab::new(pane.clone())], 0, cx);
            source
                .update(cx, |view, window, cx| {
                    view.deploy_tab_context_menu(gpui::point(px(30.), px(20.)), window, cx);
                })
                .expect("source should exist during move");
            (source, pane)
        });
        cx.run_until_parked();
        dispatch_test_menu_actions(
            source,
            &[
                "menu::SelectFirst",
                "menu::SelectNext",
                "menu::Confirm",
                "menu::Confirm",
            ],
            cx,
        );
        cx.update(|cx| {
            assert!(source.entity(cx).is_err());
            assert_eq!(cx.windows().len(), 1);
            let destination = cx
                .windows()
                .into_iter()
                .find_map(|handle| handle.downcast::<TerminalWindowView>())
                .expect("new terminal window");
            destination
                .update(cx, |view, window, cx| {
                    assert_eq!(view.tabs.len(), 1);
                    assert_eq!(view.active_tab(), Some(pane.clone()));
                    assert!(
                        view.terminal_error.is_none(),
                        "moving must not launch another shell"
                    );
                    assert!(pane.read(cx).focus_handle.is_focused(window));
                })
                .expect("new window should exist");
            pane.update(cx, |_, cx| cx.emit(TerminalTabEvent::CloseTerminal));
        });
        cx.run_until_parked();
        cx.update(|cx| assert!(cx.windows().is_empty()));
    }

    #[gpui::test]
    fn move_tab_to_closed_destination_keeps_source_and_reports_error(
        cx: &mut gpui::TestAppContext,
    ) {
        init_test_theme(cx);
        let (source, destination, pane) = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            let pane = new_test_pane(&settings, 53_000, cx);
            let source = open_test_window_with_tabs(vec![WindowTab::new(pane.clone())], 0, cx);
            let destination =
                crate::open_empty_terminal_window(cx).expect("destination should open");
            destination
                .update(cx, |_, window, _| window.remove_window())
                .expect("destination should close");
            (source, destination, pane)
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let source_view = source.entity(cx).expect("source should exist").downgrade();
            gpui::AnyWindowHandle::from(source)
                .update(cx, |_, window, cx| {
                    TerminalWindowView::request_tab_move(
                        &source_view,
                        &pane,
                        Some(destination),
                        window,
                        cx,
                    );
                })
                .expect("source should remain open");
            source
                .update(cx, |view, _, _| {
                    assert_eq!(view.active_tab(), Some(pane));
                    assert_eq!(view.pane_subscriptions.len(), 1);
                    assert!(
                        view.terminal_error
                            .as_ref()
                            .is_some_and(|error| error.starts_with("Could not move terminal tab:"))
                    );
                })
                .expect("source should remain intact");
        });
    }

    #[gpui::test]
    fn move_tab_routes_already_queued_exit_to_current_owner(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        for (keep_source, defer_move) in
            [(true, false), (true, true), (false, false), (false, true)]
        {
            let (source, destination) = cx.update(|cx| {
                let settings = TerminalSettings::get_global(cx).clone();
                let remaining = new_test_pane(&settings, 54_000, cx);
                let moving = new_test_pane(&settings, 54_001, cx);
                let destination_pane = new_test_pane(&settings, 54_002, cx);
                let mut source_tabs = Vec::new();
                if keep_source {
                    source_tabs.push(WindowTab::new(remaining));
                }
                source_tabs.push(WindowTab::new(moving.clone()));
                let source = open_test_window_with_tabs(source_tabs, usize::from(keep_source), cx);
                let destination =
                    open_test_window_with_tabs(vec![WindowTab::new(destination_pane)], 0, cx);
                moving.update(cx, |_, cx| cx.emit(TerminalTabEvent::CloseTerminal));
                let move_tab = move |cx: &mut App| {
                    source
                        .update(cx, |view, window, cx| {
                            view.move_tab_to_window(&moving, Some(destination), window, cx)
                                .expect("tab should move before the deferred exit");
                        })
                        .expect("source should exist");
                };
                if defer_move {
                    cx.defer(move_tab);
                } else {
                    move_tab(cx);
                }
                (source, destination)
            });
            cx.run_until_parked();
            cx.update(|cx| {
                if keep_source {
                    source
                        .update(cx, |view, _, _| assert_eq!(view.tabs.len(), 1))
                        .expect("source's original tab should survive");
                } else {
                    assert!(source.entity(cx).is_err(), "empty source should close");
                }
                destination
                    .update(cx, |view, _, _| assert_eq!(view.tabs.len(), 1))
                    .expect("destination's original tab should survive");
            });
        }
    }

    #[gpui::test]
    fn file_manager_requests_follow_window_preference(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let (first, second) = cx.update(|cx| {
            cx.set_global(TestTerminalLaunchFailure {
                reason: LAUNCH_FAILURE_REASON.to_string(),
            });
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_default()
                        .file_manager_open_mode = Some(settings::FileManagerOpenMode::NewTab);
                });
            });
            crate::open_new_window(cx);
            let first = cx
                .windows()
                .into_iter()
                .find_map(|handle| handle.downcast::<TerminalWindowView>())
                .expect("first terminal window should exist");
            crate::open_new_window(cx);
            let second = cx
                .windows()
                .into_iter()
                .filter_map(|handle| handle.downcast::<TerminalWindowView>())
                .find(|handle| *handle != first)
                .expect("second terminal window should exist");
            (first, second)
        });
        cx.run_until_parked();
        cx.update(|cx| {
            for handle in [first, second] {
                handle
                    .update(cx, |view, _, _| view.terminal_error = None)
                    .expect("terminal window should exist");
            }
            crate::settings_ui::open_settings_window(cx);
            crate::open_external_directory(Some(std::env::temp_dir()), cx);
            assert_eq!(cx.windows().len(), 3);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            first
                .update(cx, |view, _, _| assert!(view.terminal_error.is_none()))
                .expect("first window should be untouched");
            second
                .update(cx, |view, _, _| {
                    assert_eq!(view.terminal_error.as_deref(), Some(VISIBLE_LAUNCH_FAILURE));
                    view.terminal_error = None;
                })
                .expect("recent window should receive the tab request");
            first
                .update(cx, |_, window, _| window.activate_window())
                .expect("first window should activate");
            crate::open_external_directory(Some(std::env::temp_dir()), cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            first
                .update(cx, |view, _, _| {
                    assert_eq!(view.terminal_error.as_deref(), Some(VISIBLE_LAUNCH_FAILURE));
                })
                .expect("active window should receive the tab request");
            second
                .update(cx, |view, _, _| assert!(view.terminal_error.is_none()))
                .expect("other window should be untouched");
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_default()
                        .file_manager_open_mode = Some(settings::FileManagerOpenMode::NewWindow);
                });
            });
            crate::open_external_directory(Some(std::env::temp_dir()), cx);
            assert_eq!(cx.windows().len(), 4);
        });
    }

    #[gpui::test]
    fn file_manager_new_tab_creates_window_when_none_remain(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        cx.update(|cx| {
            cx.set_global(TestTerminalLaunchFailure {
                reason: LAUNCH_FAILURE_REASON.to_string(),
            });
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |content| {
                    content
                        .terminal
                        .get_or_insert_default()
                        .file_manager_open_mode = Some(settings::FileManagerOpenMode::NewTab);
                });
            });
            crate::open_new_window(cx);
            for handle in cx.windows() {
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .expect("window should close");
            }
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.windows().is_empty());
            crate::open_external_directory(Some(std::env::temp_dir()), cx);
            assert_eq!(cx.windows().len(), 1);
        });
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
            window_view.update(cx, |window_view, cx| window_view.spawn_new_tab(cx, None));
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
                    title_override: None,
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
            title_override: None,
        });
        let active_tab_index = tabs.len() - 1;

        let window_view = cx.new(|cx| TerminalWindowView::new(cx, None));
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

    /// The search bar must actually lay out its query field: the text field is
    /// a custom element that registers GPUI's input handler during paint, so a
    /// layout regression would silently break typing.
    #[gpui::test]
    fn the_search_bar_lays_out_its_query_field(cx: &mut gpui::TestAppContext) {
        struct SearchBarHost {
            tab: Entity<TerminalTab>,
        }

        impl Render for SearchBarHost {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                TerminalSearchBar::new(self.tab.clone())
            }
        }

        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let pane = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            new_test_pane(&settings, 70_000, cx)
        });
        cx.update(|cx| {
            pane.update(cx, |pane, _| {
                pane.search_active = true;
                pane.search_query = "needle".to_string();
            });
        });

        let (_host, window) = cx.add_window_view(|_, _| SearchBarHost { tab: pane.clone() });

        let bar = window
            .debug_bounds("terminal-search-bar")
            .expect("the search bar should render");
        assert!(bar.size.width > px(0.));
        let field = window
            .debug_bounds("terminal-search-query")
            .expect("the query field should render");
        assert!(field.size.width > px(0.));

        // An empty, unfocused field draws no caret; the placeholder alone must
        // not be the only thing the user sees once the field owns focus.
        assert!(window.debug_bounds("terminal-search-caret").is_none());

        pane.update_in(window, |pane, window, cx| {
            pane.search_focus_handle.clone().focus(window, cx);
            cx.notify();
        });
        window.run_until_parked();

        let caret = window
            .debug_bounds("terminal-search-caret")
            .expect("a focused field shows a caret even when empty");
        assert_eq!(caret.size.width, px(2.));
        assert!(caret.size.height > px(0.));
    }

    /// Builds a window view over a horizontal split of `first` and `second`,
    /// with `first` recorded as the active pane.
    fn split_window_view(
        first: Entity<TerminalTab>,
        second: Entity<TerminalTab>,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<TerminalWindowView>, &mut gpui::VisualTestContext) {
        let mut split_root = SplitNode::leaf(first.clone());
        assert!(split_root.split(&first, second, SplitDirection::Right));
        let tabs = vec![WindowTab {
            split_root: Some(split_root),
            active_pane_tab: Some(first),
            maximized_pane_tab: None,
            title_override: None,
        }];

        cx.add_window_view(move |_, cx| {
            let live_settings =
                LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
            let settings_subscription =
                cx.observe_global::<settings::SettingsStore>(TerminalWindowView::settings_changed);
            TerminalWindowView {
                focus_handle: cx.focus_handle(),
                tabs,
                active_tab_index: 0,
                context_menu: None,
                context_navigation_target: None,
                tab_rename: None,
                titlebar_mouse_down: std::cell::Cell::new(false),
                tab_bar_scroll_handle: ScrollHandle::new(),
                tab_bar_has_overflow: false,
                tab_bar_can_scroll_left: false,
                tab_bar_can_scroll_right: false,
                terminal_error: None,
                live_settings,
                pane_subscriptions: HashMap::default(),
                _subscriptions: vec![settings_subscription],
            }
        })
    }

    fn init_test_theme(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    /// A split layout must show the search bar inside the pane it searches and
    /// only for the pane the user is in; a left-click focuses a pane without
    /// going through an action, which used to leave the bar pointing at the
    /// previously active pane.
    #[gpui::test]
    fn only_the_focused_pane_shows_its_search_bar(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let (first, second) = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            (
                new_test_pane(&settings, 90_000, cx),
                new_test_pane(&settings, 90_001, cx),
            )
        });
        let (_view, window) = split_window_view(first.clone(), second.clone(), cx);
        window.run_until_parked();

        // Open search in the first pane.
        first.update_in(window, |pane, window, cx| pane.toggle_search(window, cx));
        window.run_until_parked();
        assert!(
            window.debug_bounds("terminal-search-bar").is_some(),
            "the pane that opened search should show its bar"
        );

        // Focusing the other pane takes the bar away with it.
        second.update_in(window, |pane, window, cx| {
            pane.focus_handle.clone().focus(window, cx);
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("terminal-search-bar").is_none(),
            "a pane without an open search must not show the other pane's bar"
        );

        // Returning to the first pane restores its own search bar.
        first.update_in(window, |pane, window, cx| {
            pane.focus_handle.clone().focus(window, cx);
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("terminal-search-bar").is_some(),
            "the search bar should follow focus back to its pane"
        );
    }

    /// `cmd-f` (and the context menu's Search entry, which dispatches the same
    /// action) must open search in the pane that actually holds focus, not in
    /// the pane that was active before a plain left-click moved focus.
    #[gpui::test]
    fn search_targets_the_pane_that_holds_focus(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let (first, second) = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            (
                new_test_pane(&settings, 91_000, cx),
                new_test_pane(&settings, 91_001, cx),
            )
        });
        let (view, window) = split_window_view(first.clone(), second.clone(), cx);
        window.run_until_parked();

        // The split starts with `first` recorded as active; clicking into
        // `second` only moves window focus.
        second.update_in(window, |pane, window, cx| {
            pane.focus_handle.clone().focus(window, cx);
        });
        window.run_until_parked();

        view.update_in(window, |view, window, cx| {
            view.toggle_search(&SearchTest, window, cx);
        });
        window.run_until_parked();

        window.update(|_, cx| {
            assert!(
                second.read(cx).search_active,
                "search should open in the focused pane"
            );
            assert!(
                !first.read(cx).search_active,
                "search must not open in the previously active pane"
            );
        });
        assert!(
            window.debug_bounds("terminal-search-bar").is_some(),
            "the focused pane should show the search bar"
        );
    }

    /// The painted grid has to end flush with the pane it fills. When the row
    /// count is derived from a device-rounded cell height while the rows are
    /// painted at the unrounded height, the grid and the pane disagree by up to
    /// a row: the terminal leaves a blank strip under the last line, and
    /// scrolling back to the bottom can show that strip because the alignment
    /// decision was made from the previous frame's scroll position.
    #[gpui::test]
    fn terminal_grid_fills_the_pane_and_keeps_the_last_row_visible(cx: &mut gpui::TestAppContext) {
        init_test_theme(cx);
        let pane = cx.update(|cx| {
            let settings = TerminalSettings::get_global(cx).clone();
            let pane = new_test_pane(&settings, 96_000, cx);
            pane.update(cx, |pane, cx| {
                pane.terminal.update(cx, |terminal, cx| {
                    for index in 0..200 {
                        terminal.write_output(format!("line {index}\r\n").as_bytes(), cx);
                    }
                });
            });
            pane
        });

        let tabs = vec![WindowTab::new(pane.clone())];
        let (_view, window) = cx.add_window_view(move |_, cx| {
            let live_settings =
                LiveTerminalSettings::from_settings(TerminalSettings::get_global(cx));
            let settings_subscription =
                cx.observe_global::<settings::SettingsStore>(TerminalWindowView::settings_changed);
            TerminalWindowView {
                focus_handle: cx.focus_handle(),
                tabs,
                active_tab_index: 0,
                context_menu: None,
                context_navigation_target: None,
                tab_rename: None,
                titlebar_mouse_down: std::cell::Cell::new(false),
                tab_bar_scroll_handle: ScrollHandle::new(),
                tab_bar_has_overflow: false,
                tab_bar_can_scroll_left: false,
                tab_bar_can_scroll_right: false,
                terminal_error: None,
                live_settings,
                pane_subscriptions: HashMap::default(),
                _subscriptions: vec![settings_subscription],
            }
        });
        window.run_until_parked();

        // Distance between the bottom of the painted grid and the bottom of the
        // pane, plus the painted cell height.
        let grid_geometry = |window: &mut gpui::VisualTestContext| -> (f32, f32) {
            window.update(|window, cx| {
                let bounds = pane
                    .read(cx)
                    .terminal
                    .read(cx)
                    .last_content()
                    .terminal_bounds;
                let painted_bottom = f32::from(bounds.bounds.origin.y)
                    + bounds.num_lines() as f32 * f32::from(bounds.line_height);
                let pane_bottom = f32::from(window.viewport_size().height);
                (pane_bottom - painted_bottom, f32::from(bounds.line_height))
            })
        };
        let write_bottom_prompt = |window: &mut gpui::VisualTestContext| {
            window.update(|_window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.terminal.update(cx, |terminal, cx| {
                        terminal.write_output(b"\x1b[999;1Hprompt\x1b[K", cx);
                    });
                    cx.notify();
                });
            });
            window.run_until_parked();
        };

        let line_heights = [
            settings::TerminalLineHeight::Standard,
            settings::TerminalLineHeight::Custom(1.21),
            settings::TerminalLineHeight::Custom(1.24),
            settings::TerminalLineHeight::Custom(1.29),
        ];
        for line_height in line_heights {
            window.update(|_window, cx| {
                SettingsStore::update_global(cx, |store, cx| {
                    store.update_user_settings(cx, |content| {
                        let terminal = content
                            .terminal
                            .get_or_insert_with(settings::TerminalSettingsContent::default);
                        terminal.line_height = Some(line_height.clone());
                    });
                });
            });
            for (width, height) in [(934., 596.), (1000., 700.), (901., 533.)] {
                window.simulate_resize(gpui::size(px(width), px(height)));
                window.run_until_parked();

                // At the bottom with content on the last row the grid is
                // anchored, so it must end exactly at the pane bottom.
                write_bottom_prompt(window);
                let (gap, _) = grid_geometry(window);
                assert!(
                    gap.abs() < 0.5,
                    "anchored grid must end at the pane bottom for {line_height:?} at {width}x{height}, gap was {gap}",
                );

                // Scrolled into history the grid is top-aligned; the blank
                // remainder below it must stay smaller than one row.
                window.update(|_window, cx| {
                    pane.update(cx, |pane, cx| {
                        pane.terminal
                            .update(cx, |terminal, _| terminal.scroll_up_by(5));
                        cx.notify();
                    });
                });
                window.run_until_parked();
                let (gap, line_height_px) = grid_geometry(window);
                assert!(
                    gap >= 0.0 && gap < line_height_px,
                    "scrolled grid must fill the pane up to less than one row for {line_height:?} at {width}x{height}, gap was {gap}",
                );

                // Returning to the bottom has to anchor in the same frame that
                // applies the scroll, not the one after it.
                window.update(|_window, cx| {
                    pane.update(cx, |pane, cx| {
                        pane.terminal
                            .update(cx, |terminal, _| terminal.scroll_to_bottom());
                        cx.notify();
                    });
                });
                window.run_until_parked();
                let (gap, _) = grid_geometry(window);
                assert!(
                    gap.abs() < 0.5,
                    "scrolling back to the bottom must anchor the grid immediately for {line_height:?} at {width}x{height}, gap was {gap}",
                );
            }
        }
    }
}
