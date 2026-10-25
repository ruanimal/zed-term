# 常见问题

[English](FAQ.md) | **简体中文**

## 如何关闭 zsh 中路径的下划线？

`tabby-push-toolkit` 这样的路径可能由 zsh 的语法高亮插件加上下划线。
ZedTerm 会保留 shell 输出的样式；有下划线不代表这段文本符合右键菜单 Open Link / Copy Link 的识别条件。

如果使用 [zsh-syntax-highlighting](https://github.com/zsh-users/zsh-syntax-highlighting/blob/master/docs/highlighters/main.md)，
在 `~/.zshrc` 中将以下配置放在**插件加载之后**（使用 Oh My Zsh 时，放在
`source "$ZSH/oh-my-zsh.sh"` 之后）：

```zsh
typeset -A ZSH_HIGHLIGHT_STYLES
ZSH_HIGHLIGHT_STYLES[path]='none'
ZSH_HIGHLIGHT_STYLES[path_prefix]='none'
ZSH_HIGHLIGHT_STYLES[path_pathseparator]='none'
ZSH_HIGHLIGHT_STYLES[path_prefix_pathseparator]='none'
ZSH_HIGHLIGHT_STYLES[autodirectory]='none'
```

这也会关闭未输入完整的路径、路径分隔符，以及启用 `AUTO_CD` 时作为命令使用的目录名的高亮，其他语法高亮保持启用。

如果使用 [fast-syntax-highlighting](https://github.com/zdharma-continuum/fast-syntax-highlighting/blob/master/fast-highlight)，
则在插件加载和主题选择之后，使用它对应的样式配置：

```zsh
typeset -A FAST_HIGHLIGHT_STYLES
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path]='none'
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path-to-dir]='none'
FAST_HIGHLIGHT_STYLES[${FAST_THEME_NAME}path_pathseparator]='none'
```

新开一个终端标签页，或运行 `source ~/.zshrc` 使配置生效。已经输出的文本仍保留原来的样式。

