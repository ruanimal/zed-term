# 独立终端应用（fork 定位）· 拆分计划

> 状态：WP1（内核裁剪）✅、WP0（骨架+引导）✅、WP2（渲染层移植）✅、WP3（多标签多窗口）✅ 已完成并通过用户验证；WP4（设置）进行中。
> 决策记录（已确认）：移除 vi mode；设置走 settings.json；多标签/多窗口；平台优先级 macOS → Linux。
> **应用名已定：ZedTerm**（`TERM_PROGRAM`/`ZED_TERM`=`zedterm`，`app_id`=`zedterm`）。

## 0. 实施状态

- **WP1 ✅**：`crates/terminal_core` 复制并裁剪完成（vi mode、任务系统、Headless、init command、release_channel 移除；`util::shell` 替代 `task::Shell`），99 测试全绿。
- **WP2 ✅**：`terminal_element.rs`/`terminal_scrollbar.rs` 移植并裁剪；`TerminalTab` 替代 `TerminalView`；自备 cursor/highlight 绘制。
- **WP3 ✅**：多标签（`ui::TabBar/Tab`）+ 多窗口（`open_window`）+ 窗口动作/快捷键；用户已验证（prompt 渲染、标签栏正常）。
- **WP4 进行中**：设置裁剪 + settings.json 机制 + 设置页。

### 已知问题与教训（重要）

1. **`font-kit` feature 缺失导致文字完全不可见（已修复）**：workspace 声明 `gpui = { default-features = false }`，依赖 gpui 时必须显式开 `["font-kit", "wayland", "x11"]`（gpui_platform 同理，其 `font-kit` 转发到 `gpui_macos/font-kit`）。否则 quad（背景/光标）正常、glyph 静默失败；gpui examples 因默认 features 正常渲染，极易误导排查。**排查手段**：`screencapture -l <CGWindowID>` 按窗口截图绕开遮挡；`cargo build -p gpui --example hello_world` 是黄金对照。
2. **macOS display link 只在 invalidation 时重绘**：当前以 250ms `window.refresh()` 心跳维持（workaround），后续应接通 terminal 事件 → notify → 重绘正规链路。
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
  src/tab.rs            # TerminalTab（WP3）
  src/terminal_element.rs   # 复制自 terminal_view/src/terminal_element.rs（WP2）
  src/terminal_scrollbar.rs # 复制自 terminal_view/src/terminal_scrollbar.rs
  src/terminal_path_like_target.rs # 复制并清理（WP2）
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
| 测试 | 32 个 `#[gpui::test]` 中 vi 相关用例删除，其余保留作回归 |

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

**改造**：`Shell`/`ShellKind` 是 shell 启动必需（`terminal.rs:1112, 1132, 1178, 3185`、`util::shell::get_system_shell`）→ 在 terminal_core 自备 `TerminalShell` 枚举（System / Program{program,args} + shell kind 检测），替换 `task::Shell`。**这是 WP1 的核心改动。**

#### 1.3 移除 Zed 特有项

| 位置 | 内容 |
|---|---|
| `terminal.rs:86-90, 937-970, 1062, 1446` | `HeadlessTerminal` Global + `new_display_only(_with_bounds)`（调用者仅 `acp_thread`/`eval_cli`，已确认） |
| `terminal.rs:672-684` | `insert_zed_terminal_env` → `insert_terminal_env`：`TERM_PROGRAM` 改应用名、版本号用自备常量（替代 `release_channel::AppVersion`，使用点 :1057） |
| `terminal.rs:2801, 2841, 2869` | 工作目录语义（project 依赖分支在 :243-259）→ 保留 CWD 追踪，删项目语义 |
| `terminal.rs:65-79, 86`（`terminal_settings.rs`） | `task::Shell` 转换、`project_content.merge_from_option`（见 WP4） |

**保留**：pty 生命周期与事件循环、输入输出、选择/剪贴板、搜索、滚动、超链接/OSC8、bell、`get_color_at_index` 配色、`Event` 枚举（`CloseTerminal` 保留为标签关闭信号）。

**Cargo.toml 变更**：删 `task`、`release_channel`；其余（settings/theme/theme_settings/gpui/collections/util/schemars 等）保留。

**验证**：`cargo test -p terminal_core` 全绿。

### WP2 渲染层清理（`terminal_app` 内）

- `terminal_element.rs:1849-1850`：删 telemetry（全文件对 project 仅此引用）。
- `terminal_scrollbar.rs`：不动。
- `terminal_path_like_target.rs`：`BackgroundPathResolution`（:916）用 `project::File` → 改用 `fs` crate；"打开"动作改为系统默认应用（`window.open_path` 或 OS 调用）。
- 消除 `project`/`workspace`/`zed_actions` 依赖后，Cargo.toml 依赖大幅瘦身。

