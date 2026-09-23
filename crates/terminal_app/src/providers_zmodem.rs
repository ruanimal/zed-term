//! ZMODEM (`rz`/`sz`) provider: CRC-validated autostart detection plus a
//! session built on the [`zmodem2`] crate's caller-driven state machines
//! (Phase 3 of docs/TRANSFER_EXTENSION.md).
//!
//! Direction comes from the header that triggers the session, which is what
//! the peers actually put on the wire: a remote `sz` opens with `ZRQINIT`
//! (it wants to send, so the local end receives) while a remote `rz` opens
//! with `ZRINIT` (it wants to receive, so the local end sends). Both are hex
//! headers, and §12 requires the whole header plus its CRC before a trigger
//! counts, so both the direction and the "is this really ZMODEM" question are
//! answered by the same twenty bytes.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use transfer_core::{
    Capabilities, DetectorVerdict, Direction, HostEvent, ProviderManifest, SessionAction,
    TransferDetector, TransferOffer, TransferProvider, TransferSession,
};
use zmodem2::{Action, Event as WireEvent, FileInfo, Position, Receiver, Sender};

/// `ZPAD`: padding byte that starts a frame.
const ZPAD: u8 = b'*';
/// `ZDLE`: escape byte following the padding, and the abort character (`CAN`).
const ZDLE: u8 = 0x18;
/// `ZHEX`: frame indicator for a hex header.
const ZHEX: u8 = b'B';
/// A hex header carries the frame type, four data bytes and a CRC-16, two hex
/// digits per byte.
const HEADER_HEX_DIGITS: usize = 14;
/// Frame type of `ZRQINIT`, sent by a peer that wants to send files.
const ZRQINIT: u8 = 0;
/// Frame type of `ZRINIT`, sent by a peer that wants to receive files.
const ZRINIT: u8 = 1;
/// Consecutive `CAN` bytes that mean the peer cancelled. A literal `ZDLE` in
/// payload data is always escaped, so a raw run this long is unambiguous.
const CAN_ABORT_RUN: u8 = 5;
/// ZMODEM's canonical cancel: eight `CAN` then eight backspace, which the
/// peer recognises even in the middle of a frame (§3.4).
const ABORT_BURST: [u8; 16] = [
    ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, ZDLE, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];
/// Upper bound on wire bytes held back because the state machine is blocked
/// on its own output or on a host round trip (§3.5).
const MAX_SESSION_BUFFER: usize = 4 * 1024 * 1024;
/// Subpackets the sender may stream before waiting for an acknowledgement,
/// once the peer advertises nonstop I/O. The peer's own answers are the
/// pacing signal, so a larger window only removes round trips; memory stays
/// bounded because one subpacket is queued at a time.
const STREAMING_WINDOW: usize = 64;
/// Progress events are throttled: a subpacket is at most 1 KiB, so reporting
/// every chunk would push millions of messages through an unbounded channel
/// for a large file.
const PROGRESS_INTERVAL: u64 = 64 * 1024;

/// CRC-16/XMODEM (poly `0x1021`, zero init), the checksum a ZMODEM header
/// carries over its frame type and four data bytes.
fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn abort_burst() -> Vec<u8> {
    ABORT_BURST.to_vec()
}

// ─── Trigger detection ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// No candidate in progress.
    Idle,
    /// One or more `ZPAD` seen, waiting for `ZDLE`.
    Pad,
    /// `ZPAD ZPAD ZDLE` seen; the next byte selects the header encoding.
    Encoding,
    /// `ZPAD ZPAD ZDLE ZHEX` seen; collecting the header's hex digits.
    Hex,
}

/// Trigger detector for ZMODEM autostart headers (`**\x18B<14 hex digits>`).
///
/// A single `ZPAD` starts a candidate, so its bytes are held across reads;
/// everything else passes straight through. Matching needs the complete
/// header *and* a valid CRC-16 over a session-starting frame type, which is
/// what keeps binary `cat` output from starting a session (§12).
pub struct ZmodemDetector {
    /// Bytes fed since the last `reset`; trigger ranges are reported here.
    position: u64,
    /// Absolute index of the candidate's first `ZPAD`.
    candidate_start: Option<u64>,
    state: Scan,
    hex: [u8; HEADER_HEX_DIGITS],
    digits: usize,
    /// End of an `rz\r` autostart preamble inside the current detection
    /// window: `sz` writes one before its first header, and swallowing it
    /// keeps the terminal free of `rz` junk. Only remembered while those
    /// bytes cannot have reached the parser yet (a `reset` drops it).
    preamble_end: Option<u64>,
    /// Bytes of that preamble matched so far.
    preamble_run: u8,
}

