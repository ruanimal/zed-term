# ZedTerm

[English](README.md) | **简体中文**

ZedTerm 是一个从 [Zed](https://github.com/zed-industries/zed) 分叉而来的独立终端应用。它使用已发布的 `gpui-pre` 框架快照，并拥有自己的 UI、主题、设置和终端实现，不依赖编辑器功能。

该分叉目前已与上游产生较大差异，不再跟踪上游，因此这里的修复无需维持 Zed 的行为。不过，用户与 Zed 安装共享的格式——`keymap.json`、主题和扩展目录结构以及数据目录——会有意保持兼容。修改代码时的相关说明请参阅 [`.rules`](.rules)。

macOS 是主要支持的平台，同时也支持 Linux 构建和 deb 打包。

![ZedTerm 截图](docs/screenshot.png)

## 功能

- 基于 Alacritty 终端解析的 PTY 和 shell 支持
- 滚动、搜索、选择、剪贴板和响铃通知
- 多窗口、标签页、拆分窗格以及跨窗口移动标签页
- 窗格缩放、焦点切换和标签页重排
- URL、OSC 8 和路径链接支持
- 通过 [trzsz](https://trzsz.github.io/) 和 ZMODEM 上传与下载文件
- 终端设置、快捷键编辑、主题扩展管理和文件管理器集成

常见问题与 shell 配置说明请参阅 [FAQ](docs/FAQ.zh-CN.md)。

## 项目结构

- `crates/terminal_core` — 终端状态、PTY 处理、解析、滚动和搜索
- `crates/terminal_app` — 应用启动、窗口、标签页、窗格、设置和渲染
- `crates/ui`、`crates/theme`、`crates/settings`、`crates/fs`、`crates/paths` 等 — 从 Zed 继承的基础设施。应用特有的行为应优先在上述两个 crate 中实现；只有当改动确实属于共享层时，才修改共享 crate。
- GPUI 核心、宏、平台后端、HTTP 接口和 refinement trait 来自 `gpui-pre` 系列，在 `Cargo.toml` 中固定为 `=0.3.1`，并由 `Cargo.lock` 解析。仓库中的 `gpui_util` 和 `gpui_shared_string` 仍供独立支持库的用户使用。

当前工作、计划功能和暂不支持的项目请参阅 [`docs/TODO.md`](docs/TODO.md)。[`docs/PLAN.md`](docs/PLAN.md) 和 [`docs/GAPS.md`](docs/GAPS.md) 保存了归档的项目背景。

## 开发

在仓库根目录运行以下命令：

```bash
# 检查应用
cargo check --locked -p terminal_app

# 使用独立数据目录运行开发实例
cargo run --locked -p terminal_app --bin terminal-app -- --user-data-dir /tmp/zedterm-dev

# 运行所有已修改 crate 的测试
cargo test --locked -p terminal_app --lib
cargo test --locked -p terminal_core --lib
cargo test --locked -p settings --lib

# 检查格式和 lint
cargo fmt --all -- --check
cargo clippy --locked -p terminal_app --all-targets -- --deny warnings
```

ZedTerm 的每个数据目录只允许一个进程。不传 `--user-data-dir` 时，已经运行的
ZedTerm 会打开一个新窗口，新编译的进程则正常退出。新窗口属于已有进程，
运行的仍是旧代码。使用同一目录重新运行之前，
请先关闭上一个开发实例。详细机制请参阅
[FAQ](docs/FAQ.zh-CN.md#为什么在-zedterm-中执行-cargo-run-会立即退出)。

修改代码后还应运行 `git diff --check`。影响交互行为的更改应在 macOS 上进行冒烟测试。

升级框架时还需要运行 `cargo check --locked --workspace --all-targets`、`cargo test --locked --workspace --all-targets` 和 `cargo clippy --locked --workspace --all-targets -- --deny warnings`。
核心依赖和平台依赖都必须启用 `font-kit`、`wayland` 和 `x11`；只启用核心 feature 不会选择平台后端。仅包含宏的 `inspector` feature 和共享的 proptest patch 用于规避 [`docs/GPUI_PRE_MIGRATION.md`](docs/GPUI_PRE_MIGRATION.md) 中记录的快照/测试宏缺陷。

使用 `cargo build --locked --release -p terminal_app --bin terminal-app` 构建发布版本。使用 `scripts/package-linux-deb.sh` 或 `scripts/package-macos-dmg.sh` 打包生成的二进制文件。macOS 构建仍需要 Xcode Metal 工具；外部 Apple 后端已嵌入编译后的着色器，因此不需要本地 GPUI checkout，也无需额外复制着色器资源。

## 许可证

ZedTerm 继承自 Zed 的许可证：GPL-3.0（[`LICENSE-GPL`](LICENSE-GPL)）和 Apache-2.0（[`LICENSE-APACHE`](LICENSE-APACHE)，版权归 Zed Industries, Inc. 所有）。
已发布的 `gpui-pre` 系列采用 Apache-2.0 许可证，并保留 Zed Industries 的署名。
随附的字体、图标和主题保留位于 `assets/` 下的许可证和署名文件。
