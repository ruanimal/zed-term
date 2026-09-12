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

**已完成（2026-09-11）**：设置窗口已改为多 tab 结构，现有终端设置内容原样成为「Terminal」tab，另增「Keymap」与「Themes」tab 占位。Tab 切换不清除 draft / 保存 / revision / 校验状态。

- 新增 `SettingsTab` 枚举（`Terminal`、`Keymap`、`Themes`）与 `active_tab` 字段，`render` 在标题栏与内容区之间渲染 `TabBar`，根据 `active_tab` 切换内容区。
- Tab「终端设置」：现有 `SettingsPage` render 逻辑不变，仅外层包 tab 容器；draft / 保存 / revision / 校验语义全部保持。
- Tab「快捷键」：初始为占位内容；2026-09-12 起已实现 keymap.json 编辑（见下）。
- Tab「主题下载」：初始为占位内容；2026-09-12 起已实现主题扩展下载链路（见下）。
- `switch_tab` 方法切换前调用 `clear_active_edit`，确保内联编辑不会跨 tab 泄漏。

待实现（后续阶段）：

- 无。

已实现 Tab「主题下载」（2026-09-12）：

- 新增 `crates/terminal_app/src/extension_store.rs`：直接对接 Zed 公开扩展 API（`https://api.zed.dev/extensions`）的裁剪版扩展商店。**未**复用 `extension_host`——它捆绑 WASM 运行时以及 language / grammar / LSP 支持，本 fork 都没有；主题扩展只需要下载 tar.gz、解包、读取 `extension.toml` 与 `themes/*.json`。
- 能力：列目录（`GET /extensions?provides=themes`）、安装（`/extensions/{id}/{version}/download`）、更新（目录版本高于已装版本时）、卸载。安装目录与 Zed 兼容（`paths::extensions_dir()/{id}/`），两边可共用同一 data directory。
- Tab UI（`crates/terminal_app/src/themes_tab.rs`）：搜索框（按 id / 名称 / 描述本地过滤，不重新请求 API）、「仅已安装」过滤开关、已装数量、reload 按钮、每行下载量徽标、Install / Update / Uninstall 与版本迁移显示（`1.0.0 → 2.0.0`）、主题数量徽标、逐行 busy 态与状态行（成功/失败）。
- 下载量取自目录 API 的 `download_count`；1000 以下显示原值，以上缩写为 `1.5K` / `1M`（列表列宽有限，精确值不是核心信息）。目录里没有的本机已装扩展无此项（API 是唯一来源）。
- 「仅已安装」过滤与搜索是叠加关系，便于集中处理更新与卸载；目录外的本机扩展同样保留，否则它们会变得无法卸载。
- 主题 tab 读取的扩展目录是构造期注入的字段（`ThemesTab::new`），而非直接读全局 `paths`，因此测试可指向临时目录，不会读到开发者本机真实已装扩展。
- 响应性（2026-09-12，真机反馈"输入框卡"）：目录有 628 个主题扩展，`render` 每次按键都会跑，原先三处开销叠加：
  - 列表逐行构建：628 行一次性建 element。改为 `uniform_list` 虚拟化，只构建可见行。行高固定为 `THEME_ROW_HEIGHT`（uniform_list 只量首行，要求等高），名称与描述改 `truncate()` 以免长描述撑高行。
  - Terminal tab 大树无条件构建：即使停在 Themes tab 也每帧重建整棵终端设置树（含全部行与下拉菜单）。改为惰性闭包，只在 Terminal tab 实际显示时构建。
  - 主题名与字体族枚举无条件执行：`ThemeRegistry::list_names()`（排序全部主题名）与 `FontFamilyCache::try_list_font_families()`（克隆整个字体族列表）原先在 `Render` 顶部，每次按键都跑。一并移入上述惰性闭包。
