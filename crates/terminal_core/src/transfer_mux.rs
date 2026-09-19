//! Transfer mux: the tap between the PTY and the ANSI parser, plus the
//! registry and the session driver runtime. See `docs/TRANSFER_EXTENSION.md`
//! §2–§5.

use parking_lot::Mutex;
use std::cmp::Reverse;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

pub use transfer_core::{
    Capabilities, DetectorVerdict, Direction, HostEvent, OpenedFile, ProviderManifest,
    SessionAction, TransferDetector, TransferHost, TransferOffer, TransferProvider,
    TransferSession,
};

/// Maximum bytes the tap keeps from the parser while a trigger candidate is
/// undecided (§3.2).
pub const HOLD_WINDOW_BYTES: usize = 4096;
/// How long held bytes may stay invisible before being released to the parser
/// (§3.2). Checked whenever the PTY would block, so no extra timer is needed.
pub const HOLD_RELEASE_TIMEOUT: Duration = Duration::from_millis(200);
/// Maximum wire bytes diverted but not yet consumed by the session (§3.5).
pub const WIRE_BACKPRESSURE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxState {
    /// No transfer in progress. With providers enabled this means detectors
    /// are armed; the Detecting/Idle distinction is internal.
    Idle,
    /// A session owns the byte stream.
    Active,
}