impl Default for ZmodemDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ZmodemDetector {
    pub fn new() -> Self {
        ZmodemDetector {
            position: 0,
            candidate_start: None,
            state: Scan::Idle,
            hex: [0; HEADER_HEX_DIGITS],
            digits: 0,
            preamble_end: None,
            preamble_run: 0,
        }
    }

    /// Abandon a candidate, then read the byte that broke it as a fresh one:
    /// it may itself start a header (a run of `ZPAD`s is valid padding).
    fn restart(&mut self, byte: u8, index: u64) -> DetectorVerdict {
        self.state = Scan::Idle;
        self.candidate_start = None;
        self.digits = 0;
        self.feed_idle(byte, index)
    }

    fn feed_idle(&mut self, byte: u8, index: u64) -> DetectorVerdict {
        self.track_preamble(byte, index + 1);
        if byte == ZPAD {
            self.candidate_start = Some(index);
            self.state = Scan::Pad;
            return DetectorVerdict::NeedMore;
        }
        DetectorVerdict::NoMatch
    }

    fn track_preamble(&mut self, byte: u8, end: u64) {
        if byte == b'r' {
            self.preamble_run = 1;
        } else if self.preamble_run == 1 && byte == b'z' {
            self.preamble_run = 2;
        } else if self.preamble_run == 2 && matches!(byte, b'\r' | b'\n') {
            self.preamble_end = Some(end);
            self.preamble_run = 0;
        } else {
            self.preamble_run = 0;
        }
    }

    /// The frame type, once the header's CRC-16 checks out.
    fn verified_frame(&self) -> Option<u8> {
        let digit = |index: usize| self.hex.get(index).copied().unwrap_or(0);
        let frame = (digit(0) << 4) | digit(1);
        let mut payload = [0u8; 5];
        payload[0] = frame;
        for index in 0..4 {
            payload[index + 1] = (digit(2 + index * 2) << 4) | digit(3 + index * 2);
        }
        let expected =
            u16::from_be_bytes([(digit(10) << 4) | digit(11), (digit(12) << 4) | digit(13)]);
        (crc16_xmodem(&payload) == expected).then_some(frame)
    }

    fn finish_header(&mut self, index: u64) -> DetectorVerdict {
        let candidate_start = self.candidate_start.take();
        self.state = Scan::Idle;
        self.digits = 0;
        let Some(start) = candidate_start else {
            return DetectorVerdict::NoMatch;
        };
        let direction = match self.verified_frame() {
            Some(ZRQINIT) => Direction::Download,
            Some(ZRINIT) => Direction::Upload,
            // A valid header that does not start a session (ZFILE, ZDATA,
            // ZFIN, a checksum that does not match, ...) is not a trigger.
            _ => return DetectorVerdict::NoMatch,
        };
        let trigger_start = if self.preamble_end == Some(start) {
            start.saturating_sub(3)
        } else {
            start
        };
        DetectorVerdict::Matched {
            offer: TransferOffer {
                provider_id: "zmodem".into(),
                direction: Some(direction),
                remote_names: Vec::new(),
            },
            trigger: trigger_start..index + 1,
        }
    }
}

impl TransferDetector for ZmodemDetector {
    fn feed(&mut self, byte: u8) -> DetectorVerdict {
        let index = self.position;
        self.position += 1;
        match self.state {
            Scan::Idle => self.feed_idle(byte, index),
            Scan::Pad => match byte {
                ZPAD => DetectorVerdict::NeedMore,
                ZDLE => {
                    self.state = Scan::Encoding;
                    DetectorVerdict::NeedMore
                }
                _ => self.restart(byte, index),
            },
            // Only hex headers start a session, which is what every peer that
            // speaks `ZRQINIT`/`ZRINIT` uses (and what the spec requires for
            // those two frames). A binary header is ignored, not guessed at.
            Scan::Encoding => match byte {
                ZHEX => {
                    self.state = Scan::Hex;
                    self.digits = 0;
                    DetectorVerdict::NeedMore
                }
                _ => self.restart(byte, index),
            },
            Scan::Hex => {
                let Some(value) = hex_digit(byte) else {
                    return self.restart(byte, index);
                };
                if let Some(slot) = self.hex.get_mut(self.digits) {
                    *slot = value;
                }
                self.digits += 1;
                if self.digits < HEADER_HEX_DIGITS {
                    return DetectorVerdict::NeedMore;
                }
                self.finish_header(index)
            }
        }
    }

    fn reset(&mut self) {
        *self = ZmodemDetector::new();
    }
}

// ─── Session ───────────────────────────────────────────────────────────────

