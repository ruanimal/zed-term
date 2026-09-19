# 文件传输扩展：接口与规范设计

> 状态：Phase 0 + Phase 1 已实现（2026-02）。tap 分流层、`transfer_core` trait、
> mux/会话驱动、trzsz provider（协议 v1 base64，上传/下载）、staging/安全落盘、
> 进度 UI 与取消均已落地并有测试；trzsz-rs 的 `trz`/`tsz` 真机互操作测试通过
> （`terminal_app/tests/transfer_trzsz.rs`，用 `ZEDTERM_TSZ_BIN`/`ZEDTERM_TRZ_BIN`
> 指向真实 binary 启用）。Phase 2–4 尚未实现。
>
> 与原设计的实现偏差（其余照旧）：
> - `SessionAction` 增加了带路径的文件动作（`OpenRead`/`OpenWrite`/`CloseFile`/
>   `CommitFile`），`HostEvent` 相应携带 `OpenedFile{size, local_name}`：会话
>   本身不持有 fs 状态，由 host 解析"当前文件"，避免隐式的文件游标。
> - 下载目录在会话一开始（发 ACT 之前）询问：数据流直到用户确认后才开始，
>   因此 §3.5 的"对话框期间照常灌数据进 staging"只部分适用（staging 机制在，
>   但 picker 等待期远端按其自身超时可能先行放弃）。
> - `SessionAction` 增加 `Cancelled`：用户取消（picker 返回空）时由会话显式终结，
>   UI 不再把取消显示成"完成"，也不会覆盖对话框错误信息。
> - `TransferSession` 增加 `start()`；`TransferHost::request_*` 为通知语义
>   （对话框由 UI 事件 `AwaitingUploadPaths`/`AwaitingDownloadDir` 驱动，
>   答案经 mux 控制通道回填），因为阻塞式询问会卡死会话线程、使看门狗失效。
> - Windows：tap 依赖 fd 复制的 `TapSource`，暂只在 Unix 启用；Windows 上
>   transfer 不挂载。

## 0. 结论先行

1. **扩展 ≠ 主题扩展**。`extension_store` 对接的 `api.zed.dev` 可下载扩展只适合主题这种纯数据包。
   文件传输需要 raw PTY 读写，WASM 沙箱/下载包模型给不了这个能力，也**不应该**给。
   本设计中的"扩展"指**进程内协议提供者（Transfer Provider）**：实现一个 Rust trait 并注册到
   `TransferRegistry`，另加一种**外部 helper 进程协议**，让第三方不改本仓库代码也能接新协议。
2. **拦截点必须在 Alacritty parser 之前**。PTY 输出今天是 IO 线程 `read → parser.advance` 直驱，
   没有原始字节扩展点（见 `event_loop.rs` 的 `pty_read`）。ZMODEM 二进制帧含 ESC/控制字符/非 UTF-8，
   trzsz 握手后也是协议帧——在 ANSI 解析之后或渲染层做识别都会污染终端状态。必须在 PTY 与 parser
   之间加分流层。
3. **不 fork Alacritty**。`EventLoop<T, U>` 本来就对 `T: EventedPty` 泛型，
   我们用一个实现同样 trait 的包装类型把 tap 埋进去，只改 `terminal_core` 内部的
   `open_pty` / `spawn_event_loop` 签名（从具体 `AlacrittyPty` 改成泛型/包装类型）。
4. **MVP 顺序**：先做 tap plumbing + 回环假协议（Phase 0），再做 trzsz（Phase 1），
   ZMODEM 用现成 `zmodem2` crate 接入（Phase 3）。不要从零手写 ZMODEM 状态机。
   ZMODEM 通用性更强（`lrzsz` 随处可装）但实现更难（§12 有完整对比）；先拿简单的 trzsz
   验证整条链路，ZMODEM 后面只是"再加一个 provider"。

## 1. 背景与目标

`docs/TODO.md` 的"传输文件支持"节已确认数据流与复杂度（约 8/10）：

```text
PTY raw output -> ProtocolDetector
                     |-> 未匹配：交给 Alacritty parser
                     |-> 匹配传输协议：独占当前传输会话
```

本设计回答：Detector/会话/文件 IO/UI 各住哪一层、接口长什么样、第三方怎么扩展、
安全红线是什么、按什么顺序实现。

目标：

- 单个 pane 同一时刻最多一个活跃传输会话；多 pane 互不干扰（会话归属 pane）。
- 自动检测（远端 `trz`/`tsz`/`rz`/`sz` 触发）+ 菜单手动触发（"Send File…"）两种发起方式。
- 传输期间普通键盘/鼠标/粘贴输入被仲裁，不与协议字节混写；取消/超时/异常可恢复到正常终端。
- UI 只消费结构化事件（等待选文件、等待保存目录、进度、完成、失败、取消），不解析协议。

非目标：

- 不复用/不引入 Zed `extension_host`（WASM + language/LSP 包袱，本 fork 不需要）。
- 不在 `TerminalElement` 或 ANSI parser 里做协议识别。
- Phase 1 不做断点续传、不做目录递归（多文件先支持文件列表；目录打包以后再议）。
- **暂不支持拖拽上传**：window 层 `FileDropEvent` 落点归属、拖拽中取消语义、与自动检测会话的
  冲突都需要单独设计。`Capabilities::drag_upload` 字段保留（恒为 `false`），手动上传只走菜单。

## 2. 总体架构

```text
                    ┌─ TransferRegistry ── TrapPty<P> ──────────────┐
                    │   ┌─────────┐  ┌─────────┐                     │
                    │   │ trzsz   │  │ zmodem  │  …Detector(cheap)   │ IO 线程
                    │   └────┬────┘  └────┬────┘                     │
                    │        │ match      │                         │
PTY ──► TapReader ──┴──► divert ──► TransferSession (background task)│
   │         │                       │  ▲                            │
   │         │ terminal bytes        │  │ actions                     │
   │         ▼                       ▼  │                             │
   │      parser.advance ──► Term ──► Event::Transfer ──► TerminalTab │
   │                                                              UI │ 前台线程
   └── TapWriter ◄── protocol bytes ◄── session/host ── input gating ┘
```

角色划分：

