# ZedTerm TODO

> 本文档独立记录后续需求与想法，不代表实现承诺或优先级。

## 设置页扩展

### 待实现

- 增加搜索/过滤。

### 已完成

| 支持项 | Control | Persistence | Runtime | Tests |
|---|---|---|---|---|
| `cursor blinking` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 2、7、8 |
| `font_family` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 2、7 |
| `font_weight` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 1、2、7 |
| `line_height` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 1、2、7 |
| `minimum_contrast` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 1、2、7 |
| `keep_selection_on_copy` | 已验证 | 已验证 | 已验证：既有 pane 后续交互生效 | 已验证：Properties 2、7 |
| `open_links_in_mouse_mode` | 已验证 | 已验证 | 已验证：既有 pane 后续交互生效 | 已验证：Properties 2、7 |
| 结构化 `shell` | 已验证 | 已验证 | 已验证：后续新建 pane 生效 | 已验证：Properties 2、3、4、10 |
| `env` 键值编辑 | 已验证 | 已验证 | 已验证：后续新建 pane 生效 | 已验证：Properties 2、5、10 |
| standalone `working_directory` | 已验证 | 已验证 | 已验证：后续新建 pane 生效 | 已验证：Properties 2、6、10 |
| `scrollbar.show` | 已验证 | 已验证 | 已验证：既有 pane 实时生效 | 已验证：Properties 2、9 |
| standalone 默认值（`line_height = "standard"`、`bell = "off"`、`alternate_scroll = "on"`） | 已验证 | 已验证：无覆盖时采用实际默认配置 | 已验证：运行时投影采用 `assets/settings/default.json` | 已验证：默认资源专项测试、Property 2 |
| Reset Terminal Defaults | 已验证 | 已验证：删除 terminal 覆盖并保留其余 JSONC 内容 | 已验证：既有 pane 恢复 live 默认值，后续 pane 采用 construction 默认值 | 已验证：Properties 7、10、11、12、13 |
| 写入状态及非法值校验 | 已验证：保存中、成功、失败及校验原因可见 | 已验证：差异写入、失败原子性及非法值写入前拒绝 | 已验证：revision-aware 保存/重置状态 | 已验证：Properties 1、4、5、6、12、13 |

验证基线：Properties 1–13 全部通过，每项 128 cases；`cargo test -p terminal_app --lib` 61 passed / 0 failed；目标 `terminal_core` alternate-scroll 测试 1 passed / 0 failed；`cargo clippy` 与 `cargo fmt --all -- --check` 通过。

### 低优先级

- 增加 `font_fallbacks` 设置，允许配置缺失字符的备用字体及其优先顺序。
- 增加 `font_features` 设置，允许配置 ligatures、字符变体等 OpenType 特性。

### 明确不支持

- 不支持 `default_width` 和 `default_height` 默认窗口尺寸设置。
- 不支持 `path_hyperlink_regexes` 自定义路径 hyperlink 正则。

## 设置页多 Tab 化

目标：设置窗口改为多 tab，现有设置内容不变（整体成为其中一个 tab），另增快捷键设置 tab（keymap）与主题下载 tab（复用 Zed 插件的主题下载）。

- Tab「终端设置」：现有 `SettingsPage` 内容原样迁移，仅外层加 tab 容器；draft / 保存 / revision / 校验语义不变；「设置页扩展」下的待实现（搜索/过滤）与低优先级项归属此 tab。
- Tab「快捷键」：现状按键全在 `app.rs` 硬编码 `bind_keys`，无 `keymap.json` 加载与编辑入口；需补 `paths::keymap_file()` 读写、按键列表/搜索/改键、冲突提示、重置默认。
- Tab「主题下载」：现状仅内嵌主题 + 本地 `themes_dir()`；需复用 Zed 扩展 registry 的主题扩展下载 / 安装 / 更新 / 卸载链路，注意 extension 系统依赖重，需先评估裁剪再接入；本地已安装主题仍走现有 `ThemeRegistry` + 设置页主题下拉。

