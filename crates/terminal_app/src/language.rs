use std::sync::OnceLock;

use gpui::App;
use settings::{DisplayLanguage, SettingsFile, SettingsStore};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Locale {
    #[default]
    English,
    SimplifiedChinese,
}

impl Locale {
    pub(crate) fn is_simplified_chinese(self) -> bool {
        self == Self::SimplifiedChinese
    }

    pub(crate) fn text(self, english: &str) -> &str {
        if self.is_simplified_chinese() {
            simplified_chinese(english)
        } else {
            english
        }
    }

    fn from_preferred_languages(languages: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        languages
            .into_iter()
            .find_map(|language| {
                let language = language.as_ref().to_ascii_lowercase();
                let mut subtags = language.split('-');
                match subtags.next()? {
                    "en" => Some(Self::English),
                    "zh" => match subtags.next() {
                        Some("hant" | "hk" | "mo" | "tw") => None,
                        _ => Some(Self::SimplifiedChinese),
                    },
                    _ => None,
                }
            })
            .unwrap_or_default()
    }
}

pub(crate) fn configured(cx: &App) -> DisplayLanguage {
    cx.try_global::<SettingsStore>()
        .and_then(|store| {
            // The language picker saves to the root user settings, even when a profile or OS override is active.
            store
                .get_content_for_file(SettingsFile::User)
                .and_then(|content| content.display_language)
                .or(store.merged_settings().display_language)
        })
        .unwrap_or_default()
}

pub(crate) fn current(cx: &App) -> Locale {
    match configured(cx) {
        DisplayLanguage::Auto => {
            static SYSTEM_LOCALE: OnceLock<Locale> = OnceLock::new();
            *SYSTEM_LOCALE
                .get_or_init(|| Locale::from_preferred_languages(sys_locale::get_locales()))
        }
        DisplayLanguage::English => Locale::English,
        DisplayLanguage::SimplifiedChinese => Locale::SimplifiedChinese,
    }
}

pub(crate) fn text<'a>(cx: &App, english: &'a str) -> &'a str {
    current(cx).text(english)
}

macro_rules! localized_format {
    ($locale:expr, $english:literal, $chinese:literal $(, $argument:expr)* $(,)?) => {
        if $locale.is_simplified_chinese() {
            format!($chinese $(, $argument)*)
        } else {
            format!($english $(, $argument)*)
        }
    };
}

pub(crate) use localized_format;

