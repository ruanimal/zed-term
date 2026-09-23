//! Transfer mux: the tap between the PTY and the ANSI parser, plus the
//! registry and the session driver runtime. See `docs/TRANSFER_EXTENSION.md`
//! §2–§5.

use parking_lot::Mutex;
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

    /// Queue a message for the session driver. Crate-visible so the app can
    /// drive a session directly (tests, manual uploads).
    pub(crate) fn send_driver(&self, msg: DriverMsg) {
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

/// What the IO thread should do with a processed read chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// The first `n` bytes of the chunk are parser input, still in place in
    /// the caller's read buffer. `n == 0` means the chunk was swallowed up to
    /// a match at its first byte.
    Parser(usize),
    /// The chunk was consumed by the tap; nothing in the read buffer belongs
    /// to the parser. Parser-bound bytes may still be queued in `pending`.
    Consumed,
}

/// Byte-flow state, owned by the IO thread (through the `core` lock).
///
/// Invariants:
/// * `hold` is always the tail of the bytes fed to the detectors, and
///   `fed_offset` is how many bytes were fed since the detectors were last
///   reset. Detector trigger ranges are absolute in that coordinate space, so
///   `combined_start = fed_offset_at_chunk_start - hold.len()`.
/// * Parser-bound bytes are never emitted out of order: anything held for a
///   candidate that just resolved is moved to `pending` *before* the chunk
///   that resolved it, and `pending` is always drained before new PTY bytes.
/// * A trigger can never straddle a flush: flushing resets the detectors, so
///   the range they report can never point past the flushed bytes.
struct TapCore {
    registry: Arc<TransferRegistry>,
    detectors: Vec<DetectorEntry>,
    /// Bytes withheld from the parser while a candidate is undecided.
    hold: Vec<u8>,
    /// When the most recent byte was added to `hold`; the release timeout
    /// means "no new byte for `HOLD_RELEASE_TIMEOUT`".
    hold_since: Option<Instant>,
    /// Bytes fed to the detectors since their last reset.
    fed_offset: u64,
    /// Parser-bound bytes that could not be returned in place; always served
    /// before reading more PTY data.
    pending: Vec<u8>,
    /// Whether a session owned the stream on the previous chunk, so the
    /// detectors can be reset once it ends.
    session_owned: bool,
    /// Last byte handed to the parser; a session's intercept can leave the
    /// visible line without its line break (the swallowed trigger was the
    /// line's continuation).
    parser_last: Option<u8>,
    /// Set while a just-ended session's peer may still send ZMODEM's
    /// over-and-out ("OO", sent after the closing exchange): printing it
    /// would put protocol noise right before the next prompt.
    over_and_out: Option<OverAndOut>,
    /// A line break is owed before the next parser bytes, when the visible
    /// line turns out to be partial.
    newline_pending: bool,
}

/// How long the peer's trailing over-and-out stays plausible. It follows the
/// closing exchange within milliseconds; anything later is real output.
const OVER_AND_OUT_TIMEOUT: Duration = Duration::from_secs(1);

struct OverAndOut {
    armed_at: Instant,
    /// Bytes of `OO` matched so far.
    matched: u8,
}