| 组件 | 住哪 | 线程 | 职责 |
|---|---|---|---|
| `TapReader` / `TapWriter` | `terminal_core`（新模块 `transfer_mux`） | IO 线程（event loop 的 read/write 都在此线程） | 包装 `EventedPty` 的 `Reader`/`Writer`；`TapReader` 按 mux 状态分流或直通，`TapWriter` 纯直通（输入门控在前台 `Terminal::input`，见 §3.3，Writer 侧不需要拦截） |
| `TransferDetector` | 各 provider 实现，`transfer_core` 定义 trait | IO 线程 | 流式匹配触发序列，便宜、有界内存 |
| `TransferSession` | 各 provider 实现 | background executor task | 协议状态机；只产出 `SessionAction`，不直接碰 fs/pty |
| `TransferMux` | `terminal_core` | 前台 `Terminal` 内状态 + IO 线程共享的原子状态 | Idle / Detecting / SessionActive 切换、看门狗超时 |
| 文件 IO | `terminal_app`（host 侧） | background executor | 读上传文件、写下载文件、staging+改名 |
| 进度/弹窗 | `terminal_app`（`TerminalTab` + window） | 前台 | 已有 `prompt_for_paths` / `prompt_for_new_path`、`FileDropEvent` 复用 |

依赖方向（避免循环）：

```text
transfer_core  (纯 trait + 类型，无 gpui、无 terminal、无 fs)
    ▲
terminal_core  (mux + tap + session 运行时，依赖 transfer_core)
    ▲
terminal_app   (provider 实现 trzsz/zmodem + UI + 文件 IO)
```

`transfer_core` 新建 crate 是合理的"新逻辑组件"（AGENTS.md 允许），且保持无 gpui 依赖，
使协议状态机可独立单测、独立审计。

## 3. 拦截层设计（最关键的部分）

### 3.1 为什么用包装 Pty 而不是改 Alacritty

- `EventLoop::new(terminal, event_proxy, pty, …)` 与 `pty_read`/`pty_write` 全是
  `T: EventedPty` 泛型；`EventedReadWrite` trait 允许自定义 `Reader`/`Writer` 关联类型。
  我们定义 `TapPty<P: EventedPty>` 并实现同一 trait，把它传给 `EventLoop` 即可，
  Alacritty 上游保持原样（`up` remote 照常可读）。
- 注意 impl 块的完整 bound 是 `T: EventedPty + event::OnResize + Send + 'static`：
  `Msg::Resize` 会调 `pty.on_resize`，`TapPty` 必须把 `OnResize`/`next_child_event`
  透传给内部 pty；`register`/`reregister`/`deregister` 复用内部 pty 的 fd 注册
  （`Reader` 是 tap 自己的类型，event loop 从 `reader()` 拿到的已是分流后的视图）。
- 代价：`terminal_core::spawn_event_loop` / `open_pty` 签名从具体 `AlacrittyPty`
  改成接受包装类型；`PtySender` 保持 `Msg::Input` 通道不变（排序语义不变）。

### 3.2 TapReader：hold-and-release 窗口

流式检测的根本矛盾：触发序列的前缀与普通输出无法即时区分。规则：

- reader 维护一个**有界保持窗口**（如 4 KiB，覆盖最长触发序列 + 行缓冲抖动）。
  字节先喂给所有已启用 provider 的 detector：
  - 无人 `need_more` 且无人 `matched` → 直通 parser（保持窗口 flush）。
  - 有人 `need_more` → 字节暂留窗口，不进 parser。字节数上限就是窗口大小，但**时间上无界**
    （远端吐出部分触发前缀后停摆，held 字节会一直不可见），所以 Detecting 态加时间兜底：
    如 200 ms 无新字节即把 held 前缀 flush 给 parser（detector 保留状态，后续字节续判）。
  - 有人 `matched` → 窗口中**触发序列之前**的 held 字节先 flush 给 parser，再吞掉触发序列，
    进入 `SessionActive`，后续字节全部 divert 给 session，不再进 parser；同时向前台发
    `TransferUiEvent::Detected`。（不 flush 前缀会把触发前的正常输出一起吞掉，如提示符行。）
  - 窗口满仍无人 match → 按"最长未拒绝前缀" flush 给 parser，各 detector `reset()`。
    这保证**无传输时的输出与今天逐字节一致**（Phase 0 必须有"直通字节一致"测试）。
- 跨 chunk 边界：detector 状态常驻（不按 read 调用重置），天然支持触发序列被拆散。
- PTY echo / SSH / tmux：检测面对的是远端写回的全部字节；要求 provider 声明
  `tmux_compatible`（trzsz 满足，lrzsz 传统用法不满足——这是优先做 trzsz 的理由之一）。
  转义/回显造成的修饰字节由各 detector 自行容忍（如忽略 `\r\n` / tmux 透传），mux 不猜。
- 热路径纪律（空闲时唯一开销是 detector 步进，别让它变差）：
  - 直通零拷贝零分配：detector 与 parser 喂的是同一个 read buffer 的切片，不复制；
    字节只在真正被 hold 时才写进窗口。
  - 兜底定时器只在 hold 时 armed：随"有字节被 hold"起止，不允许每字节/每 chunk 碰定时器。
  - detector 走批量喂入（§4 `feed_bytes`），行匹配类内部用 `memchr` 快扫，稳态开销压到
    memchr 量级（单 detector 参考量级 ~1–3 ns/byte）；逐字节 `feed` 只是正确性下限。

### 3.3 TapWriter 与输入仲裁

- `Terminal::write_to_pty` 是唯一 PTY 写入口（`PtySender::notify(Msg::Input)`，保序）。
  会话协议字节走**同一通道**（新建 `Terminal::transfer_write`，与普通 `input` 区分日志），
  不另开通道，避免与远端问答乱序（这正是 `ColorRequest` 注释里强调的相对顺序问题）。
- `SessionActive` 期间的前台输入门控（在 `Terminal::input` 生效**之前**拦截，
  避免触发 `scroll_to_bottom`/清选区/cwd 边界等副作用）：
  - 键盘/粘贴/鼠标上报：拒绝并给出 UI 提示（状态行 "Transfer in progress — Esc cancels" 之类），
    `Esc`（或 Cancel 按钮）走取消路径；取消键本身不进协议。
  - 只有 `transfer_write`（协议字节）与 abort 序列能写 PTY。
