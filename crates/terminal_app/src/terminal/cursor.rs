//! Cursor and highlighted-range painting helpers.
//!
//! These were copied from Zed's editor (`crates/editor/src/element.rs`) and
//! trimmed to what the terminal element needs, so `terminal_app` has no
//! dependency on the editor crate.

use std::cmp::Ordering;

use gpui::{
    App, BorderStyle, Bounds, Hsla, PathBuilder, Pixels, Point, ShapedLine, TextAlign, Window,
    fill, outline, point, px, size,
};
use util::ResultExt;

/// The shape of a selection cursor (was `language::CursorShape` in Zed).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TerminalCursorShape {
    /// A vertical bar
    #[default]
    Bar,
    /// A block that surrounds the following character
    Block,
    /// An underline that runs along the following character
    Underline,
    /// A box drawn around the following character
    Hollow,
}

/// The layout of a cursor; used for both the terminal cursor and the IME
/// composition cursor (was `editor::CursorLayout` in Zed).
pub struct CursorLayout {
    origin: Point<Pixels>,
    block_width: Pixels,
    line_height: Pixels,
    color: Hsla,
    shape: TerminalCursorShape,
    block_text: Option<ShapedLine>,
}

impl CursorLayout {
    pub fn new(
        origin: Point<Pixels>,
        block_width: Pixels,
        line_height: Pixels,
        color: Hsla,
        shape: TerminalCursorShape,
        block_text: Option<ShapedLine>,
    ) -> CursorLayout {
        CursorLayout {
            origin,
            block_width,
            line_height,
            color,
            shape,
            block_text,
        }
    }

    pub fn bounding_rect(&self, origin: Point<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: self.origin + origin,
            size: size(self.block_width, self.line_height),
        }
    }

    fn bounds(&self, origin: Point<Pixels>) -> Bounds<Pixels> {
        match self.shape {
            TerminalCursorShape::Bar => Bounds {
                origin: self.origin + origin,
                size: size(px(2.0), self.line_height),
            },
            TerminalCursorShape::Block | TerminalCursorShape::Hollow => Bounds {
                origin: self.origin + origin,
                size: size(self.block_width, self.line_height),
            },
            TerminalCursorShape::Underline => Bounds {
                origin: self.origin + origin + Point::new(Pixels::ZERO, self.line_height - px(2.0)),
                size: size(self.block_width, px(2.0)),
            },
        }
    }

    pub fn paint(&mut self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let bounds = window.pixel_snap_bounds(self.bounds(origin));

        // Draw background or border quad
        let cursor = if matches!(self.shape, TerminalCursorShape::Hollow) {
            outline(bounds, self.color, BorderStyle::Solid)
        } else {
            fill(bounds, self.color)
        };

        window.paint_quad(cursor);

        if let Some(block_text) = &self.block_text {
            block_text
                .paint(
                    self.origin + origin,
                    self.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .log_err();
        }
    }

    pub fn shape(&self) -> TerminalCursorShape {
        self.shape
    }
}

#[derive(Debug)]
pub struct HighlightedRange {
    pub start_y: Pixels,
    pub line_height: Pixels,
    pub lines: Vec<HighlightedRangeLine>,
    pub color: Hsla,
    pub corner_radius: Pixels,
}

#[derive(Debug)]
pub struct HighlightedRangeLine {
    pub start_x: Pixels,
    pub end_x: Pixels,
}

impl HighlightedRange {
    pub fn paint(&self, fill: bool, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.lines.len() >= 2 && self.lines[0].start_x > self.lines[1].end_x {
            self.paint_lines(self.start_y, &self.lines[0..1], fill, bounds, window);
            self.paint_lines(
                self.start_y + self.line_height,
                &self.lines[1..],
                fill,
                bounds,
                window,
            );
        } else {
            self.paint_lines(self.start_y, &self.lines, fill, bounds, window);
        }
    }

