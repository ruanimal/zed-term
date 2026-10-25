# FAQ

**English** | [简体中文](FAQ.zh-CN.md)

## Why does `cargo run` exit immediately when launched inside ZedTerm?

ZedTerm enforces a single process per data directory, including across installed and
development builds. When you run:

```bash
cargo run --locked -p terminal_app --bin terminal-app
```

the new process tries to acquire `open.lock` in the default data directory. If a
ZedTerm process already owns that lock, the new process reads `open.socket`, sends
the open request to the existing process over a local Unix socket, and exits with
status `0` after receiving an acknowledgment. For the command above, which passes
no directory argument, the existing process opens a new window. That window belongs
to the existing process and runs its already loaded code, so it does not test your
newly compiled changes.

Launching from inside ZedTerm normally means that an instance is already running.
Konsole can appear to work because no ZedTerm process owns the same data directory
at that time. If one does, launching from Konsole also forwards the request and
exits. This behavior depends on the data directory, not the terminal used to run
the command.

Use a separate data directory to run the development binary alongside your usual
ZedTerm instance:

```bash
cargo run --locked -p terminal_app --bin terminal-app -- --user-data-dir /tmp/zedterm-dev
```

The `--` passes the following arguments to `terminal-app` rather than Cargo.
`--user-data-dir` also selects a separate configuration directory at
`/tmp/zedterm-dev/config`, so your usual settings, keymap, and installed extensions
are not automatically loaded. Data under `/tmp` may be cleaned by the system;
choose a persistent directory if you want to keep the development configuration.

Before rerunning after a code change, close the development instance that uses
the same directory. Otherwise, the new process forwards to that instance too.
Use different data directories if you need several independent instances at once.

## How do I disable underlined paths in zsh?

Paths such as `tabby-push-toolkit` can be underlined by a zsh syntax-highlighting plugin.
ZedTerm preserves the styles emitted by the shell; these underlines do not mean that the
text qualifies for the context menu's Open Link / Copy Link actions.

For [zsh-syntax-highlighting](https://github.com/zsh-users/zsh-syntax-highlighting/blob/master/docs/highlighters/main.md),
add the following to `~/.zshrc` **after the plugin is loaded** (after
`source "$ZSH/oh-my-zsh.sh"` when using Oh My Zsh):

```zsh
typeset -A ZSH_HIGHLIGHT_STYLES
ZSH_HIGHLIGHT_STYLES[path]='none'
ZSH_HIGHLIGHT_STYLES[path_prefix]='none'
ZSH_HIGHLIGHT_STYLES[path_pathseparator]='none'
ZSH_HIGHLIGHT_STYLES[path_prefix_pathseparator]='none'
ZSH_HIGHLIGHT_STYLES[autodirectory]='none'
```

This also covers incomplete paths, path separators, and directory names used as commands
with `AUTO_CD` enabled. Other syntax highlighting remains enabled.

If you use [fast-syntax-highlighting](https://github.com/zdharma-continuum/fast-syntax-highlighting/blob/master/fast-highlight),
use its style array instead, after loading the plugin and selecting its theme:

```zsh
typeset -A FAST_HIGHLIGHT_STYLES
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path]='none'
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path-to-dir]='none'
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path_pathseparator]='none'
```

Open a new terminal tab or run `source ~/.zshrc` to apply the configuration.
Previously printed text keeps its existing styles.