- 看门狗：握手后等待用户选文件（如 60s）、会话整体无进展（如 30s 无字节）即超时，
  provider 产出 abort 字节 → 发远端 → 回到 Idle，前台收到 `TimedOut`/`Cancelled`。

### 3.4 取消与恢复语义

- 用户取消：session 产出 provider 定义的 abort 序列（如 trzsz 的取消帧 / ZMODEM 的 `CAN` 序列），
  发完即 resume；已 divert 的字节不回放（协议字节回放进终端只会显示乱码）。
- 远端取消/异常退出：session 观察到 abort/EOF，事件 `Failed(reason)`，resume。
- pane 关闭/进程退出：drop session task 即取消；staging 文件清理（复用主题扩展的 staging 思路：
  下载先写 `staging/` 再原子改名）。

### 3.5 背压：对话框期间远端照常灌数据

`SessionActive` 后远端不会等本端 UI：下载触发命中时远端已经在发数据，而用户可能在
picker 里停留到 60s（§8.1）。这段窗口的缓冲必须有界：

- 下载：host 在用户确认下载目录后立刻确定 staging 路径——**用户选定的目录内的隐藏临时名**
  （`.<name>.<pid>.<seq>.part`），session 的 `WriteFile` 一律写进 staging；全部收完后 rename 到最终位置
  （§7.3），取消/超时只删临时文件。同目录 rename ⇒ 同设备、原子，落盘不依赖 staging 与目标同区。
  目录尚未确定时（调用方先 open 后选目录）才回退到 temp staging 根目录。spool 受 `max_file_size`
  约束，超限走 §7.4 abort。staging 是临时区，不算 §7.1 的"落盘"（那指最终位置），
  确认框照常在 `Detected` 后立即弹出。
- 上传：本地是发送方，节奏由 session 控制，picker 等待不产生远端洪峰。
- 已 divert 但尚未被 session 消化的 wire 字节：session 内部缓冲上限 4 MiB，超过即按
  §3.3 看门狗语义 abort + `Failed`，不允许无界增长。
- ZMODEM 可选做得更好：zmodem2 的 `wire_written(n)` 信用机制允许本端 withhold `ZRINIT`
  ack 让远端停发；是否实现由 provider 自行决定，架构不强制。

## 4. Provider 接口（`transfer_core`）

```rust
/// 方向：远端视角还是本地视角统一用本地视角：Upload = 本地 → 远端。
pub enum Direction { Upload, Download }

pub struct Capabilities {
    pub upload: bool,
    pub download: bool,
    pub auto_detect: bool,      // 能否从 PTY 输出自动识别触发
    pub drag_upload: bool,      // 预留：拖拽上传暂不支持，恒为 false
    pub multi_file: bool,
    pub tmux_compatible: bool,
}

pub enum DetectorVerdict {
    NeedMore,
    NoMatch,
    /// 触发序列命中。`trigger` 是该序列在 detector 视角的绝对字节区间（自 reset 起计）：
    /// mux 据此把字节切成三段——区间之前 flush 给 parser，区间之内吞掉，
    /// 区间之后 divert 给 session（§3.2）。不变式：候选只在 detector alive
    /// （⇒ 字节被 hold）期间累积，因此触发序列不会被提前 flush 进 parser。
    Matched { offer: TransferOffer, trigger: std::ops::Range<u64> },
}

pub struct TransferOffer {
    /// helper id 来自配置（§5.4），运行时才有：全部 id/display_name 用 `Arc<str>`，
    /// 内置 provider 构造时 mint 一次
    pub provider_id: Arc<str>,
    /// match 时刻已知则填；ZMODEM 在 ZRQINIT 时刻判不了方向（§12），为 None。
    /// UI 用中性文案，方向由后续对话框类型揭示（NeedUploadPaths=上传 / NeedDownloadDir=下载）。
    pub direction: Option<Direction>,
    /// 远端声明的文件名（仅用于展示与建议名；落盘前必做 sanitize，见 §7）
    pub remote_names: Vec<String>,
}

/// IO 线程调用：必须便宜、无阻塞、有界内存；`feed` 可被逐字节调用。
pub trait TransferDetector: Send {
    fn feed(&mut self, byte: u8) -> DetectorVerdict;
    /// 批量喂入：host 热路径默认走这里（§3.2 热路径纪律）。语义与逐字节 `feed` 等价
    /// （任意切分下可断言，§10），返回最后一个被消费字节的 verdict。
    /// `Matched` 时提前停止：触发序列之后的剩余字节由 mux divert 给 session，不经 detector。
    fn feed_bytes(&mut self, bytes: &[u8]) -> DetectorVerdict {
        let mut verdict = DetectorVerdict::NoMatch;
        for &byte in bytes {
            verdict = self.feed(byte);
            if let DetectorVerdict::Matched { .. } = verdict {
                break;
            }
        }
        verdict
    }
    fn reset(&mut self);
}

/// 后台 task 驱动：caller-driven，与 zmodem2 的 Action 模型同构，
/// 以便 ZMODEM provider 直接包一层 zmodem2::Sender/Receiver。
pub enum SessionAction {
    /// 向 PTY 写协议字节（经 transfer_write 保序发出）
    WriteWire(Vec<u8>),
    /// 读本地文件一段（host 做 fs IO 后调 submit）
    ReadFile { offset: u64, max_len: usize },
    /// 写本地文件一段（host 做 fs IO；下载在用户确认前一律写 staging，见 §3.5/§7.3）
    WriteFile { offset: u64, data: Vec<u8> },
    /// 需要用户选上传文件 / 下载目录（host 弹对话框）。
    /// UI 事件的 request_id 由 mux 铸造（helper 会话透传 helper 的 id，§5.3）；
    /// 同一 pane 同时最多一个未决 picker，回填 HostEvent 时无需携带 id。
    NeedUploadPaths,
    /// 只选目录；文件名由远端 `#NAME` sanitize 得到（§7.2），所以没有建议名参数。
    NeedDownloadDir,
    Progress { file_index: usize, file_count: usize, bytes_done: u64, bytes_total: Option<u64> },
    Done { paths: Vec<std::path::PathBuf> },
    Failed(String),
}