### WP3 标签与窗口视图（最大工作包）

#### 3.1 `TerminalWindowView`（窗口根，蓝本：`terminal_panel.rs`）

**删除**：
- `impl Panel`（:1677-1820）
- `spawn_task` 全家（:632-1310）
- 远程/协作分支（:644-660, :847, :891, :943）
- `zed_actions`（:41, :168, :1416-1453）
- `actions!`（:45）裁为新动作集：`NewTab`/`CloseTab`/`NextTab`/`PrevTab`/`NewWindow` + 原终端动作（Copy/Paste/Clear/Scroll*/SelectAll/Search）

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

**保留**：复制/粘贴/清除/滚动/搜索/全选动作、IME、hover 路径、标题更新（`Event::TitleChanged`）。

#### 3.3 快捷键

从 `assets/keymaps/default-macos.json` / `default-linux.json` 提取 `"terminal"` context 绑定，收编为 app 自己的 keymap（去面板/task/zed 项）。

### WP4 设置（settings.json + 设置页）

#### 4.1 `TerminalSettings` 裁剪（`terminal_core/src/terminal_settings.rs:21-141`）

- **删除**：`dock`/`starts_open`/`flexible`/`button`/`show_count_badge`/`toolbar`；`WorkingDirectory` 的 CurrentFile/CurrentProject/FirstProject（保留 `Always`/`LastActiveDirectory`）；:65-79 `task::Shell` 转换；:86 project 合并。
- **保留**：`shell`（自备 `TerminalShell`）、`working_directory`、`env`、`detect_venv`、`font_size/font_family/font_fallbacks/font_features/font_weight`、`line_height`、`cursor_shape`、`blinking`、`alternate_scroll`、`option_as_meta`、`copy_on_select`、`keep_selection_on_copy`、`open_links_in_mouse_mode`、`bell`、`minimum_contrast`、`scroll_multiplier`、`max_scroll_history_lines`、`path_hyperlink_regexes`、`path_hyperlink_timeout_ms`、`scrollbar.show`。
- `default_width/default_height` 改为窗口初始尺寸语义。

#### 4.2 settings 机制（复用而非 fork）

- **直接复用** `crates/settings` + `crates/settings_content`（纯数据 crate，已核实依赖轻、无编辑耦合）——**不改这两个 crate 本身**。
- 默认模板：按 `assets/settings/default.json:1884-2033` 的 `"terminal"` 节重写**精简 default.json**（仅 terminal + theme/fonts 所需字段，其余字段由 serde default 兜底）。
- 层模型：只保留 Default/User 两层。
- **实施时验证点**：`SettingsStore` 的 `SettingsContent` 全量反序列化是否容忍缺失字段（预期可，因字段带 serde default）。

#### 4.3 设置页（新建，轻量）

- 数据层：参照 `settings_ui/src/page_data.rs:6774-7639`（`terminal_page()` 分组：Environment/Font/Display/Behavior/Layout/Advanced/Toolbar/Scrollbar → 裁剪为适配窗口应用的组）。
- 渲染层：参照 `settings_ui.rs:506-659` 表驱动模式（toggle/dropdown/text_field/number_field/font_picker renderers），组件用 ui（DropdownMenu/Switch/PopoverMenu/TextInput/Button）。
- 写入：`settings_json::update_value_in_json_text`。
- 入口：窗口内设置标签页或浮动窗口（`WindowKind::Floating`，参照 `zed.rs:1726-1744`）。

### WP5 会话持久化

- `persistence.rs`：`TerminalDb`（:410）→ `dirs::data_dir` + serde_json 轻量存储。
- 恢复内容：窗口数、每窗口标签集（cwd + shell 类型 + 窗口尺寸）。

### WP6 验证与打包

- WP1 后：`cargo test -p terminal_core` 全绿。
- WP3 后：macOS 手动冒烟（开窗、多标签、多窗口、复制粘贴、滚动、搜索、cwd 继承）。
- WP4 后：settings.json 读写回环 + 设置页视觉核对。
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
```

## 5. 待决策点

1. **应用名/产品标识**（影响 `TERM_PROGRAM`、`app_id`、包名、目录名；先定可避免返工）。
2. **fork 时是否保留历史**（影响后期操作：git fork 全仓删除 vs 复制精简）。
3. **Zed 本体终端是否在远期移除**（fork 完成后视产品决策，不影响本计划）。

## 6. 后期 fork 操作步骤（备忘）

1. 新仓库建立（选项 A：git 层 fork 全仓后删无关码；选项 B：复制 `terminal_core`/`terminal_app` + 公共依赖快照）。
2. 视仓库形态决定 `crates/terminal`、`crates/terminal_view` 的去留与替换。
3. 断开支线同步，独立演进。