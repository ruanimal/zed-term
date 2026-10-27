# Changelog

Notable user-facing changes for each release are recorded here. Entries are
ordered from newest to oldest; internal maintenance and documentation-only
changes are omitted unless they affect how users install or use ZedTerm.

## [Unreleased]

## [0.8.1]

- Advertised OSC 9;4 progress support so Cargo 1.95 and later can enable terminal
  progress integration automatically.
- Added the application logo to the About settings page.

## [0.8.0]

- Added Linux arm64 release packages.
- Refined the application icon.

## [0.7.1]

- Added OSC 9;4 terminal progress indicators to tabs and desktop docks.
- Added Chinese documentation and guidance for file transfers, development
  instances, and zsh path highlighting.

## [0.7.0]

- Fixed window control spacing and borders on Linux and macOS, including in
  fullscreen mode.

## [0.6.0]

- Added moving tabs between terminal windows, including drag-and-drop tear-off.
- Added tab titles to window move destinations.
- Added configurable window behavior when opening folders from a file manager.
- Restricted context-menu file links to absolute paths.

## [0.5.4]

- Added a configurable timeout for file-transfer notifications.

## [0.5.3]

- Added terminal tab renaming and full-title tooltips.

## [0.5.2]

- Added file-transfer settings to the terminal interface.

## [0.5.1]

- Improved file-transfer progress display.
- Hid terminal link actions for plain text that is not a link.

## [0.5.0]

- Added `trzsz` file transfer support, including protocol negotiation and
  throttled progress updates.

## [0.4.0]

- Added ZMODEM (`rz`/`sz`) file transfer support.
- Fixed terminal output and upload results after a transfer completes.

## [0.3.0]

- Added `trzsz` file transfer support through a PTY tap.

## [0.2.4]

- Added an option to open new terminals in the previous tab's working directory.

## [0.2.3]

- Reduced the size of Linux release packages.

## [0.2.2]

- Added keymap binding controls to the settings tab.

## [0.2.1]

- Completed terminal search with editing, navigation, and multi-pane support.

## [0.2.0]

- Added a macOS quarantine removal script for downloaded builds.

## [0.1.9]

- Added an About tab to settings.

## [0.1.8]

- Fixed split panes inheriting the wrong working directory.

## [0.1.7]

- Reset stale mouse modes after SSH sessions exit.

## [0.1.6]

- Added settings tabs for keymaps and theme downloads.
- Added a visible indicator for overflowing terminal tabs.

## [0.1.5]

- Added terminal link hover feedback and path-aware opening.
- Improved settings text editing and terminal redraw behavior.

## [0.1.4]

- Fixed the launch directory when opening ZedTerm from Dolphin.
- Improved the application icon.

## [0.1.3]

- Improved terminal tab control alignment and borders.

## [0.1.2]

- Improved terminal resizing responsiveness and pane focus contrast.

## [0.1.1]

- Added Dock actions for opening and reopening windows.
- Set the default window size to 960 × 600.
