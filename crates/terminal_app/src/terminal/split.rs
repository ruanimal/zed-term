//! Lightweight split-pane tree for the standalone terminal app.
//!
//! Mirrors the semantics of Zed's `Member`/`PaneAxis` (workspace/pane_group.rs)
//! without its workspace dependencies: no persistence, no dock integration, and
//! flexes are the only stored layout state.

use gpui::{
    Along, AnyElement, App, Axis, Entity, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Size, Styled as _, px, relative,
};
use theme::ActiveTheme as _;

use crate::terminal::TerminalTab;

/// Hitbox width of the divider in pixels (Zed: HANDLE_HITBOX_SIZE = 4).
pub(crate) const DIVIDER_HITBOX: Pixels = px(4.);
const DIVIDER_SIZE: Pixels = px(1.);
/// Smallest allowed flex weight, keeping a leaf visible during a drag.
const MIN_FLEX: f32 = 0.05;

/// Colors a divider draws with. Copied out of the theme up front because
/// rendering children needs a mutable `App`.
#[derive(Clone, Copy)]
struct DividerColors {
    line: gpui::Hsla,
    /// Backdrop of an unfocused pane.
    pane_backdrop: gpui::Hsla,
    /// Color a terminal pane paints over that backdrop.
    pane_content: gpui::Hsla,
}

impl DividerColors {
    fn new(cx: &App) -> Self {
        let colors = cx.theme().colors();
        Self {
            line: colors.pane_group_border,
            pane_backdrop: colors.terminal_ansi_black,
            pane_content: colors.terminal_ansi_background,
        }
    }
}

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

#[derive(Clone, Debug)]
pub(crate) struct DividerPath {
    axis: Axis,
    axis_path: Vec<usize>,
    divider_index: usize,
}

impl DividerPath {
    pub(crate) fn axis(&self) -> Axis {
        self.axis
    }
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

    /// Returns whether this subtree contains `tab`.
    pub(crate) fn contains_tab(&self, tab: &Entity<TerminalTab>) -> bool {
        match self {
            Self::Leaf { tab: leaf_tab } => leaf_tab == tab,
            Self::Axis { children, .. } => children.iter().any(|child| child.contains_tab(tab)),
        }
    }