impl TapCore {
    fn take_pending(&mut self, buf: &mut [u8]) -> usize {
        let count = buf.len().min(self.pending.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        count
    }

    /// Queue parser-bound bytes, resolving a just-ended session's tail first:
    /// eat the peer's over-and-out, and give the visible line its missing
    /// line break before whatever comes next (the prompt).
    fn queue_parser_bytes(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        let mut flushed: Vec<u8> = Vec::new();
        if let Some(tail) = self.over_and_out.take() {
            if tail.armed_at.elapsed() >= OVER_AND_OUT_TIMEOUT {
                flushed.extend_from_slice(&b"OO"[..tail.matched as usize]);
            } else {
                let OverAndOut {
                    armed_at,
                    mut matched,
                } = tail;
                while rest.first() == Some(&b'O') && matched < 2 {
                    matched += 1;
                    rest = &rest[1..];
                }
                if matched < 2 && rest.is_empty() {
                    // Could still be the over-and-out; wait for more bytes.
                    self.over_and_out = Some(OverAndOut { armed_at, matched });
                } else if matched < 2 {
                    // Not the over-and-out after all: the matched bytes are
                    // ordinary output.
                    flushed.extend_from_slice(&b"OO"[..matched as usize]);
                }
            }
        }
        if rest.is_empty() && flushed.is_empty() {
            return;
        }
        if self.newline_pending {
            self.newline_pending = false;
            if self.parser_last.is_some_and(|byte| byte != b'\n') {
                self.pending.extend_from_slice(b"\r\n");
            }
        }
        self.pending.extend_from_slice(&flushed);
        self.pending.extend_from_slice(rest);
        self.parser_last = self.pending.last().copied();
    }

    /// Deliver `count` leading bytes of `chunk` to the parser: in place when
    /// nothing needs rewriting, through `pending` when a just-ended session's
    /// tail does.
    fn deliver_parser_prefix(&mut self, chunk: &[u8], count: usize) -> Delivery {
        if self.over_and_out.is_some() || self.newline_pending {
            self.queue_parser_bytes(&chunk[..count]);
            return Delivery::Consumed;
        }
        if let Some(&last) = chunk[..count].last() {
            self.parser_last = Some(last);
        }
        Delivery::Parser(count)
    }

    fn is_holding(&self) -> bool {
        !self.hold.is_empty()
    }

    /// Reset the detectors and their coordinate space. Detector state and
    /// `fed_offset` must only ever be reset together.
    fn reset_detectors(&mut self) {
        for entry in &mut self.detectors {
            entry.detector.reset();
        }
        self.fed_offset = 0;
    }

    /// Release held bytes to the parser and reset detection, so a trigger can
    /// never straddle a flush (§3.2 invariant).
    fn release_hold(&mut self) {
        if !self.hold.is_empty() {
            let hold = std::mem::take(&mut self.hold);
            self.queue_parser_bytes(&hold);
        }
        self.hold_since = None;
        self.reset_detectors();
    }

    /// Process one read chunk.
    fn process(&mut self, chunk: &[u8], shared: &TransferShared) -> Delivery {
        // An active session owns the stream whether or not this terminal has
        // detectors (a manual upload can run without any).
        if shared.is_active() {
            self.session_owned = true;
            shared.divert(chunk);
            return Delivery::Consumed;
        }
        if self.session_owned {
            // The session ended: its match left the detectors mid-candidate
            // and `fed_offset` at zero, so start detection fresh. Its peer
            // may still send the over-and-out, and the intercept swallowed
            // the bytes that ended the visible line.
            self.session_owned = false;
            self.hold.clear();
            self.hold_since = None;
            self.reset_detectors();
            self.over_and_out = Some(OverAndOut {
                armed_at: Instant::now(),
                matched: 0,
            });
            self.newline_pending = true;
        }
        if self.detectors.is_empty() {
            return self.deliver_parser_prefix(chunk, chunk.len());
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
                (self.registry.priority_rank(&offer.provider_id), *index)
            });

        if let Some((_, offer, trigger)) = matched {
            return self.begin_session(offer, trigger, fed_before, chunk, shared);
        }

        if verdicts
            .iter()
            .any(|verdict| matches!(verdict, DetectorVerdict::NeedMore))
        {
            self.fed_offset += chunk.len() as u64;
            self.hold.extend_from_slice(chunk);
            self.hold_since = Some(Instant::now());
            if self.hold.len() > HOLD_WINDOW_BYTES {
                // The window is full with no decision, so no trigger starting
                // inside it can still be completed. Release everything held,
                // in order, and detect again after it. Never re-feed the same
                // bytes: that would recurse without bound (§3.2).
                self.release_hold();
            }
            return Delivery::Consumed;
        }

        // NoMatch: a candidate that just failed must reach the parser before
        // this chunk, so the two are queued together and in order.
        self.reset_detectors();
        if self.hold.is_empty() {
            return self.deliver_parser_prefix(chunk, chunk.len());
        }
        let hold = std::mem::take(&mut self.hold);
        self.hold_since = None;
        self.queue_parser_bytes(&hold);
        self.queue_parser_bytes(chunk);
        Delivery::Consumed
    }

