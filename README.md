# ZedTerm

ZedTerm is a standalone terminal application forked from [Zed](https://github.com/zed-industries/zed). It reuses Zed's GPUI, UI, theme, and settings infrastructure without depending on its editor features.

The fork has since diverged and does not track upstream, so a fix here is free to leave Zed's behavior behind. Formats users share with a Zed installation — `keymap.json`, the themes and extensions layout, the data directory — stay compatible on purpose. See [`.rules`](.rules) for what this means when changing code.

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
- Everything else under `crates/` (55 workspace members: `gpui`, `ui`, `theme`, `settings`, `fs`,
  `paths`, ...) — infrastructure inherited from Zed that every screen goes through. Prefer the two
  crates above when behavior is specific to this app, and change a shared crate only when the change
  belongs there.

See [`docs/TODO.md`](docs/TODO.md) for current work, planned features, and unsupported items. [`docs/PLAN.md`](docs/PLAN.md) and [`docs/GAPS.md`](docs/GAPS.md) contain archived project context.

## Development

Run commands from the repository root:

```bash
# Check the application
cargo check -p terminal_app

# Run the application
cargo run -p terminal_app --bin terminal-app

# Run the tests of every crate you touched
cargo test -p terminal_app --lib
cargo test -p terminal_core --lib
cargo test -p settings --lib

# Check formatting and lints
cargo fmt --all -- --check
cargo clippy -p terminal_app --all-targets -- --deny warnings
```

For code changes, also run `git diff --check`. Changes affecting interaction behavior should be smoke-tested on macOS.

## License

ZedTerm inherits its licenses from Zed: GPL-3.0 ([`LICENSE-GPL`](LICENSE-GPL)) and Apache-2.0 ([`LICENSE-APACHE`](LICENSE-APACHE), copyright Zed Industries, Inc.).
