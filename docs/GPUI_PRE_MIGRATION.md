# 迁移到 gpui-pre 的方案

> 状态：待实施。2026-10-24 已完成源码和依赖静态分析，尚未进行迁移后的编译、测试和平台验收。

## 1. 目标与范围

将仓库内维护的 GPUI 核心及平台后端改为发布版 `gpui-pre` 系列依赖，减少内置框架源码和维护负担。
迁移保留现有终端实现、UI、主题、设置和窗口结构，不引入 `gpui-kit`、`gpui-base` 或 `gpui-component`。

`gpui-pre` 是 GPUI 源码快照的发布包，仍提供实体、窗口、布局、绘制和任务调度。
本方案只调整框架依赖来源，不以重写界面为前提，也不涉及同步、合并或重置到 Zed 分支。

必须保留的用户行为与格式：

- `keymap.json` 的格式、用户覆盖及上下文匹配行为。
- 主题格式、扩展目录布局，以及 `extensions_dir` 所在的数据目录。
- 终端绘制、输入法、剪贴板、搜索、链接、分屏与跨窗口移动标签页。
- 自定义数据目录、文件管理器打开目录、窗口恢复与应用重启参数。
- 主题下载和文件传输的现有功能及错误反馈。

收益主要是源码和维护范围缩小。框架代码仍需编译，构建耗时、产物体积和运行性能的变化需实测。

## 2. 已核实的现状与可行性

- 工作区有 11 个 `gpui*` 本地 crate；15 个业务或支撑 crate 在普通依赖中直接使用 `gpui`。
- [根 Cargo.toml](../Cargo.toml) 统一声明这些路径依赖，适合通过 workspace dependency 别名保留 `gpui::` 等代码路径。
- 已定位 Zed 分叉提交，当前 GPUI 核心和平台后端的 `src` 与分叉点一致；分叉后的相关变动主要是 manifest、示例及测试资源裁剪。
  首轮迁移应以该源码基线选择发布快照，而非直接采用 Kit 当前使用的版本，具体比较见第 3.1 节。
- [终端渲染器](../crates/terminal_app/src/terminal/terminal_element.rs) 使用自定义 `Element` 和 `InputHandler`，目标是保留实现并处理实际接口差异。
- 此前对 `0.3.9` 的抽查中，`TextRun`、`TextStyle`、`UnderlineStyle`、`WindowOptions` 未发现公开字段名称的增删；
  这不是较早版本的兼容性结论，选定候选版本后需重新核对字段类型、完整接口和行为。
- `with_restart_arguments` 和 `on_reopen` 在已核对的 `0.3.0`–`0.3.3` 及 `0.3.9` 中仍存在。
- 本方案继续通过 `gpui_platform::application()` 和 GPUI 的 `cx.open_window` 创建窗口，不增加 Base Root。
  现有以 `TerminalWindowView` 为根实体、通过 `downcast` 查找窗口的结构可以保留。

判断：可行性较高，主要工作是依赖统一、接口适配与平台回归；目前不保证直接编译通过，也不承诺具体工期。

## 3. 版本与依赖策略

### 3.1 以分叉提交选择初始版本

版本选择顺序：**分叉提交 → 各发布包的 `zed-rev` → 核心与平台源码差异 → 打包完整性及编译/测试结果**。
不以 Kit 的版本、GPUI crate 中的 `0.2.2` 版本号或发布日期单独判断兼容性。

已核实的本项目基线：

- 首个 ZedTerm 提交：`12519e6133dc32967673b66cf4b0919e04d8abce`，2026-08-25 14:12:52 +08:00。
- 该提交的父提交，也是本次迁移使用的 Zed 分叉点：`5631830c564afa89b3aba679f45d9c3345f9460f`，2026-08-25 04:32:44 +08:00。
- 该分叉点到当前版本的 GPUI 核心及平台后端 `src` 没有差异；最近一次分叉前的 `gpui` 目录修改是
  `f1d27d545e79f98ffde7ebf3229f8eb8bad791aa`（Linux 字体缓存预热）。不能把这个目录修改提交误认成整个项目的分叉点。

已下载发布包，读取 `Cargo.toml.orig` 中的 `[package.metadata.gpui-pre].zed-rev`，并逐字节比较核心 `src/**/*.rs`：

