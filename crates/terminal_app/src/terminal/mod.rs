//! Terminal rendering for the standalone terminal app.

mod cursor;
#[path = "terminal_element.rs"]
mod element;
#[path = "terminal_scrollbar.rs"]
mod scrollbar;
mod search_bar;
pub(crate) mod tab;

pub use cursor::{CursorLayout, HighlightedRange, HighlightedRangeLine, TerminalCursorShape};
pub use element::TerminalElement;
pub use scrollbar::TerminalScrollHandle;
pub use search_bar::TerminalSearchBar;
pub use tab::TerminalTab;