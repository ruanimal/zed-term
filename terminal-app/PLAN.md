# 独立终端应用（fork 定位）· 拆分计划

> 状态：WP0 ✅、WP1 ✅、WP2 ✅、WP3 ⚠️（骨架完成，遗留项已并入 WP4 且已完成，见下）、WP4 ✅ 已实现（设置裁剪 + settings.json 机制 + 设置页 + WP3 收尾；**待用户编译验证，环境无 cargo**）；WP5 待做。
> 决策记录（已确认）：移除 vi mode；设置走 settings.json；多标签/多窗口；平台优先级 macOS → Linux。
> **应用名已定：ZedTerm**（`TERM_PROGRAM`/`ZED_TERM`=`zedterm`，`app_id`=`zedterm`）。

## 0. 实施状态

- **WP1 ✅**：`crates/terminal_core` 复制并裁剪完成（vi mode、任务系统、Headless、init command、release_channel 移除；`util::shell::Shell` 替代 `task::Shell`）。裁剪后测试共 52 个（`terminal.rs` 28 个 `#[gpui::test]` + 18 个 `#[test]`、`alacritty.rs` 5 个、`pty_info.rs` 1 个）。注：`new_display_only*` 保留在 `#[cfg(any(test, feature = "test-support"))]` 下作测试辅助；`terminal_settings.rs` 的 project 合并待 WP4 删除。
- **WP2 ✅**：`terminal_element.rs`/`terminal_scrollbar.rs` 移植并裁剪（零 project/workspace/telemetry 引用）；`TerminalTab` 替代 `TerminalView`；自备 cursor/highlight 绘制（`cursor.rs`）。注：`terminal_path_like_target.rs` 未移植，hover 路径功能整体缺失，归入 WP4 收尾。
- **WP3 ⚠️ 骨架完成**：多标签（`ui::TabBar/Tab`，关闭标签即 drop `Entity<Terminal>` → pty 子进程终止）+ 多窗口（`open_window`，最后标签关闭时 `remove_window`）+ 窗口动作/快捷键（`cmd-t`/`cmd-w`/`ctrl-tab`/`ctrl-shift-tab`/`cmd-n`）。**遗留（并入 WP4）**：Copy/Paste/Clear/Scroll*/SelectAll/SearchTest 等原终端动作未接线（`terminal_core` 中 `actions!` 已保留但 app 层无 `on_action`/`KeyBinding`）、剪贴板集成缺失、hover 路径未移植、标题更新无事件订阅（靠 250ms 心跳兜底）。
- **WP4 进行中 → ✅ 已实现（未编译验证）**：设置裁剪 + settings.json 机制 + 设置页 + **WP3 收尾**（见 §4.4）。遗留小项：hover 路径 tooltip（仅做了 `Event::Open` 打开 + 既有高亮）。

### 已知问题与教训（重要）

1. **`font-kit` feature 缺失导致文字完全不可见（已修复）**：workspace 声明 `gpui = { default-features = false }`，依赖 gpui 时必须显式开 `["font-kit", "wayland", "x11"]`（gpui_platform 同理，其 `font-kit` 转发到 `gpui_macos/font-kit`）。否则 quad（背景/光标）正常、glyph 静默失败；gpui examples 因默认 features 正常渲染，极易误导排查。**排查手段**：`screencapture -l <CGWindowID>` 按窗口截图绕开遮挡；`cargo build -p gpui --example hello_world` 是黄金对照。
2. **macOS display link 只在 invalidation 时重绘**：当前以 250ms `window.refresh()` 心跳维持（workaround），正规链路（terminal 事件 → notify → 重绘）随 WP4 §4.4 的 `Event::TitleChanged`/`SelectionsChanged` 订阅一并接通。
3. **窗口激活需延迟**：`activate_window()` 需在窗口挂到屏幕后（~300ms）调用，否则 display link 不启动。
4. **待办（用户反馈）**：字体渲染有小瑕疵（现象待复现确认，非阻塞）。

---

## 1. 定位与 fork 策略

**产品定位**：从 Zed 提取传统终端能力（不含 vi mode、任务系统、远程/协作、init command、Headless 模式），做成独立终端应用。功能范围 = 传统终端能力 + 终端相关设置项（含设置页面）。