/// Host ↔ UI events (§6). The UI consumes these and never parses protocol
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferUiEvent {
    Detected {
        provider_id: Arc<str>,
        direction: Option<Direction>,
        remote_names: Vec<String>,
    },
    AwaitingUploadPaths {
        request_id: u64,
    },
    AwaitingDownloadDir {
        request_id: u64,
        suggested_name: Option<String>,
    },
    Progress {
        provider_id: Arc<str>,
        file_index: usize,
        file_count: usize,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    Completed {
        paths: Vec<PathBuf>,
    },
    Failed {
        reason: String,
    },
    Cancelled,
    /// A second trigger arrived while a session was already active.
    BusyRejected,
    /// User input was refused because a transfer is in progress.
    InputRefused,
}

/// Messages into the session driver thread. One channel, so the driver can
/// wait with a timeout (watchdogs) without selecting.
pub enum DriverMsg {
    /// Mux matched a trigger: start this session.
    StartSession(TransferOffer),
    /// Wire bytes diverted from the parser.
    Wire(Vec<u8>),
    /// Foreground: answer to a picker request.
    UploadPaths(Option<Vec<PathBuf>>),
    DownloadDir(Option<PathBuf>),
    /// Foreground: user cancelled.
    Cancel,
    /// Terminal is going away.
    Shutdown,
    /// Wire backpressure exceeded; the session must abort.
    Overflow,
}

/// Shared mux state: the IO-thread byte-flow core behind a lock, the cheap
/// state flag for the foreground, the wire writer callback (set once the
/// event loop exists, so session writes share the ordered input channel),
/// and the channels to the driver and the UI.
pub struct TransferShared {
    core: Mutex<TapCore>,
    state: AtomicU8,
    driver_tx: Mutex<std::sync::mpsc::Sender<DriverMsg>>,
    ui_tx: async_channel::Sender<TransferUiEvent>,
    ui_rx: async_channel::Receiver<TransferUiEvent>,
    wire_writer: Mutex<Option<Arc<dyn Fn(&[u8]) + Send + Sync>>>,
    /// Wire bytes queued for the driver but not yet consumed (§3.5 bound).
    wire_in_flight: AtomicUsize,
}

impl TransferShared {
    pub fn state(&self) -> MuxState {
        match self.state.load(Ordering::Acquire) {
            0 => MuxState::Idle,
            _ => MuxState::Active,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state() == MuxState::Active
    }

    /// Install the PTY writer used for session protocol bytes. Called once
    /// after the event loop is spawned.
    pub fn set_wire_writer(&self, writer: Arc<dyn Fn(&[u8]) + Send + Sync>) {
        *self.wire_writer.lock() = Some(writer);
    }

    /// User cancel (Esc / Cancel button). The driver sends the provider's
    /// abort sequence and returns the terminal to idle.
    pub fn cancel(&self) {
        self.send_driver(DriverMsg::Cancel);
    }

    /// Answer to `AwaitingUploadPaths`.
    pub fn answer_upload_paths(&self, paths: Option<Vec<PathBuf>>) {
        self.send_driver(DriverMsg::UploadPaths(paths));
    }

    /// Answer to `AwaitingDownloadDir`.
    pub fn answer_download_dir(&self, dir: Option<PathBuf>) {
        self.send_driver(DriverMsg::DownloadDir(dir));
    }

    /// Subscribe to UI events.
    pub fn ui_events(&self) -> async_channel::Receiver<TransferUiEvent> {
        self.ui_rx.clone()
    }

    /// Report refused user input so the UI can hint at the ongoing transfer.
    pub fn notify_input_refused(&self) {
        self.emit_ui(TransferUiEvent::InputRefused);
    }

    fn send_driver(&self, msg: DriverMsg) {
        // A send failure means the driver is gone (transfer finished or the
        // terminal closed); leftover triggers/answers are meaningless then.
        if let Err(error) = self.driver_tx.lock().send(msg) {
            log::debug!("transfer driver unavailable: {error}");
        }
    }

    fn emit_ui(&self, event: TransferUiEvent) {
        if let Err(error) = self.ui_tx.try_send(event) {
            // Unbounded channel; failure means no receiver (relay dropped).
            log::debug!("transfer ui channel unavailable: {error}");
        }
    }

    fn write_wire(&self, bytes: &[u8]) {
        let writer = self.wire_writer.lock().clone();
        match writer {
            Some(writer) => writer(bytes),
            None => log::warn!("dropped {} transfer bytes: no PTY writer", bytes.len()),
        }
    }

    fn divert(&self, bytes: &[u8]) {
        let in_flight = self.wire_in_flight.fetch_add(bytes.len(), Ordering::AcqRel) + bytes.len();
        if in_flight > WIRE_BACKPRESSURE_BYTES {
            self.send_driver(DriverMsg::Overflow);
        }
        self.send_driver(DriverMsg::Wire(bytes.to_vec()));
    }
}

struct DetectorEntry {
    detector: Box<dyn TransferDetector>,
}

/// Byte-flow state, owned by the IO thread (through the `core` lock).
struct TapCore {
    registry: Arc<TransferRegistry>,
    detectors: Vec<DetectorEntry>,
    /// Bytes withheld from the parser while a candidate is undecided.
    hold: Vec<u8>,
    hold_since: Option<Instant>,
    /// Bytes fed to the detectors since their last reset; detector trigger
    /// ranges are absolute in this coordinate space. The `hold` bytes are the
    /// tail of this range.
    fed_offset: u64,
    /// Parser-bound bytes that could not be returned in place (they include
    /// previously held bytes); served by the next `read` call.
    pending: Vec<u8>,
}

impl TapCore {
    fn take_pending(&mut self, buf: &mut [u8]) -> usize {
        let count = buf.len().min(self.pending.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        count
    }

    /// Release held bytes to the parser and reset the detectors, so a trigger
    /// can never straddle a flush (§3.2 invariant).
    fn release_hold(&mut self) {
        if !self.hold.is_empty() {
            self.pending.extend_from_slice(&self.hold);
            self.hold.clear();
        }
        self.hold_since = None;
        for entry in &mut self.detectors {
            entry.detector.reset();
        }
        self.fed_offset = 0;
    }

    /// Process one read chunk. Returns how many leading bytes of the chunk
    /// are parser bytes still in the read buffer (in-place passthrough);
    /// `None` when nothing can be handed to the parser from this call (bytes
    /// were held, diverted, or are pending and served on the next call).
    fn process(&mut self, chunk: &[u8], shared: &TransferShared) -> Option<usize> {
        if self.detectors.is_empty() {
            return Some(chunk.len());
        }

        if shared.is_active() {
            shared.divert(chunk);
            return None;
        }

        let fed_before = self.fed_offset;
        let verdicts: Vec<DetectorVerdict> = self
            .detectors
            .iter_mut()
            .map(|entry| entry.detector.feed_bytes(chunk))
            .collect();

        // Highest-priority match wins; ties break by registration order (§4).
        let matched = verdicts
            .iter()
            .enumerate()
            .filter_map(|(index, verdict)| match verdict {
                DetectorVerdict::Matched { offer, trigger } => {
                    Some((index, offer.clone(), trigger.clone()))
                }
                _ => None,
            })
            .min_by_key(|(index, offer, _)| {
                (
                    Reverse(self.registry.priority_rank(&offer.provider_id)),
                    *index,
                )
            });

        if let Some((_, offer, trigger)) = matched {
            return Some(self.begin_session(offer, trigger, fed_before, chunk, shared));
        }

        if verdicts
            .iter()
            .any(|verdict| matches!(verdict, DetectorVerdict::NeedMore))
        {
            self.fed_offset += chunk.len() as u64;
            self.hold.extend_from_slice(chunk);
            self.hold_since.get_or_insert_with(Instant::now);
            if self.hold.len() > HOLD_WINDOW_BYTES {
                // Window full without a match: release what was held so far
                // and re-detect this chunk from scratch (§3.2).
                self.release_hold();
                return self.process(chunk, shared);
            }
            return None;
        }

        for entry in &mut self.detectors {
            entry.detector.reset();
        }
        self.fed_offset = 0;
        Some(chunk.len())
    }

    fn begin_session(
        &mut self,
        offer: TransferOffer,
        trigger: std::ops::Range<u64>,
        fed_before: u64,
        chunk: &[u8],
        shared: &TransferShared,
    ) -> usize {
        let combined: Vec<u8> = {
            let mut combined = std::mem::take(&mut self.hold);
            combined.extend_from_slice(chunk);
            combined
        };
        self.hold_since = None;
        self.fed_offset = 0;

        // Detector coordinates are absolute since the last reset; `combined`
        // starts at `fed_before - held.len()` in that space.
        let combined_start = fed_before - (combined.len() - chunk.len()) as u64;
        let start = trigger.start.saturating_sub(combined_start) as usize;
        let end = (trigger.end.saturating_sub(combined_start)) as usize;

        let session_bytes = combined[end.min(combined.len())..].to_vec();

        shared.state.store(1, Ordering::Release);
        shared.send_driver(DriverMsg::StartSession(offer.clone()));
        if !session_bytes.is_empty() {
            shared.divert(&session_bytes);
        }
        shared.emit_ui(TransferUiEvent::Detected {
            provider_id: offer.provider_id.clone(),
            direction: offer.direction,
            remote_names: offer.remote_names,
        });

        // Parser bytes before the trigger: return them in place when they
        // are entirely inside this chunk (the common, zero-copy case);
        // otherwise they include previously held bytes and go via `pending`.
        if combined.len() == chunk.len() && start <= chunk.len() {
            return start;
        }
        self.pending
            .extend_from_slice(&combined[..start.min(combined.len())]);
        0
    }
}

/// A PTY whose read stream can be tapped. Unix ptys hand out a duplicate of
/// the master fd: the same open file description, so the registered
/// readiness and the tap's reads see the same stream.
pub trait TapSource: EventedPty {
    fn tap_reader(&self) -> io::Result<Self::Reader>;
}

#[cfg(unix)]
impl TapSource for alacritty_terminal::tty::Pty {
    fn tap_reader(&self) -> io::Result<std::fs::File> {
        self.file().try_clone()
    }
}

/// Wrapper around a PTY that taps its read stream through the transfer mux
/// (§3.1). Writes pass straight through: user input is gated in the
/// foreground `Terminal`, and session protocol bytes take the same ordered
/// path as user input.
pub struct TapPty<P: TapSource> {
    inner: P,
    #[cfg(unix)]
    reader: TapReader<P::Reader>,
}

pub struct TapReader<R> {
    inner: R,
    shared: Arc<TransferShared>,
}

impl<R: io::Read> io::Read for TapReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 1. Parser bytes queued by an earlier match or flush.
        // 2. Timed-out hold: release it to the parser now that the PTY has
        //    nothing to say (§3.2 time bound; no dedicated timer needed).
        {
            let mut core = self.shared.core.lock();
            let count = core.take_pending(buf);
            if count > 0 {
                return Ok(count);
            }
            let expired = core
                .hold_since
                .is_some_and(|since| since.elapsed() >= HOLD_RELEASE_TIMEOUT);
            if expired {
                core.release_hold();
                let count = core.take_pending(buf);
                if count > 0 {
                    return Ok(count);
                }
            }
        }

        let count = self.inner.read(buf)?;
        if count == 0 {
            return Ok(0);
        }

        let decision = self.shared.core.lock().process(&buf[..count], &self.shared);
        match decision {
            // Passthrough: the bytes are already where the event loop
            // expects them. A zero length means the chunk was fully
            // swallowed (match at the chunk start); it must surface as
            // WouldBlock, never as `Ok(0)` (EOF).
            Some(parser_bytes) if parser_bytes > 0 => Ok(parser_bytes),
            // The chunk was held, diverted, or queued as pending; the event
            // loop must not treat the (consumed) bytes as parser input.
            _ => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }
}

impl<P: TapSource> TapPty<P> {
    /// Wrap `pty` with the transfer tap. `shared` comes from
    /// [`TransferRuntime::shared`].
    pub fn new(pty: P, shared: Arc<TransferShared>) -> io::Result<Self> {
        let reader = TapReader {
            inner: pty.tap_reader()?,
            shared,
        };
        Ok(TapPty { inner: pty, reader })
    }
}

impl<P: TapSource> EventedReadWrite for TapPty<P> {
    #[cfg(unix)]
    type Reader = TapReader<P::Reader>;
    #[cfg(not(unix))]
    type Reader = P::Reader;
    type Writer = P::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        // Registration covers the underlying fds; on unix the tap reader
        // holds a duplicate of the master fd, so readiness applies to both.
        unsafe { self.inner.register(poll, interest, poll_opts) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        poll_opts: PollMode,
    ) -> io::Result<()> {
        self.inner.reregister(poll, interest, poll_opts)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    #[cfg(unix)]
    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
    }

    #[cfg(not(unix))]
    fn reader(&mut self) -> &mut Self::Reader {
        self.inner.reader()
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl<P: TapSource> EventedPty for TapPty<P> {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl<P: TapSource + OnResize> OnResize for TapPty<P> {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.inner.on_resize(window_size);
    }
}

/// Provider registry, shared by all panes (§4). `configure` completes at
/// startup; afterwards the registry only calls the `&self` factory methods,
/// so per-pane session state stays isolated.
pub struct TransferRegistry {
    providers: Vec<Arc<dyn TransferProvider>>,
    priority: Vec<Arc<str>>,
}

impl TransferRegistry {
    pub fn new(providers: Vec<Arc<dyn TransferProvider>>, priority: Vec<Arc<str>>) -> Self {
        TransferRegistry {
            providers,
            priority,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn providers(&self) -> &[Arc<dyn TransferProvider>] {
        &self.providers
    }

    pub fn provider(&self, id: &str) -> Option<Arc<dyn TransferProvider>> {
        self.providers
            .iter()
            .find(|provider| &*provider.id() == id)
            .cloned()
    }

    /// Adjudication rank: lower is better. Unknown ids rank after configured
    /// priorities, in registration order.
    fn priority_rank(&self, id: &str) -> usize {
        self.priority
            .iter()
            .position(|candidate| &**candidate == id)
            .unwrap_or_else(|| {
                self.priority.len()
                    + self
                        .providers
                        .iter()
                        .position(|provider| &*provider.id() == id)
                        .unwrap_or(0)
            })
    }
}

/// Everything the session driver resolves from settings (§8.1).
#[derive(Clone, Debug)]
pub struct TransferPolicy {
    pub picker_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_file_size: u64,
    pub max_session_bytes: u64,
}

impl Default for TransferPolicy {
    fn default() -> Self {
        TransferPolicy {
            picker_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(30),
            max_file_size: 2 * 1024 * 1024 * 1024,
            max_session_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

/// Per-terminal transfer runtime: shared mux state plus the driver thread
/// that pumps `SessionAction`s through the provider and the host.
pub struct TransferRuntime {
    shared: Arc<TransferShared>,
}

impl TransferRuntime {
    pub fn new(
        registry: Arc<TransferRegistry>,
        host: Arc<dyn TransferHost>,
        policy: TransferPolicy,
    ) -> Self {
        let (driver_tx, driver_rx) = std::sync::mpsc::channel();
        let (ui_tx, ui_rx) = async_channel::unbounded();

        let shared = Arc::new(TransferShared {
            core: Mutex::new(TapCore {
                detectors: registry
                    .providers()
                    .iter()
                    .filter(|provider| provider.capabilities().auto_detect)
                    .map(|provider| DetectorEntry {
                        detector: provider.new_detector(),
                    })
                    .collect(),
                hold: Vec::new(),
                hold_since: None,
                fed_offset: 0,
                pending: Vec::new(),
                registry: registry.clone(),
            }),
            state: AtomicU8::new(0),
            driver_tx: Mutex::new(driver_tx),
            ui_tx,
            ui_rx,
            wire_writer: Mutex::new(None),
            wire_in_flight: AtomicUsize::new(0),
        });

        let driver_shared = shared.clone();
        // The driver thread ends on Shutdown (runtime drop), on session
        // completion, or when the host is unavailable. It is detached: the
        // runtime cannot join it without blocking on host calls.
        let _ = std::thread::Builder::new()
            .name("transfer-driver".into())
            .spawn(move || run_driver(driver_shared, driver_rx, registry, host, policy));

        TransferRuntime { shared }
    }

    pub fn shared(&self) -> Arc<TransferShared> {
        self.shared.clone()
    }

    pub fn ui_events(&self) -> async_channel::Receiver<TransferUiEvent> {
        self.shared.ui_events()
    }

    pub fn is_active(&self) -> bool {
        self.shared.is_active()
    }
}

impl Drop for TransferRuntime {
    fn drop(&mut self) {
        self.shared.send_driver(DriverMsg::Shutdown);
    }
}

/// One live session on the driver thread.
struct DriverSession {
    provider_id: Arc<str>,
    session: Box<dyn TransferSession>,
    session_bytes: u64,
    last_activity: Instant,
    picker_deadline: Option<Instant>,
    next_request_id: u64,
}

fn run_driver(
    shared: Arc<TransferShared>,
    receiver: std::sync::mpsc::Receiver<DriverMsg>,
    registry: Arc<TransferRegistry>,
    host: Arc<dyn TransferHost>,
    policy: TransferPolicy,
) {
    let mut slot: Option<DriverSession> = None;

    loop {
        let timeout: Option<Duration> = match &slot {
            None => {
                // Nothing running; wait for the next trigger.
                match receiver.recv() {
                    Ok(msg) => {
                        handle_driver_msg(msg, &mut slot, &shared, &registry, &host, &policy);
                        continue;
                    }
                    Err(_) => return,
                }
            }
            Some(session) => {
                // While a picker is pending no wire bytes are expected — the
                // idle timeout must not fire under the user's dialog; the
                // picker timeout owns the wait (§5.5.4). Otherwise
                // inactivity (no wire either way) is what expires.
                let now = Instant::now();
                let deadline = session
                    .picker_deadline
                    .unwrap_or_else(|| session.last_activity + policy.idle_timeout);
                Some(deadline.saturating_duration_since(now))
            }
        };

        let message = match timeout {
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => return,
            },
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(message) => message,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => DriverMsg::Overflow,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
        };
        handle_driver_msg(message, &mut slot, &shared, &registry, &host, &policy);
    }
}

fn handle_driver_msg(
    msg: DriverMsg,
    slot: &mut Option<DriverSession>,
    shared: &Arc<TransferShared>,
    registry: &Arc<TransferRegistry>,
    host: &Arc<dyn TransferHost>,
    policy: &TransferPolicy,
) {
    match msg {
        DriverMsg::StartSession(offer) => {
            if slot.is_some() {
                shared.emit_ui(TransferUiEvent::BusyRejected);
                return;
            }
            let Some(provider) = registry.provider(&offer.provider_id) else {
                shared.emit_ui(TransferUiEvent::Failed {
                    reason: format!("provider {} is not available", offer.provider_id),
                });
                return;
            };
            let provider_id = provider.id();
            let mut session = provider.start_session(&offer);
            let actions = session.start();
            *slot = Some(DriverSession {
                provider_id,
                session,
                session_bytes: 0,
                last_activity: Instant::now(),
                picker_deadline: None,
                next_request_id: 0,
            });
            run_actions(actions, slot, shared, host, policy);
        }
        DriverMsg::Wire(bytes) => {
            shared
                .wire_in_flight
                .fetch_sub(bytes.len(), Ordering::AcqRel);
            let Some(driver) = slot.as_mut() else { return };
            driver.last_activity = Instant::now();
            driver.session_bytes += bytes.len() as u64;
            if driver.session_bytes > policy.max_session_bytes {
                abort_session(
                    slot,
                    shared,
                    host,
                    TransferUiEvent::Failed {
                        reason: "session byte limit exceeded".into(),
                    },
                );
                return;
            }
            let actions = driver.session.feed_wire(&bytes);
            run_actions(actions, slot, shared, host, policy);
        }
        DriverMsg::UploadPaths(paths) => {
            if let Some(driver) = slot.as_mut() {
                driver.picker_deadline = None;
                let actions = driver.session.submit(HostEvent::UploadPaths(paths));
                run_actions(actions, slot, shared, host, policy);
            }
        }
        DriverMsg::DownloadDir(dir) => {
            if let Some(destination) = &dir {
                host.set_destination(destination);
            }
            if let Some(driver) = slot.as_mut() {
                driver.picker_deadline = None;
                let actions = driver.session.submit(HostEvent::DownloadDir(dir));
                run_actions(actions, slot, shared, host, policy);
            }
        }
        DriverMsg::Cancel => {
            abort_session(slot, shared, host, TransferUiEvent::Cancelled);
        }
        DriverMsg::Overflow => {
            // Either the wire backpressure limit was hit or a watchdog
            // expired; both end the session (§3.3).
            if let Some(driver) = slot.as_mut() {
                if driver
                    .picker_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    abort_session(
                        slot,
                        shared,
                        host,
                        TransferUiEvent::Failed {
                            reason: "timed out waiting for file selection".into(),
                        },
                    );
                    return;
                }
                if Instant::now() >= driver.last_activity + policy.idle_timeout {
                    abort_session(
                        slot,
                        shared,
                        host,
                        TransferUiEvent::Failed {
                            reason: "transfer timed out".into(),
                        },
                    );
                    return;
                }
            }
            abort_session(
                slot,
                shared,
                host,
                TransferUiEvent::Failed {
                    reason: "transfer buffer overflow".into(),
                },
            );
        }
        DriverMsg::Shutdown => {
            abort_session(slot, shared, host, TransferUiEvent::Cancelled);
        }
    }
}

/// Execute the actions a session produced, recursing into `submit` for host
/// round-trips (file IO answers resolve synchronously on this thread; picker
/// answers arrive as separate driver messages).
fn run_actions(
    actions: Vec<SessionAction>,
    slot: &mut Option<DriverSession>,
    shared: &Arc<TransferShared>,
    host: &Arc<dyn TransferHost>,
    policy: &TransferPolicy,
) {
    for action in actions {
        let Some(driver) = slot.as_mut() else { return };
        match action {
            SessionAction::WriteWire(bytes) => {
                driver.last_activity = Instant::now();
                shared.write_wire(&bytes);
            }
            SessionAction::OpenRead { path } => {
                let result = host.open_read(&path).map(|size| OpenedFile {
                    size,
                    local_name: None,
                });
                let actions = driver.session.submit(HostEvent::FileOpened(result));
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::ReadFile { offset, max_len } => {
                if (offset.saturating_add(max_len as u64)) > policy.max_file_size {
                    fail_session(slot, shared, host, "file size limit exceeded".into());
                    return;
                }
                let result = host.read_chunk(offset, max_len);
                let actions = driver
                    .session
                    .submit(HostEvent::FileData { offset, result });
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::OpenWrite { remote_name, size } => {
                if size.is_some_and(|size| size > policy.max_file_size) {
                    fail_session(slot, shared, host, "file size limit exceeded".into());
                    return;
                }
                let result = host
                    .open_write(&remote_name, size)
                    .map(|local_name| OpenedFile {
                        size: 0,
                        local_name: Some(local_name),
                    });
                let actions = driver.session.submit(HostEvent::FileOpened(result));
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::WriteFile { offset, data } => {
                let result = host.write_chunk(offset, &data);
                let actions = driver
                    .session
                    .submit(HostEvent::FileWritten { offset, result });
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::CloseFile => {
                let result = host.close_write();
                let actions = driver.session.submit(HostEvent::FileClosed(result));
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::CommitFile => {
                let result = host.commit();
                let actions = driver.session.submit(HostEvent::FileCommitted(result));
                run_actions(actions, slot, shared, host, policy);
            }
            SessionAction::NeedUploadPaths => {
                driver.next_request_id += 1;
                driver.picker_deadline = Some(Instant::now() + policy.picker_timeout);
                shared.emit_ui(TransferUiEvent::AwaitingUploadPaths {
                    request_id: driver.next_request_id,
                });
                host.request_upload_paths();
            }
            SessionAction::NeedDownloadDir { suggested_name } => {
                driver.next_request_id += 1;
                driver.picker_deadline = Some(Instant::now() + policy.picker_timeout);
                shared.emit_ui(TransferUiEvent::AwaitingDownloadDir {
                    request_id: driver.next_request_id,
                    suggested_name: suggested_name.clone(),
                });
                host.request_download_dir(suggested_name.as_deref());
            }
            SessionAction::Progress {
                file_index,
                file_count,
                bytes_done,
                bytes_total,
            } => {
                shared.emit_ui(TransferUiEvent::Progress {
                    provider_id: driver.provider_id.clone(),
                    file_index,
                    file_count,
                    bytes_done,
                    bytes_total,
                });
            }
            SessionAction::Done { paths } => {
                end_session(slot, shared, TransferUiEvent::Completed { paths });
                return;
            }
            SessionAction::Failed(reason) => {
                fail_session(slot, shared, host, reason);
                return;
            }
            SessionAction::Cancelled => {
                end_session(slot, shared, TransferUiEvent::Cancelled);
                return;
            }
        }
    }
}

/// Session failed: send the provider abort sequence to the remote, drop any
/// staged files, and return the terminal to idle.
fn fail_session(
    slot: &mut Option<DriverSession>,
    shared: &Arc<TransferShared>,
    host: &Arc<dyn TransferHost>,
    reason: String,
) {
    abort_session(slot, shared, host, TransferUiEvent::Failed { reason });
}

/// Session ends (cancel/failure/shutdown): send the abort sequence, clean up
/// staged files, flip the mux back to idle, notify the UI.
fn abort_session(
    slot: &mut Option<DriverSession>,
    shared: &Arc<TransferShared>,
    host: &Arc<dyn TransferHost>,
    event: TransferUiEvent,
) {
    let Some(mut driver) = slot.take() else {
        // Nothing running: a late cancel/timeout must not emit a UI event
        // after the session already reported its outcome.
        return;
    };
    let bytes = driver.session.abort_bytes();
    if !bytes.is_empty() {
        shared.write_wire(&bytes);
    }
    host.discard_staged();
    shared.state.store(0, Ordering::Release);
    shared.emit_ui(event);
}

/// Session completed successfully: no abort, no cleanup, notify the UI.
fn end_session(
    slot: &mut Option<DriverSession>,
    shared: &Arc<TransferShared>,
    event: TransferUiEvent,
) {
    slot.take();
    shared.state.store(0, Ordering::Release);
    shared.emit_ui(event);
}

/// Everything needed to attach transfers to a new terminal, resolved by the
/// app layer (§8): the configured registry, the host implementation and the
/// watchdog policy.
#[derive(Clone)]
pub struct TransferSetup {
    pub registry: Arc<TransferRegistry>,
    pub host: Arc<dyn TransferHost>,
    pub policy: TransferPolicy,
}

/// Fixed-trigger echo provider used to exercise the whole mux chain without
/// a real protocol (Phase 0). Trigger: `ZTLOOP:START\n`; the session echoes
/// every wire byte back until it sees `ZTLOOP:END\n`.
pub struct LoopbackProvider;

const LOOPBACK_PREFIX: &[u8] = b"ZTLOOP:START";
const LOOPBACK_END: &[u8] = b"ZTLOOP:END\n";

/// Line-prefix trigger detector: matches `<prefix><line>\n`, tolerating the
/// prefix appearing anywhere in the output and the line being split across
/// reads arbitrarily.
pub struct LineTriggerDetector {
    prefix: Vec<u8>,
    /// Longest suffix of the consumed stream that is a proper prefix of
    /// `prefix`; these bytes must not reach the parser yet.
    tail: Vec<u8>,
    candidate_start: Option<u64>,
    line_len: usize,
    position: u64,
}

impl LineTriggerDetector {
    pub fn new(prefix: &[u8]) -> Self {
        LineTriggerDetector {
            prefix: prefix.to_vec(),
            tail: Vec::new(),
            candidate_start: None,
            line_len: 0,
            position: 0,
        }
    }
}

impl TransferDetector for LineTriggerDetector {
    fn feed(&mut self, byte: u8) -> DetectorVerdict {
        let index = self.position;
        self.position += 1;

        if let Some(start) = self.candidate_start {
            self.line_len += 1;
            if byte == b'\n' {
                let end = index + 1;
                self.candidate_start = None;
                self.line_len = 0;
                return DetectorVerdict::Matched {
                    offer: TransferOffer {
                        provider_id: "loopback".into(),
                        direction: None,
                        remote_names: Vec::new(),
                    },
                    trigger: start..end,
                };
            }
            if self.line_len > HOLD_WINDOW_BYTES {
                self.reset();
                return DetectorVerdict::NoMatch;
            }
            return DetectorVerdict::NeedMore;
        }

        self.tail.push(byte);
        // Longest suffix of `tail` that is a prefix of `prefix`; a full match
        // opens a candidate, a partial match keeps the tail held.
        let max = self.tail.len().min(self.prefix.len());
        let mut keep = 0;
        for length in (0..=max).rev() {
            let suffix = &self.tail[self.tail.len() - length..];
            if suffix == &self.prefix[..length] {
                keep = length;
                break;
            }
        }
        self.tail.drain(..self.tail.len() - keep);
        if keep == self.prefix.len() {
            self.candidate_start = Some(index + 1 - self.prefix.len() as u64);
            self.line_len = self.prefix.len();
            self.tail.clear();
            return DetectorVerdict::NeedMore;
        }
        if keep > 0 {
            return DetectorVerdict::NeedMore;
        }
        DetectorVerdict::NoMatch
    }

    fn reset(&mut self) {
        self.tail.clear();
        self.candidate_start = None;
        self.line_len = 0;
        self.position = 0;
    }
}

struct LoopbackSession {
    buffer: Vec<u8>,
}

impl TransferSession for LoopbackSession {
    fn start(&mut self) -> Vec<SessionAction> {
        vec![SessionAction::WriteWire(b"ZTLOOP:ACK\n".to_vec())]
    }

    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction> {
        self.buffer.extend_from_slice(bytes);
        let mut actions = vec![SessionAction::WriteWire(bytes.to_vec())];
        if self.buffer.ends_with(LOOPBACK_END) {
            actions.push(SessionAction::Done { paths: Vec::new() });
        }
        actions
    }

    fn submit(&mut self, _event: HostEvent) -> Vec<SessionAction> {
        Vec::new()
    }

    fn abort_bytes(&mut self) -> Vec<u8> {
        b"ZTLOOP:ABORT\n".to_vec()
    }
}

impl TransferProvider for LoopbackProvider {
    fn id(&self) -> Arc<str> {
        "loopback".into()
    }

    fn display_name(&self) -> Arc<str> {
        "Loopback".into()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            auto_detect: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            id: self.id(),
            display_name: self.display_name(),
            capabilities: self.capabilities(),
            default_config: serde_json::json!({ "enabled": true }),
            config_schema: serde_json::json!({}),
        }
    }

    fn configure(&mut self, _config: &serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }

    fn new_detector(&self) -> Box<dyn TransferDetector> {
        Box::new(LineTriggerDetector::new(LOOPBACK_PREFIX))
    }

    fn start_session(&self, _offer: &TransferOffer) -> Box<dyn TransferSession> {
        Box::new(LoopbackSession { buffer: Vec::new() })
    }

    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Read as _;

    /// Read source that yields preset chunks regardless of the requested
    /// size, then blocks forever.
    struct ChunkReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl io::Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    let count = chunk.len().min(buf.len());
                    buf[..count].copy_from_slice(&chunk[..count]);
                    if count < chunk.len() {
                        self.chunks.push_front(chunk[count..].to_vec());
                    }
                    Ok(count)
                }
                None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }
    }

    /// Returns the runtime too: dropping it sends Shutdown, which would tear
    /// down any session the test starts afterwards.
    fn test_shared(
        providers: Vec<Arc<dyn TransferProvider>>,
    ) -> (TransferRuntime, Arc<TransferShared>) {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(providers, vec![])),
            Arc::new(NoopHost),
            TransferPolicy::default(),
        );
        let shared = runtime.shared();
        (runtime, shared)
    }

    struct NoopHost;

    impl TransferHost for NoopHost {
        fn open_read(&self, _path: &std::path::Path) -> io::Result<u64> {
            Err(io::Error::other("noop"))
        }
        fn read_chunk(&self, _offset: u64, _max_len: usize) -> io::Result<Vec<u8>> {
            Err(io::Error::other("noop"))
        }
        fn open_write(&self, _remote_name: &str, _size: Option<u64>) -> io::Result<String> {
            Err(io::Error::other("noop"))
        }
        fn set_destination(&self, _destination: &std::path::Path) {}
        fn write_chunk(&self, _offset: u64, _data: &[u8]) -> io::Result<()> {
            Err(io::Error::other("noop"))
        }
        fn close_write(&self) -> io::Result<PathBuf> {
            Err(io::Error::other("noop"))
        }
        fn commit(&self) -> io::Result<PathBuf> {
            Err(io::Error::other("noop"))
        }
        fn request_upload_paths(&self) -> Option<Vec<PathBuf>> {
            None
        }
        fn request_download_dir(&self, _suggested_name: Option<&str>) -> Option<PathBuf> {
            None
        }
        fn discard_staged(&self) {}
    }

    #[test]
    fn passthrough_is_byte_identical_under_random_chunking() {
        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let mut input = Vec::new();
        for _ in 0..64 {
            input.extend_from_slice(b"plain terminal output without triggers;\n");
        }
        // Random-ish chunk sizes, deterministic.
        let mut state = 0xdeadbeefu32;
        let mut reader = ChunkReader {
            chunks: VecDeque::new(),
        };
        let mut rest = &input[..];
        while !rest.is_empty() {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            let take = ((state >> 16) as usize % 13) + 1;
            let (chunk, remainder) = rest.split_at(take.min(rest.len()));
            reader.chunks.push_back(chunk.to_vec());
            rest = remainder;
        }

        let mut tap = TapReader {
            inner: reader,
            shared,
        };
        let mut output = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match tap.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&buf[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(output, input, "passthrough output must be byte identical");
    }

    #[test]
    fn trigger_split_across_reads_is_diverted_not_parsed() {
        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let reader = ChunkReader {
            chunks: VecDeque::from(vec![
                b"hello ".to_vec(),
                b"ZTLOOP:ST".to_vec(),
                b"ART\nDATA".to_vec(),
            ]),
        };
        let mut tap = TapReader {
            inner: reader,
            shared: shared.clone(),
        };
        let mut buf = [0u8; 4096];

        assert_eq!(tap.read(&mut buf).unwrap(), 6);
        assert_eq!(&buf[..6], b"hello ");

        // The candidate is undecided: the bytes must not reach the parser.
        assert_would_block(&mut tap, &mut buf);

        // Keep the hold fresh: under parallel test load the 200 ms release
        // window could otherwise expire between reads (that path has its own
        // test below).
        shared.core.lock().hold_since = Some(Instant::now());

        // Match: the trigger line is swallowed, `DATA` goes to the session,
        // and there is nothing for the parser in this read.
        assert_would_block(&mut tap, &mut buf);
        assert!(shared.is_active());
    }

    fn assert_would_block<R: io::Read>(tap: &mut TapReader<R>, buf: &mut [u8]) {
        match tap.read(buf) {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
            other => panic!("expected WouldBlock, got {other:?}"),
        }
    }

    #[test]
    fn held_prefix_is_released_after_timeout() {
        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let reader = ChunkReader {
            chunks: VecDeque::from(vec![b"ZTLOOP:ST".to_vec()]),
        };
        let mut tap = TapReader {
            inner: reader,
            shared: shared.clone(),
        };
        let mut buf = [0u8; 4096];
        assert_would_block(&mut tap, &mut buf);

        // Age the hold past the release timeout.
        {
            let mut core = shared.core.lock();
            core.hold_since = Some(Instant::now() - HOLD_RELEASE_TIMEOUT - Duration::from_secs(1));
        }
        // Next read (PTY quiet) must flush the held bytes to the parser.
        let count = tap.read(&mut buf).unwrap();
        assert_eq!(&buf[..count], b"ZTLOOP:ST");
        assert!(!shared.is_active());
    }

    #[test]
    fn loopback_session_round_trip_through_driver() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(LoopbackProvider)],
                vec![],
            )),
            Arc::new(NoopHost),
            TransferPolicy::default(),
        );
        let shared = runtime.shared();

        let (written_tx, written_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
            let _ = written_tx.send(bytes.to_vec());
        }));

        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "loopback".into(),
            direction: None,
            remote_names: Vec::new(),
        }));
        shared.send_driver(DriverMsg::Wire(b"ping".to_vec()));
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"ZTLOOP:ACK\n"
        );
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"ping"
        );

        shared.send_driver(DriverMsg::Wire(b"ZTLOOP:END\n".to_vec()));

        let ui_rx = shared.ui_events();
        let event = wait_event(&ui_rx, Duration::from_secs(5));
        assert!(matches!(event, Some(TransferUiEvent::Completed { .. })));
        assert!(!shared.is_active());
    }

    fn wait_event(
        receiver: &async_channel::Receiver<TransferUiEvent>,
        timeout: Duration,
    ) -> Option<TransferUiEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(event) = receiver.try_recv() {
                return Some(event);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Provider whose session asks for upload paths and, once answered,
    /// echoes the count back and finishes. Used to exercise the picker
    /// watchdog against the idle watchdog.
    struct PickerProvider;

    struct PickerSession {
        answered: bool,
    }

    impl TransferSession for PickerSession {
        fn start(&mut self) -> Vec<SessionAction> {
            vec![SessionAction::NeedUploadPaths]
        }

        fn feed_wire(&mut self, _bytes: &[u8]) -> Vec<SessionAction> {
            Vec::new()
        }

        fn submit(&mut self, event: HostEvent) -> Vec<SessionAction> {
            match event {
                HostEvent::UploadPaths(paths) => {
                    self.answered = true;
                    let count = paths.map_or(0, |paths| paths.len()) as u64;
                    vec![
                        SessionAction::WriteWire(format!("COUNT:{count}\n").into_bytes()),
                        SessionAction::Done { paths: Vec::new() },
                    ]
                }
                _ => Vec::new(),
            }
        }

        fn abort_bytes(&mut self) -> Vec<u8> {
            b"ABORT\n".to_vec()
        }
    }

    impl TransferProvider for PickerProvider {
        fn id(&self) -> Arc<str> {
            "picker".into()
        }

        fn display_name(&self) -> Arc<str> {
            "Picker".into()
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                auto_detect: true,
                ..Capabilities::default()
            }
        }

        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: self.id(),
                display_name: self.display_name(),
                capabilities: self.capabilities(),
                default_config: serde_json::json!({ "enabled": true }),
                config_schema: serde_json::json!({}),
            }
        }

        fn configure(&mut self, _config: &serde_json::Value) -> anyhow::Result<()> {
            Ok(())
        }

        fn new_detector(&self) -> Box<dyn TransferDetector> {
            Box::new(LineTriggerDetector::new(b"PICK:GO\n"))
        }

        fn start_session(&self, _offer: &TransferOffer) -> Box<dyn TransferSession> {
            Box::new(PickerSession { answered: false })
        }

        fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
            None
        }
    }

    /// Regression: while the user is inside the picker dialog the idle
    /// watchdog must not fire — an answer arriving after the idle timeout
    /// (but within the picker timeout) must still reach the session.
    #[test]
    fn picker_answer_survives_the_idle_watchdog() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(PickerProvider)],
                vec![],
            )),
            Arc::new(NoopHost),
            TransferPolicy {
                picker_timeout: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(1),
                ..TransferPolicy::default()
            },
        );
        let shared = runtime.shared();
        let (written_tx, written_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
            let _ = written_tx.send(bytes.to_vec());
        }));

        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "picker".into(),
            direction: Some(Direction::Upload),
            remote_names: Vec::new(),
        }));

        // Answer only after the idle timeout (1s) has passed.
        std::thread::sleep(Duration::from_millis(1500));
        shared.answer_upload_paths(Some(vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]));

        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"COUNT:2\n"
        );
        let ui_rx = shared.ui_events();
        // Skip the earlier AwaitingUploadPaths on the way to the outcome.
        let outcome = loop {
            match wait_event(&ui_rx, Duration::from_secs(5)) {
                Some(event @ TransferUiEvent::AwaitingUploadPaths { .. }) => {
                    assert!(matches!(event, TransferUiEvent::AwaitingUploadPaths { .. }));
                }
                other => break other,
            }
        };
        assert!(matches!(outcome, Some(TransferUiEvent::Completed { .. })));
    }

    /// No answer within the picker timeout: the session aborts with the
    /// picker-specific message, not the idle one.
    #[test]
    fn unanswered_picker_times_out_with_picker_message() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(PickerProvider)],
                vec![],
            )),
            Arc::new(NoopHost),
            TransferPolicy {
                picker_timeout: Duration::from_secs(1),
                idle_timeout: Duration::from_secs(30),
                ..TransferPolicy::default()
            },
        );
        let shared = runtime.shared();
        shared.set_wire_writer(Arc::new(|_bytes: &[u8]| {}));

        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "picker".into(),
            direction: Some(Direction::Upload),
            remote_names: Vec::new(),
        }));

        let ui_rx = shared.ui_events();
        let event = loop {
            match wait_event(&ui_rx, Duration::from_secs(10)) {
                Some(TransferUiEvent::AwaitingUploadPaths { .. }) => continue,
                other => break other,
            }
        };
        assert_eq!(
            event,
            Some(TransferUiEvent::Failed {
                reason: "timed out waiting for file selection".into()
            })
        );
        assert!(!shared.is_active());
    }

    #[test]
    fn cancel_sends_abort_and_releases_mux() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(LoopbackProvider)],
                vec![],
            )),
            Arc::new(NoopHost),
            TransferPolicy::default(),
        );
        let shared = runtime.shared();
        let (written_tx, written_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
            let _ = written_tx.send(bytes.to_vec());
        }));

        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "loopback".into(),
            direction: None,
            remote_names: Vec::new(),
        }));
        let _ = written_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        shared.cancel();
        assert_eq!(
            written_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            b"ZTLOOP:ABORT\n"
        );

        let ui_rx = shared.ui_events();
        let event = wait_event(&ui_rx, Duration::from_secs(5));
        assert_eq!(event, Some(TransferUiEvent::Cancelled));
        assert!(!shared.is_active());
    }
}