/// A file the peer announced with `ZFILE`.
struct AnnouncedFile {
    name: String,
    size: Option<u64>,
}

/// Protocol work that must wait until the state machine has no queued output:
/// pushing it earlier would put an `fsync` (or a picker answer) in front of a
/// reply the peer is waiting for.
enum Deferred {
    /// A host round trip (close or commit a staged download).
    Action(SessionAction),
    /// Upload: open the next chosen file.
    OpenNextUpload,
    /// Upload: end the session after the last file.
    FinishUpload,
}

enum Machine {
    /// The local end receives (the peer runs `sz`).
    Receiver(Box<Receiver>),
    /// The local end sends (the peer runs `rz`).
    Sender(Box<Sender>),
    /// The state machine could not be built (its fixed buffers do not fit the
    /// handshake); the session reports this instead of hanging.
    Broken(String),
}

/// One `poll()` result, with the borrowed payload copied out.
enum Step {
    Wire(Vec<u8>),
    WriteFile(Vec<u8>),
    ReadFile { offset: u64, max_len: usize },
    Event(WireEventOwned),
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WireEventOwned {
    FileStarted { name: String, size: Option<u64> },
    FileCompleted,
    SessionCompleted,
    Aborted,
}

fn own_event(event: WireEvent<'_>) -> Option<WireEventOwned> {
    match event {
        WireEvent::FileStarted(info) => Some(WireEventOwned::FileStarted {
            name: String::from_utf8_lossy(info.name).into_owned(),
            size: info.size.map(|size| u64::from(size.get())),
        }),
        WireEvent::FileCompleted => Some(WireEventOwned::FileCompleted),
        WireEvent::SessionCompleted => Some(WireEventOwned::SessionCompleted),
        WireEvent::Aborted => Some(WireEventOwned::Aborted),
        // `Event` is non-exhaustive; a future variant is not actionable here.
        _ => None,
    }
}

/// The ZMODEM protocol session. One instance per transfer.
pub struct ZmodemSession {
    machine: Machine,
    /// Upload: the files the user chose.
    paths: Vec<PathBuf>,
    /// Upload: index of the file being sent.
    file_index: usize,
    /// Download: the user confirmed where downloads land, so the withheld
    /// handshake byte may go out.
    destination_confirmed: bool,
    /// Download: file announced by the peer, waiting for its staging file.
    announced: Option<AnnouncedFile>,
    /// Bytes of the current file already read (upload) or written (download).
    file_offset: u64,
    /// Announced size of the current file, when the protocol carries one.
    file_total: Option<u64>,
    /// `file_offset` last reported to the UI (progress throttling).
    reported_bytes: u64,
    /// Length of the download chunk a pending `FileWritten` answer refers to.
    inflight_write: usize,
    /// Wire bytes the state machine could not consume while it had output
    /// queued.
    pending_wire: Vec<u8>,
    /// Work queued for the next idle moment.
    deferred: VecDeque<Deferred>,
    /// Consecutive `CAN` bytes seen on the wire.
    can_run: u8,
    /// The protocol reported the session complete; `Done` follows once the
    /// queued reply bytes and file commits have gone through.
    protocol_complete: bool,
    /// Local files the user ended up with (sent sources / saved paths).
    completed: Vec<PathBuf>,
    finished: bool,
}

impl ZmodemSession {
    pub fn new(direction: Direction) -> Self {
        let machine = match direction {
            Direction::Download => match Receiver::with_flow_control(0, true) {
                Ok(mut receiver) => {
                    // The remote must not be told to start before the user
                    // confirmed where the file lands (§7.1).
                    receiver.set_manual_file_accept(true);
                    Machine::Receiver(Box::new(receiver))
                }
                Err(error) => Machine::Broken(format!("cannot start the ZMODEM receiver: {error}")),
            },
            Direction::Upload => match Sender::new() {
                Ok(mut sender) => {
                    sender.set_streaming_window(STREAMING_WINDOW);
                    // Escape every control character whatever the peer's
                    // ZRINIT asks for: lrzsz 0.12.20 leaves IEXTEN on when
                    // it sets its tty raw (`rbsb.c` `io_mode`), so VLNEXT
                    // and the other extended editing characters silently eat
                    // raw control bytes out of the stream — a bare 0x16 in a
                    // CRC-32 killed the whole subpacket at `rz`. The escaped
                    // form is decoded by every receiver.
                    sender.set_escape_control(true);
                    Machine::Sender(Box::new(sender))
                }
                Err(error) => Machine::Broken(format!("cannot start the ZMODEM sender: {error}")),
            },
        };
        ZmodemSession {
            machine,
            paths: Vec::new(),
            file_index: 0,
            destination_confirmed: false,
            announced: None,
            file_offset: 0,
            file_total: None,
            reported_bytes: 0,
            inflight_write: 0,
            pending_wire: Vec::new(),
            deferred: VecDeque::new(),
            can_run: 0,
            protocol_complete: false,
            completed: Vec::new(),
            finished: false,
        }
    }

