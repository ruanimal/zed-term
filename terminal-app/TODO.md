# ZedTerm TODO

> 本文档独立记录后续需求与想法，不代表实现承诺或优先级。

## 设置页扩展

### 待实现

- 修复现有 `cursor blinking` 设置，为终端光标接入实际闪烁状态。
- 增加 `font_family`、`font_weight`、`line_height`、`minimum_contrast` 设置控件。
- 增加 `keep_selection_on_copy`、`open_links_in_mouse_mode` 开关。
- 增加结构化 `shell` 设置，支持系统 shell、自定义程序及参数，不使用单一文本输入框。
- 增加 `env` 键值编辑 UI，并将配置接入 PTY builder，使其对新终端生效。
- 增加 standalone `working_directory` 设置，仅支持 Home 和固定目录。
- 实现终端滚动条，并接入 `scrollbar.show` 设置。
- 核对并统一 standalone 设置默认值；参考 Zed 时以 `assets/settings/default.json` 的实际配置为准，重点确认 `line_height = "standard"`、`bell = "off"` 和 `alternate_scroll = "on"`，不采用可能过期的 Rust 文档默认值。
- 增加搜索/过滤和恢复默认值入口。
- 增加写入成功/失败状态及非法值校验。

### 低优先级

- 增加 `font_fallbacks` 设置，允许配置缺失字符的备用字体及其优先顺序。
- 增加 `font_features` 设置，允许配置 ligatures、字符变体等 OpenType 特性。

### 明确不支持

- 不支持 `default_width` 和 `default_height` 默认窗口尺寸设置。
- 不支持 `path_hyperlink_regexes` 自定义路径 hyperlink 正则。

## Pane 方向性跳转

当前已有按叶子视觉序循环的 ActivateNextPane/ActivatePreviousPane。可增加 ActivatePaneInDirection，根据 pane 几何位置选择上、下、左、右方向的最近邻。

建议验收：嵌套横向/纵向 split 中，四向跳转均选择视觉上最近且方向正确的 pane；边缘无候选时保持当前焦点。

## Tab 溢出可见指示

当前 tab 栏支持横向滚动，激活 tab 时也会自动滚回可视区。可增加类似 Zed 的边缘提示，例如用 2px 边框表示一侧仍有被裁剪的 tab。

建议验收：提示随滚动位置实时出现或消失，不遮挡 tab 内容，也不影响点击、拖拽排序和窗口拖动。

## 跨窗口 Tab 拖拽

当前 tab 拖拽排序仅限同一窗口。可支持把完整 `WindowTab` 在 terminal-app 窗口之间移动，同时保留 terminal entity、split 树、zoom、bell 与焦点状态。

建议验收：源窗口和目标窗口状态一致；移动最后一个 tab 时窗口关闭语义正确；拖拽期间 pane 退出或 tab 关闭时安全取消。