| gpui-pre 版本 | 包内 Zed 源码提交 | 与本地相同 / 有差异的源文件 | 发布包新增源文件 |
|---|---|---|---|
| `0.3.0` | `b1a7ef0cf66dfbf9d7661170c96d97c7df916c68` | 69 / 21 | 0 |
| `0.3.1` | `801c087af22dd189dc1aa49e2f370b4f04190b19` | 69 / 21 | 0 |
| `0.3.2` | `801c087af22dd189dc1aa49e2f370b4f04190b19` | 68 / 22 | 0 |
| `0.3.3` | `5b055fa789a8b8d38ac951a6e0cde272f66b4495` | 68 / 22 | 0 |
| `0.3.9` | `93d745668805029476e6d7fbb19652116fed51de` | 39 / 51 | 3 |

本地比较范围共 90 个源文件；文件数量不代表差异大小或编译修改量，也不覆盖平台后端。
`zed-rev` 相同仍可能有发布侧修改，例如 `0.3.2` 相对 `0.3.1` 修改了 `actions!` 宏的 derive 路径。
已比较的版本没有与分叉点完全一致的核心源码；不据此宣称其他版本或平台已验证兼容。

**首轮编译候选调整为 `0.3.1`，最终固定版本待验证。** 原因是它接近当前源码基线，同时补齐了
`0.3.0` 的 `test-support` 打包缺口：`0.3.0` 在启用该 feature 时重导出 `proptest`，但未将其声明为普通/可选依赖；
`0.3.1` 增加可选依赖并通过 feature 启用。项目依赖 GPUI 测试设施，因此不能只按源码接近程度选 `0.3.0`。
配套的 `gpui-pre-platform` 和 `gpui-pre-macros 0.3.1` 发布包也已核对，标注相同的 `zed-rev`。

实施时补齐候选版本在平台后端、宏、HTTP 接口及测试支持上的比较，再通过编译与平台验收决定最终版本。
若需要使用较新版本，记录具体阻塞问题及其修复依据；`0.3.9` 保留为此前分析的比较对象，不再作为默认迁移目标。
同一 GPUI 系列中直接声明的快照包使用相同的精确版本，提交 `Cargo.lock`；后续升级单独评估。

根 `Cargo.toml` 的核心调整示意：

```toml
[workspace.dependencies]
gpui = { package = "gpui-pre", version = "=0.3.1", default-features = false }
gpui_platform = { package = "gpui-pre-platform", version = "=0.3.1", default-features = false }
gpui_macros = { package = "gpui-pre-macros", version = "=0.3.1" }
```

这是以 `0.3.1` 为候选的核心依赖示意，不是完整可直接应用的补丁，也不表示已通过编译。应用当前需要的 `font-kit`、`wayland`、`x11`，
以及测试需要的 `test-support`，应核对后继续在使用方启用；平台支持不能只依赖核心包的同名 feature。
macOS 着色器构建、资源定位和部署目标也需验证，不直接复制 Kit 的全部 feature。

### 3.2 按类型边界处理配套 crate

| 范围 | 处理策略 |
|---|---|
| `gpui`、`gpui_platform`、`gpui_macros` | 改为配套发布包，避免旧宏或旧后端搭配新核心。`ui` 直接调用的 inspector 宏也在检查范围内。 |
| `gpui_linux`、`gpui_macos`、`gpui_apple`、`gpui_windows`、`gpui_wgpu`、`gpui_web` | 由外部平台包拉取所需后端；核实无本地调用方后，移出 workspace 并删除本地副本。 |
| `http_client` | 必须处理跨 GPUI 的 trait 边界。优先验证统一到与最终候选同版本的 `gpui-pre-http-client`，检查本地辅助 API 是否有差异。 |
| `reqwest_client` | 优先保留本地实现，将其适配到统一后的 HTTP trait；不直接替换成外部客户端。 |
| `gpui_shared_string`、`gpui_util`、`refineable` | 按公开类型、trait 和宏的实际流向决定统一、适配或保留。相同源码不意味着不同包的类型相同。 |
| `collections`、`scheduler`、`sum_tree` 等 | 不因外部 GPUI 存在同名功能就全部替换；允许不跨边界的独立实现共存，检查公开类型相交处。 |
| `ui`、`theme`、`settings`、`assets`、`terminal_core`、`terminal_app` | 保留，针对新 GPUI 的编译或行为差异做必要适配。 |

