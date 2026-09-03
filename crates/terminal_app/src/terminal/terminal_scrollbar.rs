use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui::{Bounds, Point, point, size};
use terminal_core::Terminal;
use ui::{Pixels, ScrollableHandle, px};

#[derive(Debug)]
struct ScrollHandleState {
    line_height: Pixels,
    total_lines: usize,
    viewport_lines: usize,
    display_offset: usize,
}

impl ScrollHandleState {
    fn new(terminal: &Terminal) -> Self {
        Self {
            line_height: terminal.last_content().terminal_bounds.line_height,
            total_lines: terminal.total_lines(),
            viewport_lines: terminal.viewport_lines(),
            display_offset: terminal.last_content().display_offset,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalScrollHandle {
    state: Rc<RefCell<ScrollHandleState>>,
    pub future_display_offset: Rc<Cell<Option<usize>>>,
}

impl TerminalScrollHandle {
    pub fn new(terminal: &Terminal) -> Self {
        Self {
            state: Rc::new(RefCell::new(ScrollHandleState::new(terminal))),
            future_display_offset: Rc::new(Cell::new(None)),
        }
    }

    pub fn update(&self, terminal: &Terminal) {
        *self.state.borrow_mut() = ScrollHandleState::new(terminal);
    }
}

impl ScrollableHandle for TerminalScrollHandle {
    fn max_offset(&self) -> Point<Pixels> {
        let state = self.state.borrow();
        point(
            Pixels::ZERO,
            state.total_lines.saturating_sub(state.viewport_lines) as f32 * state.line_height,
        )
    }

    fn offset(&self) -> Point<Pixels> {
        let state = self.state.borrow();
        let scroll_offset = state
            .total_lines
            .saturating_sub(state.viewport_lines)
            .saturating_sub(state.display_offset);
        Point::new(Pixels::ZERO, -(scroll_offset as f32 * state.line_height))
    }

    fn set_offset(&self, point: Point<Pixels>) {
        let state = self.state.borrow();
        let offset_delta = (point.y / state.line_height).round() as i32;

        let max_offset = state.total_lines.saturating_sub(state.viewport_lines);
        let display_offset = (max_offset as i32 + offset_delta).clamp(0, max_offset as i32);

        self.future_display_offset
            .set(Some(display_offset as usize));
    }

    fn viewport(&self) -> Bounds<Pixels> {
        let state = self.state.borrow();
        Bounds::new(
            Point::new(px(0.), px(0.)),
            size(
                Pixels::ZERO,
                state.viewport_lines as f32 * state.line_height,
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scroll_handle(
        line_height: f32,
        total_lines: usize,
        viewport_lines: usize,
        display_offset: usize,
    ) -> TerminalScrollHandle {
        TerminalScrollHandle {
            state: Rc::new(RefCell::new(ScrollHandleState {
                line_height: px(line_height),
                total_lines,
                viewport_lines,
                display_offset,
            })),
            future_display_offset: Rc::new(Cell::new(None)),
        }
    }

    #[test]
    fn content_smaller_than_viewport_has_zero_offsets() {
        let handle = scroll_handle(8.0, 4, 10, 3);

        assert_eq!(handle.max_offset(), point(px(0.0), px(0.0)));
        assert_eq!(handle.offset(), point(px(0.0), px(0.0)));
        assert_eq!(handle.viewport().size.height, px(80.0));

        handle.set_offset(point(px(0.0), px(-10_000.0)));
        assert_eq!(handle.future_display_offset.get(), Some(0));
        handle.set_offset(point(px(0.0), px(10_000.0)));
        assert_eq!(handle.future_display_offset.get(), Some(0));
    }

    #[test]
    fn top_and_bottom_offsets_map_to_display_offset_bounds() {
        let top = scroll_handle(10.0, 100, 20, 80);
        let bottom = scroll_handle(10.0, 100, 20, 0);

        assert_eq!(top.max_offset(), point(px(0.0), px(800.0)));
        assert_eq!(top.offset(), point(px(0.0), px(0.0)));
        assert_eq!(bottom.offset(), point(px(0.0), px(-800.0)));

        bottom.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(bottom.future_display_offset.get(), Some(80));
        bottom.set_offset(point(px(0.0), px(-800.0)));
        assert_eq!(bottom.future_display_offset.get(), Some(0));
    }

    #[test]
    fn fractional_offsets_round_and_clamp_to_valid_display_offsets() {
        let handle = scroll_handle(10.0, 100, 20, 0);

        handle.set_offset(point(px(0.0), px(-14.9)));
        assert_eq!(handle.future_display_offset.get(), Some(79));
        handle.set_offset(point(px(0.0), px(-15.0)));
        assert_eq!(handle.future_display_offset.get(), Some(78));
        handle.set_offset(point(px(0.0), px(-10_000.0)));
        assert_eq!(handle.future_display_offset.get(), Some(0));
        handle.set_offset(point(px(0.0), px(10_000.0)));
        assert_eq!(handle.future_display_offset.get(), Some(80));
    }

    #[test]
    fn pending_offset_is_shared_replaced_and_consumed_once() {
        let handle = scroll_handle(10.0, 100, 20, 0);
        let cloned_handle = handle.clone();

        assert_eq!(handle.future_display_offset.get(), None);
        handle.set_offset(point(px(0.0), px(-200.0)));
        assert_eq!(cloned_handle.future_display_offset.get(), Some(60));

        cloned_handle.set_offset(point(px(0.0), px(-300.0)));
        assert_eq!(handle.future_display_offset.take(), Some(50));
        assert_eq!(cloned_handle.future_display_offset.get(), None);
    }
}