建议验收：三 tab 切换不丢各 tab 未保存状态；快捷键改键后新窗口生效、重置可恢复；主题下载安装后出现在主题下拉并可即时切换，卸载后回落默认主题。

## Pane 方向性跳转

当前已有按叶子视觉序循环的 ActivateNextPane/ActivatePreviousPane。可增加 ActivatePaneInDirection，根据 pane 几何位置选择上、下、左、右方向的最近邻。

建议验收：嵌套横向/纵向 split 中，四向跳转均选择视觉上最近且方向正确的 pane；边缘无候选时保持当前焦点。

## Tab 溢出可见指示

当前 tab 栏支持横向滚动，激活 tab 时也会自动滚回可视区。可增加类似 Zed 的边缘提示，例如用 2px 边框表示一侧仍有被裁剪的 tab。

建议验收：提示随滚动位置实时出现或消失，不遮挡 tab 内容，也不影响点击、拖拽排序和窗口拖动。

## 跨窗口 Tab 拖拽

当前 tab 拖拽排序仅限同一窗口。可支持把完整 `WindowTab` 在 terminal-app 窗口之间移动，同时保留 terminal entity、split 树、zoom、bell 与焦点状态。

建议验收：源窗口和目标窗口状态一致；移动最后一个 tab 时窗口关闭语义正确；拖拽期间 pane 退出或 tab 关闭时安全取消。

## 链接可打开支持

**已完成（2026-09-10）**：`terminal_core` 既有的链接发现与 Cmd-click 链路（OSC8 / URL 正则 / path 正则三路 `find_from_grid_point`，`mouse_down` / `mouse_up` 手势仲裁，`Event::Open` 经 `TerminalTab` 调 `cx.open_url`）语义不变，本阶段补齐 app 层的 hover 反馈与 `PathLike` 落点。

- hover 高亮：`terminal_element` 两处 `layout_grid` 由 `hyperlink = None` 改为传入 `last_hovered_word.word_match` 与 `link_text_hover` 下划线样式；样式仅在 hover 命中时构造，逐 cell 只多一次 range 判断，OSC8 与正则命中的 URL / path 现在都有悬停高亮。
- 光标反馈：paint 由固定 `IBeam` 改为按 `hovered_link && window.modifiers().secondary()` 在 `PointingHand` 与 `IBeam` 间切换，松开修饰键即恢复；mouse_mode 下 `mouse_move` 本就不做链接搜索（Shift 逃逸除外），与点击语义一致。
- hover 状态驱动 UI：`TerminalTab` 订阅 `Event::NewNavigationTarget` 并请求重绘 —— 清除 hover 时 `terminal_core` 只 emit 事件而不 notify，手型光标要靠这次重绘恢复；高亮与光标取自同一份 `last_hovered_word`，不会出现"已无链接仍显示手型"。
- 右键菜单：右键按下时按命中位置解析链接（新增 `Terminal::navigation_target_at`，含 bounds 检查与 scroll 偏移换算），命中才追加 "Open Link" / "Copy Link"，作用于命中位置而非焦点 pane；`Copy Link` 复制终端内原始文本。
- `PathLike` 落点：`resolve_path_like_target` 用 `PathWithPosition::parse_str` 剥离 `:line[:column]` 与 `(line,column)` 后缀，相对路径按命中行 `working_directory` 解析，路径不存在时不打开（仅记 warn 日志）；打开改用 `Url::from_file_path`，不再做 `file://{maybe_path}` 字符串拼接。
- `path_hyperlink_regexes` 自定义仍按"明确不支持"处理（仅 `default.json` 默认值生效），本需求未改变该决策；hover tooltip 仍未实现（G8 已关闭）。