`gpui_shared_string` 和 `gpui_util` 仍有本地使用方，因此不能按 `gpui*` 前缀整体删除目录。
不再使用的旧 GPUI 核心和后端应退出 workspace，避免 `--workspace --all-targets` 继续编译旧实现。

### 3.3 HTTP 边界

[启动代码](../crates/terminal_app/src/main.rs) 将本地 `ReqwestClient` 注入 GPUI；
[该客户端](../crates/reqwest_client/src/reqwest_client.rs) 实现的是本地 `http_client::HttpClient`。
外部 GPUI 接收的是 `gpui-pre-http-client` 的 trait，两者不能仅凭接口相同而互换。

优先将工作区的 HTTP 接口依赖统一到配套发布包，保留本地客户端的逐请求重定向策略适配、代理处理、
超时及错误处理。同步适配主题商店、扩展下载和 `FakeHttpClient` 等测试调用方。
若发布包缺少本地辅助功能，先评估在应用侧保留辅助模块；确需桥接时明确请求、响应及 body 的转换边界。

## 4. 实施步骤与阶段出口

### 阶段一：记录迁移前基线

1. 按第 3.1 节复核分叉提交、发布包 `zed-rev` 与实际源码差异，先确定编译候选；保留源码和打包差异的记录。
2. 使用隔离分支或 checkout，保留当前工作区已有的未提交修改，不把其他功能变化混进迁移。
3. 运行现有编译与测试，记录已存在的失败及其环境条件。
4. 在同一环境记录主平台的输入、绘制、多窗口、主题下载和打包结果，供迁移后比较。
5. 列出跨 GPUI 边界的公共类型及直接调用的宏，确定配套依赖调整清单。

出口：有可重现的基线、明确的依赖边界和独立的迁移范围。

### 阶段二：替换依赖并取得可编译版本

1. 调整 workspace dependency 别名，统一核心、平台和宏的版本，解决 HTTP trait 边界。
2. 将不再使用的旧核心和后端移出 workspace；源码清理放在验证后进行。
3. 保留现有应用启动、窗口根实体和 UI，按编译器报告逐项适配，不同时重构界面。
4. 同步迁移 `test-support`、测试客户端和跨 crate 测试夹具。
5. 更新 Cargo profile 中按包名匹配的条目，例如 `gpui_macros` 对应的优化配置。
   依赖别名不会把真实 package 名称改回原名；其他被替换包同样检查。
6. 检查根 `[patch.crates-io]` 是否继续需要、是否影响新依赖图，不批量删除或照搬外部项目配置。
7. 生成并审阅 `Cargo.lock`，检查重复框架实例和新增依赖来源。

出口：应用和全部保留的 workspace target 可编译；依赖图中不存在同时供应用交互使用的两套 GPUI 类型。

### 阶段三：测试、平台与打包验收

先运行受影响 crate 的测试，再完成整个工作区检查；明确区分测试失败和平台能力受限。
本次依赖替换覆盖框架、平台与测试设施，不能仅以 `cargo check -p terminal_app` 作为完成标准。

```bash
cargo check --locked --workspace --all-targets
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- --deny warnings
cargo fmt --all -- --check
cargo build --locked --release -p terminal_app --bin terminal-app
git diff --check
```

使用 `cargo tree --duplicates` 查看重复包，并结合 `cargo tree -i gpui-pre` 和
`cargo tree -i gpui-pre-platform` 检查来源；普通工具库的重复实例不直接判为失败，重点检查共享类型边界。

主平台 macOS 与 Linux 的人工验收范围：