    fn begin_session(
        &mut self,
        offer: TransferOffer,
        trigger: std::ops::Range<u64>,
        fed_before: u64,
        chunk: &[u8],
        shared: &TransferShared,
    ) -> Delivery {
        let combined: Vec<u8> = {
            let mut combined = std::mem::take(&mut self.hold);
            combined.extend_from_slice(chunk);
            combined
        };
        self.hold_since = None;
        self.fed_offset = 0;
        self.session_owned = true;

        // Detector coordinates are absolute since the last reset; `combined`
        // starts at `fed_before - held.len()` in that space.
        let combined_start = fed_before.saturating_sub((combined.len() - chunk.len()) as u64);
        let start = trigger.start.saturating_sub(combined_start) as usize;
        let end = trigger.end.saturating_sub(combined_start) as usize;

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
            return self.deliver_parser_prefix(chunk, start);
        }
        self.queue_parser_bytes(&combined[..start.min(combined.len())]);
        Delivery::Consumed
    }
}

/// A PTY whose read stream can be tapped. Unix ptys hand out a duplicate of
/// the master fd: the same open file description, so the registered
/// readiness and the tap's reads see the same stream.
pub trait TapSource: EventedPty {
    fn tap_reader(&self) -> io::Result<Self::Reader>;

    /// Descriptor the tap can wait on while a trigger candidate is
    /// undecided. `None` when the reader is not a real PTY (tests), in which
    /// case the caller drives further reads itself.
    fn tap_fd(&self) -> Option<i32> {
        None
    }
}

#[cfg(unix)]
impl TapSource for alacritty_terminal::tty::Pty {
    fn tap_reader(&self) -> io::Result<std::fs::File> {
        self.file().try_clone()
    }

    fn tap_fd(&self) -> Option<i32> {
        use std::os::fd::AsRawFd as _;
        Some(self.file().as_raw_fd())
    }
}

/// Wrapper around a PTY that taps its read stream through the transfer mux
/// (§3.1). Writes pass straight through: user input is gated in the
/// foreground `Terminal`, and session protocol bytes take the same ordered
/// path as user input.
pub struct TapPty<P: TapSource> {
    inner: P,
    reader: TapReader<P::Reader>,
}

pub struct TapReader<R> {
    inner: R,
    shared: Arc<TransferShared>,
    /// Duplicate master fd used to wait (bounded) for the rest of a possible
    /// trigger. `None` in tests, which drive the reads themselves.
    fd: Option<i32>,
}

fn would_block() -> io::Error {
    io::Error::from(io::ErrorKind::WouldBlock)
}

impl<R> TapReader<R> {
    pub fn new(inner: R, shared: Arc<TransferShared>, fd: Option<i32>) -> Self {
        TapReader { inner, shared, fd }
    }

    /// Wait up to `timeout` for more PTY bytes while a candidate is held.
    /// `false` means the timeout elapsed with nothing to read.
    #[cfg(unix)]
    fn wait_readable(&self, timeout: Duration) -> io::Result<bool> {
        let Some(fd) = self.fd else {
            return Ok(false);
        };
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        loop {
            let result = unsafe { libc::poll(&mut poll_fd, 1, millis) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            return Ok(result > 0);
        }
    }

    #[cfg(not(unix))]
    fn wait_readable(&self, _timeout: Duration) -> io::Result<bool> {
        Ok(false)
    }
}

impl<R: io::Read> io::Read for TapReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 1. Parser bytes queued by an earlier match or flush.
        // 2. A hold whose time bound already elapsed (the path tests exercise;
        //    production flushes from the wait below instead).
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

