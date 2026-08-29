//! Lightweight split-pane tree for the standalone terminal app.
//!
//! Mirrors the semantics of Zed's `Member`/`PaneAxis` (workspace/pane_group.rs)
//! without its workspace dependencies: no persistence, no dock integration, and
//! flexes are the only stored layout state.

use gpui::{
    AnyElement, App, Axis, Entity, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Styled as _, px,
};
use theme::ActiveTheme as _;

use crate::terminal::TerminalTab;

/// Hitbox width of the divider in pixels (Zed: HANDLE_HITBOX_SIZE = 4).
pub(crate) const DIVIDER_HITBOX: Pixels = px(6.);
/// Smallest allowed flex weight, keeping a leaf visible during a drag.
const MIN_FLEX: f32 = 0.05;

/// A split direction, mirroring Zed's `SplitDirection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SplitDirection {
    pub(crate) fn axis(self) -> Axis {
        match self {
            Self::Up | Self::Down => Axis::Vertical,
            Self::Left | Self::Right => Axis::Horizontal,
        }
    }

    /// Whether the split inserts after the target leaf (Zed:
    /// `SplitDirection::increasing`).
    fn increasing(self) -> bool {
        match self {
            Self::Left | Self::Up => false,
            Self::Down | Self::Right => true,
        }
    }
}

/// A node in the split tree: either a leaf holding one terminal tab or an
/// axis holding children along one direction.
#[derive(Clone)]
pub(crate) enum SplitNode {
    Leaf {
        tab: Entity<TerminalTab>,
    },
    Axis {
        axis: Axis,
        /// Flex weight per child; all `1.` means equal split.
        flexes: Vec<f32>,
        children: Vec<SplitNode>,
    },
}

thread_local! {
    /// The tab of the leaf under the cursor when a right-click lands on a
    /// split pane. Consumed by the window's right-click handler so the
    /// context menu acts on the clicked pane, not just the focused one.
    static CLICKED_LEAF: std::cell::Cell<Option<Entity<TerminalTab>>> =
        const { std::cell::Cell::new(None) };
}

/// Records which leaf a right-click landed on.
pub(crate) fn set_clicked_leaf(tab: Entity<TerminalTab>) {
    CLICKED_LEAF.with(|cell| cell.set(Some(tab)));
}

/// Returns and clears the recorded right-clicked leaf, if any.
pub(crate) fn take_clicked_leaf() -> Option<Entity<TerminalTab>> {
    CLICKED_LEAF.with(|cell| cell.take())
}

impl SplitNode {
    pub(crate) fn leaf(tab: Entity<TerminalTab>) -> Self {
        Self::Leaf { tab }
    }

    fn new_axis(axis: Axis, first: Entity<TerminalTab>, second: Entity<TerminalTab>) -> Self {
        Self::Axis {
            axis,
            flexes: vec![1., 1.],
            children: vec![Self::leaf(first), Self::leaf(second)],
        }
    }

    fn is_leaf_with(&self, tab: &Entity<TerminalTab>) -> bool {
        match self {
            Self::Leaf { tab: leaf_tab } => leaf_tab == tab,
            Self::Axis { .. } => false,
        }
    }

