# ZedTerm

ZedTerm 是基于 Zed 终端能力构建的独立终端应用。本 README 面向开发者和 AI Agent，说明项目定位、代码入口、当前能力和常用开发流程。

## 项目定位

ZedTerm 从 Zed workspace 中提取终端相关能力，复用 GPUI、UI、主题和 settings 基础设施，但不引入 `workspace`、`project`、`editor`、`language` 等编辑器依赖。

## 当前能力

- PTY、shell、Alacritty 终端解析、滚动、搜索、选择、剪贴板和 bell。
- 多窗口、多标签和标签内 split pane，支持 pane zoom、焦点切换、标签拖拽排序及窗口几何持久化。
- URL、OSC 8 和路径链接的悬停反馈、复制与系统打开。
- 基于 `settings.json` 的终端设置，包含 Terminal、Keymap 和 Themes 页面；支持用户键位编辑、冲突提示、主题扩展安装/更新/卸载。
- 以 macOS 为优先平台，同时提供 Linux 构建和 deb 打包配置。

后续未完成、低优先级和明确不支持的事项，以 [`docs/TODO.md`](docs/TODO.md) 为准。

## 文档入口

```text
docs/
├── TODO.md       # 当前需求、后续工作和明确不支持项
├── PLAN.md       # 已归档：架构决策和历史实施计划
└── GAPS.md       # 已归档：缺口消除与验收记录
```

`PLAN.md` 和 `GAPS.md` 只用于追溯历史背景，不作为当前进度或新需求入口。

## 代码导航

```text
crates/terminal_core/
└── src/terminal.rs                 # PTY、shell、终端状态、滚动、搜索等内核

crates/terminal_app/
├── src/main.rs                     # terminal-app 进程入口
├── src/app.rs                      # GPUI 初始化、窗口、主题和用户 keymap
├── src/window.rs                   # TerminalWindowView、WindowTab 和窗口级路由
├── src/keymap.rs                   # keymap.json 解析、编辑和冲突检测
├── src/extension_store.rs          # 主题扩展目录、下载、安装和卸载
├── src/themes_tab.rs               # Themes 设置页
├── src/settings_ui.rs              # Terminal、Keymap、Themes 设置窗口
├── src/persistence.rs              # 窗口几何持久化
└── src/terminal/
    ├── tab.rs                      # 单个终端 pane 的状态和输入路由
    ├── split.rs                    # SplitNode、pane 分屏和 divider 拖动
    ├── terminal_element.rs         # 终端内容渲染
    ├── terminal_scrollbar.rs       # 终端滚动条
    ├── cursor.rs                   # 光标和选择区域绘制
    └── search_bar.rs               # 终端搜索 UI
```

## 核心架构

- `TerminalWindowView` 管理窗口级标签、窗口动作和焦点路由。
- 每个 `WindowTab` 拥有自己的 split 树；一个 tab 可以包含多个 pane，但 split pane 不会额外出现在标签栏中。
- `SplitNode` 是递归树：`Leaf` 持有一个 `TerminalTab`，`Axis` 持有方向、子节点和 flex 权重。
- pane 的输入、复制、滚动、搜索和上下文菜单操作应路由到当前实际获得焦点的 `TerminalTab`，不要只依赖标签栏选中项。
- GPUI 的实体和渲染运行在前台线程；耗时或可能阻塞的工作使用 `background_spawn`，结果回到 UI 层时要传播可读错误。
- 终端设置通过现有 settings/settings.json 机制读写；新增设置时同时考虑默认值、运行时生效范围、设置页和测试。

## 常用命令

以下命令均在仓库根目录执行：

```bash
# 编译检查
cargo check -p terminal_app

# 运行应用
cargo run -p terminal_app --bin terminal-app

# 运行 terminal_app 单元测试
cargo test -p terminal_app --lib

# 运行 terminal_core 测试
cargo test -p terminal_core --lib

# 格式检查
cargo fmt --all -- --check

# 静态检查
cargo clippy -p terminal_app --all-targets -- --deny warnings

# 检查 diff 中的空白错误
git diff --check
```

涉及终端内核或跨 crate 改动时，至少同时运行对应的 `cargo check`、测试、`cargo fmt` 和 `git diff --check`。涉及交互行为时，还需要在 macOS 上进行手动冒烟测试。

## macOS 应用无法打开

如果从可信来源获取的 ZedTerm 应用被 macOS 提示“应用已损坏，无法打开”，通常可以先将应用移动到 `/Applications`，再移除下载隔离属性：

```bash
sudo /usr/bin/xattr -rd com.apple.quarantine "/Applications/ZedTerm.app"
```

将命令中的应用路径替换为实际路径，然后重新打开应用。该命令会移除 macOS 的 quarantine 属性，仅应对确认来源可信的应用执行。

## AI Agent 工作流程

1. 先阅读本文件和 [`docs/TODO.md`](docs/TODO.md)，确认需求是否已经在源码中完成。
2. 只有需要追溯历史决策或验收背景时，才阅读已归档的 `docs/PLAN.md` 或 `docs/GAPS.md`，不要把已关闭条目当成当前任务。
3. 修改前确认目标 crate 和依赖边界，优先复用现有模块，避免把 `workspace`、`project` 或编辑器依赖引入 `terminal_app`。
4. 先做最小改动，遵守 Rust 错误传播和 GPUI 实体更新规则，避免 `unwrap()`、越界索引和在实体更新回调中重入更新同一实体。
5. 修改后至少运行与变更相关的 Cargo check、测试和格式检查；涉及交互行为时补做 macOS 手动冒烟测试。
6. 检查 `git diff` 和 `git status`，不要把无关文件或生成物带入改动。除非用户明确要求，不要自动创建提交。

## 变更边界

- `crates/terminal_app` 可以实现独立终端的窗口、标签、pane、设置和渲染行为。
- `crates/terminal_core` 可以实现终端内核行为，但应保持独立终端定位，不重新引入 Zed 的任务、协作、远程或 workspace 语义。
- 不要为了复用少量 UI 代码引入完整 `workspace` 依赖树；优先使用已有 `ui::Tab`、`ui::TabBar`、`ui::ContextMenu` 和 GPUI 原语。
- 新功能应同步更新 `docs/TODO.md` 或相关文档，不要把一次性的实现细节堆入 README。