        loop {
            match self.inner.read(buf) {
                // End of stream: never drop a candidate that was being held,
                // it can no longer be completed.
                Ok(0) => {
                    let mut core = self.shared.core.lock();
                    if core.is_holding() {
                        core.release_hold();
                        let queued = core.take_pending(buf);
                        if queued > 0 {
                            return Ok(queued);
                        }
                    }
                    return Ok(0);
                }
                Ok(count) => {
                    let delivery = self.shared.core.lock().process(&buf[..count], &self.shared);
                    match delivery {
                        // Passthrough: the bytes are already where the event
                        // loop expects them.
                        Delivery::Parser(parser_bytes) if parser_bytes > 0 => {
                            return Ok(parser_bytes);
                        }
                        // Fully swallowed (a match at the chunk start): this
                        // must surface as WouldBlock, never as `Ok(0)` (EOF).
                        Delivery::Parser(_) => return Err(would_block()),
                        Delivery::Consumed => {
                            let mut core = self.shared.core.lock();
                            let queued = core.take_pending(buf);
                            if queued > 0 {
                                return Ok(queued);
                            }
                            if !core.is_holding() {
                                // Diverted to a session, or a match whose
                                // prefix came from this chunk only.
                                return Err(would_block());
                            }
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if !self.shared.core.lock().is_holding() {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }

            // A candidate is held and the stream has nothing to give right
            // now. The event loop only reads when the PTY signals readiness,
            // so waiting here is what makes the §3.2 time bound real: without
            // it a prefix held just before the remote goes quiet would never
            // be released.
            if self.fd.is_none() {
                // Not a waitable stream: leave the hold for the caller's next
                // read (the expiry check above releases it).
                return Err(would_block());
            }
            if self.wait_readable(HOLD_RELEASE_TIMEOUT)? {
                continue;
            }
            let mut core = self.shared.core.lock();
            core.release_hold();
            let queued = core.take_pending(buf);
            return if queued > 0 {
                Ok(queued)
            } else {
                Err(would_block())
            };
        }
    }
}

impl<P: TapSource> TapPty<P> {
    /// Wrap `pty` with the transfer tap. `shared` comes from
    /// [`TransferRuntime::shared`].
    pub fn new(pty: P, shared: Arc<TransferShared>) -> io::Result<Self> {
        let fd = pty.tap_fd();
        let reader = TapReader::new(pty.tap_reader()?, shared, fd);
        Ok(TapPty { inner: pty, reader })
    }
}

impl<P: TapSource> EventedReadWrite for TapPty<P> {
    type Reader = TapReader<P::Reader>;
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

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
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
                session_owned: false,
                parser_last: None,
                over_and_out: None,
                newline_pending: false,
                registry: registry.clone(),
            }),
            state: AtomicU8::new(0),
            driver_tx: Mutex::new(driver_tx),
            ui_tx,
            ui_rx,
            wire_writer: Mutex::new(None),
            wire_in_flight: AtomicUsize::new(0),
        });

        // A host can outlive one session runtime (a restarted terminal reuses
        // its `TransferSetup`), so start from a clean slate rather than the
        // previous session's destination or staged files.
        host.reset();

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
                // Enforce the per-file cap before the size is announced to the
                // remote, rather than failing halfway through the upload.
                if let Ok(opened) = &result
                    && opened.size > policy.max_file_size
                {
                    fail_session(slot, shared, host, "file size limit exceeded".into());
                    return;
                }
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
                // The provider may not know the final size when it opens a
                // staging file (trzsz sends SIZE only after NAME), so the cap
                // is enforced here against the file position the session
                // reports.
                if offset.saturating_add(data.len() as u64) > policy.max_file_size {
                    fail_session(slot, shared, host, "file size limit exceeded".into());
                    return;
                }
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
            SessionAction::NeedDownloadDir => {
                driver.next_request_id += 1;
                driver.picker_deadline = Some(Instant::now() + policy.picker_timeout);
                shared.emit_ui(TransferUiEvent::AwaitingDownloadDir {
                    request_id: driver.next_request_id,
                });
                host.request_download_dir();
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
    provider_id: Arc<str>,
    /// Longest suffix of the consumed stream that is a proper prefix of
    /// `prefix`; these bytes must not reach the parser yet.
    tail: Vec<u8>,
    candidate_start: Option<u64>,
    line_len: usize,
    /// Bytes fed since the last `reset`. Trigger ranges are reported in this
    /// coordinate space, so only `reset()` may touch it.
    position: u64,
}