**Fork 策略（本计划的核心约束）**：

1. **初期**：在 zed workspace 内开发，但**不修改 zed 本体任何现有源文件**（`crates/terminal`、`crates/terminal_view`、`crates/zed` 等一律不动，零回归风险）。新增两个 crate：
   - `crates/terminal_core`：从 `crates/terminal` **复制**后裁剪（vi mode / 任务 / Headless 等）。
   - `crates/terminal_app`：独立应用（入口、窗口、标签、设置页、资产）。
2. **后期 fork**（两选项，取决于是否保留历史，届时决策）：
   - **保留历史**：git 层 fork 整个仓库 → 在新仓库中删除无关代码（git 历史仍在）。
   - **不保留历史**：仅将 `terminal_core`、`terminal_app` 及公共依赖快照（gpui/ui/theme/theme_settings/settings/settings_content/fs/util/collections/paths/menu/assets 精简版）复制到新仓库 → 独立演进，新仓库可 squash 历史。
3. **同步机制**：初期开发阶段，若 zed 本体的 `crates/terminal` 有重要修复，手动移植到 `crates/terminal_core`（两处代码量级小，可接受；fork 后不再同步）。

---

## 1. 现状盘点（已核实的证据）

### 1.1 内核层 `crates/terminal`（~1.0 万行）

- `terminal.rs`（213KB）：域类型（`Content`/`Cell`/`Modes`/`Point`/`Search`/`Hyperlink`）+ `Terminal`/`TerminalBuilder` + pty 生命周期/事件循环 + 输入输出/选择/搜索/滚动。
- `alacritty.rs` + `alacritty/hyperlinks.rs`：Alacritty 后端适配（backend-neutral 边界见 `terminal_view/README.md:13-23`）。
- `mappings/`：键鼠→ANSI 协议映射；`pty_info.rs`：进程追踪。
- 对 gpui 仅用**类型**（`terminal.rs:56-60` 一个 import 块），无 UI 渲染。

### 1.2 UI 层 `crates/terminal_view`（~1.1 万行）

| 文件 | 行数 | fork 处理 |
|---|---|---|
| `terminal_element.rs` | 3005 | **复制**进 terminal_app，删 10 处 project 引用（telemetry） |
| `terminal_scrollbar.rs` | 87 | 复制，不动 |
| `terminal_view.rs` | 3280 | 不复用，作为 TerminalTab 重写蓝本 |
| `terminal_panel.rs` | 3370 | 不复用，作为 TerminalWindowView 重写蓝本 |
| `persistence.rs` | 544 | 重写为 JSON 会话存储 |
| `terminal_path_like_target.rs` | 1013 | 复制后清理 project 依赖 |

### 1.3 可复用基建（已验证零/轻耦合）

- `ui::Tab`/`ui::TabBar`（`crates/ui/src/components/tab.rs:33-107`、`tab_bar.rs:17-86`）：零 workspace 依赖，直接用。
- `menu` crate：仅依赖 gpui，可带走。
- `settings` crate：依赖基础 + `settings_content`（纯数据 crate，依赖仅 anyhow/collections/gpui/schemars/serde/settings_json/settings_macros/util 等，**无 project/editor/workspace**）。
- `settings_json`：settings.json 文本编辑，可带走。
- gpui 多窗口：`App::open_window`（`gpui/src/app.rs:1259-1292`）；窗口枚举 `App::windows()` + `downcast`（`zed.rs:1712-1716` 模式）。
- 先例：`crates/edit_prediction_cli` 独立 bin + GPUI headless + 调用 `terminal_view::init`（`headless.rs:117`）——证明终端初始化可脱离完整 IDE。
- 窗口引导链参照 `crates/zed/src/main.rs`：`build_application`（:86-93）→ `settings::init`（:503）→ `theme_settings::init`（:675）→ `load_embedded_fonts`（:734，实现 :1836-1858）→ `open_window`；`WindowOptions` 参照 `zed::build_window_options`（`zed.rs:361-428`），浮动窗参照 AboutWindow（`zed.rs:1726-1744`）。

---

## 2. 工作包清单

### WP0 应用骨架 `crates/terminal_app`（可与 WP1 并行）