pub trait TransferSession: Send {
    /// 远端字节 → session；返回下一步 host 动作（可能多个，Vec）。
    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction>;
    /// host 完成的动作回填（文件读写结果、用户选择、取消/超时），见 HostEvent。
    fn submit(&mut self, event: HostEvent) -> Vec<SessionAction>;
    fn abort_bytes(&mut self) -> Vec<u8>;
}

/// `submit` 的回填事件，与 `SessionAction` 的请求一一对应。
/// 文件 IO 结果用 `io::Result`：Err 由 session 转成 abort/`Failed`。
pub enum HostEvent {
    /// `ReadFile` 的结果
    FileData { offset: u64, result: std::io::Result<Vec<u8>> },
    /// `WriteFile` 的结果
    FileWritten { offset: u64, result: std::io::Result<()> },
    /// `NeedUploadPaths` 的结果；`None` = 用户取消
    UploadPaths(Option<Vec<std::path::PathBuf>>),
    /// `NeedDownloadDir` 的结果；`None` = 用户取消
    DownloadDir(Option<std::path::PathBuf>),
    Cancelled,
    TimedOut,
}

pub trait TransferProvider: Send + Sync {
    fn id(&self) -> Arc<str>;            // "trzsz" / "zmodem" / helper id
    fn display_name(&self) -> Arc<str>;  // UI 显示
    fn capabilities(&self) -> Capabilities;
    /// 扩展规范声明：默认配置 + 除 enabled 外的参数 schema（见 §8.3）。
    /// host 用它做三件事：未知 provider 的 fallback、settings 校验、设置页动态渲染。
    fn manifest(&self) -> ProviderManifest;
    /// host 把 settings 里该 id 整节 JSON 交给 provider；provider 自行校验。
    /// 返回 Err → 该 provider 禁用 + warn（见 §8.2 合并规则）。
    fn configure(&mut self, config: &serde_json::Value) -> anyhow::Result<()>;
    fn new_detector(&self) -> Box<dyn TransferDetector>;
    fn start_session(&self, offer: &TransferOffer) -> Box<dyn TransferSession>;
    /// 无远端触发的手动上传（菜单 "Send File…"）：直接建 session。
    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>>;
}
```

ProviderManifest / ProviderConfigSchema 的具体形状是 transfer_core 的一部分，
与 §8.2 的 providers 开放表、§5.4 的 helper 触发声明保持同一套字段命名；
helper 的 manifest 等价物就是它的那节配置（id/display_name/trigger/direction）。

注册表（`terminal_core` 持有，每 pane 共享定义、各 pane 独立 mux 状态）：

```rust
pub struct TransferRegistry {
    // configure 在 terminal_app 启动时完成，之后 Arc 冻结；registry 只调用 &self 的
    // 工厂方法（new_detector/start_session/start_manual_upload），各 pane 状态隔离
    providers: Vec<Arc<dyn TransferProvider>>,
    // 同一字节流多家 match 时的确定性裁决（§8.1 settings；helper id 也允许出现）
    priority: Vec<Arc<str>>,
}
```

- 多家同时 `Matched`：按 `priority` 顺序取第一家（默认 trzsz > zmodem），事件里带上
  `provider_id`，便于测试断言与问题排查。
- 开关：settings 见 §8；禁用 = 不建它的 detector，零开销、零误触。

`TransferOffer` 的触发序列原文**不写进本设计**：以各协议测试向量为准
（trzsz 取其仓库的 `TRZSZ:TRANSFER` 系列握手样本；ZMODEM 取 ZRQINIT/ZSINIT 头样本），
detector 单测必须覆盖"按 1 字节/随机切分喂入仍能 match"。

## 5. 外部 helper 进程协议（第三方扩展点）

> 本节是**面向第三方的完整规范**：任何人按此实现一个可执行文件，经用户配置即可
> 成为一个新的传输 provider，无需改本仓库代码、无需懂 Rust。内置 provider
> （trzsz / ZMODEM）是 `TransferProvider` 的进程内实现；helper 是同一 trait 的
> 子进程适配器（`HelperProvider`），两者在 mux/UI/安全层走同一条路。

### 5.1 模型与信任边界

```text
 PTY ──► host mux ──stdin──► helper ──stdout──► host mux ──► PTY
              │                   │文件 IO helper 自己做
              └── 对话框/进度/取消（host）