| 范围 | 验收内容 |
|---|---|
| 绘制与字体 | 字体及图标资源、中文/宽字符、组合字符、emoji、块字符、光标、选区、链接下划线；缩放和窗口 resize 后网格及最后一行正常。 |
| 输入与快捷键 | 中文 IME 预编辑和候选位置、连续输入、Tab、Ctrl-C、复制粘贴、用户覆盖、多键序列、设置输入框、搜索及标签重命名。 |
| 窗口与焦点 | 新建/关闭/重开、最小化、全屏、跨显示器、文件管理器启动、自定义数据目录、重启参数、跨窗口移动带分屏的标签页。 |
| 终端与任务 | PTY 交互、持续输出、滚动回看、进程退出、搜索刷新、后台任务和定时行为。 |
| 网络与文件传输 | 主题目录加载、安装/更新/卸载、失败反馈；trzsz/ZMODEM 现有互操作测试及文件选择流程。 |
| Linux 后端 | 分别记录 X11 与 Wayland 的输入法、剪贴板、窗口装饰、焦点、缩放、持续输出和空闲重绘行为。 |
| 发布包 | 使用现有脚本生成 deb 和两种架构的 macOS DMG，安装后检查字体、图标、着色器、启动与系统集成。 |

macOS 部署目标以当前项目配置为基线，验证外部包的最低系统要求；不要在未说明的情况下提高最低版本。
Windows 和 Web 本轮不新增支持承诺；对实际维护、启用或受 CI 覆盖的目标，记录迁移影响和验证结果。
未能执行的平台项目应保留为待验收项，不能记为通过。

出口：自动检查通过，macOS/Linux 及发布包的必要验收有记录；剩余限制有明确说明。

### 阶段四：清理与文档更新

1. 删除已确认无引用的本地 GPUI 核心、宏和平台后端副本，保留仍被使用的支撑 crate。
2. 更新 workspace 清单、构建 profile、相关脚本及 README 中的项目结构和构建说明。
3. 按新增依赖和发布包检查许可证及归属信息。
4. 在本方案中记录最终快照版本、适配点、验证平台和未解决项。

出口：仓库只保留确有使用方的本地基础设施，依赖与文档一致，发布构建使用已提交的 lockfile。

## 5. 风险、拆分与回退

- 快照可能改变 API 和交互行为。通过精确版本、lockfile、应用测试及平台验收控制变化，不能只依赖版本号判断兼容。
- 绘制、IME、快捷键和平台事件循环的变化可能无法通过纯编译发现，必须保留人工验收。
- 外部化减少修改底层框架的便利。遇到阻塞缺陷时，先评估应用层适配；确需框架修复再单独决定发布版本或补丁来源。
- 自动化测试、手工验收和性能结果分别记录。使用相同环境比较冷构建、增量构建、产物体积及输出/空闲场景，不承诺预设收益。

建议将依赖替换和必要适配作为一个可独立回退的逻辑单元，验证通过后再提交源码清理。
回退时同时恢复对应 manifest、lockfile、profile 和适配代码，避免新旧类型混用；不回退或迁移用户配置及数据。

## 6. 参考资料

- [Zed 分叉提交](https://github.com/zed-industries/zed/commit/5631830c564afa89b3aba679f45d9c3345f9460f)
- [首个 ZedTerm 提交](https://github.com/ruanimal/zed-term/commit/12519e6133dc32967673b66cf4b0919e04d8abce)
- [gpui-pre 版本索引](https://index.crates.io/gp/ui/gpui-pre)
- [gpui-pre 0.3.0 发布包源码](https://docs.rs/crate/gpui-pre/0.3.0/source/)
- [gpui-pre 0.3.1 发布包源码](https://docs.rs/crate/gpui-pre/0.3.1/source/)
- [gpui-pre-platform 0.3.1 发布包源码](https://docs.rs/crate/gpui-pre-platform/0.3.1/source/)
- [GPUI Kit 0.8.0 的快照配置](https://github.com/longbridge/gpui-kit/blob/v0.8.0/Cargo.toml#L58-L76)：仅作此前分析的版本背景。
- [gpui-pre 0.3.9 发布包源码](https://docs.rs/crate/gpui-pre/0.3.9/source/)
- [gpui-pre-platform 0.3.9 发布包源码](https://docs.rs/crate/gpui-pre-platform/0.3.9/source/)
- [GPUI Kit 窗口入口实现](https://github.com/longbridge/gpui-kit/blob/v0.8.0/crates/kit/src/lib.rs#L165-L185)：作为边界说明，本方案不采用其 Base Root 包装。