    fn receiver_mut(&mut self) -> Option<&mut Receiver> {
        match &mut self.machine {
            Machine::Receiver(receiver) => Some(receiver),
            Machine::Sender(_) | Machine::Broken(_) => None,
        }
    }

    fn sender_mut(&mut self) -> Option<&mut Sender> {
        match &mut self.machine {
            Machine::Sender(sender) => Some(sender),
            Machine::Receiver(_) | Machine::Broken(_) => None,
        }
    }

    fn fail(&mut self, reason: impl Into<String>) -> Vec<SessionAction> {
        self.finished = true;
        vec![SessionAction::Failed(reason.into())]
    }

    /// The user declined a picker: tell the peer instead of leaving it to
    /// time out, and report a cancel so the UI never shows "completed" for a
    /// transfer that sent nothing (§3.4).
    fn cancel(&mut self) -> Vec<SessionAction> {
        self.finished = true;
        vec![
            SessionAction::WriteWire(abort_burst()),
            SessionAction::Cancelled,
        ]
    }

    fn step(&mut self) -> Step {
        match &mut self.machine {
            Machine::Receiver(receiver) => match receiver.poll() {
                Action::WriteWire(bytes) => Step::Wire(bytes.to_vec()),
                Action::WriteFile(bytes) => Step::WriteFile(bytes.to_vec()),
                Action::Event(event) => match own_event(event) {
                    Some(event) => Step::Event(event),
                    None => Step::Idle,
                },
                // A receiver never reads local files.
                _ => Step::Idle,
            },
            Machine::Sender(sender) => match sender.poll() {
                Action::WriteWire(bytes) => Step::Wire(bytes.to_vec()),
                Action::ReadFile { offset, max_len } => Step::ReadFile {
                    offset: u64::from(offset.get()),
                    max_len,
                },
                Action::Event(event) => match own_event(event) {
                    Some(event) => Step::Event(event),
                    None => Step::Idle,
                },
                // A sender never writes local files.
                _ => Step::Idle,
            },
            Machine::Broken(_) => Step::Idle,
        }
    }

    /// Bytes the machine could not consume earlier, retried whenever it has
    /// drained its output. The returned count is what it accepted now.
    fn submit_pending_wire(&mut self) -> Result<usize, String> {
        if self.pending_wire.is_empty() {
            return Ok(0);
        }
        let result = match &mut self.machine {
            Machine::Receiver(receiver) => receiver.submit_wire(&self.pending_wire),
            Machine::Sender(sender) => sender.submit_wire(&self.pending_wire),
            Machine::Broken(_) => return Ok(0),
        };
        result.map_err(|error| format!("ZMODEM protocol error: {error}"))
    }

    fn wire_written(&mut self, length: usize) {
        match &mut self.machine {
            Machine::Receiver(receiver) => receiver.wire_written(length),
            Machine::Sender(sender) => sender.wire_written(length),
            Machine::Broken(_) => {}
        }
    }

    fn progress_action(&self) -> SessionAction {
        // The batch size is only known for uploads: ZMODEM announces one file
        // at a time, so a download reports a single file and the bar shows
        // bytes rather than a misleading "file 2/2".
        let (file_index, file_count) = match self.machine {
            Machine::Sender(_) => (self.file_index, self.paths.len().max(1)),
            Machine::Receiver(_) | Machine::Broken(_) => (0, 1),
        };
        SessionAction::Progress {
            file_index,
            file_count,
            bytes_done: self.file_offset,
            bytes_total: self.file_total,
        }
    }

    /// Local files the session ends up reporting: what was sent for an
    /// upload, what was committed for a download.
    fn resulting_paths(&self) -> Vec<PathBuf> {
        match self.machine {
            Machine::Sender(_) => self.paths.clone(),
            Machine::Receiver(_) | Machine::Broken(_) => self.completed.clone(),
        }
    }

    fn maybe_progress(&mut self, force: bool) -> Option<SessionAction> {
        if !force && self.file_offset.saturating_sub(self.reported_bytes) < PROGRESS_INTERVAL {
            return None;
        }
        self.reported_bytes = self.file_offset;
        Some(self.progress_action())
    }