```

- helper 是用户在 settings 里**显式配置**的本地命令，与 app 同权限（`transfer.helpers`
  默认空，不配置 = 不存在）。
- **远端永远不能指定跑哪个 helper**（这是否决项，防 RCE）：触发序列只能命中
  已配置 helper 声明的 `trigger`，helper 身份只来自本地配置。
- helper 的 stderr 直接进 app 日志（便于第三方调试）；stdout 只允许协议帧，
  解析失败的行记 warn 并忽略，不中断会话。

### 5.2 会话生命周期

1. **spawn**：host 在以下任一时刻启动 helper 进程：
   - 自动检测：某 helper 的 `trigger` 在 PTY 输出中命中（见 5.4）；
   - 手动上传：用户点菜单 "Send File via \<name\>…"，host 带 `{"reason":"manual_upload"}` 启动。
   - 同一时刻同 pane 只允许一个 helper 会话；已在会话中又有触发命中 → `BusyRejected`。
2. **serve**：双方按 5.3 的帧协议交换，helper 做协议状态机 + 文件 IO，
   host 负责 PTY 字节 shuttle、对话框、进度展示。
3. **结束**（任一即终结，host 负责 kill 并回收）：
   - helper 发 `done` / `error`；
   - 用户取消（host 先发 `cancel`，等 2s 不退出则 kill）；
   - 看门狗超时（host 直接 kill，按 `error{reason:"timeout"}` 上报 UI）；
   - pane 关闭 / 进程退出（直接 kill，无 UI 事件）。

### 5.3 帧协议 v1

- 传输：双方 stdin/stdout，**换行分隔的 JSON**（JSONL），UTF-8。
  二进制载荷一律 base64（标准字母表，带 padding）。
- 每个帧：`{"v":1,"type":"<type>","seq":<u64>,"payload":{…}}`。
  `seq` 单调递增（双方各自计数），便于日志对齐；未知 `type` 必须忽略（向前兼容）。
- 行长度上限 16 MiB（超了记 error 并 kill，防内存耗尽）。

host → helper：

| type | payload | 说明 |
|---|---|---|
| `start` | `{reason, provider_id, trigger_bytes_b64?, columns, rows, config}` | 会话开始；`trigger_bytes_b64` 仅自动检测时带（命中的触发原文）；`config` = `transfer.providers.<id>` 节原样 JSON（§8.2），helper 自行校验 |
| `wire` | `{data_b64}` | PTY 输出 divert 来的字节 |
| `picker_result` | `{request_id, paths[] \| null}` | 对话框结果；`null` = 用户取消 |
| `cancel` | `{}` | 用户取消 / 超时前通知 |

helper → host：

| type | payload | 说明 |
|---|---|---|
| `wire` | `{data_b64}` | 发往 PTY 的字节（host 经 `transfer_write` 保序发出） |
| `need_paths` | `{request_id, multiple, title?}` | 请 host 弹文件选择框 |
| `need_dir` | `{request_id, suggested_name?, title?}` | 请 host 弹目录选择框（下载落点确认，§7 第 1 条照样适用） |
| `progress` | `{file_index, file_count, bytes_done, bytes_total?}` | 转 `TransferUiEvent::Progress` |
| `done` | `{paths[]}` | helper 已落盘/已发完的本地路径（展示用） |
| `error` | `{reason}` | 可展示的失败原因（原文展示，不做解析） |
| `log` | `{level?, message}` | 透传进 app 日志（`level` 仅 `debug/info/warn/error`） |

`request_id` 由 helper 自选（`u64`，会话内唯一）；host 原样回填。
未完成的 picker 请求在会话结束时自动按取消处理。

最小 transcript 示例（上传，helper 读本地文件自己发）：

```text
H→P {"v":1,"type":"start","seq":0,"payload":{"reason":"detected","provider_id":"myproto","trigger_bytes_b64":"TVlQUk9UTzpVUExPQUQK","columns":120,"rows":30,"config":{}}}
H→P {"v":1,"type":"wire","seq":1,"payload":{"data_b64":"aGVsbG8gaGVscGVy"}}
P→H {"v":1,"type":"need_paths","seq":0,"payload":{"request_id":7,"multiple":true}}
H→P {"v":1,"type":"picker_result","seq":2,"payload":{"request_id":7,"paths":["/home/u/a.bin"]}}
P→H {"v":1,"type":"progress","seq":1,"payload":{"file_index":0,"file_count":1,"bytes_done":512,"bytes_total":1024}}
P→H {"v":1,"type":"wire","seq":2,"payload":{"data_b64":"ZmlsZS1jaHVuay0x"}}
P→H {"v":1,"type":"done","seq":3,"payload":{"paths":["/home/u/a.bin"]}}
```

### 5.4 触发声明（自动检测）

helper 的自动检测能力由配置声明，不是由 helper 代码上报（host 在 spawn 前就要做匹配）：

```jsonc
{ "transfer": { "helpers": [{
  "id": "myproto",                       // [a-z0-9_-]{1,32}，UI 显示与日志用
  "display_name": "MyProto",
  "command": "/usr/local/bin/myproto-helper",
  "args": ["--serve"],
  "trigger": { "kind": "line_prefix", "prefix": "MYPROTO:UPLOAD", "max_line_bytes": 4096 },
  "direction": "upload"                  // upload | download（UI 文案与对话框类型）
}] } }
```

- 开关不在本节：helper 的启用开关与参数统一放 `transfer.providers.<id>`（§8.2），
  参数经 §5.3 `start.config` 送达；本节只声明身份与启动方式。
- `kind: "line_prefix"`：以行为单位，行首匹配 `prefix` 即命中（trzsz 类文本握手够用）。
- `kind: "bytes"`：`pattern_b64` 为触发字节串的子序列匹配（ZMODEM 类二进制头够用，
  同样要求"收齐 + 校验"由 helper 在 serve 阶段二次确认——host 只做粗筛，被误 spawn 的
  helper 应立刻回 `error{reason:"false trigger"}` 退出，host 把这次误触记入日志）。
- 未来的 `kind` 新增走"未知 kind = 该 helper 禁用 + 启动时 warn"的兼容规则。

### 5.5 资源与安全约束（host 强制执行）

1. 落盘确认：`need_dir`/`need_paths` 必须经 host 对话框（或已配置目录的确认框）；
   helper 自己静默写盘不受信任——host 对 `done.paths` 只做展示，不做背书。
2. 文件名 jail 与 staging：helper 落盘同样适用 §7（host 侧对下载目录做最终校验：
   `done.paths` 不在下载目录内 → `Failed` + warn 日志）。
3. 上限：单 `wire` 帧数据 ≤ 4 MiB；会话累计双向字节默认 ≤ 4 GiB（`max_session_mb` 可配）；
   超限 kill + `Failed`。
4. 超时：spawn 后 10s 无任何出站帧（`log` 也算；要等远端数据才有话说的 helper 先发一帧
   `log` 保活）→ kill；`start` 后等待 picker 结果 60s（与内置一致）；
   会话无 `wire`/`progress` 30s → `cancel` + kill。
5. 环境：helper 继承 app 环境 + `ZEDTERM_TRANSFER=1`、`ZEDTERM_TRANSFER_ID=<id>`；
   cwd 为用户主目录（不暴露 pane cwd，避免信息泄漏；需要 cwd 的 helper 应经 picker）。
6. 不 shell-out：`command` 只取本地配置；helper 的参数不支持任何模板展开。

### 5.6 版本与兼容

- `v` 字段即协议版本；host 实现 v1。helper 发 `{"v":1,…}` 即声明兼容；
  收到 `v < 1` 或 `v > 1` 的帧直接 `error{reason:"unsupported protocol version"}` + kill。
  版本不匹配不按"未知 type 忽略"处理：v2 可能重构已知 type 的 payload，静默忽略只会表现
  为挂死后被看门狗杀，第三方无从排查；版本相同的帧内未知 `type` 才忽略（§5.3 向前兼容）。
- host 启动时校验配置（`id` 格式、`command` 可执行、trigger 合法），失败逐条 warn
  并禁用该 helper，不影响内置 provider。
- 本节的帧表是 v1 冻结部分；`need_paths` 经 host 转发的双向问答**已包含在 v1**
  （上一版设计写 v1.1，现收回——没有它手动上传走不通）。

### 5.7 合规自测清单（给第三方）

helper 发布前应通过：触发行被拆成 1 字节喂入仍能 serve；收到 `cancel` 后 2s 内退出；
下载会话的 `done.paths` 全部位于用户确认的目录内（上传会话：`done.paths` 与
`picker_result` 的路径一致）；stdout 无协议外输出（stderr 随便打）；
`error.reason` 是人能看懂的一句话。

## 6. Host ↔ UI 事件与状态机（`terminal_core::Event` 扩展）

```rust
pub enum TransferUiEvent {
    // direction 为 None 时（ZMODEM match 时刻判不了方向，§12）UI 用中性文案"文件传输中"，
    // 方向由随后出现的对话框类型揭示（选文件=上传 / 选目录=下载）
    Detected { provider_id: Arc<str>, direction: Option<Direction>, remote_names: Vec<String> },
    AwaitingUploadPaths { request_id: u64 },       // Tab 弹文件选择框
    AwaitingDownloadDir { request_id: u64 },
    Progress { provider_id: Arc<str>, file_index: usize, file_count: usize,
               bytes_done: u64, bytes_total: Option<u64> },
    Completed { paths: Vec<PathBuf> },
    Failed { reason: String },
    Cancelled,
    BusyRejected,  // 第二个触发到来时正忙：UI 提示，不干扰当前会话
}
```

- `TerminalTab` 订阅 `Event::Transfer(…)`，持有本 pane 的传输 UI 状态
  （覆盖在 pane 顶部的进度条 + Cancel，复用搜索条"挂在 pane 内"的模式，
  而不是窗口层全局条——多 pane 归属问题搜索条已经踩过一次）。
- Actions：`TransferCancel`（Esc 在会话中即它）、右键菜单/命令 "Send File via trzsz…"、
  "Receive File via trzsz…"（无触发时的手动发起；上传走 `start_manual_upload`，
  下载的手动发起语义 = 向远端粘贴 `tsz <name>`？不——手动下载仍需远端配合，
  菜单项只做提示/文档位，Phase 1 不实现"无远端参与的下载"）。
- 拖拽上传暂不支持（见 §1 非目标）；`FileDropEvent` 接线留到后续设计。

## 7. 安全规范（MUST）

远端字节视为**攻击者可控**（被攻陷的服务器、恶意 `curl | sh` 输出）：

1. **下载永远先确认**：自动检测到下载触发也必须弹目录选择框（或使用配置好的
   `download_dir` + `confirm_before_download=false`），禁止静默落盘到未确认的位置。
   上传选文件同理（本地用户动作）。
2. **文件名 jail**：剥离目录成分，拒绝绝对路径/`..`/NUL/控制字符，并截断到
   200 字节（保留扩展名、按 UTF-8 边界切；`NAME_MAX` 255 减去隐藏临时名前后缀）；
   下载框只选**目录**，文件名由远端 `#NAME` sanitize 得到（不向用户索要文件名，
   也不让对话框的默认名变成文件名）：最终路径 = 下载目录 join(sanitized basename)；
   目标已存在 → 覆盖前确认（或自动 `name (2)`，Phase 1 选"确认"，简单可预测）。