    /// Splits the leaf holding `old_tab` by inserting `new_tab` next to it in
    /// `direction`. Returns false when `old_tab` is not in this subtree.
    /// Mirrors Zed's `PaneAxis::split` (workspace/pane_group.rs). A single
    /// leaf is promoted in place to a two-leaf axis in `direction`'s axis.
    pub(crate) fn split(
        &mut self,
        old_tab: &Entity<TerminalTab>,
        new_tab: Entity<TerminalTab>,
        direction: SplitDirection,
    ) -> bool {
        match self {
            Self::Leaf { tab } if tab == old_tab => {
                // First split: promote the lone leaf to a two-leaf axis.
                let tab = tab.clone();
                *self = Self::new_axis(direction.axis(), tab, new_tab);
                true
            }
            Self::Leaf { .. } => false,
            Self::Axis {
                axis,
                children,
                flexes,
            } => {
                for index in 0..children.len() {
                    if children[index].is_leaf_with(old_tab) {
                        if *axis == direction.axis() {
                            // Same axis: the new leaf becomes a sibling.
                            let insert_at = if direction.increasing() {
                                index + 1
                            } else {
                                index
                            };
                            children.insert(insert_at, Self::leaf(new_tab));
                            *flexes = vec![1.; children.len()];
                        } else {
                            // Different axis: replace the leaf with a fresh
                            // two-leaf axis in the split direction.
                            let Self::Leaf { tab } = std::mem::replace(
                                &mut children[index],
                                Self::leaf(new_tab.clone()),
                            ) else {
                                unreachable!("just checked is_leaf_with");
                            };
                            children[index] = Self::new_axis(direction.axis(), tab, new_tab);
                        }
                        return true;
                    }
                    // Borrowck: clone the tab handle so the recursive call
                    // can run while iterating.
                    let old_tab = old_tab.clone();
                    if children[index].split(&old_tab, new_tab.clone(), direction) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Removes the leaf holding `tab` and collapses the tree the same way
    /// Zed's `PaneAxis::remove` does: an axis with a single remaining child
    /// is replaced by that child. Returns `Some(replacement)` when the caller
    /// should replace this node.
    pub(crate) fn remove(&mut self, tab: &Entity<TerminalTab>) -> Option<SplitNode> {
        let Self::Axis {
            children, flexes, ..
        } = self
        else {
            return None;
        };
        for index in 0..children.len() {
            let child = &mut children[index];
            if child.is_leaf_with(tab) {
                children.remove(index);
                flexes.truncate(children.len());
                if children.is_empty() {
                    return None;
                }
                if children.len() == 1 {
                    let replacement = std::mem::replace(&mut children[0], Self::leaf(tab.clone()));
                    return Some(replacement);
                }
                *flexes = vec![1.; children.len()];
                return None;
            }
            if let Some(replacement) = child.remove(tab) {
                *child = replacement;
                return None;
            }
        }
        None
    }

    /// Flattened leaf tabs in visual order (left→right / top→bottom).
    pub(crate) fn collect_tabs<'a>(&'a self, out: &mut Vec<&'a Entity<TerminalTab>>) {
        match self {
            Self::Leaf { tab } => out.push(tab),
            Self::Axis { children, .. } => {
                for child in children {
                    child.collect_tabs(out);
                }
            }
        }
    }

    /// Recursively renders the tree; each leaf is laid out with flex-grow
    /// proportional to its stored flex weight, with dividers between
    /// siblings. Mouse-down on a divider latches a `DividerDragState` (via
    /// `set_drag_anchor`); the window-level mouse-move handler then calls
    /// `resize_flexes` every frame — the same split of responsibilities as
    /// Zed's `PaneAxisHandleLayout` + window drag tracking, chosen because
    /// divider-local `on_mouse_move` stops firing once the cursor leaves the
    /// 6px hitbox.
    pub(crate) fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut App,
        leaf_renderer: &mut dyn FnMut(&Entity<TerminalTab>) -> AnyElement,
    ) -> AnyElement {
        match self {
            Self::Leaf { tab } => gpui::div()
                .size_full()
                .on_mouse_down(MouseButton::Right, {
                    let tab = tab.clone();
                    move |_event: &MouseDownEvent, _window: &mut gpui::Window, _cx: &mut App| {
                        // Remember which pane the right-click hit so the
                        // context menu acts on it (split layout).
                        set_clicked_leaf(tab.clone());
                    }
                })
                .child(leaf_renderer(tab))
                .into_any_element(),
            Self::Axis {
                axis,
                flexes,
                children,
            } => {
                let divider_color = cx.theme().colors().border;
                let axis_for_id = *axis;
                let container = gpui::div().size_full().flex();
                let container = match axis {
                    Axis::Horizontal => container.flex_row(),
                    Axis::Vertical => container.flex_col(),
                };
                let mut elements = Vec::with_capacity(children.len() * 2);
                let children_len = children.len();
                for (index, child) in children.iter_mut().enumerate() {
                    let rendered = child.render(_window, cx, leaf_renderer);
                    // Taffy distributes free space proportionally to
                    // flex-grow, so the weight is the whole sizing story.
                    elements.push(
                        gpui::div()
                            .flex_grow(flex_weight(flexes, index))
                            .min_w_0()
                            .min_h_0()
                            .child(rendered)
                            .into_any_element(),
                    );
                    if index + 1 < children_len {
                        let divider = render_divider(*axis, divider_color)
                            .id(gpui::ElementId::NamedInteger(
                                format!("{axis_for_id:?}").into(),
                                index as u64,
                            ))
                            .on_mouse_down(MouseButton::Left, {
                                let axis_for_drag = *axis;
                                move |_event: &MouseDownEvent, window: &mut gpui::Window, _cx| {
                                    let position = window.mouse_position();
                                    let position = if axis_for_drag == Axis::Horizontal {
                                        position.x
                                    } else {
                                        position.y
                                    };
                                    set_drag_anchor(axis_for_drag, position);
                                }
                            })
                            .into_any_element();
                        elements.push(divider);
                    }
                }
                container.children(elements).into_any_element()
            }
        }
    }

    /// Resizes the flex pair following the first divider on `axis` whose
    /// stored bounds the cursor crossed. Called from the window mouse-move
    /// handler while a drag is latched.
    pub(crate) fn resize_flexes(&mut self, axis: Axis, container_length: Pixels, delta: Pixels) {
        let Self::Axis {
            axis: node_axis,
            flexes,
            ..
        } = self
        else {
            return;
        };
        if *node_axis != axis {
            return;
        }
        if flexes.len() < 2 || container_length <= Pixels::ZERO {
            return;
        }
        let total: f32 = flexes.iter().sum();
        if total <= 0. {
            return;
        }
        let delta_flex = (delta / container_length) * total;
        let left_index = 0;
        let new_left = (flexes[left_index] + delta_flex).max(MIN_FLEX);
        let new_right = flexes[left_index + 1] - (new_left - flexes[left_index]);
        if new_right < MIN_FLEX {
            return;
        }
        flexes[left_index] = new_left;
        flexes[left_index + 1] = new_right;
    }
}

fn flex_weight(flexes: &[f32], index: usize) -> f32 {
    flexes.get(index).copied().unwrap_or(1.).max(MIN_FLEX)
}

fn render_divider(axis: Axis, color: gpui::Hsla) -> gpui::Div {
    match axis {
        Axis::Horizontal => gpui::div().flex_none().bg(color).w(DIVIDER_HITBOX).h_full(),
        Axis::Vertical => gpui::div().flex_none().bg(color).w_full().h(DIVIDER_HITBOX),
    }
}

thread_local! {
    static DRAG_ANCHOR: std::cell::Cell<Option<gpui::Pixels>> =
        const { std::cell::Cell::new(None) };
    static DRAG_AXIS: std::cell::Cell<Option<Axis>> = const { std::cell::Cell::new(None) };
}

/// Latches the divider drag anchor at the given axis-relative position.
pub(crate) fn set_drag_anchor(axis: Axis, position: Pixels) {
    DRAG_ANCHOR.with(|cell| cell.set(Some(position)));
    DRAG_AXIS.with(|cell| cell.set(Some(axis)));
}

/// Returns the latched drag axis, if any.
pub(crate) fn drag_axis() -> Option<Axis> {
    DRAG_AXIS.with(|cell| cell.get())
}

/// Returns and clears the latched drag anchor (used to detect drag end).
pub(crate) fn take_drag_anchor() -> Option<Pixels> {
    DRAG_AXIS.with(|cell| cell.take());
    DRAG_ANCHOR.with(|cell| cell.take())
}

/// Reads the drag anchor without clearing it.
pub(crate) fn drag_anchor() -> Option<Pixels> {
    DRAG_ANCHOR.with(|cell| cell.get())
}

/// Rewrites the anchor to `position` after a resize step.
pub(crate) fn update_drag_anchor(position: Pixels) {
    DRAG_ANCHOR.with(|cell| cell.set(Some(position)));
}
