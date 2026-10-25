# FAQ

**English** | [简体中文](FAQ.zh-CN.md)

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