    /// Run the state machine until it needs the host (file IO, a picker) or
    /// has nothing left to do, turning its actions into session actions.
    fn pump(&mut self) -> Vec<SessionAction> {
        let mut actions = Vec::new();
        while !self.finished {
            match self.step() {
                Step::Wire(bytes) => {
                    let length = bytes.len();
                    self.wire_written(length);
                    actions.push(SessionAction::WriteWire(bytes));
                }
                Step::WriteFile(data) => {
                    self.inflight_write = data.len();
                    actions.push(SessionAction::WriteFile {
                        offset: self.file_offset,
                        data,
                    });
                    break;
                }
                Step::ReadFile { offset, max_len } => {
                    actions.push(SessionAction::ReadFile { offset, max_len });
                    break;
                }
                Step::Event(event) => self.apply_event(event, &mut actions),
                Step::Idle => {
                    match self.submit_pending_wire() {
                        Ok(consumed) if consumed > 0 => {
                            self.pending_wire.drain(..consumed);
                            continue;
                        }
                        Ok(_) => {}
                        Err(reason) => {
                            actions.push(SessionAction::Failed(reason));
                            self.finished = true;
                            break;
                        }
                    }
                    match self.deferred.pop_front() {
                        Some(Deferred::Action(action)) => {
                            actions.push(action);
                            break;
                        }
                        Some(Deferred::OpenNextUpload) => match self.paths.get(self.file_index) {
                            Some(path) => {
                                actions.push(SessionAction::OpenRead { path: path.clone() });
                                break;
                            }
                            None => {
                                actions.push(SessionAction::Failed("no file to send".to_string()));
                                self.finished = true;
                                break;
                            }
                        },
                        Some(Deferred::FinishUpload) => {
                            let outcome = match self.sender_mut() {
                                Some(sender) => sender.finish().map_err(|error| error.to_string()),
                                None => Err("no sender".to_string()),
                            };
                            if let Err(error) = outcome {
                                actions.push(SessionAction::Failed(format!(
                                    "cannot finish the ZMODEM session: {error}"
                                )));
                                self.finished = true;
                                break;
                            }
                            continue;
                        }
                        None => {}
                    }
                    if let Some(announced) = self.announced.take() {
                        if self.destination_confirmed {
                            actions.push(SessionAction::OpenWrite {
                                remote_name: announced.name,
                                size: announced.size,
                            });
                            break;
                        }
                        // No destination yet: the remote is still waiting for
                        // our handshake byte, so the announcement can wait.
                        self.announced = Some(announced);
                    }
                    if self.protocol_complete {
                        actions.push(SessionAction::Done {
                            paths: self.resulting_paths(),
                        });
                        self.finished = true;
                    }
                    break;
                }
            }
        }
        actions
    }

    fn apply_event(&mut self, event: WireEventOwned, actions: &mut Vec<SessionAction>) {
        match event {
            WireEventOwned::FileStarted { name, size } => {
                self.file_offset = 0;
                self.file_total = size;
                self.announced = Some(AnnouncedFile { name, size });
                self.maybe_progress(true).into_iter().for_each(|progress| {
                    actions.push(progress);
                });
            }
            WireEventOwned::FileCompleted => {
                self.file_offset = self.file_total.unwrap_or(self.file_offset);
                self.maybe_progress(true).into_iter().for_each(|progress| {
                    actions.push(progress);
                });
                match self.machine {
                    // The reply that keeps the peer sending must not queue
                    // behind a disk sync, so closing waits for idle.
                    Machine::Receiver(_) => {
                        self.deferred
                            .push_back(Deferred::Action(SessionAction::CloseFile));
                    }
                    Machine::Sender(_) => {
                        self.file_index += 1;
                        if self.file_index < self.paths.len() {
                            self.deferred.push_back(Deferred::OpenNextUpload);
                        } else {
                            self.deferred.push_back(Deferred::FinishUpload);
                        }
                    }
                    Machine::Broken(_) => {}
                }
            }
            WireEventOwned::SessionCompleted => self.protocol_complete = true,
            WireEventOwned::Aborted => {
                actions.push(SessionAction::Failed(
                    "the remote aborted the transfer".to_string(),
                ));
                self.finished = true;
            }
        }
    }

    /// A download's staging file is open: ask the peer to start at offset 0.
    fn accept_download(&mut self) -> Vec<SessionAction> {
        let outcome = match self.receiver_mut() {
            Some(receiver) => receiver
                .accept_file_at(0)
                .map_err(|error| error.to_string()),
            None => Err("no receiver".to_string()),
        };
        if let Err(error) = outcome {
            return self.fail(format!("cannot start receiving the file: {error}"));
        }
        let mut actions = Vec::new();
        actions.extend(self.maybe_progress(true));
        actions.extend(self.pump());
        actions
    }

