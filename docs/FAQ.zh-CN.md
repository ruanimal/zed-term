# 常见问题

[English](FAQ.md) | **简体中文**

## 哪些命令可以在标签页和任务栏显示进度？

ZedTerm 接收 `OSC 9;4` 进度协议，需要程序主动发送；不会识别 `apt update`
等命令输出的普通百分比文本。新终端会话会设置 `TERM_FEATURES=P` 声明进度支持，
Cargo 1.95 及更新版本等程序可以据此自动启用终端进度集成。可以在 POSIX shell 中测试：

```sh
printf '\033]9;4;1;50\007'  # 50% 进度
printf '\033]9;4;3\007'     # 未知进度，标签页显示动画
printf '\033]9;4;2;50\007'  # 50% 时发生错误
printf '\033]9;4;4;50\007'  # 暂停在 50%
printf '\033]9;4;0\007'     # 清除
```

支持 BEL 和 `ESC \\` 两种结束符。每个终端分屏独立保存进度；标签页显示其中最近更新的进度。
macOS Dock、KDE Plasma 任务栏和兼容的 GNOME Dock 显示当前进程所有窗口中最近更新的进度，
该任务清除或关闭后，会回退到另一个仍有进度的任务。

标签页支持错误色和暂停色。Dock 使用原生进度条，并支持未知进度动画。
Linux 通过 Unity Launcher D-Bus 接口发送数值进度，兼容 KDE Plasma、GNOME 的
[Ubuntu Dock](https://github.com/micheleg/dash-to-dock/tree/ubuntu-dock) 和
[Dash to Dock](https://github.com/micheleg/dash-to-dock)。未知进度时不显示系统图标进度条；
需要安装 `dev.zed.ZedTerm.desktop` 启动项才能正确关联应用。
进度有效期间会定期重新发送，因此启用或重启 Dock 后也能恢复显示，无需等待命令的百分比变化。

原生 GNOME Shell 的 Dash 没有提供这一进度接口，需要启用 Ubuntu Dock 或 Dash to Dock。
扩展的 `show-icons-emblems` 选项须保持开启（默认开启），勿扰模式也可能抑制图标标记。
没有兼容 Dock 时，仍可查看 ZedTerm 标签页进度。其他 Linux 桌面需要支持同一接口的任务栏或扩展。

达到 100% 不会自动清除，程序应主动发送状态 `0`，包括中断退出时。
终端复位（`ESC c`）、shell 退出和关闭分屏也会清除进度。
没有无活动超时，避免更新较慢的任务提前消失；如果命令异常退出后进度残留，
可以执行上面的清除指令。终端复用器需要支持或透传这些控制序列。

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