    /// Returns the number of terminal panes in this subtree.
    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Axis { children, .. } => children.iter().map(Self::leaf_count).sum(),
        }
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
    /// siblings. A divider captures its path through the tree so the
    /// window-level mouse-move handler can resize that exact axis.
    pub(crate) fn render(
        &mut self,
        axis_path: &[usize],
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
                let divider_colors = DividerColors::new(cx);
                let axis_for_id = *axis;
                let axis_for_drag = *axis;
                let container = gpui::div().size_full().flex();
                let container = match axis {
                    Axis::Horizontal => container.flex_row(),
                    Axis::Vertical => container.flex_col(),
                };
                let mut elements = Vec::with_capacity(children.len() * 2);
                let children_len = children.len();
                for (index, child) in children.iter_mut().enumerate() {
                    let mut child_path = axis_path.to_vec();
                    child_path.push(index);
                    let rendered = child.render(&child_path, _window, cx, leaf_renderer);
                    // Taffy distributes free space proportionally to
                    // flex-grow, so the weight is the whole sizing story.
                    elements.push(
                        gpui::div()
                            .flex_grow(flex_weight(flexes, index))
                            .flex_basis(relative(0.))
                            .min_w_0()
                            .min_h_0()
                            .child(rendered)
                            .into_any_element(),
                    );
                    if index + 1 < children_len {
                        let divider_path = DividerPath {
                            axis: *axis,
                            axis_path: axis_path.to_vec(),
                            divider_index: index,
                        };
                        let divider = render_divider(*axis, divider_colors)
                            .id(gpui::ElementId::NamedInteger(
                                format!("{axis_for_id:?}:{axis_path:?}").into(),
                                index as u64,
                            ))
                            .on_mouse_down(MouseButton::Left, {
                                let divider_path = divider_path.clone();
                                move |_event: &MouseDownEvent, window: &mut gpui::Window, cx| {
                                    let position = window.mouse_position();
                                    let position = if axis_for_drag == Axis::Horizontal {
                                        position.x
                                    } else {
                                        position.y
                                    };
                                    set_drag_anchor(divider_path.clone(), position);
                                    cx.stop_propagation();
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

    /// Resizes the divider identified by its path through the split tree.
    pub(crate) fn resize_divider(
        &mut self,
        divider: &DividerPath,
        root_size: Size<Pixels>,
        delta: Pixels,
    ) -> bool {
        self.resize_divider_at_path(&divider.axis_path, divider.divider_index, root_size, delta)
    }

    fn resize_divider_at_path(
        &mut self,
        axis_path: &[usize],
        divider_index: usize,
        container_size: Size<Pixels>,
        delta: Pixels,
    ) -> bool {
        match self {
            Self::Leaf { .. } => false,
            Self::Axis {
                axis,
                flexes,
                children,
            } => {
                if axis_path.is_empty() {
                    let Some(container_length) =
                        flex_container_length(container_size.along(*axis), flexes.len())
                    else {
                        return false;
                    };
                    resize_axis(*axis, flexes, container_length, divider_index, delta)
                } else {
                    let Some((&child_index, remaining_path)) = axis_path.split_first() else {
                        return false;
                    };
                    let Some(child) = children.get_mut(child_index) else {
                        return false;
                    };
                    let Some(child_size) =
                        child_layout_size(container_size, *axis, flexes, child_index)
                    else {
                        return false;
                    };
                    child.resize_divider_at_path(remaining_path, divider_index, child_size, delta)
                }
            }
        }
    }
}

const HORIZONTAL_MIN_SIZE: Pixels = px(80.);
const VERTICAL_MIN_SIZE: Pixels = px(100.);

fn child_layout_size(
    container_size: Size<Pixels>,
    axis: Axis,
    flexes: &[f32],
    child_index: usize,
) -> Option<Size<Pixels>> {
    let container_length = flex_container_length(container_size.along(axis), flexes.len())?;
    let child_flex = *flexes.get(child_index)?;
    let child_length = container_length * (child_flex / flexes.len() as f32);
    Some(container_size.apply_along(axis, |_| child_length))
}

fn flex_container_length(total_length: Pixels, child_count: usize) -> Option<Pixels> {
    if child_count == 0 {
        return None;
    }
    let divider_count = child_count.saturating_sub(1) as f32;
    Some((total_length - DIVIDER_HITBOX * divider_count).max(Pixels::ZERO))
}

fn resize_axis(
    axis: Axis,
    flexes: &mut [f32],
    container_length: Pixels,
    divider_index: usize,
    delta: Pixels,
) -> bool {
    let child_count = flexes.len();
    if child_count < 2 || divider_index >= child_count - 1 || container_length <= Pixels::ZERO {
        return false;
    }

    let min_size = match axis {
        Axis::Horizontal => HORIZONTAL_MIN_SIZE,
        Axis::Vertical => VERTICAL_MIN_SIZE,
    };
    let size = |flex: f32| container_length * (flex / child_count as f32);
    let Some(divider_flex) = flexes.get(divider_index).copied() else {
        return false;
    };
    if size(divider_flex) < min_size - px(1.) {
        return false;
    }

    let mut proposed_change = delta;
    let moving_forward = proposed_change > Pixels::ZERO;
    let mut offset = 0;
    while proposed_change.abs() > Pixels::ZERO {
        let current_index = if moving_forward {
            divider_index
                .checked_add(offset)
                .filter(|index| *index < child_count.saturating_sub(1))
        } else {
            divider_index.checked_sub(offset)
        };
        let Some(current_index) = current_index else {
            break;
        };
        let Some(next_index) = current_index.checked_add(1) else {
            break;
        };

        let Some(current_size) = flexes.get(current_index).copied().map(size) else {
            break;
        };
        let Some(next_size) = flexes.get(next_index).copied().map(size) else {
            break;
        };
        let next_target_size = Pixels::max(next_size - proposed_change, min_size);
        let current_target_size =
            Pixels::max(current_size + next_size - next_target_size, min_size);
        let current_pixel_change = current_target_size - current_size;
        if current_pixel_change == Pixels::ZERO {
            offset += 1;
            continue;
        }

        let flex_change = child_count as f32 * current_pixel_change / container_length;
        let Some((current_flex, remaining_flexes)) = flexes
            .get_mut(current_index..)
            .and_then(|values| values.split_first_mut())
        else {
            break;
        };
        let Some(next_flex) = remaining_flexes.first_mut() else {
            break;
        };
        *current_flex += flex_change;
        *next_flex -= flex_change;
        proposed_change -= current_pixel_change;
        offset += 1;
    }
    true
}

fn flex_weight(flexes: &[f32], index: usize) -> f32 {
    flexes.get(index).copied().unwrap_or(1.).max(MIN_FLEX)
}

fn render_divider(axis: Axis, colors: DividerColors) -> gpui::Div {
    // The line is centered in the hit box: with a bare `div()` (which lays out
    // as a block) `justify_center`/`items_center` are ignored, leaving the line
    // flush against one edge of the hit box and the remaining gutter showing
    // whatever is painted behind it.
    let divider = match axis {
        Axis::Horizontal => gpui::div()
            .flex_none()
            .w(DIVIDER_HITBOX)
            .h_full()
            .flex()
            .flex_row()
            .justify_center()
            .child(unfocused_pane_backdrop(colors))
            .child(gpui::div().w(DIVIDER_SIZE).h_full().bg(colors.line)),
        Axis::Vertical => gpui::div()
            .flex_none()
            .w_full()
            .h(DIVIDER_HITBOX)
            .flex()
            .flex_col()
            .justify_center()
            .child(unfocused_pane_backdrop(colors))
            .child(gpui::div().w_full().h(DIVIDER_SIZE).bg(colors.line)),
    };

    let divider = match axis {
        Axis::Horizontal => divider.cursor_col_resize(),
        Axis::Vertical => divider.cursor_row_resize(),
    };
    divider.block_mouse_except_scroll()
}

/// Fills a divider's hit box with the color an unfocused pane shows, so the
/// hit box's extra width does not expose the window background. The window
/// background is the focused pane's color, so an unpainted hit box reads as a
/// gap between the line and the unfocused pane - an artifact whose visibility
/// depends on how far a theme's dimmed pane strays from its background.
fn unfocused_pane_backdrop(colors: DividerColors) -> gpui::Div {
    gpui::div()
        .absolute()
        .inset_0()
        .bg(colors.pane_backdrop)
        .child(
            gpui::div()
                .size_full()
                .bg(colors.pane_content)
                .opacity(crate::terminal::INACTIVE_PANE_OPACITY),
        )
}

thread_local! {
    static DRAG_DIVIDER: std::cell::RefCell<Option<DividerPath>> =
        const { std::cell::RefCell::new(None) };
    static DRAG_ANCHOR: std::cell::Cell<Option<Pixels>> =
        const { std::cell::Cell::new(None) };
}

/// Latches the divider drag anchor at the given axis-relative position.
pub(crate) fn set_drag_anchor(divider: DividerPath, position: Pixels) {
    DRAG_DIVIDER.with(|cell| cell.replace(Some(divider)));
    DRAG_ANCHOR.with(|cell| cell.set(Some(position)));
}

/// Returns the currently dragged divider, if any.
pub(crate) fn drag_divider() -> Option<DividerPath> {
    DRAG_DIVIDER.with(|cell| cell.borrow().clone())
}

/// Returns and clears the latched drag anchor (used to detect drag end).
pub(crate) fn take_drag_anchor() -> Option<Pixels> {
    DRAG_DIVIDER.with(|cell| cell.replace(None));
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        Along as _, AnyWindowHandle, AppContext as _, Axis, Bounds, Context, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, Window, div, px, relative,
    };

    use super::{DIVIDER_HITBOX, DIVIDER_SIZE, DividerColors, render_divider};

    #[derive(Default)]
    struct LaidOut {
        divider: Option<Bounds<gpui::Pixels>>,
        divider_children: Vec<Bounds<gpui::Pixels>>,
    }

    struct SplitAxis {
        axis: Axis,
        laid_out: Rc<RefCell<LaidOut>>,
    }

    impl Render for SplitAxis {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let colors = DividerColors {
                line: gpui::black(),
                pane_backdrop: gpui::black(),
                pane_content: gpui::white(),
            };
            let divider_laid_out = self.laid_out.clone();
            let container_laid_out = self.laid_out.clone();
            let divider =
                render_divider(self.axis, colors).on_children_prepainted(move |bounds, _, _| {
                    divider_laid_out.borrow_mut().divider_children = bounds;
                });

            let container = div()
                .size_full()
                .flex()
                .on_children_prepainted(move |bounds, _, _| {
                    container_laid_out.borrow_mut().divider = bounds.get(1).copied();
                })
                .child(pane())
                .child(divider)
                .child(pane());
            match self.axis {
                Axis::Horizontal => container.flex_row(),
                Axis::Vertical => container.flex_col(),
            }
        }
    }

    fn pane() -> gpui::Div {
        div()
            .flex_grow(1.)
            .flex_basis(relative(0.))
            .min_w_0()
            .min_h_0()
            .child(div().size_full().bg(gpui::red()))
    }

    /// Lays out a two-pane split and returns the divider's bounds together
    /// with the bounds of its children (`[backdrop, line]`).
    fn lay_out_divider(
        axis: Axis,
        cx: &mut TestAppContext,
    ) -> (Bounds<gpui::Pixels>, Vec<Bounds<gpui::Pixels>>) {
        let laid_out = Rc::new(RefCell::new(LaidOut::default()));
        let window = cx.add_window({
            let laid_out = laid_out.clone();
            move |_, _| SplitAxis { axis, laid_out }
        });
        cx.update_window(AnyWindowHandle::from(window), |_, window, cx| {
            window.draw(cx).clear(cx)
        })
        .unwrap();

        let laid_out = laid_out.borrow();
        (
            laid_out.divider.expect("divider was laid out"),
            laid_out.divider_children.clone(),
        )
    }

    #[gpui::test]
    fn divider_line_is_centered_in_its_hit_box(cx: &mut TestAppContext) {
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let (divider, children) = lay_out_divider(axis, cx);
            let [backdrop, line] = children[..] else {
                panic!("divider should lay out a backdrop and a line");
            };

            // The hit box is wider than the line so the divider stays grabbable.
            // `justify_center` only works on a flex container, so a divider that
            // is not flexible leaves the line flush against one edge.
            let hit_box_length = divider.size.along(axis);
            let line_length = line.size.along(axis);
            assert_eq!(hit_box_length, DIVIDER_HITBOX);
            assert_eq!(line_length, DIVIDER_SIZE);
            let centered = (hit_box_length - line_length) / 2.;
            let offset = (line.origin.along(axis) - divider.origin.along(axis)).abs();
            assert!(
                (offset - centered).abs() <= px(0.5),
                "line should sit centered in the hit box on {axis:?}, but was offset by {offset:?}"
            );

            // Anything the line does not cover has to be painted by the divider
            // itself, otherwise the window background shows through between the
            // line and the unfocused pane.
            assert_eq!(backdrop.origin, divider.origin);
            assert_eq!(backdrop.size, divider.size);
        }
    }
}
