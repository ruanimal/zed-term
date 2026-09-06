# ZedTerm 开发指南

本文档面向后续开发者和 AI Agent，说明独立终端应用的代码入口、架构边界和常用开发流程。

## 项目定位

ZedTerm 是从 Zed workspace 中提取出的独立终端应用。它复用 GPUI、UI、主题和设置基础设施，但不引入 `workspace`、`project`、`editor`、`language` 等编辑器依赖。

需要注意：`terminal-app/` 是本项目的开发文档目录，不是 Rust crate。可执行应用的源码位于 `crates/terminal_app`，终端内核位于 `crates/terminal_core`。

## 代码导航

```text
terminal-app/                         # 本目录：计划、缺口和开发说明
├── README.md
├── TODO.md                            # 当前后续需求入口
├── PLAN.md                            # 架构决策和历史实施计划
└── GAPS.md                            # 已归档的缺口与验收记录

crates/terminal_app/
├── src/main.rs                        # terminal-app 进程入口
├── src/app.rs                         # GPUI 应用初始化、窗口和 keymap
├── src/window.rs                      # TerminalWindowView、tab、pane 路由
├── src/settings_ui.rs                 # 独立终端设置页
├── src/persistence.rs                 # 窗口几何持久化
└── src/terminal/
    ├── tab.rs                         # 单个终端 pane 的状态和输入路由
    ├── split.rs                       # SplitNode、pane 分屏和 divider 拖动
    ├── terminal_element.rs            # 终端内容渲染
    ├── terminal_scrollbar.rs           # 终端滚动条
    ├── cursor.rs                      # 光标和选择区域绘制
    └── search_bar.rs                  # 终端搜索 UI

crates/terminal_core/                   # PTY、shell、终端状态、滚动、搜索等内核
```

## 核心架构

- `TerminalWindowView` 管理窗口级 tab、焦点和窗口动作。
- 每个 `WindowTab` 拥有自己的 split 树；一个 tab 可以包含多个 pane，但 split pane 不会额外出现在 tab 栏中。
- `SplitNode` 是递归树：`Leaf` 持有一个 `TerminalTab`，`Axis` 持有方向、子节点和 flex 权重。
- pane 的输入、复制、滚动、搜索等操作应路由到当前实际获得焦点的 `TerminalTab`，不要只依赖 tab 栏选中项。
- GPUI 的实体和渲染运行在前台线程；耗时或可能阻塞的工作使用 `background_spawn`，结果回到 UI 层时要传播可读错误。
- 终端设置通过现有 settings/settings.json 机制读写；新增设置时同时考虑默认值、运行时生效范围、设置页和测试。

## 常用命令

在仓库根目录 `/Users/ruan/projects/zed` 执行：

```bash
# 编译检查
cargo check -p terminal_app

# 运行应用
cargo run -p terminal_app --bin terminal-app

# 运行 terminal_app 单元测试
cargo test -p terminal_app --lib

# 格式检查
cargo fmt --all -- --check

# 检查 diff 中的空白错误
git diff --check
```

涉及终端内核时，同时验证：

```bash
cargo test -p terminal_core
cargo check -p terminal_app
```

## AI Agent 工作流程

1. 先阅读本文件、`TODO.md`，再按问题范围阅读 `PLAN.md` 或 `GAPS.md`；`GAPS.md` 已归档，不要把已关闭条目当成当前任务。
2. 修改前确认目标 crate 和依赖边界，优先复用现有模块，避免把 `workspace`/`project` 依赖引入 `terminal_app`。
3. 修改 Rust 源码前确认仓库根目录 `README.md` 的审阅标记存在；不要删除该标记。
4. 先做最小改动，遵守 Rust 错误传播和 GPUI 实体更新规则，避免 `unwrap()`、越界索引和在实体更新回调中重入更新同一实体。
5. 修改后至少运行与变更相关的 Cargo check、测试和格式检查；涉及交互行为时补做 macOS 手动冒烟测试。
6. 检查 `git diff` 和 `git status`，不要把无关文件或生成物带入改动。除非用户明确要求，不要自动创建提交。

## 变更边界

- `crates/terminal_app` 可以实现独立终端的窗口、tab、pane、设置和渲染行为。
- `crates/terminal_core` 可以实现终端内核行为，但应保持独立终端定位，不重新引入 Zed 的任务、协作、远程或 workspace 语义。
- 不要为了复用少量 UI 代码引入完整 `workspace` 依赖树；优先使用已有 `ui::Tab`、`ui::TabBar`、`ui::ContextMenu` 和 GPUI 原语。
- 新功能应同步更新 `TODO.md` 或相关文档，但不要把一次性的实现细节堆入 README。

## 当前需求入口

后续未完成或低优先级事项以 `TODO.md` 为准。开始新任务前先确认需求是否已经在源码中完成，避免重复实现或恢复已明确不支持的功能。