验收：按住 Cmd（Linux / Windows 上为 Ctrl）悬停 URL / path / OSC8 链接时出现下划线高亮加手型光标，松开即恢复；Cmd-click（mouse_mode 下按 `open_links_in_mouse_mode` 语义，关闭时需 Shift 逃逸）能调起系统打开 http(s) / file / OSC8 链接；相对路径按命中行 `working_directory` 解析，`file:line:col` 至少能打开文件本体；命中链接右键出现打开 / 拷贝项，不影响选择、拖拽与窗口拖动。

验证：`cargo test -p terminal_app --lib` 64 passed / 0 failed；`cargo test -p terminal_core --lib` 96 passed / 5 failed（失败项均为 PTY spawn 用例，受限沙箱下 `Operation not permitted`，与本改动无关）；`cargo clippy -p terminal_app -p terminal_core --all-targets -- --deny warnings` 与 `cargo fmt --all -- --check` 通过。新增测试覆盖：`navigation_target_at` 命中 / 同文本未命中 / 越界（core）；`resolve_path_like_target` 的相对路径、`:` 与 `(,)` 后缀、绝对路径、缺失路径、无 cwd；`Copy Link` 文本；`Event::Open` → 系统打开 URL 与文件路径。真机视觉与交互验收待做。

## 传输文件支持
需要考虑是否设计通用的扩展接口
- rz/sz 支持
- trzsz 支持 https://github.com/ruanimal/trzsz-rs

### 复杂度评估

实现真正可用的 rz/sz 支持属于高复杂度功能，约为 8/10。它不是终端 UI 层的小功能，而是 `terminal_core` 的 PTY 传输层扩展。

当前数据流是：

```text
PTY -> Alacritty EventLoop -> ANSI/vte parser -> terminal_core::Terminal -> terminal_app 渲染
```

当前 PTY 输出由 Alacritty EventLoop 直接解析，ZedTerm 没有原始 PTY 输出的协议扩展点。ZMODEM 的二进制帧可能包含 `ESC`、`CR/LF`、控制字符、非 UTF-8 字节以及类似 ANSI 的内容，因此不能在终端渲染或 ANSI 解析之后再识别，必须在原始 PTY 输出和 Alacritty parser 之间加入协议分流层。

实现范围主要包括：

- `rz`：检测远端接收端握手，弹出本地文件选择器，读取本地文件并通过 PTY 发送。
- `sz`：检测远端发送端握手，选择本地保存位置，接收二进制内容并写入文件。
- 增加协议状态机、CRC、转义、分片、重试、超时、取消和异常退出处理。
- 传输期间仲裁普通键盘、鼠标、粘贴和协议输入，避免与终端输入混写。
- 增加异步文件读写、传输进度、错误和取消状态 UI。
- 覆盖任意 chunk 边界、PTY echo、SSH、文件名编码、空文件和大文件等测试场景。

推荐的通用边界为：

```text
PTY raw output -> ProtocolDetector
                     |-> 未匹配：交给 Alacritty parser
                     |-> 匹配传输协议：独占当前传输会话
```

协议逻辑不应放在 `TerminalElement` 或 ANSI parser 中。协议层应向 UI 暴露结构化状态，例如等待上传、等待保存路径、传输开始、进度变化、传输完成、失败和取消。未来 rz/sz 与 trzsz 可以复用这一边界。

建议按以下顺序实施：

1. 先设计通用 raw PTY 协议扩展接口。
2. 先做单 pane、显式触发、单文件的 MVP。
3. 优先评估成熟的 ZMODEM 或 trzsz 实现，避免从头编写协议状态机。
4. 再增加自动检测、多文件、进度、重试和跨平台行为。

如果目标只是实现文件上传下载，不要求兼容传统 rz/sz，建议优先评估 trzsz。它可以减少协议实现工作，但不能省略 raw PTY 拦截、文件选择和 UI 集成。跨平台、多文件、自动检测和 rz/sz/trzsz 共存应按独立传输子系统规划，而不是作为终端渲染层的零散功能。

## 多设置 profile 支持
类似 iterm2 的多 profile 支持