- 行的搜索文本（id/名称/描述的小写拼接，以 NUL 分隔）在 `rebuild_rows` 时预计算，避免每次按键对整目录做 `to_lowercase`；实测这项不是主要瓶颈（去掉后专门的时间断言仍通过，故未保留该断言），但改动廉价且语义等价。
- 目录里不存在的本机已装扩展也会列出（可卸载），避免它变成无法移除的孤儿。
- 主题生效：启动时 `app::load_extension_themes_in_background` 注册扩展主题；安装/卸载成功后 `app::load_extension_themes` 重新注册并 `theme_settings::reload_theme`，因此新装主题立即出现在 Terminal tab 的 Color theme 选择器里。
- 原子性：下载先解包到 `staging/` 子目录，校验 manifest 的 `id` 与请求一致后才替换安装目录。校验失败或解包失败只清理 staging，**不会**破坏已可用安装。`staging/` 永不作为已装扩展被列出。
- 安全：扩展 id 只允许 ASCII 字母数字与 `-` / `_`（防目录穿越）；archive 条目路径由 `async_tar` 校验不得逃出目标目录；manifest 里的 theme 路径同样拒绝非普通相对路径；下载体积上限 64 MiB。
- 网络层：本 fork 的 `Remove unused ZedTerm files`（18c10650c5）删掉了 `reqwest_client` / `http_client_tls`，而 gpui 桌面端默认是 `NullHttpClient`（所有请求直接报错），因此主题下载原本无从发起。本阶段恢复 `crates/reqwest_client`，并改为依赖 crates.io 上游 `reqwest`（Zed 的 `zed-reqwest` 只以 GitHub git 依赖发布，本 fork 需要能仅靠 crates.io 构建）；`main.rs` 在启动时用 `with_http_client` 注入真实 client。
- 适配上游 `reqwest` 的两处差异：上游只支持在 **构造 client** 时设定 redirect policy（Zed fork 支持逐请求设定），故按 policy 缓存少量 client；上游对代理 scheme 延迟到请求时才校验，故在构造期显式校验 scheme，保持「非法代理只被忽略、不拖垮 client」的既有语义。
- `rustls` 显式选用 `ring` provider（`default-features = false`）：默认的 `aws-lc-rs` 需要 cmake 与 C 工具链；`ring` 也是 reqwest `rustls-tls-native-roots` 自身所用的 provider，两端一致。

验证：`cargo test -p terminal_app --lib` 118 passed / 0 failed（新增 16 个 extension store 测试 + 24 个 themes tab 测试，含 archive 往返、重装替换、损坏/错配 archive 不破坏既有安装、路径穿越拒绝、staging 不被列出、搜索过滤、「仅已安装」过滤、下载量格式与渲染、更新判定、更新/卸载渲染、628 行目录一致性、虚拟化只构建可见行）；`cargo test -p reqwest_client` 4 passed / 0 failed；`cargo check --workspace --all-targets`、`cargo clippy -p terminal_app -p reqwest_client --all-targets -- --deny warnings` 与 `cargo fmt --all -- --check` 通过；`cargo build -p terminal_app --bin terminal-app` 成功。

真机联调：对 `api.zed.dev` 实测抓取目录 → 下载 tar.gz → 解包 → 读取 manifest → 解析主题全部通过（`catppuccin 0.2.26` 解析出 2 个 family / 8 个主题；`gruvbox-material 1.1.0` 解析出 1 个主题），并验证卸载后目录被移除。真机 GUI 视觉与交互验收待做。


验证：`cargo check -p terminal_app`、`cargo test -p terminal_app --lib` 64 passed / 0 failed、`cargo clippy -p terminal_app --all-targets -- --deny warnings` 与 `cargo fmt --all -- --check` 通过。

已实现 Tab「快捷键」（2026-09-12）：

- `keymap.json` 读写：启动时 `app::load_user_keymap` 读取并在内置绑定之后叠加，`watch_config_file` 监听外部改动；重建 keymap 走 `clear_key_bindings` + 内置绑定 + 用户绑定（`App::bind_keys` 只能追加，无法覆盖）。
- 内置绑定补上 `KeybindSource::Default` 元数据，用户覆盖标 `User`，tab 内因此能区分来源。这是"来源"列与冲突判定的前提。
- 按键列表：`keymap::process_bindings` 汇总全部生效绑定，未绑定的 action 也列出以便新增；按来源 + action 名排序。
- 搜索/过滤：按 action 名、humanized 名、按键显示文本过滤，另有"仅用户覆盖""仅冲突"两个开关。
- 改键/新增：点击行内铅笔打开编辑器，复用从已删除的 `crates/keymap_editor` 移植来的 `KeystrokeInput` 录制控件（仅依赖 gpui + ui）；写回复用 `KeymapFile::update_keybinding`，保留 JSONC 注释、按 tab size 缩进、必要时自动追加 `unbind` 抑制默认绑定。无绑定的 action 走 `Add`，有绑定的走 `Replace`。
- 冲突提示：移植 Zed 的 `ConflictState`，同按键同上下文按来源判定覆盖关系；提交前检测冲突并阻止写入，行内用警告图标标注。
- 重置默认：把 `keymap.json` 写回 `keymaps/initial.json` 模板内容并重载。