3. **staging + 原子改名**：下载先在**用户选定的下载目录内**用隐藏临时名
   （`.<name>.<pid>.<seq>.part`）写，完成后 rename 到最终名字（§3.5）；
   临时名与最终名同目录 ⇒ rename 同设备、原子，不需要跨设备拷贝；
   未确定目录时才回退到 temp staging 根目录，此时 commit 跨设备回退为
   "拷到目标目录的隐藏兄弟文件再改名"。取消/失败/超时只删临时文件，不碰已有文件。
4. **上限**：单文件与单会话字节上限（默认如 2 GiB/4 GiB，可配），超限 abort；
   detector 保持窗口与 session 缓冲都有界。
5. **权限**：落盘文件不带可执行位（0600/0644 按 umask，不 `chmod +x`）；
   不跟随远端指定的 symlink（Phase 1 不支持 symlink 项，遇到即 `Failed` 可见错误）。
6. **不 shell-out**：永远不用远端文件名拼 shell 命令；helper 命令只来自本地 settings。
7. **可审计**：完成/失败事件带 provider、文件名、字节数，进日志（`log`，不记内容）。

## 8. 设置项（host 通用设置 + provider manifest 驱动）

原则：**host 只硬编码自己能强制执行的通用设置；provider 的开关与参数由 provider
manifest 声明驱动，settings 里以 provider id 为 key 的开放表承载。新增 provider
（尤其第三方 helper）不需要改 settings schema 代码，也不需要改设置页代码。**
§8 现在这个草案如果写成 `trzsz: true` / `zmodem: false` 的固定 struct，就是反例。

### 8.1 host 通用设置（可硬编码）

这些是 host 职责（资源、安全、UI、裁决），host 必须理解，所以可以是固定 schema：

```jsonc
{
  "terminal": {
    "transfer": {
      "download_dir": null,          // 与 confirm_before_download=false 搭配时作为免询问的落盘目录；否则仍弹目录选择框（§7.1）
      "max_file_size_mb": 2048,      // §7.4
      "max_session_mb": 4096,        // §5.5.3（含 helper 会话）
      "confirm_before_download": true,
      "picker_timeout_secs": 60,     // §5.5.4
      "idle_timeout_secs": 30,       // §5.5.4
      "priority": ["trzsz", "zmodem"] // 多家同时 match 的裁决顺序；helper id 也允许出现；未知 id 忽略
    }
  }
}
```

### 8.2 provider 配置表（开放 map，禁止逐协议硬编码）

```jsonc
{
  "terminal": {
    "transfer": {
      "providers": {
        "trzsz": { "enabled": true },
        "zmodem": { "enabled": false },
        "myproto": { "enabled": true, "mode": "fast" }
      }
    }
  }
}
```

