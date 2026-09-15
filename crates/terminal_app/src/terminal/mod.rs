//! Terminal rendering for the standalone terminal app.

mod cursor;
#[path = "terminal_element.rs"]
mod element;
#[path = "terminal_scrollbar.rs"]
mod scrollbar;
mod search_bar;
pub(crate) mod split;
pub(crate) mod tab;

/// Opacity applied to the content of a pane that does not hold focus.
///
/// The split divider paints the same backdrop layers inside its hit box, so
/// this value is what keeps the divider gutter and a dimmed pane the same
/// color (see `split::render_divider`).
pub(crate) const INACTIVE_PANE_OPACITY: f32 = 0.72;

pub use cursor::{CursorLayout, HighlightedRange, HighlightedRangeLine, TerminalCursorShape};
pub use element::TerminalElement;
pub use scrollbar::TerminalScrollHandle;
pub use search_bar::TerminalSearchBar;
pub use tab::TerminalTab;
