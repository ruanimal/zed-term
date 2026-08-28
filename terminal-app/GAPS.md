# ZedTerm 缺口清单（GAPS）

> 定位：本文件是**唯一切进度依据的活文档**。PLAN.md（terminal-app/PLAN.md）保留为决策记录与架构事实，不再用于宣告完成状态。
> 背景：WP0–WP5 按 PLAN 执行完毕且测试全绿，但实际使用发现"获得 Zed 终端体验"的目标仍有明显距离——差距集中在应用骨架交互层（原 workspace pane/editor 生态被剥离后未补齐的部分）。此后的工作从"工作包驱动"改为"缺口驱动"：试用中发现的每一条差距记入此处，按痛感排序消除。
>
> 用户已确认的主要痛点方向：**应用骨架交互**。

## P0 — 骨架级缺失

### G1. Split panes 分屏
（方案草稿见上）**已完成（2026-08-28）**：
- 新文件 `terminal/split.rs`：`SplitNode` 树（Leaf 持 `Entity<TerminalTab>` / Axis 持 axis+flexes+children），`split`/`remove`（含单子节点 collapse）语义对齐 Zed `PaneAxis`（pane_group.rs:686/733）；`collect_tabs` 扁平化叶子供焦点循环与渲染。
- window.rs：`split_root`（None=单窗格 tab 布局）+ `active_pane_tab`（焦点 pane 的 tab）；SplitRight/Left/Up/Down 四向 action（cmd-d、ctrl-alt-四向），首次 split 自动把单窗格布局提升为 split 树；新终端经 `build_terminal_in` 异步进入新叶子；ActivateNext/PreviousPane（cmd-{ / cmd-}、cmd-alt-left/right）按叶子视觉序循环并同步 tab 栏选中。
- divider 拖拽：divider 元素 mouse_down latch（thread_local anchor+axis），窗口级 on_mouse_move 每帧调 `resize_flexes`（相邻 flex 对守恒分配、MIN_FLEX=0.05 兜底），mouse_up 清 anchor——解决 divider 局部 on_mouse_move 移出 6px 命中区丢事件的问题（Zed 同样在 element 层做窗口级拖拽）。
- 渲染：`SplitNode::render` 递归 flex_row/col，flex_grow 权重布局，divider 1px 主题色 border；叶子渲染复用现有 `TerminalElement`。
- ClosePane 已实现（关 pane 收缩树 + 同步 tab 栏；最后一个叶子保留 tab 栏语义）。zoom（pane 独占内容区）留待后续，见下方备注。
- 验证：cargo check/test/clippy（terminal_app 26 通过、terminal_core 99 通过、clippy --deny warnings 全绿）。
- 待用户真机验收：cmd-d 分屏、拖 divider、cmd-} 循环焦点、关 pane 收缩。
- 备注：zoom split（激活 pane 独占）与方向性跳转（ActivatePaneInDirection 几何相邻）未做，属增量项，若用户需要再补录。
- 方案草稿（2026-08-28）：
  - **数据结构**（新文件 `terminal/split.rs`）：`enum SplitNode { Leaf { tab: Entity<TerminalTab> }, Axis { axis: Axis, flexes: Vec<f32>, children: Vec<SplitNode> } }`，窗口持有 `root: SplitNode` + `active_leaf_path`。语义对齐 Zed `Member`/`PaneAxis`（pane_group.rs:296/648）但砍掉 workspace 依赖：无 bounding_boxes 缓存、无持久化。
  - **split 语义**：沿 Zed `PaneAxis::split`（pane_group.rs:686）——找到目标 Leaf；若其父 Axis 与 split 方向同轴则插入兄弟 Leaf（flexes 重置为 1），否则把 Leaf 原位替换为新的二元 Axis。新 Leaf 新建 terminal（走既有 `spawn_new_terminal` 的 builder 路径）并持有焦点。
  - **渲染**：递归 `SplitNode::render` 产出 flex 容器（Horizontal→`flex_row`，Vertical→`flex_col`），子节点按 `flexes[i]` 设 `flex_grow`；divider 是 1px 可拖拽元素，拖拽时把像素位移换算为相邻两个 flex 的增减（同 Zed compute_resize 思路但简化为只动相邻一对）。叶子 = 现有 `TerminalElement` 渲染路径。
  - **焦点**：`ActivateNextPane/ActivatePreviousPane`（cmd-k left/right 之外的 Zed 绑定是 cmd-shift-[ / cmd-shift-]，见 keymap）按叶子扁平序循环；`ActivatePaneInDirection` 用几何相邻算法（叶子的 bounding box 沿方向投影最近者）——首轮可只做 Next/Prev，方向跳转后补。
  - **关闭**：`CloseActivePane` 移除叶子后向上收缩（父 Axis 只剩一个孩子时用孩子替换 Axis，对齐 Zed `remove` 的 collapse 逻辑）；最后一个叶子 = 关窗口（复用 close_tab 语义）。
  - **zoom**：`zoom_window` 已有双击最大化先例；分屏内的 zoom（激活 pane 独占内容区）用 `maximized_leaf: Option<()>` 标记 + render 时只渲染该叶子实现。
  - **keymap**（对齐 default-macos.json Terminal context）：cmd-d→SplitRight、ctrl-alt-up/down/left/right→四向 Split、cmd-shift-[ / cmd-shift-]→前后 pane、cmd-w 语义不变（关 tab→关 pane）。
  - **验收**：cmd-d 右分出新终端、divider 可拖、cmd-shift-[/] 循环焦点、关 split 后布局收缩、cmd-w 关窗前最后一个 pane 行为正确。