已移除 `base_keymap`（2026-09-12）：该设置在本 fork 中无法生效——它加载的是**叠加层**（`keymaps/{linux,macos}/*.json`，注释自述"只包含与 Zed 默认键位不同的部分"），需叠在 Zed 完整的 IDE 默认键位上；ZedTerm 没有那份基底，也没有 `Editor`/`Workspace`/`Pane` 等上下文。实测加载 `vscode.json` 只能解析出 2/15 条绑定，且全部因 action 不存在或上下文不匹配而无效。

- 删除 `crates/settings/src/base_keymap_setting.rs`（含 `from_settings` 里的 `s.base_keymap.unwrap()`）。
- 删除 `SettingsContent::base_keymap` 字段与 `BaseKeymapContent` 枚举。
- 删除 `assets/settings/default.json` 的 `"base_keymap": "Zed"` —— 此前正是靠这一行才没让上面那个 `unwrap()` 在启动时 panic（`SettingsStore::new` → `load_settings_types()` → `from_settings`）。
- 删除 `crates/settings/src/vscode_import.rs`（1179 行）及 `SettingsStore::import_vscode_settings` / `get_vscode_edits`；调用方仅剩测试。
- 删除 `assets/keymaps/{linux,macos}/` 共 13 个死资产，以及 `DEFAULT_KEYMAP_PATH` / `VIM_KEYMAP_PATH` / `SPECIFIC_OVERRIDES_KEYMAP_PATH` / `default_keymap` / `vim_keymap` 等无调用方的访问器（运行时实际只用 `keymaps/initial.json`）。
- 删除仅服务该导入链路的 `paths::{vscode,cursor}_settings_file_paths` 等 3 个函数。
- 测试夹具改用 `reduce_motion` 作为"非 terminal 设置应原样保留"的样本；保留 `test_edits_for_update_*`（JSONC 合并逻辑另有独立覆盖）。

验证：`cargo test -p terminal_app -p settings -p settings_content -p paths --lib` 78 + 30 + 38 passed / 0 failed；`cargo build -p terminal_app` 并实机启动无 panic；clippy / fmt 通过（`window_chrome.rs` 两条既有 lint 与本改动无关）。

未移植（依赖已删除的 IDE crate，无法复用）：Zed keymap_editor 的表格视图、命令面板 action 补全、JSON 语法高亮、action 参数编辑器、键位搜索模式（`KeystrokeInput` 的 search 变体保留但未接线）。

修复（2026-09-12，真机反馈）：改键后 status 卡在「Saving keybinding…」——成功/失败分支都没有复用 `clear_write_status`，`write_in_progress` 也未复位，导致后续保存被静默拒绝；搜索框混入按键——录制走的是页面 IME 处理器，会追加到当时活跃的内联字段（`KeymapSearch`），改为打开编辑器前先 `clear_active_edit`；改键结果变成 `super-v ctrl-shift-v` 组合键——旧按键被当作输入值而非 placeholder，重录时追加到旧组合上；另外补上 `keystroke_input` 上下文的 `enter` / `escape escape escape` / `delete` 绑定，否则录制可以开始但无法结束。

验证：`cargo test -p terminal_app --lib` 78 passed / 0 failed（新增 8 个 keymap 测试：humanize、上下文谓词等价、内置绑定自冲突、action 覆盖完整性、用户覆盖判定、搜索过滤、写回往返、无绑定走 Add 路径；另有 4 个回归测试覆盖上述 4 个缺陷）；`cargo check -p terminal_app --all-targets` 通过；`cargo clippy -p terminal_app --all-targets -- --deny warnings` 与 `cargo fmt --all -- --check` 通过（`window_chrome.rs` 两条既有 lint 与本次改动无关，已 stash 验证为改动前同样失败）。真机视觉与交互验收待做。

## Pane 方向性跳转

当前已有按叶子视觉序循环的 ActivateNextPane/ActivatePreviousPane。可增加 ActivatePaneInDirection，根据 pane 几何位置选择上、下、左、右方向的最近邻。

建议验收：嵌套横向/纵向 split 中，四向跳转均选择视觉上最近且方向正确的 pane；边缘无候选时保持当前焦点。

## Tab 溢出可见指示

**已完成（2026-09-11）**：Tab 栏溢出时在右侧固定区域显示左右 Chevron 指示按钮。按钮根据 `ScrollHandle` 的实际滚动位置实时启用或禁用，点击后按可视区域滚动 Tab；新建、关闭和批量关闭 Tab 后会自动确保活动 Tab 可见。指示器复用现有 `IconButton` 的 `XSmall`、`Square`、`Subtle` 样式，不遮挡 Tab 内容，也不影响点击、拖拽排序和窗口拖动。

验证：`cargo check -p terminal_app`、`cargo test -p terminal_app`、`cargo fmt --all -- --check` 与 `git diff --check` 通过。

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
