# ZedTerm

ZedTerm is a standalone terminal application built on Zed's terminal capabilities. It reuses GPUI, UI, theme, and settings infrastructure without depending on Zed's editor features.

macOS is the primary platform. Linux builds and deb packaging are also supported.

## Features

- PTY and shell support with Alacritty terminal parsing
- Scrolling, search, selection, clipboard, and bell notifications
- Multiple windows, tabs, and split panes
- Pane zoom, focus switching, and tab reordering
- URL, OSC 8, and path link support
- Terminal settings, keymap editing, and theme extension management

## Project Structure

- `crates/terminal_core` — Terminal state, PTY handling, parsing, scrolling, and search
- `crates/terminal_app` — Application startup, windows, tabs, panes, settings, and rendering

See [`docs/TODO.md`](docs/TODO.md) for current work, planned features, and unsupported items. [`docs/PLAN.md`](docs/PLAN.md) and [`docs/GAPS.md`](docs/GAPS.md) contain archived project context.

## Development

Run commands from the repository root:

```bash
# Check the application
cargo check -p terminal_app

# Run the application
cargo run -p terminal_app --bin terminal-app

# Run tests
cargo test -p terminal_app --lib
cargo test -p terminal_core --lib

# Check formatting and lints
cargo fmt --all -- --check
cargo clippy -p terminal_app --all-targets -- --deny warnings
```

For code changes, also run `git diff --check`. Changes affecting interaction behavior should be smoke-tested on macOS.
