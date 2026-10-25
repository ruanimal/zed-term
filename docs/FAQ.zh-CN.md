# 常见问题

[English](FAQ.md) | **简体中文**

## 为什么在 ZedTerm 中执行 `cargo run` 会立即退出？

ZedTerm 对每个数据目录只允许一个进程，安装版和开发构建也遵循同一限制。执行：

```bash
cargo run --locked -p terminal_app --bin terminal-app
```

新进程会尝试获取默认数据目录中的 `open.lock`。如果已有 ZedTerm 进程持有该锁，
新进程就会读取 `open.socket`，通过本地 Unix socket 将打开请求发送给已有进程，
收到确认后以退出码 `0` 正常退出。上面的命令没有传入目录参数，已有进程会
因此打开一个新窗口。这个窗口属于已有进程，运行的仍是它已经加载的旧代码，
因此无法验证新编译的改动。

在 ZedTerm 内执行时，通常已经有一个实例正在运行。Konsole 中看起来正常，
可能是因为当时没有 ZedTerm 进程占用相同数据目录；如果有，从 Konsole 启动也会
转发请求并退出。这个行为取决于数据目录，与执行命令所用的终端无关。

使用独立数据目录，可以让开发构建与日常使用的 ZedTerm 同时运行：

```bash
cargo run --locked -p terminal_app --bin terminal-app -- --user-data-dir /tmp/zedterm-dev
```

其中 `--` 将后续参数传给 `terminal-app`，而不是 Cargo。`--user-data-dir` 还会
将配置目录切换到 `/tmp/zedterm-dev/config`，因此不会自动加载日常使用的设置、
快捷键和已安装扩展。系统可能清理 `/tmp` 下的数据；如果需要保留开发配置，
请改用持久目录。

修改代码后再次运行之前，请关闭使用同一目录的开发实例，否则新进程仍会转发给
那个实例。如果需要同时运行多个独立实例，请分别指定不同的数据目录。

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