```
crates/terminal_app/
  Cargo.toml            # [[bin]] terminal-app + [lib]
  src/main.rs           # 入口：Application 引导（参照 main.rs:86-93）
  src/app.rs            # 初始化链：settings::init → theme_settings::init → 字体加载 → open_window
  src/window.rs         # TerminalWindowView（多标签 + 多窗口，WP3）
  src/terminal/mod.rs   # TerminalTab（WP3）
  src/terminal/tab.rs           # TerminalTab：view 状态（IME/滚动/输入转发）
  src/terminal/terminal_element.rs   # 复制自 terminal_view/src/terminal_element.rs（WP2）
  src/terminal/terminal_scrollbar.rs # 复制自 terminal_view/src/terminal_scrollbar.rs
  src/terminal/cursor.rs        # 自备 cursor/highlight 绘制（WP2）
  src/terminal_path_like_target.rs # 未移植；hover 路径归 WP4 §4.4
  src/persistence.rs    # JSON 会话存储（WP5）
  src/settings_ui.rs    # 设置页（WP4）
  src/assets.rs         # AssetSource 精简版（fonts + themes + icons，参照 crates/assets/src/assets.rs:7-38）
```

### WP1 `crates/terminal_core`（复制 + 裁剪 内核）

**方式**：`cp -r crates/terminal crates/terminal_core`（改 Cargo.toml 包名与 lib path），随后裁剪。zed 本体 `crates/terminal` 不动。

#### 1.1 移除 vi mode（`terminal.rs` + `alacritty.rs`）

| 位置 | 内容 |
|---|---|
| `terminal.rs:72-74` | import：`toggle_vi_mode as toggle_term_vi_mode, update_selection_to_vi_cursor, update_vi_cursor_for_scroll, vi_goto_point, vi_motion` |
| `terminal.rs:106` | `enum ViMotion` |
| `terminal.rs:611` | `actions!` 中 `ToggleViMode` 条目 |
| `terminal.rs:715` | `InternalEvent::ViMotion(ViMotion)` |
| `terminal.rs:999, 1276` | 两处 `vi_mode_enabled: false` 初始化 |
| `terminal.rs:1470` | 字段 `vi_mode_enabled: bool` |
| `terminal.rs:1688-1690` | scroll 时 vi cursor 更新分支 |
| `terminal.rs:1753, 1758-1763` | `InternalEvent` 匹配分支 |
| `terminal.rs:1929` | `if self.vi_mode_enabled` 分支 |
| `terminal.rs:2201-2244` | `pub fn toggle_vi_mode` / `pub fn vi_motion` |
| `terminal.rs:2289, 2296` | `try_keystroke` 内 vi 分支 |
| `terminal.rs:3070` | `pub fn vi_mode_enabled()` |
| `alacritty.rs` | `toggle_vi_mode`/`vi_goto_point`/`vi_motion`/`update_vi_cursor_for_scroll`/`update_selection_to_vi_cursor` 及相应测试 |
| 测试 | 原 35 个 `#[gpui::test]` 中 vi/Headless 相关用例删除（现 28 个），另有 18 个 `#[test]`；`alacritty.rs` 5 个、`pty_info.rs` 1 个，合计 52 |

#### 1.2 移除任务集成（`task` 依赖）

| 位置 | 内容 |
|---|---|
| `terminal.rs:31` | `use task::{HideStrategy, Shell, ShellKind, SpawnInTerminal}` |
| `terminal.rs:1515-1533` | `TaskState`/`TaskStatus` + `task_summary`（:3097） |
| `terminal.rs:1469` | 字段 `task: Option<TaskState>` |
| `terminal.rs:2944, 2980, 2984` | `kill_active_task`/`task()`/`wait_for_completed_task` |
| `terminal.rs:3058-3062` | `HideStrategy` 分支 |
| `terminal.rs:2074, 2133, 2141` | init command 握手三函数（Zed 特有） |
| `terminal.rs:902-925` | `init_command_startup_marker_command(shell_kind)` |

