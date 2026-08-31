# ZedTerm TODO

> 本文档独立记录后续需求与想法，不代表实现承诺或优先级。

## 设置页扩展

- 增加 shell、env、font_family 文本输入控件。
- 补充 font_fallbacks、font_features、font_weight、line_height、working_directory、minimum_contrast、path_hyperlink_regexes、scrollbar.show。
- 增加搜索/过滤和恢复默认值入口。
- 增加写入成功/失败状态及非法值校验。

## Pane 方向性跳转

当前已有按叶子视觉序循环的 ActivateNextPane/ActivatePreviousPane。可增加 ActivatePaneInDirection，根据 pane 几何位置选择上、下、左、右方向的最近邻。

建议验收：嵌套横向/纵向 split 中，四向跳转均选择视觉上最近且方向正确的 pane；边缘无候选时保持当前焦点。

## Tab 溢出可见指示

当前 tab 栏支持横向滚动，激活 tab 时也会自动滚回可视区。可增加类似 Zed 的边缘提示，例如用 2px 边框表示一侧仍有被裁剪的 tab。

建议验收：提示随滚动位置实时出现或消失，不遮挡 tab 内容，也不影响点击、拖拽排序和窗口拖动。

## 跨窗口 Tab 拖拽

当前 tab 拖拽排序仅限同一窗口。可支持把完整 `WindowTab` 在 terminal-app 窗口之间移动，同时保留 terminal entity、split 树、zoom、bell 与焦点状态。

建议验收：源窗口和目标窗口状态一致；移动最后一个 tab 时窗口关闭语义正确；拖拽期间 pane 退出或 tab 关闭时安全取消。