    fn paint_lines(
        &self,
        start_y: Pixels,
        lines: &[HighlightedRangeLine],
        fill: bool,
        _bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        if lines.is_empty() {
            return;
        }

        let first_line = lines.first().unwrap();
        let last_line = lines.last().unwrap();

        let first_top_left = point(first_line.start_x, start_y);
        let first_top_right = point(first_line.end_x, start_y);

        let curve_height = Point::new(Pixels::ZERO, self.corner_radius);
        let curve_width = |start_x: Pixels, end_x: Pixels| {
            let max = (end_x - start_x) / 2.;
            let width = if max < self.corner_radius {
                max
            } else {
                self.corner_radius
            };

            Point::new(width, Pixels::ZERO)
        };

        let top_curve_width = curve_width(first_line.start_x, first_line.end_x);
        let mut builder = if fill {
            PathBuilder::fill()
        } else {
            PathBuilder::stroke(px(1.))
        };
        builder.move_to(first_top_right - top_curve_width);
        builder.curve_to(first_top_right + curve_height, first_top_right);

        let mut iter = lines.iter().enumerate().peekable();
        while let Some((ix, line)) = iter.next() {
            let bottom_right = point(line.end_x, start_y + (ix + 1) as f32 * self.line_height);

            if let Some((_, next_line)) = iter.peek() {
                let next_top_right = point(next_line.end_x, bottom_right.y);

                match next_top_right.x.partial_cmp(&bottom_right.x).unwrap() {
                    Ordering::Equal => {
                        builder.line_to(bottom_right);
                    }
                    Ordering::Less => {
                        let curve_width = curve_width(next_top_right.x, bottom_right.x);
                        builder.line_to(bottom_right - curve_height);
                        if self.corner_radius > Pixels::ZERO {
                            builder.curve_to(bottom_right - curve_width, bottom_right);
                        }
                        builder.line_to(next_top_right + curve_width);
                        if self.corner_radius > Pixels::ZERO {
                            builder.curve_to(next_top_right + curve_height, next_top_right);
                        }
                    }
                    Ordering::Greater => {
                        let curve_width = curve_width(bottom_right.x, next_top_right.x);
                        builder.line_to(bottom_right - curve_height);
                        if self.corner_radius > Pixels::ZERO {
                            builder.curve_to(bottom_right + curve_width, bottom_right);
                        }
                        builder.line_to(next_top_right - curve_width);
                        if self.corner_radius > Pixels::ZERO {
                            builder.curve_to(next_top_right + curve_height, next_top_right);
                        }
                    }
                }
            } else {
                let curve_width = curve_width(line.start_x, line.end_x);
                builder.line_to(bottom_right - curve_height);
                if self.corner_radius > Pixels::ZERO {
                    builder.curve_to(bottom_right - curve_width, bottom_right);
                }

                let bottom_left = point(line.start_x, bottom_right.y);
                builder.line_to(bottom_left + curve_width);
                if self.corner_radius > Pixels::ZERO {
                    builder.curve_to(bottom_left - curve_height, bottom_left);
                }
            }
        }

        if first_line.start_x > last_line.start_x {
            let curve_width = curve_width(last_line.start_x, first_line.start_x);
            let second_top_left = point(last_line.start_x, start_y + self.line_height);
            builder.line_to(second_top_left + curve_height);
            if self.corner_radius > Pixels::ZERO {
                builder.curve_to(second_top_left + curve_width, second_top_left);
            }
            let first_bottom_left = point(first_line.start_x, second_top_left.y);
            builder.line_to(first_bottom_left - curve_width);
            if self.corner_radius > Pixels::ZERO {
                builder.curve_to(first_bottom_left - curve_height, first_bottom_left);
            }
        }

        builder.line_to(first_top_left + curve_height);
        if self.corner_radius > Pixels::ZERO {
            builder.curve_to(first_top_left + top_curve_width, first_top_left);
        }
        builder.line_to(first_top_right - top_curve_width);

        if let Ok(path) = builder.build() {
            window.paint_path(path, self.color);
        }
    }
}