**改造（已完成）**：`Shell`/`ShellKind` 是 shell 启动必需（`terminal.rs:1112, 1132, 1178, 3185`、`util::shell::get_system_shell`）→ 直接复用 `util::shell::Shell`（System / Program{program,args,env}），替换 `task::Shell`（原计划的"自备 `TerminalShell` 枚举"未做，复用现成类型功能等价且更省）。**这是 WP1 的核心改动。**

#### 1.3 移除 Zed 特有项

| 位置 | 内容 |
|---|---|
| `terminal.rs:86-90, 937-970, 1062, 1446` | `HeadlessTerminal` Global + `new_display_only(_with_bounds)`（调用者仅 `acp_thread`/`eval_cli`，已确认）。状态：Global 已删；`new_display_only*` 保留但收进 `#[cfg(any(test, feature = "test-support"))]` 作测试辅助，无产品路径 |
| `terminal.rs:672-684` | `insert_zed_terminal_env` → `insert_terminal_env`：`TERM_PROGRAM` 改应用名、版本号用自备常量（替代 `release_channel::AppVersion`，使用点 :1057） |
| `terminal.rs:2801, 2841, 2869` | 工作目录语义（project 依赖分支在 :243-259）→ 保留 CWD 追踪，删项目语义 |
| `terminal.rs:65-79, 86`（`terminal_settings.rs`） | `task::Shell` 转换、`project_content.merge_from_option`（见 WP4） |

**保留**：pty 生命周期与事件循环、输入输出、选择/剪贴板、搜索、滚动、超链接/OSC8、bell、`get_color_at_index` 配色、`Event` 枚举（`CloseTerminal` 保留为标签关闭信号）。

**Cargo.toml 变更**：删 `task`、`release_channel`；其余（settings/theme/theme_settings/gpui/collections/util/schemars 等）保留。

**验证**：`cargo test -p terminal_core`（裁剪后 52 个测试；当前环境无 cargo，未复跑，WP6 一并验证）。

### WP2 渲染层清理（`terminal_app` 内）

- `terminal_element.rs:1849-1850`：删 telemetry（全文件对 project 仅此引用）。
- `terminal_scrollbar.rs`：不动。
- `terminal_path_like_target.rs`：**未移植**（`BackgroundPathResolution` :916 用 `project::File`，需改用 `fs` crate + 系统默认应用打开）。hover 路径功能整体缺失，归 WP4 §4.4 收尾。
- 消除 `project`/`workspace`/`zed_actions` 依赖后，Cargo.toml 依赖大幅瘦身（已达成：`terminal_app` 依赖无 project/workspace/editor/language/task/db）。

### WP3 标签与窗口视图（最大工作包）

#### 3.1 `TerminalWindowView`（窗口根，蓝本：`terminal_panel.rs`）

**删除**：
- `impl Panel`（:1677-1820）
- `spawn_task` 全家（:632-1310）
- 远程/协作分支（:644-660, :847, :891, :943）
- `zed_actions`（:41, :168, :1416-1453）
- `actions!`（:45）裁为新动作集：`NewTab`/`CloseTab`/`NextTab`/`PrevTab`/`NewWindow` ✅（已在 `window.rs` 接线并绑定快捷键）+ 原终端动作（Copy/Paste/Clear/Scroll*/SelectAll/Search）⚠️ **未接线**：`terminal_core` 的 `actions!` 已保留（`terminal.rs:568-600`），但 `terminal_app` 无 `on_action`/`KeyBinding` 绑定，剪贴板集成缺失 → 归 WP4 §4.4

**多标签**：`ui::TabBar` + `ui::Tab`（渲染模式参照 `workspace/src/pane.rs:2907, 3553`；组件本身零 workspace 耦合）。每 Tag 持有 `Entity<Terminal>` + 标题；关闭 = drop entity（pty 终止）。

**多窗口**：`App::open_window`（`gpui/src/app.rs:1259-1292`）+ `WindowOptions`（参照 `zed.rs:361-428`）；窗口注册表用 `App::windows()` + `downcast`（`zed.rs:1712-1716` 模式）或自定义 Global。右键菜单用 `menu` crate。

#### 3.2 `TerminalTab`（蓝本：`terminal_view.rs` 中 `TerminalView`）