### G2. macOS 键盘手感件（SendKeystroke / SendText 组）
Zed 的 Terminal keymap 中有一组非动作类绑定，靠 `SendKeystroke`/`SendText` 把组合键翻译成 shell 快捷键：
- `cmd-backspace` → ctrl-u（清整行）
- `cmd-delete` → ctrl-k（删至行尾）
- `cmd-right/left` → ctrl-e/ctrl-a（行尾/行首）
- `alt-delete` → ESC d、`alt-b/alt-f/alt-left/right` → ESC b/f（词跳转）
- `ctrl-delete` → ESC [3;5~、`ctrl-backspace` → ctrl-w
- 显式拦截 up/down/pageup/pagedown/escape/enter/ctrl-c/ctrl-r 保证正确透传
现状：只有裸 `on_key_down` → `try_keystroke` 透传（commit 9d27364792），无这一组映射，且内核 `try_keystroke` 是否覆盖全部场景未系统核对。
- 来源：同上 keymap 对照；是"键不对劲"感受的直接来源之一。
- **已完成（2026-08-28）**：app.rs 定义带参 action `SendText(String)`/`SendKeystroke(String)`（`#[action(namespace = terminal_app)]`，与 Zed terminal_view 同名同语义），window.rs 各接 handler（SendText → `Terminal::input`；SendKeystroke → parse 后走 `Terminal::try_keystroke`，解析失败记日志忽略，同 Zed）；keymap 按 default-macos.json Terminal context 补齐 10 条：cmd-backspace→ctrl-u、cmd-delete→ctrl-k、cmd-right/left→ctrl-e/a、ctrl-backspace→ctrl-w、alt-delete→ESC d、alt-left/right→ESC b/f、alt-b/f→ESC b/f、ctrl-delete→ESC[3;5~。Zed 的显式拦截组（up/down/pageup/pagedown/escape/enter/ctrl-c/ctrl-r）无需照抄——ZedTerm 无 keymap 消费这些键，裸 on_key_down 透传路径行为与 SendKeystroke 一致。注意 ctrl-r 未被占用（ZedTerm 的 SearchTest 绑定 cmd-f，不冲突）。

## P1 — 明确缺席的功能

### G3. ScrollHalfPageUp / ScrollHalfPageDown 未绑定
内核已有动作（terminal.rs actions!），Zed 默认绑定为 cmd-shift-up/down（核实确切默认键位）；ZedTerm app 层完全未接线。
- 核实结果（2026-08-28）：GAPS 原记录有误——**Zed 上游（origin/main 全仓检索）ScrollHalfPageUp/Down 仅在 actions! 中声明，既无 handler 也无任何 keymap 绑定**（assets/keymaps/*.json 零命中），是上游半成品，无"默认键位"可对照。
- 方案（已采用）：terminal_core 已提供 `scroll_up_by/down_by(lines)` + `viewport_lines()`，app 层组合即可；alacritty grid 对 Delta 在顶端/底端 clamp、alt screen 下 history 为 0 时自然归零（grid/mod.rs scroll_display），无需额外模式判断。
- **已完成（2026-08-28）**：tab.rs ScrollAction 增加 HalfPageUp/HalfPageDown（取 `viewport_lines()/2`，min 1 行）；window.rs 接 terminal_core::ScrollHalfPageUp/Down handler；keymap 采用用户惯用 `cmd-shift-up/down` 绑定（Zed 上游未绑定，此为 ZedTerm 自定默认值）。
### G4. Reopen Closed Tab
Zed: `cmd-shift-t`（pane::ReopenClosedItem，仅编辑器 tab）。终端场景同样高频（误关恢复）。需要闭_tab 时暂存 TerminalBuilder 所需信息（cwd/shell）而非 Entity 本身。
- **已完成（2026-08-28）**：close_tab 时把该 tab 的 `Terminal::working_directory()` 压入 `closed_tab_cwds` 栈（上限 10 条，GAPS 方案里"暂存 TerminalBuilder 信息"落地为只存 cwd——shell/env 走 settings 即时值，重开时语义更正确）；`ReopenClosedTab` action（cmd-shift-t，对齐 Zed）+ tab 右键菜单 "Reopen Closed Tab" 项，`build_terminal_in(Some(cwd))` 以原 cwd 起新 shell。验证：cargo check/test/clippy 通过。待真机验收（cmd-shift-t 恢复 + cwd 正确）。
### G5. tab 溢出行为
多标签超出宽度时的表现未验证：原版 ui::TabBar 自带 `overflow_x_scroll()`（tab_bar.rs:133），需确认我们的 TabBar 用法下生效且有可见的滚动指示；否则改为允许横向滚动的容器。
- 核实结果（2026-08-28）：TabBar 内部 tab 容器固定 `overflow_x_scroll()`（tab_bar.rs:139），且滚动与否与是否传 handle 无关——无 handle 时 gpui 走 element_state 内部 offset（div.rs:2169）。故溢出时**滚轮横滚一直可用**，缺的是"激活 tab 自动滚回可视区"（Zed pane.rs:1512 `scroll_to_item`）。
- **已完成（2026-08-28）**：window.rs 增加 `tab_bar_scroll_handle: ScrollHandle`，TabBar `track_scroll` 接线；新增 `activate_tab(index)` 统一激活入口（tab 点击/右键/next/previous tab 均走它），激活即 `scroll_to_item(index)` 保证当前 tab 滚回可视区，对齐 Zed Pane 行为。未加"滚动指示边框"（Zed 用 2px 右边框提示左侧有被裁剪的 tab，成本高收益低，若用户仍觉不明显再补录）。

## P2 — 观感与细节

### G11. 窗口边框/标题栏自绘：tab 栏即标题栏（对齐 Zed，用户已确认形态）
Zed 的做法：系统标题栏透明化（`appears_transparent: true` + `traffic_light_position: point(px(9), px(9))`），**不存在独立的标题栏行**——tab 栏自身占据标题栏位置，红绿灯悬浮在 tab 栏左端同一水平线；窗口拖拽由内容区处理（`app_owns_titlebar_drag: true` + 空白处 `window.start_window_move()`）。用户确认 ZedTerm 采用同一形态。
现状：terminal_app 的 `window_options`（app.rs:46）为 `titlebar: None` + `WindowDecorations::Server`——系统原生标题栏独占一行，tab 栏在其下方，观感割裂。
- 复用性核实（2026-08-27）：`title_bar` crate 硬依赖 workspace/project/client/call，`platform_title_bar` 依赖 workspace——均不可复用；所需机制全部为公开 API：`ui::utils::platform_title_bar_height()`、`TRAFFIC_LIGHT_PADDING`、`gpui::Window::start_window_move`，自装零新依赖。
- 方案：`window_options` 改为 `titlebar: Some(appears_transparent + traffic_light_position)` + `app_owns_titlebar_drag: true`（Linux 走 `WindowDecorations::Client`）；TabBar 行高对齐 `platform_title_bar_height(window)`，左侧预留 `TRAFFIC_LIGHT_PADDING`；tab 空白区域/TabBar 背景按下即 `start_window_move()`（仅空白处，避免吞掉 tab 点击）。
- 验收：红绿灯与 tab 同行悬浮、按住 tab 栏空白可拖动窗口、双击空白可最大化、tab 点击/右键菜单不受影响。
- **已完成（2026-08-27，commit 69497f798e）**：window_options 透明标题栏 + `render_title_bar` 接线（mouse_down 置 flag → mouse_move 触发 `start_window_move`，仿 Zed PlatformTitleBar 模式）；tab/加号/设置按钮 mouse_down `stop_propagation` 隔离拖拽；双击 `zoom_window`。待用户视觉验收（红绿灯位置/拖拽/双击最大化）。

### G12. 设置页精细化
当前 settings_ui.rs 为应急实现：5 个终端项的自绘行控件（stepper/cycle/toggle），覆盖 font_size/cursor_shape/blinking/option_as_meta/copy_on_select。用户反馈过于粗糙。差距：
- 覆盖面：font_family/font_fallbacks/font_features/font_weight/line_height/env/working_directory/scroll_multiplier/max_scroll_history_lines/bell/minimum_contrast/path_hyperlink_regexes/scrollbar.show/alternate_scroll 均未收录
- 交互形态：文本输入类设置（shell、env、字体族）无输入控件；分组与节标题缺失；无搜索/过滤；无恢复默认值入口
- 反馈：写入成功/失败无 toast 或状态提示；非法值校验缺失
- 方案约束：不引入 settings_ui crate 的表驱动体系（依赖面大，PLAN §4.3 已决策），在现有自绘路线上补齐控件类型（text input 用 ui::TextInput）与布局层级。
- 注：原 G10（设置页覆盖面）已并入本条，G10 撤销。

### G13. Zed 主题支持（复用 Zed 主题，用户提出）
现状核实（2026-08-27）：**终端配色已经主题化**——内核 `get_color_at_index` 读 `theme.colors().terminal_ansi_*`，`terminal_element.rs` 已用 `cx.theme()` 取前景/背景/搜索高亮色，且 `theme_settings::init`（app.rs:151）已注册 One Dark/Light 内置主题并读取 settings.json 的 `theme` 节。缺的只是：用户主题加载、主题切换入口、UI 硬编码色收尾。
- 待做 1：加载用户主题——照抄 Zed `load_user_themes_in_background`（zed/main.rs:1904）模式：扫 `paths::themes_dir()` + `theme_settings::load_user_theme`（pub，theme_settings.rs:244）。注意 themes_dir 仍指 Zed 目录（APP_NAME 未分家），客观上直接复用已装 Zed 主题。
- 待做 2：内置主题集补全——目前只有 One 家族 fallback；Zed 其余内置主题在 `assets/themes/`（one/ayu/gruvbox 家族），启动时批量注册。
- 待做 3：UI 硬编码色清零——window.rs 仅 2 处 `rgb(0x14151a)` 改 `cx.theme().colors().tab_bar_background`。
- 待做 4：设置页加主题选择（跟随系统/light/dark + 主题列表），写入走既有 update_settings_file。
- 成本评估：低（半天内）；格式原生兼容 Zed 主题 JSON（ThemeFamilyContent serde）。
- 与 G11 绑定为"观感包"：G11 完成后 tab 栏融入主题色才完整，两处动同一小片代码。
- **已完成（2026-08-27，commit 69497f798e）**：`load_embedded_themes`（asset source 内 One/ayu/gruvbox 家族）+ `load_user_themes_in_background`（扫 `paths::themes_dir()`，与 Zed 共享）；硬编码色已清零（tab 栏→`tab_bar_background`、窗口底→`terminal_background`）。**待做（归 G12）**：设置页主题选择入口；`theme` 节 settings.json 已原生支持手改即时生效。

### G6. 字体渲染瑕疵
用户早期反馈"字体渲染有小瑕疵"，现象至今未复现确认。待定位（可能方向：font fallback 链、连字开关、line_height 取整）。
- 已修复相关项（2026-08-28，commit 6aa63569b7）：**tab 标题闪烁**——用户报告"输入命令时标题闪一下（如 ls -al）"。根因：`title()` 读前台进程信息，敲回车时 PTY 前台进程组瞬间从 shell 切给命令（pty-fork 实验实测 ~40ms 后切换），每次 Wakeup 采样都把瞬时快照提交，标题两连跳（`zed — zsh` → `zed — ls -al --color=auto` → `zed — zsh`）。修复：`pty_info.rs` 增加稳定确认——前台 pid 变化时延迟 750ms 复采，期间命令退出则跳过该快照；长驻程序（vim/top/ssh）标题照常更新，仅延迟 750ms。注意：这属于标题稳定性问题，与 G6 原指的"字体渲染"是两回事，G6 字体问题仍待复现。
- 已修复相关项（2026-08-28）：**tab 宽度不固定放大闪烁感知**——用户指出 tab 宽度随标题长度伸缩，即使标题微变整条 tab 栏也会抖动。修复（window.rs）：tab 内容区固定 `TAB_TITLE_WIDTH`(140px)，标题字符串先 `truncate_and_trailoff`（24 字符，对齐编辑器 `MAX_TAB_TITLE_LEN`），再经 `Label.single_line().truncate()` 布局级 ellipsis 兜底；标题变化只换文字不动宽度。
### G7. bell 无声音/视觉提示
内核有 bell 事件路径，但 app 层无任何呈现（响铃或标题栏闪动）。
### G8. hover tooltip 路径预览
悬停超链接/路径时无 tooltip 预览（PLAN §4.4 遗留项收录于此）。
### G9. 标签页拖拽排序 / pinned
依赖自绘 drag&drop（ui::TabBar 无现成实现，grep证实零 drag 支持），排 G9 因成本高收益一般。
（G10 已并入 G12，撤销。）

---

## 工作方式备忘

- 每条 gap 修完在此勾销并附一句实证（命令/现象对照）。
- 新差距按"用的时候卡住"随时补录，宁可碎勿漏。
- 大项（G1）动手前先在本文档里补一小节方案草稿再实施。

## 实施顺序（2026-08-27 对齐，待用户确认）

```
第 1 批 观感包      G11 标题栏（收尾半成品）→ G13 主题支持
第 2 批 手感包      G2 键盘手感件 → G3 半页滚动 → G5 tab 溢出验证
第 3 批 骨架大件    G1 分屏 → G4 Reopen Closed Tab
第 4 批 收尾打磨    G12 设置页精细化（含主题选择入口）→ G6 字体瑕疵定位 → G8 hover tooltip
暂缓/待议           G7 bell、G9 拖拽排序/pinned（默认不做，用户可捞回）
```

排序理由：
1. G11 已动工一半且是全局观感地基（tab 栏几何确定后，主题色/分屏 divider 才有稳定坐标）
2. G13 紧随 G11，动同一小片代码，合并验证省一轮
3. G2/G3/G5 量级小、痛感密集（"键不对劲"的直接来源），在大件前清掉
4. G1 分屏量级半个 WP3，放在地基（标题栏几何）与手感（快捷键语义）都稳定后
5. G12 依赖前面定型的 UI 骨架（主题下拉、分屏相关设置项才好布局）