    /// Offer `size` bytes of the current upload file to the sender.
    fn start_upload_file(&mut self, size: u64) -> Vec<SessionAction> {
        let Some(path) = self.paths.get(self.file_index).cloned() else {
            return self.fail("no file to send");
        };
        let Some(name) = path
            .file_name()
            .map(|name| name.as_encoded_bytes().to_vec())
        else {
            return self.fail(format!("{} has no file name", path.display()));
        };
        // ZMODEM positions are 32-bit; a bigger file cannot be announced.
        if size > u64::from(u32::MAX) {
            return self.fail(format!(
                "{} is larger than ZMODEM's 4 GiB limit",
                path.display()
            ));
        }
        self.file_offset = 0;
        self.file_total = Some(size);
        let offered = match self.sender_mut() {
            Some(sender) => sender
                .start_file(FileInfo::new(&name, Some(Position::new(size as u32))))
                .map_err(|error| error.to_string()),
            None => Err("no sender".to_string()),
        };
        if let Err(error) = offered {
            return self.fail(format!("cannot offer the file: {error}"));
        }
        let mut actions = Vec::new();
        actions.extend(self.maybe_progress(true));
        actions.extend(self.pump());
        actions
    }

    /// Offer `data`, the bytes the sender's last read request asked for at
    /// `read_offset`, to it. A `ZRPOS` from the peer can move that offset
    /// backwards mid-file (error recovery resends from the last good byte),
    /// so progress tracks the request instead of counting bytes upwards.
    fn submit_upload_data(&mut self, read_offset: u64, data: &[u8]) -> Vec<SessionAction> {
        if data.is_empty() {
            return self.fail("the file ended before its announced size");
        }
        if read_offset.saturating_add(data.len() as u64) > self.file_total.unwrap_or(u64::MAX) {
            return self.fail("the file changed while it was being sent");
        }
        let submitted = match self.sender_mut() {
            Some(sender) => sender.submit_file(data).map_err(|error| error.to_string()),
            None => Err("no sender".to_string()),
        };
        if let Err(error) = submitted {
            return self.fail(format!("cannot send file data: {error}"));
        }
        self.file_offset = read_offset.saturating_add(data.len() as u64);
        let mut actions = Vec::new();
        actions.extend(self.maybe_progress(false));
        actions.extend(self.pump());
        actions
    }

    fn on_file_written(&mut self) -> Vec<SessionAction> {
        let length = std::mem::take(&mut self.inflight_write);
        // Every `FileWritten` answers one of our `WriteFile` actions, so a
        // zero here would mean the state machine is done with that chunk;
        // acknowledging it again would re-send it forever.
        if length == 0 {
            log::warn!("zmodem: file write answer without a pending chunk");
            return Vec::new();
        }
        let result = match self.receiver_mut() {
            Some(receiver) => receiver
                .file_written(length)
                .map_err(|error| error.to_string()),
            None => Err("no receiver".to_string()),
        };
        if let Err(error) = result {
            return self.fail(format!("cannot persist the download: {error}"));
        }
        self.file_offset += length as u64;
        let mut actions = Vec::new();
        actions.extend(self.maybe_progress(false));
        actions.extend(self.pump());
        actions
    }
}

impl TransferSession for ZmodemSession {
    fn start(&mut self) -> Vec<SessionAction> {
        if let Machine::Broken(reason) = &self.machine {
            let reason = reason.clone();
            return self.fail(reason);
        }
        match self.machine {
            Machine::Receiver(_) => {
                // The receiver already queued its `ZRINIT`; it is withheld
                // until the user confirms the destination (§7.1), and the
                // peer keeps repeating its own handshake while it waits.
                vec![SessionAction::NeedDownloadDir]
            }
            Machine::Sender(_) => {
                // Announcing ourselves immediately is what a local `sz` does,
                // and each `ZRQINIT` the peer sees buys it more time to wait
                // for the file picker.
                let mut actions = vec![SessionAction::NeedUploadPaths];
                actions.extend(self.pump());
                actions
            }
            Machine::Broken(_) => Vec::new(),
        }
    }

    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction> {
        if self.finished {
            return Vec::new();
        }
        for &byte in bytes {
            if byte == ZDLE {
                self.can_run += 1;
            } else {
                self.can_run = 0;
            }
        }
        if self.can_run >= CAN_ABORT_RUN {
            return self.fail("the remote cancelled the transfer");
        }
        self.pending_wire.extend_from_slice(bytes);
        if self.pending_wire.len() > MAX_SESSION_BUFFER {
            return self.fail("protocol data exceeds the session buffer limit");
        }
        self.pump()
    }