**删除**：
- `impl Item`（:1440）/`SerializableItem`（:1849）/`SearchableItem`
- `TerminalView::new` 的 workspace/project 参数（:233-239）→ 改为 `TerminalBuilder` + 自备 cwd 解析
- vi 引用（:833-834, :997-998, :1274-1281, :1368）
- 任务 UI：`rerun_button`（:1088）、`terminal_rerun_override`（:1108-1109）
- 工作目录解析（:2124-2160）→ 基于 cwd/env 的简化版
- drop 中 `ProjectEntryId`（:1713）→ 简化为终止 pty
- rename 弹窗 `editor` 依赖 → `ui::TextInput`
- `breadcrumbs`/`language`/`db` 依赖

**保留**（已实现）：IME（`tab.rs` ImeState + 输入处理器）、标题更新兜底（250ms 心跳重绘时读取 `title()`，无事件订阅）。
**保留**（⚠️ 未实现，归 WP4 §4.4）：复制/粘贴/清除/滚动/搜索/全选动作、hover 路径、`Event::TitleChanged` 订阅 → notify 正规链路。

#### 3.3 快捷键

从 `assets/keymaps/default-macos.json` / `default-linux.json` 提取 `"terminal"` context 绑定，收编为 app 自己的 keymap（去面板/task/zed 项）。

### WP4 设置（settings.json + 设置页）

#### 4.1 `TerminalSettings` 裁剪（`terminal_core/src/terminal_settings.rs:21-141`）

- **删除**：`dock`/`starts_open`/`flexible`/`button`/`show_count_badge`/`toolbar`；`WorkingDirectory` 的 CurrentFile/CurrentProject/FirstProject（保留 `Always`/`LastActiveDirectory`）；:65-79 `task::Shell` 转换；:86 project 合并。
- **保留**：`shell`（`util::shell::Shell`，非自备枚举）、`working_directory`、`env`、`detect_venv`、`font_size/font_family/font_fallbacks/font_features/font_weight`、`line_height`、`cursor_shape`、`blinking`、`alternate_scroll`、`option_as_meta`、`copy_on_select`、`keep_selection_on_copy`、`open_links_in_mouse_mode`、`bell`、`minimum_contrast`、`scroll_multiplier`、`max_scroll_history_lines`、`path_hyperlink_regexes`、`path_hyperlink_timeout_ms`、`scrollbar.show`。
- `default_width/default_height` 改为窗口初始尺寸语义。

#### 4.2 settings 机制（复用而非 fork）

- **直接复用** ✅ `crates/settings` + `crates/settings_content`（未改这两个 crate）。
- 默认模板 ⚠️ **偏差**：`settings::init` 的 Default 层来自 settings crate 编译期 RustEmbed 的 Zed 全量 `default.json`（settings crate 不可改），故**未做精简 default.json**；缺失字段由 `SettingsContent` 的 Option 兜底（验证点：全量反序列化容忍缺失已验证可行），ZedTerm 只消费被链接的 terminal/theme 设置类型，全量文件无副作用。
- 层模型 ✅ 只保留 Default/User 两层（未引入 global/server/project）。
- 写入 ✅ `SettingsStore::update_settings_file`（内部 `settings_json::update_value_in_json_text`）保留注释与格式。

#### 4.3 设置页（新建，轻量）

- ✅ `src/settings_ui.rs`：浮动窗口（`WindowKind::Floating`，`cmd-,` 打开），自绘行控件（stepper/cycle/toggle），覆盖 font_size、cursor_shape、blinking、option_as_meta、copy_on_select；写入走 `update_settings_file`，文件 watcher + `refresh_windows` 即时生效。
- ⚠️ 偏差：未用 `settings_ui` crate 的表驱动/`ui::DropdownMenu`（依赖面大），控件为 div 自绘；font_family/font_fallbacks/scrollbar 等项未入页（后续补）。

#### 4.4 WP3 收尾（终端动作接线与交互缺失项，自 WP3 移入）