pub(crate) fn simplified_chinese(english: &str) -> &str {
    match english {
        "10,000 (default)" => "10,000（默认）",
        "400 (default)" => "400（默认）",
        "<global>" => "<全局>",
        "A custom shell program cannot contain NUL." => "自定义 Shell 程序不能包含 NUL 字符。",
        "A custom shell program is required." => "请输入自定义 Shell 程序。",
        "A fixed working directory cannot contain NUL." => "固定工作目录不能包含 NUL 字符。",
        "A fixed working directory is required." => "请输入固定工作目录。",
        "A fixed working directory must be absolute or begin with `~/`." => {
            "固定工作目录必须为绝对路径，或以 `~/` 开头。"
        }
        "A positive line-height multiplier." => "大于零的行高倍数。",
        "A shell program cannot contain NUL." => "Shell 程序不能包含 NUL 字符。",
        "A shell program is required." => "请输入 Shell 程序。",
        "A standalone terminal application based on Zed's terminal." => "基于 Zed 终端的独立应用。",
        "About" => "关于",
        "Activate Next Pane" => "切到下一个窗格",
        "Activate Previous Pane" => "切到上一个窗格",
        "Add argument" => "添加参数",
        "Add keybinding" => "添加快捷键",
        "Add variable" => "添加变量",
        "Allow Command-click links while an application receives mouse input." => {
            "当终端程序接管鼠标输入时，仍可按住 Command 点击链接。"
        }
        "Already transferring — ignoring new transfer request" => "正在传输，已忽略新的传输请求",
        "Alternate scroll" => "备用屏幕滚动",
        "always" => "始终显示",
        "An existing directory. `~` is expanded before it is saved." => {
            "必须是已有的目录。`~` 会在保存前展开。"
        }
        "APCA minimum contrast from 0 to 106." => "APCA 最低对比度，范围为 0 到 106。",
        "Appearance" => "外观",
        "Argument" => "参数",
        "Ask where each download should be saved. When disabled, the configured directory is used without prompting." => {
            "每次下载时询问保存位置。关闭后，直接使用已配置的目录。"
        }
        "Auto" => "自动（跟随系统）",
        "auto" => "自动",
        "Automatically copy selected text to the clipboard." => "自动将选中的文字复制到剪贴板。",
        "bar" => "竖线",
        "base" => "基础",
        "Behavior" => "行为",
        "Bell" => "终端响铃",
        "block" => "实心方块",
        "Cancel" => "取消",
        "Cancel transfer" => "取消传输",
        "Change keybinding" => "更改快捷键",
        "Choose an installed font, or use the application default." => {
            "选择已安装的字体，或使用应用默认字体。"
        }
        "Choose the color theme for the terminal and UI." => "选择终端和界面的配色主题。",
        "Auto follows your system language. Save to apply the change." => {
            "自动模式跟随系统语言，保存后立即生效。"
        }
        "Choose the system shell or a structured program and argument list. Applies only to newly created terminals and panes." => {
            "选择系统 Shell，或指定程序和参数。仅对新建的终端和窗格生效。"
        }
        "Choose when the terminal scrollbar is visible." => "选择何时显示终端滚动条。",
        "Clear" => "清屏",
        "Clear Keystrokes" => "清除录入的按键",
        "clear to start of line" => "清除至行首",
        "Clears the terminal screen." => "清空终端屏幕。",
        "Click the ▶ button (or press Enter) to record, then press Escape three times to finish." => {
            "点击 ▶ 或按 Enter 开始录制，连按三次 Esc 结束录制。"
        }
        "Close" => "关闭",
        "Close All" => "全部关闭",
        "Close Left" => "关闭左侧标签页",
        "Close Other Tabs" => "关闭其他标签页",
        "Close Others" => "关闭其他标签页",
        "Close Pane" => "关闭窗格",
        "Close Right" => "关闭右侧标签页",
        "Close Search (escape)" => "关闭搜索（Esc）",
        "Close Tab" => "关闭标签页",
        "Close Terminal Tab" => "关闭终端标签页",
        "Closes the focused pane; the last pane closes the window." => {
            "关闭当前窗格。如果这是最后一个窗格，则关闭窗口。"
        }
        "Color theme" => "配色主题",
        "comfortable" => "宽松",
        "Confirm download location" => "确认下载位置",
        "Context" => "上下文",
        "Controls the actual cursor blink animation for each terminal pane." => {
            "设置各终端窗格中的光标是否闪烁。"
        }
        "Convert scroll events to key presses in the alternate screen." => {
            "在备用屏幕中将滚轮操作转换为按键。"
        }
        "Copies selected text to the clipboard." => "将选中的文字复制到剪贴板。",
        "Copies the link under the terminal context menu's right-click." => {
            "复制在终端中右键点击的链接。"
        }
        "Copy" => "复制",
        "Copy Link" => "复制链接",
        "Copy on select" => "选中即复制",
        "Could not restore keybinding" => "无法恢复快捷键",
        "Could not start another settings write." => "无法发起新的设置写入。",
        "Could not unbind keybinding" => "无法解除绑定",
        "CSS font weight from 100 to 900. The default uses the normal font weight." => {
            "CSS 字重，范围为 100 到 900，默认使用常规字重。"
        }
        "Cursor" => "光标",
        "Cursor blinking" => "光标闪烁",
        "Cursor shape" => "光标形状",
        "custom" => "自定义",
        "Custom line height" => "自定义行高",
        "custom program" => "自定义程序",
        "default" => "默认",
        "Default keybinding restored." => "已恢复默认快捷键。",
        "delete to end of line" => "删除至行尾",
        "delete word after cursor" => "删除光标后的单词",
        "delete word before cursor" => "删除光标前的单词",
        "Discard" => "放弃",
        "Discard changes" => "放弃更改",
        "Discard unsaved settings changes?" => "放弃未保存的设置更改吗？",
        "Dismiss" => "关闭提示",
        "Display language" => "界面语言",
        "Download directory" => "下载目录",
        "Downloading" => "正在下载",
        "e.g. /bin/zsh" => "例如 /bin/zsh",
        "e.g. /Users/name/Downloads" => "例如 /Users/name/Downloads",
        "Enter to rename · Esc to cancel" => "按 Enter 重命名，按 Esc 取消",
        "Environment variable names and values cannot contain NUL." => {
            "环境变量名和值不能包含 NUL 字符。"
        }
        "Environment variable names cannot be empty." => "环境变量名不能为空。",
        "Environment variable names cannot contain `=`." => "环境变量名不能包含 `=`。",
        "Environment variable names must be unique." => "环境变量名不能重复。",
        "Environment variables" => "环境变量",
        "Exit Full Screen" => "退出全屏",
        "File transfer" => "文件传输",
        "Filter by action or keystroke…" => "按操作或按键筛选…",
        "Filter theme extensions…" => "筛选主题扩展…",
        "fixed directory" => "固定目录",
        "Fixed working directory" => "固定工作目录",
        "Font family" => "字体",
        "Font size" => "字号",
        "Font weight" => "字重",
        "GitHub repository" => "GitHub 仓库",
        "Hit it three times to execute" => "连按三次执行",
        "hollow" => "空心方块",
        "home" => "主目录",
        "Input and scroll" => "输入与滚动",
        "Install" => "安装",
        "Installing…" => "正在安装…",
        "Invalid pattern" => "无效的搜索表达式",
        "Keep a terminal selection after it is copied manually." => "手动复制后保留终端内的选区。",
        "Keep selection on copy" => "复制后保留选区",
        "Keybinding saved." => "快捷键已保存。",
        "Keybinding unbound." => "已解除绑定。",
        "Keymap" => "快捷键",
        "Keymap reset to defaults." => "快捷键已恢复为默认设置。",
        "keymap.json value: " => "keymap.json 中的值：",
        "Keystrokes cannot be empty." => "请录入按键。",
        "License" => "许可证",
        "Line height" => "行高",
        "Loading theme extensions…" => "正在加载主题扩展…",
        "Max scrollback lines" => "最大回滚行数",
        "Maximize" => "最大化",
        "Minimize" => "最小化",
        "Minimum contrast" => "最低对比度",
        "move back one word" => "向左移动一个单词",
        "move forward one word" => "向右移动一个单词",
        "Move Tab to Window" => "将标签页移到窗口",
        "move to end of line" => "移至行尾",
        "move to start of line" => "移至行首",
        "Moves focus to the next pane (wrapping)." => "将焦点移到下一个窗格，到末尾后回到第一个。",
        "Moves focus to the previous pane (wrapping)." => {
            "将焦点移到上一个窗格，到开头后跳到最后一个。"
        }
        "NAME" => "变量名",
        "never" => "从不显示",
        "New tab" => "新建标签页",
        "New Tab" => "新建标签页",
        "New tabs inherit the active tab's working directory when available; otherwise they use Home." => {
            "新标签页优先继承当前标签页的工作目录，否则使用主目录。"
        }
        "New Terminal" => "新建终端标签页",
        "New window" => "新建窗口",
        "New Window" => "新建窗口",
        "Newly created terminals and panes use Home when an inherited workspace directory is configured." => {
            "如果配置为继承工作区目录，新建的终端和窗格会使用主目录。"
        }
        "Next Tab" => "下一个标签页",
        "No matches" => "没有匹配项",
        "No matching key bindings." => "没有匹配的快捷键。",
        "No settings changes to save." => "没有需要保存的设置更改。",
        "No theme extensions are available." => "暂无可用主题扩展。",
        "No theme extensions match this search." => "没有匹配的主题扩展。",
        "off" => "关闭",
        "on" => "开启",
        "Only newly created terminals and panes use the selected Home or verified fixed directory." => {
            "所选的主目录或已校验的固定目录仅对新建的终端和窗格生效。"
        }
        "Open directories in a new window or a new tab in the most recently active terminal window." => {
            "在新窗口或最近使用的终端窗口的新标签页中打开目录。"
        }
        "Open from file manager" => "从文件管理器打开",
        "Open Link" => "打开链接",
        "Open links in mouse mode" => "在鼠标模式下打开链接",
        "Open Settings" => "打开设置",
        "Opens the link under the terminal context menu's right-click." => {
            "打开在终端中右键点击的链接。"
        }
        "Option as meta" => "将 Option 用作 Meta",
        "Optional title" => "可选标题",
        "Optional title used for tabs started with this shell." => {
            "为使用此 Shell 启动的标签页指定标题，可留空。"
        }
        "Paste" => "粘贴",
        "Paste Text" => "粘贴纯文本",
        "Pastes from the clipboard." => "从剪贴板粘贴内容。",
        "Pastes the text from the clipboard." => "粘贴剪贴板中的纯文本。",
        "previous tab" => "上一个标签页",
        "Previous Tab" => "上一个标签页",
        "program with arguments" => "程序和参数",
        "REC" => "录制",
        "Reload the theme extension list" => "重新加载主题扩展列表",
        "Removing…" => "正在卸载…",
        "Rename" => "重命名",
        "Rename Tab" => "重命名标签页",
        "Repository" => "代码仓库",
        "Reset" => "重置",
        "Reset keymap.json to defaults" => "将 keymap.json 恢复为默认设置",
        "Reset Terminal Defaults" => "恢复终端默认设置",
        "Resetting keymap…" => "正在重置快捷键…",
        "Restore" => "还原",
        "Restore default keybinding" => "恢复默认快捷键",
        "Restore Panes" => "恢复窗格布局",
        "Restore terminal defaults in this draft. Click Save to persist the changes." => {
            "将当前草稿中的终端设置恢复为默认值。点击“保存”后生效。"
        }
        "Restoring keybinding…" => "正在恢复快捷键…",
        "Save" => "保存",
        "Saved" => "已保存",
        "Saving keybinding…" => "正在保存快捷键…",
        "Saving settings…" => "正在保存设置…",
        "Saving…" => "正在保存…",
        "Scroll Half Page Down" => "向下滚动半页",
        "Scroll Half Page Up" => "向上滚动半页",
        "Scroll Line Down" => "向下滚动一行",
        "Scroll Line Up" => "向上滚动一行",
        "Scroll multiplier" => "滚动倍数",
        "Scroll Page Down" => "向下滚动一页",
        "Scroll Page Up" => "向上滚动一页",
        "Scroll tabs left" => "向左滚动标签栏",
        "Scroll tabs right" => "向右滚动标签栏",
        "Scroll To Bottom" => "滚动到底部",
        "Scroll To Top" => "滚动到顶部",
        "Scrollbar" => "滚动条",
        "Scrolls down by half a page." => "向下滚动半页。",
        "Scrolls down by one line." => "向下滚动一行。",
        "Scrolls down by one page." => "向下滚动一页。",
        "Scrolls to the bottom of the terminal buffer." => "滚动到终端缓冲区底部。",
        "Scrolls to the top of the terminal buffer." => "滚动到终端缓冲区顶部。",
        "Scrolls up by half a page." => "向上滚动半页。",
        "Scrolls up by one line." => "向上滚动一行。",
        "Scrolls up by one page." => "向上滚动一页。",
        "Search" => "搜索",
        "SEARCH" => "搜索",
        "Search Test" => "搜索终端",
        "Searches for text in the terminal." => "在终端中搜索文本。",
        "Search…" => "搜索…",
        "Seconds before a finished transfer notification closes. Set to 0 to keep it open." => {
            "传输结束后通知保留的秒数。设为 0 可保持显示。"
        }
        "Select All" => "全选",
        "Select Next Match (enter)" => "下一个匹配项（Enter）",
        "Select Previous Match (shift-enter)" => "上一个匹配项（Shift+Enter）",
        "Selects all text in the terminal." => "选择终端中的全部文本。",
        "Send Keystroke" => "发送按键",
        "Send Keystroke replays a keystroke through the terminal, Send Text writes its value to the shell byte for byte." => {
            "“发送按键”通过终端重放一次按键。“发送文本”会将内容逐字节写入 Shell。"
        }
        "Send Text" => "发送文本",
        "Sends raw text (escape sequences included) directly to the PTY, mirroring\nZed's `terminal::SendText`." => {
            "直接向 PTY 发送原始文本，包括转义序列。行为与 Zed 的 `terminal::SendText` 一致。"
        }
        "Sends the given keystroke through the terminal's keystroke translation\n(`Terminal::try_keystroke`), so e.g. `cmd-backspace` can send `ctrl-u`,\nmirroring Zed's `terminal::SendKeystroke`." => {
            "按终端的按键映射发送指定按键。例如，`cmd-backspace` 可以发送 `ctrl-u`。"
        }
        "Settings" => "设置",
        "Settings are still being saved." => "设置仍在保存中。",
        "Settings saved." => "设置已保存。",
        "Settings saved. Additional changes remain." => "设置已保存，仍有其他更改未保存。",
        "Shell and environment" => "Shell 与环境",
        "Shell arguments cannot contain NUL." => "Shell 参数不能包含 NUL 字符。",
        "Shell mode" => "Shell 模式",
        "Shell program" => "Shell 程序",
        "Shell title override" => "自定义 Shell 标题",
        "Show Character Palette" => "显示字符检视器",
        "Show conflicting bindings only" => "仅显示冲突的快捷键",
        "Show installed extensions only" => "仅显示已安装的扩展",
        "Show user overrides only" => "仅显示用户自定义快捷键",
        "Shows the character palette for special characters." => {
            "打开系统字符检视器以输入特殊字符。"
        }
        "Split Down" => "向下拆分",
        "Split Left" => "向左拆分",
        "Split Right" => "向右拆分",
        "Split Up" => "向上拆分",
        "Splits the focused pane downward (new terminal)." => "在当前窗格下方新建终端。",
        "Splits the focused pane to the left (new terminal)." => "在当前窗格左侧新建终端。",
        "Splits the focused pane to the right (new terminal)." => "在当前窗格右侧新建终端。",
        "Splits the focused pane upward (new terminal)." => "在当前窗格上方新建终端。",
        "standard" => "标准",
        "Standard is the standalone default and works best with terminal UIs." => {
            "默认使用标准行高，适合大多数终端界面。"
        }
        "Start Recording" => "开始录制",
        "Start Searching" => "开始搜索",
        "Starting terminal…" => "正在启动终端…",
        "Stop Recording" => "停止录制",
        "Stop Searching" => "停止搜索",
        "system" => "系统",
        "Tab name" => "标签页名称",
        "Terminal" => "终端",
        "terminal-controlled" => "由终端控制",
        "The executable to run for each new terminal." => "新建终端时运行的可执行文件。",
        "The fixed working directory must be a directory." => "指定的固定工作路径必须为目录。",
        "The maximum number of lines to keep in scrollback for newly created terminals and panes." => {
            "新建终端和窗格可保留的最大历史行数。"
        }
        "The multiplier for scrolling with the mouse wheel." => "鼠标滚轮的滚动倍数。",
        "The platform home directory is unavailable." => "无法获取系统主目录。",
        "The settings update was canceled." => "设置更新已取消。",
        "The shape of the terminal cursor." => "选择终端光标的形状。",
        "The size of the terminal font. The default is the application terminal font size." => {
            "设置终端的字号，默认使用应用的终端字号。"
        }
        "The title override cannot contain NUL." => "自定义标题不能包含 NUL 字符。",
        "The version of ZedTerm currently running." => "当前运行的 ZedTerm 版本。",
        "Themes" => "主题",
        "This binding conflicts with another binding." => "此快捷键与另一项快捷键冲突。",
        "This binding takes precedence" => "此快捷键优先生效",
        "This shortcut is unbound" => "此快捷键已解除绑定",
        "Toggle Full Screen" => "切换全屏",
        "Toggle Zoom" => "切换窗格放大状态",
        "Toggles the focused pane between split layout and full content area." => {
            "在拆分布局和占满内容区域之间切换当前窗格。"
        }
        "Toggles the window's native fullscreen mode." => "切换窗口的系统全屏模式。",
        "Transfer cancelled" => "传输已取消",
        "Transfer completed" => "传输完成",
        "Transfer in progress — press Esc to cancel" => "正在传输，按 Esc 取消",
        "Transfer notification timeout" => "传输通知显示时长",
        "Transferring" => "正在传输",
        "Unbind keybinding" => "解除绑定",
        "Unbinding keybinding…" => "正在解除绑定…",
        "Unbound" => "未绑定",
        "underline" => "下划线",
        "Uninstall this theme extension" => "卸载此主题扩展",
        "Unsaved changes discarded." => "已放弃未保存的更改。",
        "Update" => "更新",
        "Uploading" => "正在上传",
        "Use Regular Expressions" => "使用正则表达式",
        "Use the option key as the meta key." => "将 Option 键用作 Meta 键。",
        "user" => "用户",
        "Value" => "值",
        "Variables are validated and apply only to newly created terminals and panes." => {
            "变量会经过校验，仅对新建的终端和窗格生效。"
        }
        "Version" => "版本",
        "View the ZedTerm source code." => "查看 ZedTerm 源代码。",
        "What to do when the BEL character is printed. The standalone default is off." => {
            "设置收到 BEL 字符时的行为，默认关闭。"
        }
        "Where received files are saved. Leave blank to choose a directory for each download." => {
            "保存接收文件的目录。留空则每次下载时选择目录。"
        }
        "Working directory" => "工作目录",
        "Working…" => "正在处理…",
        "Your changes have not been saved." => "这些更改尚未保存。",
        "ZedTerm is distributed under the GPL-3.0-or-later license." => {
            "ZedTerm 按照 GPL-3.0-or-later 许可证发布。"
        }
        "Zoom Pane" => "放大窗格",
        _ => english,
    }
}

#[cfg(test)]
mod tests {
    use super::Locale::{self, English, SimplifiedChinese};

    #[test]
    fn auto_display_language_uses_supported_system_preferences() {
        for (preferences, expected) in [
            (&["zh-Hans-CN", "en-US"][..], SimplifiedChinese),
            (&["en-GB", "zh-CN"], English),
            (&["ja-JP", "zh-SG"], SimplifiedChinese),
            (&["zh-hans-HK"], SimplifiedChinese),
            (&["zh-Hant-CN", "zh-TW", "zh-HK", "zh-MO"], English),
            (&["fr-FR"], English),
            (&[], English),
        ] {
            assert_eq!(Locale::from_preferred_languages(preferences), expected);
        }
    }
}