impl LineTriggerDetector {
    pub fn new(prefix: &[u8], provider_id: Arc<str>) -> Self {
        LineTriggerDetector {
            prefix: prefix.to_vec(),
            provider_id,
            tail: Vec::new(),
            candidate_start: None,
            line_len: 0,
            position: 0,
        }
    }

    /// Drop an open candidate without touching `position`: the caller (the
    /// mux) still measures trigger ranges from the last `reset`, so a false
    /// alarm in the middle of a batch must not rebase them.
    fn clear_candidate(&mut self) {
        self.tail.clear();
        self.candidate_start = None;
        self.line_len = 0;
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
                        provider_id: self.provider_id.clone(),
                        direction: None,
                        remote_names: Vec::new(),
                    },
                    trigger: start..end,
                };
            }
            if self.line_len > HOLD_WINDOW_BYTES {
                self.clear_candidate();
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
        self.clear_candidate();
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
        Box::new(LineTriggerDetector::new(LOOPBACK_PREFIX, self.id()))
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
        test_shared_with_priority(providers, Vec::new())
    }

    fn test_shared_with_priority(
        providers: Vec<Arc<dyn TransferProvider>>,
        priority: Vec<Arc<str>>,
    ) -> (TransferRuntime, Arc<TransferShared>) {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(providers, priority)),
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
        fn request_download_dir(&self) -> Option<PathBuf> {
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

        let mut tap = TapReader::new(reader, shared, None);
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
        let mut tap = TapReader::new(reader, shared.clone(), None);
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
        let mut tap = TapReader::new(reader, shared.clone(), None);
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

    /// Regression: a candidate that is refuted by the next read used to leave
    /// the held prefix behind while the new chunk was parsed first, so the
    /// terminal showed the bytes out of order (and a later match could compute
    /// a bogus trigger range from the stale hold).
    #[test]
    fn refuted_candidate_reaches_the_parser_in_order() {
        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let reader = ChunkReader {
            chunks: VecDeque::from(vec![b"hello ZTLOOP:ST".to_vec(), b"OP!".to_vec()]),
        };
        let mut tap = TapReader::new(reader, shared.clone(), None);
        let mut buf = [0u8; 4096];

        // The partial prefix is held, so nothing reaches the parser yet.
        assert_would_block(&mut tap, &mut buf);

        // The candidate is refuted: both chunks must arrive, in order.
        let count = tap.read(&mut buf).unwrap();
        assert_eq!(&buf[..count], b"hello ZTLOOP:STOP!");
        assert!(!shared.is_active());
    }

    /// Regression: a read chunk that filled the hold window used to re-feed
    /// itself through `process` and recurse until the stack overflowed. Any
    /// `> HOLD_WINDOW_BYTES` burst ending in a partial trigger could kill the
    /// process.
    #[test]
    fn oversized_hold_window_flushes_without_recursing() {
        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let mut chunk = vec![b'a'; HOLD_WINDOW_BYTES + 16];
        let tail = b"ZTLOOP:ST";
        let start = chunk.len() - tail.len();
        chunk[start..].copy_from_slice(tail);

        let reader = ChunkReader {
            chunks: VecDeque::from(vec![chunk.clone()]),
        };
        let mut tap = TapReader::new(reader, shared.clone(), None);
        let mut buf = [0u8; 8192];
        let mut output = Vec::new();
        loop {
            match tap.read(&mut buf) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&buf[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(output, chunk, "the held window must not be dropped");
        assert!(!shared.is_active());
    }

    /// A provider under a chosen id, so two providers can match the same bytes
    /// and priority adjudication is observable through `Detected`.
    struct AliasedLoopback(Arc<str>);

    impl TransferProvider for AliasedLoopback {
        fn id(&self) -> Arc<str> {
            self.0.clone()
        }

        fn display_name(&self) -> Arc<str> {
            self.0.clone()
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
            Box::new(LineTriggerDetector::new(LOOPBACK_PREFIX, self.id()))
        }

        fn start_session(&self, _offer: &TransferOffer) -> Box<dyn TransferSession> {
            Box::new(LoopbackSession { buffer: Vec::new() })
        }

        fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
            None
        }
    }

    /// Regression: the adjudication used `min_by_key(Reverse(rank))`, which
    /// selects the *worst* rank. With one provider this was invisible; the
    /// configured order must actually win.
    #[test]
    fn priority_order_picks_the_first_configured_provider() {
        let cases: [(Vec<&str>, &str); 2] = [
            (vec!["beta", "alpha"], "beta"),
            (vec!["alpha", "beta"], "alpha"),
        ];
        for (priority, expected) in cases {
            let (_runtime, shared) = test_shared_with_priority(
                vec![
                    Arc::new(AliasedLoopback("alpha".into())),
                    Arc::new(AliasedLoopback("beta".into())),
                ],
                priority.iter().map(|id| Arc::from(*id)).collect(),
            );
            let reader = ChunkReader {
                chunks: VecDeque::from(vec![b"ZTLOOP:START\n".to_vec()]),
            };
            let mut tap = TapReader::new(reader, shared.clone(), None);
            let mut buf = [0u8; 4096];
            let _ = tap.read(&mut buf);

            let ui_rx = shared.ui_events();
            match wait_event(&ui_rx, Duration::from_secs(5)) {
                Some(TransferUiEvent::Detected { provider_id, .. }) => {
                    assert_eq!(&*provider_id, expected, "priority {priority:?}")
                }
                other => panic!("expected {expected} to win, got {other:?}"),
            }
        }
    }

    /// The time bound must not depend on the event loop reading again: with a
    /// real, waitable stream, a quiet PTY after a partial trigger releases the
    /// held prefix instead of hiding it forever.
    #[cfg(unix)]
    #[test]
    fn quiet_stream_releases_the_held_prefix() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        writer.write_all(b"ZTLOOP:ST").unwrap();

        let fd = reader.as_raw_fd();
        let mut tap = TapReader::new(reader, shared.clone(), Some(fd));
        let mut buf = [0u8; 4096];
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

    /// After a session ends the stream still carries its tail: the peer's
    /// over-and-out ("OO", protocol noise) and, when the intercept left the
    /// visible line partial, a missing line break before the next prompt.
    #[test]
    fn post_session_tail_is_eaten_and_the_line_ended() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let (written_tx, _written_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
            let _ = written_tx.send(bytes.to_vec());
        }));

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        let fd = reader.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut tap = TapReader::new(reader, shared.clone(), Some(fd));
        let mut buf = [0u8; 4096];
        let mut shown = Vec::new();
        let mut drain = |shown: &mut Vec<u8>| match tap.read(&mut buf) {
            Ok(count) => shown.extend_from_slice(&buf[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("tap read failed: {error}"),
        };

        // A partial visible line first: a ZMODEM banner ends without one.
        writer.write_all(b"banner.").unwrap();
        drain(&mut shown);
        assert_eq!(shown, b"banner.");

        // The trigger line (swallowed) and what follows it (the session's),
        // then the completion input.
        writer.write_all(b"ZTLOOP:START here\nnoise").unwrap();
        writer.write_all(LOOPBACK_END).unwrap();
        for _ in 0..4 {
            drain(&mut shown);
        }
        let ui_rx = shared.ui_events();
        loop {
            match wait_event(&ui_rx, Duration::from_secs(5)) {
                Some(TransferUiEvent::Completed { .. }) => break,
                Some(_) => {}
                None => panic!("the session must complete before its tail arrives"),
            }
        }

        // The peer's over-and-out and the prompt after it.
        writer.write_all(b"OOprompt> ").unwrap();
        for _ in 0..4 {
            drain(&mut shown);
        }
        assert_eq!(shown, b"banner.\r\nprompt> ");
    }

    /// A complete line gets no line break, and a lone leading `O` that is
    /// not the over-and-out is put back untouched.
    #[test]
    fn post_session_tail_keeps_a_complete_line_and_a_stray_o() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let (_runtime, shared) = test_shared(vec![Arc::new(LoopbackProvider)]);
        let (written_tx, _written_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
            let _ = written_tx.send(bytes.to_vec());
        }));

        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        let fd = reader.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut tap = TapReader::new(reader, shared.clone(), Some(fd));
        let mut buf = [0u8; 4096];
        let mut shown = Vec::new();

        writer.write_all(b"done\n").unwrap();
        writer.write_all(b"ZTLOOP:START here\n").unwrap();
        writer.write_all(LOOPBACK_END).unwrap();
        for _ in 0..4 {
            match tap.read(&mut buf) {
                Ok(count) => shown.extend_from_slice(&buf[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("tap read failed: {error}"),
            }
        }
        assert_eq!(shown, b"done\n");
        let ui_rx = shared.ui_events();
        loop {
            match wait_event(&ui_rx, Duration::from_secs(5)) {
                Some(TransferUiEvent::Completed { .. }) => break,
                Some(_) => {}
                None => panic!("the session must complete before its tail arrives"),
            }
        }

        writer.write_all(b"Ola!").unwrap();
        for _ in 0..4 {
            match tap.read(&mut buf) {
                Ok(count) => shown.extend_from_slice(&buf[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("tap read failed: {error}"),
            }
        }
        assert_eq!(shown, b"done\nOla!");
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
            Box::new(LineTriggerDetector::new(b"PICK:GO\n", self.id()))
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

    /// Host that accepts every file operation, so a test observes driver
    /// policy rather than host IO.
    struct PermissiveHost {
        read_size: u64,
    }

    impl TransferHost for PermissiveHost {
        fn open_read(&self, _path: &std::path::Path) -> io::Result<u64> {
            Ok(self.read_size)
        }
        fn read_chunk(&self, _offset: u64, _max_len: usize) -> io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn open_write(&self, _remote_name: &str, _size: Option<u64>) -> io::Result<String> {
            Ok("big.bin".to_string())
        }
        fn write_chunk(&self, _offset: u64, _data: &[u8]) -> io::Result<()> {
            Ok(())
        }
        fn close_write(&self) -> io::Result<PathBuf> {
            Ok(PathBuf::from("/tmp/big.bin"))
        }
        fn commit(&self) -> io::Result<PathBuf> {
            Ok(PathBuf::from("/tmp/big.bin"))
        }
        fn set_destination(&self, _destination: &std::path::Path) {}
        fn request_upload_paths(&self) -> Option<Vec<PathBuf>> {
            None
        }
        fn request_download_dir(&self) -> Option<PathBuf> {
            None
        }
        fn discard_staged(&self) {}
    }

    /// A download that opens a staging file with no declared size and then
    /// writes past the per-file cap (the trzsz NAME-before-SIZE ordering).
    struct OversizedWriteProvider;

    struct OversizedWriteSession {
        wrote: bool,
    }

    impl TransferSession for OversizedWriteSession {
        fn start(&mut self) -> Vec<SessionAction> {
            vec![SessionAction::OpenWrite {
                remote_name: "big.bin".into(),
                size: None,
            }]
        }
        fn feed_wire(&mut self, _bytes: &[u8]) -> Vec<SessionAction> {
            Vec::new()
        }
        fn submit(&mut self, event: HostEvent) -> Vec<SessionAction> {
            match event {
                HostEvent::FileOpened(Ok(_)) if !self.wrote => {
                    self.wrote = true;
                    vec![SessionAction::WriteFile {
                        offset: 0,
                        data: vec![0u8; 64],
                    }]
                }
                _ => Vec::new(),
            }
        }
        fn abort_bytes(&mut self) -> Vec<u8> {
            Vec::new()
        }
    }

    impl TransferProvider for OversizedWriteProvider {
        fn id(&self) -> Arc<str> {
            "oversized-write".into()
        }
        fn display_name(&self) -> Arc<str> {
            "Oversized write".into()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: self.id(),
                display_name: self.display_name(),
                capabilities: self.capabilities(),
                default_config: serde_json::json!({}),
                config_schema: serde_json::json!({}),
            }
        }
        fn configure(&mut self, _config: &serde_json::Value) -> anyhow::Result<()> {
            Ok(())
        }
        fn new_detector(&self) -> Box<dyn TransferDetector> {
            Box::new(LineTriggerDetector::new(b"NEVER", self.id()))
        }
        fn start_session(&self, _offer: &TransferOffer) -> Box<dyn TransferSession> {
            Box::new(OversizedWriteSession { wrote: false })
        }
        fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
            None
        }
    }

    /// Regression: the cap was checked only against the size a provider
    /// declared at `OpenWrite`; trzsz declares it later, so a remote SIZE
    /// could exceed `max_file_size` unchecked.
    #[test]
    fn download_write_past_the_file_cap_fails_the_session() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(OversizedWriteProvider)],
                vec![],
            )),
            Arc::new(PermissiveHost { read_size: 0 }),
            TransferPolicy {
                max_file_size: 8,
                ..TransferPolicy::default()
            },
        );
        let shared = runtime.shared();
        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "oversized-write".into(),
            direction: Some(Direction::Download),
            remote_names: Vec::new(),
        }));
        let ui_rx = shared.ui_events();
        let event = wait_event(&ui_rx, Duration::from_secs(5));
        assert!(
            matches!(event, Some(TransferUiEvent::Failed { ref reason }) if reason.contains("file size limit")),
            "expected a size-cap failure, got {event:?}"
        );
        assert!(!shared.is_active());
    }

    struct OversizedReadProvider;

    struct OversizedReadSession;

    impl TransferSession for OversizedReadSession {
        fn start(&mut self) -> Vec<SessionAction> {
            vec![SessionAction::OpenRead {
                path: PathBuf::from("/tmp/huge.bin"),
            }]
        }
        fn feed_wire(&mut self, _bytes: &[u8]) -> Vec<SessionAction> {
            Vec::new()
        }
        fn submit(&mut self, _event: HostEvent) -> Vec<SessionAction> {
            Vec::new()
        }
        fn abort_bytes(&mut self) -> Vec<u8> {
            Vec::new()
        }
    }

    impl TransferProvider for OversizedReadProvider {
        fn id(&self) -> Arc<str> {
            "oversized-read".into()
        }
        fn display_name(&self) -> Arc<str> {
            "Oversized read".into()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn manifest(&self) -> ProviderManifest {
            ProviderManifest {
                id: self.id(),
                display_name: self.display_name(),
                capabilities: self.capabilities(),
                default_config: serde_json::json!({}),
                config_schema: serde_json::json!({}),
            }
        }
        fn configure(&mut self, _config: &serde_json::Value) -> anyhow::Result<()> {
            Ok(())
        }
        fn new_detector(&self) -> Box<dyn TransferDetector> {
            Box::new(LineTriggerDetector::new(b"NEVER", self.id()))
        }
        fn start_session(&self, _offer: &TransferOffer) -> Box<dyn TransferSession> {
            Box::new(OversizedReadSession)
        }
        fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
            None
        }
    }

    /// Regression: an upload larger than the cap used to start, stream up to
    /// the cap, and only then fail.
    #[test]
    fn upload_is_rejected_before_announcing_an_oversized_file() {
        let runtime = TransferRuntime::new(
            Arc::new(TransferRegistry::new(
                vec![Arc::new(OversizedReadProvider)],
                vec![],
            )),
            Arc::new(PermissiveHost { read_size: 1024 }),
            TransferPolicy {
                max_file_size: 8,
                ..TransferPolicy::default()
            },
        );
        let shared = runtime.shared();
        shared.send_driver(DriverMsg::StartSession(TransferOffer {
            provider_id: "oversized-read".into(),
            direction: Some(Direction::Upload),
            remote_names: Vec::new(),
        }));
        let ui_rx = shared.ui_events();
        let event = wait_event(&ui_rx, Duration::from_secs(5));
        assert!(
            matches!(event, Some(TransferUiEvent::Failed { ref reason }) if reason.contains("file size limit")),
            "expected a size-cap failure, got {event:?}"
        );
        assert!(!shared.is_active());
    }
}