- **终端动作接线**：`terminal_core::terminal` 的 `actions!`（Clear/Copy/Paste/PasteText/ShowCharacterPalette/SearchTest/Scroll*/SelectAll）已在 kernel 保留 → 在 `TerminalTab`/`TerminalElement` 上实现各动作 handler（复制选中/粘贴/清除/滚动/全选/搜索），`on_action(cx.listener(...))` 绑定；剪贴板读写经 `window.write_to_clipboard`/`read_from_clipboard`。
- **快捷键**：从 `assets/keymaps/default-macos.json` / `default-linux.json` 的 `"terminal"` context 提取原终端绑定（cmd-c/cmd-v/cmd-a/cmd-k/滚动等），收编进 `app.rs` keymap；`copy_on_select`/`keep_selection_on_copy` 设置联动。
- **hover 路径与打开**：✅ 部分完成——`Event::Open` 订阅到手（`MaybeNavigationTarget::Url` → `open_url`；`PathLike` → `file://` 默认应用），hover 高亮沿用 element 既有渲染；**未做**：悬停 tooltip（原 `terminal_path_like_target.rs` 的 worktree 解析不适用，去 project 化移植留待后续）。
- **标题更新正规链路**：✅ `TerminalTab` 订阅 `Event::TitleChanged`/`BreadcrumbsChanged` → `cx.notify()`；250ms 心跳保留（shell 输出重绘仍需，见已知问题 2）。
- **`terminal_settings.rs` 遗留清理**：✅ `merge_from_option` 已删（§4.1 一并完成）。
- **右键菜单**（PLAN WP3.1 提及项，补录）✅：终端区右键菜单（New Terminal / Copy / Paste / Paste Text / Select All / Clear / Close Terminal Tab——对齐原版终端的上下文菜单，去掉 workspace/assistant 项；右键在无选区时先选中单词，mouse mode 下不拦截）；标签栏右键菜单（New Tab / New Window / Close Tab / Close Other Tabs）。复用 `ui::ContextMenu`，无需 `menu` crate 直接依赖。

### WP5 会话持久化

- `persistence.rs`：`TerminalDb`（:410）→ `dirs::data_dir` + serde_json 轻量存储。
- 恢复内容：窗口数、每窗口标签集（cwd + shell 类型 + 窗口尺寸）。

### WP6 验证与打包

- WP1 后：`cargo test -p terminal_core`（52 个测试；当前环境无 cargo 未复跑，WP6 一并验证）。
- WP3 后：macOS 手动冒烟（开窗、多标签、多窗口、cwd 继承；已通过——prompt 渲染、标签栏正常）。
- WP4 后（含 §4.4 收尾）：settings.json 读写回环 + 设置页视觉核对 + 终端动作冒烟（复制粘贴、选择、清除、滚动、搜索、hover 路径打开）。
- WP6：`./script/clippy` + macOS .app 打包；Linux 打包（AppImage/deb）。

---

## 3. 依赖清单

**带走（复用）**：`gpui`(+`gpui_macos`/`gpui_linux`)、`ui`、`theme`、`theme_settings`、`settings`、`settings_content`、`settings_json`、`fs`、`util`（shell/command/process）、`collections`、`paths`、`menu`、`assets`（仅 fonts/themes/icons 子集，经 terminal_app 自备 AssetSource）。
**fork 复制**：`crates/terminal_core`（内核）、`crates/terminal_app`（应用）。
**剥离（不引用）**：`workspace`、`project`、`editor`、`language`、`task`、`tasks_ui`、`db`、`breadcrumbs`、`remote`、`rpc`、`zed`、`zed_actions`、`release_channel`。

---

## 4. 执行顺序

```
WP0 ─┐
WP1 ─┴→ WP2 → WP3 ─→ WP5 → WP6
WP1 ───→ WP4 ─────────┘
             （WP4 含 §4.4 WP3 收尾）
```

## 5. 待决策点

1. **应用名/产品标识**（影响 `TERM_PROGRAM`、`app_id`、包名、目录名；先定可避免返工）。
2. **fork 时是否保留历史**（影响后期操作：git fork 全仓删除 vs 复制精简）。
3. **Zed 本体终端是否在远期移除**（fork 完成后视产品决策，不影响本计划）。

## 6. 后期 fork 操作步骤（备忘）

1. 新仓库建立（选项 A：git 层 fork 全仓后删无关码；选项 B：复制 `terminal_core`/`terminal_app` + 公共依赖快照）。
2. 视仓库形态决定 `crates/terminal`、`crates/terminal_view` 的去留与替换。
3. 断开支线同步，独立演进。