- `enabled` 由 host 理解：`false` = 不建它的 detector，零开销、零误触。
- 其余参数 host **不解释、只透传**：启动 provider 时把整节 JSON 交给它，
  provider 按自己 manifest 的 schema 校验；非法值 → 该 provider 禁用 + warn，
  不影响其他 provider。
- helper 的参数也以 helper id 为 key 放这里，经 §5.3 `start.config` 原样送达，helper 自行
  校验（helper 的启用开关也只有这一处，§5.4 不再单列 `enabled`）。
- settings schema 侧 `providers` 必须是开放 map（如 `HashMap<String, Value>`），
  **禁止为每个协议加固定字段**；`schemars` 只描述 §8.1 的通用部分。
- 设置页 UI 同理：provider 列表（开关 + 参数行）按 registry + manifest 动态渲染，
  不为单个协议写死控件（Phase 4）。

### 8.3 provider manifest（扩展规范的一部分）

每个 provider 声明（内置 provider 在代码里声明，helper 在 §5.4 的配置里声明）：

- `id` / `display_name` / `capabilities`（见 §4）；
- `default_config`：默认开关由 provider 自己声明。内置：trzsz `{enabled: true}`，
  zmodem `{enabled: false}`（默认关的理由：触发头误触成本高于 trzsz，见 §12）；
- `config_schema`：JSON Schema 子集，描述除 `enabled` 外的参数；host 用它校验用户配置。

合并规则：用户没写某 provider 节 → 用 `default_config`；用户写了未知 id → warn 后忽略
（helper 被删但配置残留时不炸）；provider 升级新增参数 → 按 schema 默认值补齐；
用户参数与 schema 冲突 → 该 provider 禁用 + 状态行提示，不阻断终端与其它 provider。

## 9. 实现路线

- **Phase 0 — tap plumbing（无真实协议）**：`transfer_core` trait + `TransferMux` +
  `TapPty` + `LoopbackProvider`（测试用：固定触发串 + echo 会话）。
  验收：无匹配时输出**逐字节一致**（property test：随机切分）；match 时触发序列不进格子；
  取消/timeout/pane 关闭恢复正常；单测 + `terminal_core` 集成测试。