    fn submit(&mut self, event: HostEvent) -> Vec<SessionAction> {
        if self.finished {
            return Vec::new();
        }
        match event {
            HostEvent::DownloadDir(Some(_)) => {
                self.destination_confirmed = true;
                self.pump()
            }
            HostEvent::DownloadDir(None) => self.cancel(),
            HostEvent::UploadPaths(Some(paths)) => {
                if paths.is_empty() {
                    return self.cancel();
                }
                self.paths = paths;
                self.file_index = 0;
                self.deferred.push_back(Deferred::OpenNextUpload);
                self.pump()
            }
            HostEvent::UploadPaths(None) => self.cancel(),
            HostEvent::FileOpened(Ok(opened)) => {
                if self.sender_mut().is_some() {
                    self.start_upload_file(opened.size)
                } else {
                    self.accept_download()
                }
            }
            HostEvent::FileOpened(Err(error)) => {
                self.fail(format!("cannot open the file: {error}"))
            }
            HostEvent::FileData { offset, result } => match result {
                Ok(data) => self.submit_upload_data(offset, &data),
                Err(error) => self.fail(format!("cannot read the file: {error}")),
            },
            HostEvent::FileWritten { result, .. } => match result {
                Ok(()) => self.on_file_written(),
                Err(error) => self.fail(format!("cannot write the file: {error}")),
            },
            HostEvent::FileClosed(Ok(_staged)) => {
                self.deferred
                    .push_back(Deferred::Action(SessionAction::CommitFile));
                self.pump()
            }
            HostEvent::FileClosed(Err(error)) => {
                self.fail(format!("cannot finish the file: {error}"))
            }
            HostEvent::FileCommitted(Ok(path)) => {
                self.completed.push(path);
                self.file_offset = 0;
                self.file_total = None;
                self.pump()
            }
            HostEvent::FileCommitted(Err(error)) => {
                self.fail(format!("cannot save the file: {error}"))
            }
            HostEvent::Cancelled | HostEvent::TimedOut => {
                self.finished = true;
                Vec::new()
            }
        }
    }

    fn abort_bytes(&mut self) -> Vec<u8> {
        self.finished = true;
        abort_burst()
    }
}

// ─── Provider ──────────────────────────────────────────────────────────────

pub struct ZmodemProvider;

impl TransferProvider for ZmodemProvider {
    fn id(&self) -> Arc<str> {
        "zmodem".into()
    }

    fn display_name(&self) -> Arc<str> {
        "ZMODEM (rz/sz)".into()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: true,
            download: true,
            auto_detect: true,
            drag_upload: false,
            multi_file: true,
            // Classic lrzsz inside tmux still mangles its own frames; our side
            // of the link is not what breaks.
            tmux_compatible: false,
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

    fn configure(&mut self, config: &serde_json::Value) -> anyhow::Result<()> {
        match config.get("enabled") {
            Some(serde_json::Value::Bool(_)) | None => Ok(()),
            Some(other) => anyhow::bail!("zmodem: `enabled` must be a boolean, got {other}"),
        }
    }

    fn new_detector(&self) -> Box<dyn TransferDetector> {
        Box::new(ZmodemDetector::new())
    }

    fn start_session(&self, offer: &TransferOffer) -> Box<dyn TransferSession> {
        Box::new(ZmodemSession::new(
            offer.direction.unwrap_or(Direction::Download),
        ))
    }

    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
        // A manual upload would have to be a sender with nobody listening:
        // ZMODEM uploads only work once the peer's `rz` has announced itself,
        // which is the detected path.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact first bytes of the lrzsz peers: `rz` opens with a `ZRINIT`
    /// (it wants to receive), `sz` with an `rz\r` preamble and a `ZRQINIT`.
    const RZ_HEADER: &[u8] = b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11";
    const SZ_HEADER: &[u8] = b"rz\r**\x18B00000000000000\r\x8a\x11";

    fn verdict(payload: &[u8]) -> DetectorVerdict {
        let mut detector = ZmodemDetector::new();
        detector.feed_bytes(payload)
    }

    fn matched(payload: &[u8]) -> (TransferOffer, std::ops::Range<u64>) {
        match verdict(payload) {
            DetectorVerdict::Matched { offer, trigger } => (offer, trigger),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn zrinit_triggers_an_upload_and_zrqinit_a_download() {
        let (offer, trigger) = matched(RZ_HEADER);
        assert_eq!(offer.direction, Some(Direction::Upload));
        assert_eq!(&*offer.provider_id, "zmodem");
        // The whole header is swallowed, the `\r\x8a\x11` trailer is the
        // session's business.
        assert_eq!(
            &RZ_HEADER[trigger.start as usize..trigger.end as usize],
            b"**\x18B0100000023be50"
        );

        let (offer, trigger) = matched(SZ_HEADER);
        assert_eq!(offer.direction, Some(Direction::Download));
        // `sz` writes an `rz\r` autostart preamble right before the header;
        // swallowing it keeps "rz" out of the terminal.
        assert_eq!(
            &SZ_HEADER[trigger.start as usize..trigger.end as usize],
            b"rz\r**\x18B00000000000000"
        );
    }

    #[test]
    fn detection_survives_arbitrary_chunking() {
        for payload in [RZ_HEADER, SZ_HEADER] {
            let single = matched(payload);
            // Every split point, plus one-byte feeding, must agree.
            for split in 0..=payload.len() {
                let mut detector = ZmodemDetector::new();
                let mut verdict = detector.feed_bytes(&payload[..split]);
                if !matches!(verdict, DetectorVerdict::Matched { .. }) {
                    verdict = detector.feed_bytes(&payload[split..]);
                }
                match verdict {
                    DetectorVerdict::Matched { offer, trigger } => {
                        assert_eq!((offer, trigger), single, "split at {split}");
                    }
                    other => panic!("split at {split} did not match: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn corrupted_headers_do_not_match() {
        for payload in [RZ_HEADER, SZ_HEADER] {
            let start = payload
                .windows(3)
                .position(|window| window == b"**\x18")
                .expect("header start");
            // Flip the frame type to a non-session frame and corrupt the CRC.
            for offset in [4, 5, 10, 14] {
                let mut mutated = payload.to_vec();
                mutated[start + offset] ^= 0x01;
                let mut detector = ZmodemDetector::new();
                assert!(
                    !matches!(
                        detector.feed_bytes(&mutated),
                        DetectorVerdict::Matched { .. }
                    ),
                    "mutation at {offset} must not match"
                );
            }
            // A header that is one hex digit short stays undecided (the mux
            // holds it and releases it later), never matched.
            let header_end = start + 3 + 1 + HEADER_HEX_DIGITS;
            let mut detector = ZmodemDetector::new();
            assert!(
                matches!(
                    detector.feed_bytes(&payload[..header_end - 1]),
                    DetectorVerdict::NeedMore
                ),
                "a truncated header must stay undecided"
            );
        }
    }

    #[test]
    fn other_frames_and_binary_junk_do_not_match() {
        // `ZFILE` (4) and `ZFIN` (8) are valid protocol frames, but they never
        // start a session.
        for frame in [b"04", b"08", b"0a"] {
            let mut payload = b"**\x18B".to_vec();
            payload.extend_from_slice(frame);
            payload.extend_from_slice(b"00000000");
            let crc = crc16_xmodem(&[frame[0], frame[1], b'0', b'0', b'0']);
            payload.extend_from_slice(format!("{crc:04x}").as_bytes());
            let mut detector = ZmodemDetector::new();
            assert!(
                !matches!(
                    detector.feed_bytes(&payload),
                    DetectorVerdict::Matched { .. }
                ),
                "frame {frame:?} must not trigger"
            );
        }

        // Deterministic pseudo-random binary junk, including `ZPAD`/`ZDLE`
        // bytes, must not match either.
        let mut state = 0x12345678u32;
        let junk: Vec<u8> = (0..64 * 1024)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        let mut detector = ZmodemDetector::new();
        assert!(!matches!(
            detector.feed_bytes(&junk),
            DetectorVerdict::Matched { .. }
        ));
    }

    #[test]
    fn plain_output_passes_through() {
        // A lone `*` holds its byte until the next one decides, and then the
        // whole line is released: the detector must not report a match.
        let mut detector = ZmodemDetector::new();
        assert!(matches!(
            detector.feed_bytes(b"2 * 3 = 6\r\n"),
            DetectorVerdict::NoMatch
        ));
        let mut detector = ZmodemDetector::new();
        assert!(matches!(
            detector.feed_bytes(b"** not a header **\n"),
            DetectorVerdict::NoMatch
        ));
    }

    #[test]
    fn crc16_matches_the_reference_header() {
        // `**\x18B0100000023be50`: ZRINIT, data 00 00 00 23, CRC be50.
        assert_eq!(crc16_xmodem(&[0x01, 0x00, 0x00, 0x00, 0x23]), 0xbe50);
        // ZRQINIT with an all-zero payload hashes to zero.
        assert_eq!(crc16_xmodem(&[0x00, 0x00, 0x00, 0x00, 0x00]), 0x0000);
    }
}