- **Phase 1 — trzsz MVP**：单文件上传/下载、自动检测、文件/目录对话框、进度+取消、
  staging 落盘。会话层包一层 [`trzsz-rs`](https://github.com/ruanimal/trzsz-rs)
  （已确认可用）为 `TransferSession`；若接口不合用，退路是按 trzsz 协议文档实现最小
  客户端：握手→JSON 控制→base64 数据帧，量级不大。
  验收：与 trzsz go/py 服务端对传通过；tmux 内同样通过；真机联调。
- **Phase 2 — 多文件 + 手动菜单**：`multi_file`、右键菜单项、下载目录记忆。
  （拖拽上传暂不支持，不在本阶段。）
- **Phase 3 — ZMODEM**：`zmodem2` crate（`Sender`/`Receiver` caller-driven 状态机，
  接口形状与 `SessionAction` 同构，适配层薄）包成 provider；与 trzsz 共存优先级；
  默认关闭，收集误触数据。
- **Phase 4（可选）**：helper 子进程协议 v1、设置页 UI、单会话限速显示。

每阶段独立可验收，不预支下一阶段接口。

## 10. 测试策略

- detector：触发序列按 1 字节/随机 N 切分喂入，逐字节 `feed` 与批量 `feed_bytes`
  结果等价；变异前缀（缺一字节、多一字节）必须 `NoMatch`；
  满窗口无匹配 flush 后输出一致。
- mux：假 PTY 双工对端脚本（先输出垃圾+半截触发，再续后半）→ 事件序列断言
  `Detected → Progress* → Completed`；取消/超时/远端 abort/双触发并发各一例。
- session：内存 duplex 对端（Phase 1 用 trzsz 真实服务端 binary 做黑盒互操作，至少覆盖
  空文件/1 字节/跨 chunk 边界/中文文件名/大文件抽查）。
- UI：`pty_write_log` 在 `terminal_core`（test-support feature，`terminal_app`
  dev-dependencies 已开启）断言 abort/协议字节；`did_prompt_for_paths` 是 gpui
  `TestAppContext` 的 helper，断言对话框触发。两者 terminal_app 测试目前都还没用过，
  直接引入即可。
- 性能：空闲/直通路径零拷贝零分配（热路径分配计数断言，§3.2 热路径纪律的验收）；
  默认配置（仅 trzsz detector）在 burst 输入（如 50 MB/s cat）下 IO 线程附加 CPU
  占比保持个位数百分比——以基准测试守护，不靠人工体感。
- 回归红线：关闭所有 provider 时，`cargo test -p terminal_core` 现有用例零改动通过
  （tap 默认直通）。

## 11. 待决策

1. MVP 先只做 trzsz，还是 trzsz+zmodem 双轨？（建议：trzsz 先行，§8、§12 有理由）
2. 外部 helper 进程协议是否纳入 v1 设计？（已纳入：§5 为完整规范；实现放 Phase 4）

## 12. ZMODEM 与 trzsz 的实现差异

本质：**ZMODEM 是为 1980 年代串口线设计的二进制协议**（信道不可靠，所以自带 CRC、
重传、断点续位）；**trzsz 是为今天的 SSH/PTY 可靠流设计的文本协议**（信道本身保序可靠，
只做分帧，不做重传）。

| 层面 | ZMODEM（`rz`/`sz`） | trzsz（`trz`/`tsz`） |
|---|---|---|
| 通用性 | 强：`lrzsz` 在服务器上几乎随处可装 | 弱：远端需装 trzsz（go/py/js 任一实现） |
| 触发检测 | 二进制 autostart 头（`ZPAD ZDLE` 同步，即 `** \x18 …` 开头，后跟帧类型+参数+CRC）。必须**收齐整个头并校验 CRC 后才算 match**，否则 `cat` 二进制文件的误触不可接受 | 可读文本握手行（`TRZSZ:TRANSFER…` 系列），纯字符串匹配 |
| 对 parser 的威胁 | 高：帧里含 ESC、XON/XOFF、CAN、非 UTF-8，直接喂 parser 会污染终端状态，divert 必须精确 | 低一些：base64 文本漏进终端也只是乱码行，不改终端状态（握手行同样不能进格子，divert 照样需要） |
| 会话状态机 | 大：`ZRQINIT → ZRINIT → ZFILE → ZRPOS → ZDATA → ZEOF → ZFIN`，发送窗口、NAK 重传、`CAN×5` 中止 | 小：握手 → 文件信息 JSON → 数据帧 → 结束；中止就是一个取消帧 |
| 可靠性 | CRC32 + 重传 + ZRPOS 续传（串口时代遗产，在 SSH 上是免费 bonus） | 依赖流本身可靠；续传 = 重来（Phase 1 可接受） |
| tmux | 经典 lrzsz 在 tmux 下会坏（透传问题），历史包袱 | 为 tmux 兼容而设计，这是 trzsz 存在的理由之一 |
| 方向判定 | `ZRQINIT` 判不了方向：远端 `sz`（下载）与远端 `rz`（上传）启动时都发 ZRQINIT——`rz` 先发是为了触发本地 autostart（敲 `rz` 弹上传框的机制）。方向由 **ZFILE 的发送方**确定：远端发 ZFILE = 下载；本地需发 ZFILE（先回 ZRINIT）= 上传。故 `TransferOffer.direction` 在 match 时刻是 `None`，会话必须立即回 ZRINIT 再按 ZFILE 分流 | 握手行里直接声明方向 |
| 手动上传 | 发送方可主动发起（`sz` 方先发 `ZRQINIT`），机制与 trzsz 的 `start_manual_upload` 同构 | 同左 |
| 文件名元数据 | `ZFILE` 帧带文件名+大小+时间 | JSON 里带。**两者都是攻击者可控**，§7 的 sanitize/staging/确认框要求完全一样 |
| 进度 | 从 `ZDATA` 偏移推导（转义膨胀导致不精确） | 协议层原生进度 |
| Rust 选型 | `zmodem2`（`no_std`、caller-driven `Sender`/`Receiver` + `poll() → Action`，与 `SessionAction` 同构，适配层薄） | 包一层 [`trzsz-rs`](https://github.com/ruanimal/trzsz-rs)（已确认可用，§9）；不行再按协议文档手写最小客户端（量级远小于 ZMODEM） |

对本架构的影响：**架构不用改**。`TransferDetector`/`TransferSession`/`SessionAction`
就是照着"二进制难搞的协议也能塞进来"设计的（`ReadFile`/`WriteFile` 几乎就是
`zmodem2::Action` 的形状）。差异只落在 provider 内部：

1. **Detector**：ZMODEM 贵在"攒够一整个头 + CRC 校验"，保持窗口按最大帧头定；
   trzsz 就是行匹配。
2. **Session**：ZMODEM 用 `zmodem2` 包一层；trzsz 包 `trzsz-rs` 或手写。
   `feed_wire`/`submit`/`abort_bytes` 签名一样。
3. **Mux/UI/安全**：完全复用。
4. **方向**：`TransferOffer.direction` 是 `Option<Direction>`，ZMODEM match 时刻为 `None`
   （见上表）；UI 中性文案，方向由后续 `NeedUploadPaths`/`NeedDownloadDir` 揭示。
   ZMODEM 会话从 match 起就要立即回 `ZRINIT`，不能等对话框——caller-driven 模型天然允许
   （`feed_wire(ZRQINIT) → [WriteWire(ZRINIT)]`）。

先做 trzsz 的理由：detector 和 session 都简单，Phase 0→1 最快跑通端到端；
ZMODEM 的复杂度集中在 detector 正确性上，而 hold-and-release 语义正好拿 trzsz
先验证，ZMODEM 的 detector 测试可复用同一套 harness。

## 附：文件布局

```text
crates/transfer_core/src/transfer_core.rs   # trait + 类型（新 crate，无 gpui 依赖）
crates/terminal_core/src/transfer_mux.rs    # mux 状态机 + TapPty + 注册表接入
crates/terminal_core/src/terminal.rs        # Terminal 增 transfer 状态字段 + transfer_write + input 门控
crates/terminal_app/src/transfer_ui.rs      # TerminalTab 侧状态、进度条、对话框调用（新文件，UI 逻辑）
crates/terminal_app/src/transfer_io.rs      # 后台文件读写、staging、sanitize（新文件，可测）
crates/terminal_app/src/transfer_helper.rs  # HelperProvider：helper 子进程 spawn/shuttle/看门狗（Phase 4）
crates/terminal_app/src/providers_trzsz.rs  # trzsz detector+session（Phase 1）
crates/terminal_app/src/providers_zmodem.rs # zmodem2 适配（Phase 3）
```

---

*参考*：trzsz 可嵌入终端应用的 Go 库模式（`trzsz-go` 同时是 CLI 与可嵌入库）——
见 [trzsz-go](https://trzsz.github.io/go)（第三方资料，仅作方向参考）；
ZMODEM Rust 实现 `zmodem2`（caller-driven `Sender`/`Receiver` 状态机）——
见 [zmodem2 on docs.rs](https://docs.rs/zmodem2)（第三方资料，仅作选型参考）；
ZMODEM 帧结构（三帧头编码、`ZRQINIT`/`ZRINIT` 须用 Hex 头）——
见 [TeraTerm ZMODEM Protocol](https://github.com/TeraTermProject/teraterm/wiki/ZMODEM-Protocol)
与 [ZMODEM Protocol Reference](http://www.ethernetgateway.com/zmodemreference.html)
（第三方资料，仅作协议背景参考）。
`rz` 启动时同样发送 ZRQINIT（方向判定依据，见 §12）——
见 [unix.stackexchange: rz 产生的 `**B0100000023be50`](https://unix.stackexchange.com/questions/365422/z-waiting-to-receive-b0100000023be50-when-i-use-rz-to-upload-file)
与 [ZOC Terminal help](https://www.emtec.com/kb/en/2007/zmodem-transfer-to-from-linux)（第三方资料，仅作协议背景参考）。
Alacritty 侧结论来自本仓库 `Cargo.toml` 锁定的上游源码
（`EventLoop<T: EventedPty>`、`EventedReadWrite::Reader/Writer`），非网页资料。
