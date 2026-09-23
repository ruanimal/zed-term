//! ZMODEM session tests (Phase 3 of docs/TRANSFER_EXTENSION.md): both
//! directions are driven against the `zmodem2` crate's own state machines,
//! which is the same implementation the session wraps, and the mux's
//! swallow-the-trigger behaviour is reproduced so the sessions are exercised
//! exactly as the tap would create them.
//!
//! When real lrzsz binaries are available, `ZEDTERM_SZ_BIN` (a download
//! peer) and `ZEDTERM_RZ_BIN` (an upload peer) additionally run the same
//! sessions against the de-facto standard implementation.

use std::collections::VecDeque;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use terminal_app::providers_zmodem::{ZmodemDetector, ZmodemProvider};
use terminal_app::transfer_io::AppTransferHost;
use transfer_core::{
    DetectorVerdict, Direction, HostEvent, OpenedFile, SessionAction, TransferDetector,
    TransferHost, TransferOffer, TransferProvider as _, TransferSession,
};
use zmodem2::{Action, Event as WireEvent, FileInfo, Position, Receiver, Sender};

const ZDLE: u8 = 0x18;

fn download_offer() -> TransferOffer {
    TransferOffer {
        provider_id: "zmodem".into(),
        direction: Some(Direction::Download),
        remote_names: Vec::new(),
    }
}

fn upload_offer() -> TransferOffer {
    TransferOffer {
        provider_id: "zmodem".into(),
        direction: Some(Direction::Upload),
        remote_names: Vec::new(),
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("zedterm-zmodem-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn payload(length: usize, modulus: u32) -> Vec<u8> {
    (0..length as u32)
        .map(|index| (index % modulus) as u8)
        .collect()
}

/// Drops the peer's autostart header, the way the mux does before a session
/// exists: the trigger bytes are swallowed by the tap, so a session never
/// sees them. Feeding them would make these tests more forgiving than
/// production.
struct TriggerStripper {
    scan: StripScan,
    digits: usize,
}

enum StripScan {
    /// Looking for `ZPAD ZPAD ZDLE`.
    Seeking,
    /// One or more `ZPAD` seen.
    Padding,
    /// `ZPAD ZPAD ZDLE` seen; the encoding byte decides.
    Encoding,
    /// Collecting the header's hex digits.
    Digits,
    /// The trigger is behind us; everything else belongs to the session.
    Done,
}

impl TriggerStripper {
    fn new() -> Self {
        TriggerStripper {
            scan: StripScan::Seeking,
            digits: 0,
        }
    }

    /// Whether the peer's autostart header has been swallowed already.
    fn done(&self) -> bool {
        matches!(self.scan, StripScan::Done)
    }

    fn strip(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut kept = Vec::new();
        for &byte in bytes {
            match self.scan {
                StripScan::Done => kept.push(byte),
                StripScan::Seeking => {
                    if byte == b'*' {
                        self.scan = StripScan::Padding;
                    } else {
                        kept.push(byte);
                    }
                }
                StripScan::Padding => match byte {
                    b'*' => {}
                    ZDLE => self.scan = StripScan::Encoding,
                    _ => {
                        self.scan = StripScan::Seeking;
                        kept.push(byte);
                    }
                },
                StripScan::Encoding => match byte {
                    b'B' => {
                        self.scan = StripScan::Digits;
                        self.digits = 0;
                    }
                    _ => self.scan = StripScan::Seeking,
                },
                StripScan::Digits => {
                    self.digits += 1;
                    if self.digits >= 14 {
                        self.scan = StripScan::Done;
                    }
                }
            }
        }
        kept
    }
}

/// The host side of a session run: answers pickers, file IO and staging the
/// same way the app's driver does.
struct HostAnswers<'a> {
    host: &'a AppTransferHost,
    upload_paths: Vec<PathBuf>,
    download_dir: Option<PathBuf>,
}

impl HostAnswers<'_> {
    fn run(
        &self,
        action: SessionAction,
        session: &mut Box<dyn TransferSession>,
    ) -> Option<Vec<SessionAction>> {
        let follow = match action {
            SessionAction::OpenRead { path } => {
                let size = self.host.open_read(&path).unwrap();
                session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                    size,
                    local_name: None,
                })))
            }
            SessionAction::ReadFile { offset, max_len } => {
                let result = self.host.read_chunk(offset, max_len);
                session.submit(HostEvent::FileData { offset, result })
            }
            SessionAction::OpenWrite { remote_name, size } => {
                let local_name = self.host.open_write(&remote_name, size).unwrap();
                session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                    size: 0,
                    local_name: Some(local_name),
                })))
            }
            SessionAction::WriteFile { offset, data } => {
                let result = self.host.write_chunk(offset, &data);
                session.submit(HostEvent::FileWritten { offset, result })
            }
            SessionAction::CloseFile => {
                let result = self.host.close_write();
                session.submit(HostEvent::FileClosed(result))
            }
            SessionAction::CommitFile => {
                let result = self.host.commit();
                session.submit(HostEvent::FileCommitted(result))
            }
            SessionAction::NeedUploadPaths => {
                session.submit(HostEvent::UploadPaths(Some(self.upload_paths.clone())))
            }
            SessionAction::NeedDownloadDir => {
                // The driver records the confirmed destination on the host
                // before it hands the answer to the session.
                let destination = self
                    .download_dir
                    .clone()
                    .expect("the test must provide a download directory");
                self.host.set_destination(&destination);
                session.submit(HostEvent::DownloadDir(Some(destination)))
            }
            _ => return None,
        };
        Some(follow)
    }
}

/// Drives a session against a caller-driven peer until the session reports an
/// outcome. The peer closure receives wire bytes and returns what it wants to
/// send back, pumping its own state machine meanwhile.
fn exchange(
    mut session: Box<dyn TransferSession>,
    answers: &HostAnswers,
    mut peer: impl FnMut(Vec<u8>) -> Vec<u8>,
) -> Result<Vec<PathBuf>, String> {
    let mut actions: VecDeque<SessionAction> = session.start().into();
    let mut to_peer: Vec<u8> = Vec::new();
    let mut loops = 0;

    loop {
        loops += 1;
        assert!(loops < 100_000, "the session did not terminate");

        if let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(bytes) => to_peer.extend_from_slice(&bytes),
                SessionAction::Progress { .. } => {}
                SessionAction::Done { paths } => {
                    // The closing handshake still belongs to the peer, the way
                    // the driver writes it to the PTY before ending the
                    // session.
                    peer(std::mem::take(&mut to_peer));
                    return Ok(paths);
                }
                SessionAction::Failed(reason) => return Err(reason),
                SessionAction::Cancelled => return Err("cancelled".to_string()),
                other => {
                    if let Some(follow) = answers.run(other, &mut session) {
                        actions.extend(follow);
                    }
                }
            }
            continue;
        }

        let from_peer = peer(std::mem::take(&mut to_peer));
        if from_peer.is_empty() {
            panic!("no progress: both sides are waiting");
        }
        actions.extend(session.feed_wire(&from_peer));
    }
}

// ─── Peers built on zmodem2 ────────────────────────────────────────────────

/// One owned step of a peer state machine (`Action` borrows the machine).
enum PeerStep {
    Wire(Vec<u8>),
    /// The peer wants file bytes it announced.
    Read {
        offset: u32,
        max_len: usize,
    },
    /// The peer received file bytes to persist.
    Write(Vec<u8>),
    FileStarted(String),
    FileCompleted,
    SessionCompleted,
    Aborted,
    Idle,
}

/// A peer that sends files (what a remote `sz` does).
struct SendingPeer {
    sender: Sender,
    /// Files still to offer, in the order `sz` would send them.
    files: VecDeque<PathBuf>,
    file: Option<std::fs::File>,
    pending: Vec<u8>,
    finished: bool,
    /// The peer saw the receiver's `ZFIN`; its own `OO` still has to go out.
    session_completed: bool,
}

impl SendingPeer {
    fn new(paths: &[PathBuf]) -> Self {
        let mut files: VecDeque<PathBuf> = paths.iter().cloned().collect();
        let mut sender = Sender::new().unwrap();
        let first = files.pop_front().expect("a peer needs at least one file");
        let file = offer_file(&mut sender, &first);
        SendingPeer {
            sender,
            files,
            file: Some(file),
            pending: Vec::new(),
            finished: false,
            session_completed: false,
        }
    }

    fn step(&mut self) -> PeerStep {
        match self.sender.poll() {
            Action::WriteWire(bytes) => PeerStep::Wire(bytes.to_vec()),
            Action::ReadFile { offset, max_len } => PeerStep::Read {
                offset: offset.get(),
                max_len,
            },
            Action::Event(WireEvent::FileCompleted) => PeerStep::FileCompleted,
            Action::Event(WireEvent::SessionCompleted) => PeerStep::SessionCompleted,
            Action::Event(WireEvent::Aborted) => PeerStep::Aborted,
            _ => PeerStep::Idle,
        }
    }

    fn pump(&mut self, input: Vec<u8>) -> Vec<u8> {
        self.pending.extend_from_slice(&input);
        let mut out = Vec::new();
        let mut loops = 0;
        loop {
            loops += 1;
            assert!(loops < 100_000, "the peer did not terminate");
            match self.step() {
                PeerStep::Wire(bytes) => {
                    self.sender.wire_written(bytes.len());
                    out.extend_from_slice(&bytes);
                }
                PeerStep::Read { offset, max_len } => {
                    let file = self.file.as_mut().expect("a file is being sent");
                    file.seek(SeekFrom::Start(u64::from(offset))).unwrap();
                    let mut buffer = vec![0u8; max_len];
                    let count = file.read(&mut buffer).unwrap();
                    buffer.truncate(count);
                    self.sender.submit_file(&buffer).unwrap();
                }
                PeerStep::FileCompleted => match self.files.pop_front() {
                    Some(next) => self.file = Some(offer_file(&mut self.sender, &next)),
                    None => {
                        if !self.finished {
                            self.finished = true;
                            self.sender.finish().unwrap();
                        }
                    }
                },
                PeerStep::SessionCompleted => self.session_completed = true,
                PeerStep::Idle => {
                    if !self.pending.is_empty() {
                        let consumed = self.sender.submit_wire(&self.pending).unwrap();
                        if consumed > 0 {
                            self.pending.drain(..consumed);
                            continue;
                        }
                    }
                    if self.session_completed {
                        break;
                    }
                    if self.pending.is_empty() {
                        break;
                    }
                    let consumed = self.sender.submit_wire(&self.pending).unwrap();
                    if consumed == 0 {
                        break;
                    }
                    self.pending.drain(..consumed);
                }
                other => panic!("a sending peer has nothing to do with {other:?}"),
            }
        }
        out
    }
}

/// Announces `path` over the sender, the way `sz` offers each file.
fn offer_file(sender: &mut Sender, path: &Path) -> std::fs::File {
    let file = std::fs::File::open(path).unwrap();
    let size = file.metadata().unwrap().len() as u32;
    let name = path.file_name().unwrap().as_encoded_bytes().to_vec();
    sender
        .start_file(FileInfo::new(&name, Some(Position::new(size))))
        .unwrap();
    file
}

impl std::fmt::Debug for PeerStep {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerStep::Wire(bytes) => write!(formatter, "Wire({} bytes)", bytes.len()),
            PeerStep::Read { offset, max_len } => {
                write!(formatter, "Read({offset}, {max_len})")
            }
            PeerStep::Write(bytes) => write!(formatter, "Write({} bytes)", bytes.len()),
            PeerStep::FileStarted(name) => write!(formatter, "FileStarted({name})"),
            PeerStep::FileCompleted => write!(formatter, "FileCompleted"),
            PeerStep::SessionCompleted => write!(formatter, "SessionCompleted"),
            PeerStep::Aborted => write!(formatter, "Aborted"),
            PeerStep::Idle => write!(formatter, "Idle"),
        }
    }
}

/// A peer that receives files into a directory (what a remote `rz` does).
struct ReceivingPeer {
    receiver: Receiver,
    directory: PathBuf,
    current: Option<std::fs::File>,
    received: Vec<PathBuf>,
    pending: Vec<u8>,
    /// Retries of the handshake, standing in for the peer's own timer: the
    /// first `ZRINIT` was swallowed as the trigger, so a real `rz` repeats it
    /// until the sender appears.
    handshake_retries: u8,
    /// The peer saw the sender's `ZFIN`; its own reply still has to go out.
    session_completed: bool,
}

impl ReceivingPeer {
    fn new(directory: &Path) -> Self {
        ReceivingPeer {
            receiver: Receiver::with_flow_control(0, true).unwrap(),
            directory: directory.to_path_buf(),
            current: None,
            received: Vec::new(),
            pending: Vec::new(),
            handshake_retries: 0,
            session_completed: false,
        }
    }

    fn step(&mut self) -> PeerStep {
        match self.receiver.poll() {
            Action::WriteWire(bytes) => PeerStep::Wire(bytes.to_vec()),
            Action::WriteFile(bytes) => PeerStep::Write(bytes.to_vec()),
            Action::Event(WireEvent::FileStarted(info)) => {
                PeerStep::FileStarted(String::from_utf8_lossy(info.name).into_owned())
            }
            Action::Event(WireEvent::FileCompleted) => PeerStep::FileCompleted,
            Action::Event(WireEvent::SessionCompleted) => PeerStep::SessionCompleted,
            Action::Event(WireEvent::Aborted) => PeerStep::Aborted,
            _ => PeerStep::Idle,
        }
    }

    fn pump(&mut self, input: Vec<u8>) -> Vec<u8> {
        self.pending.extend_from_slice(&input);
        let mut out = Vec::new();
        let mut loops = 0;
        loop {
            loops += 1;
            assert!(loops < 100_000, "the peer did not terminate");
            match self.step() {
                PeerStep::Wire(bytes) => {
                    self.receiver.wire_written(bytes.len());
                    out.extend_from_slice(&bytes);
                }
                PeerStep::Write(bytes) => {
                    let count = bytes.len();
                    if let Some(file) = self.current.as_mut() {
                        file.write_all(&bytes).unwrap();
                    }
                    self.receiver.file_written(count).unwrap();
                }
                PeerStep::FileStarted(remote_name) => {
                    let name = Path::new(&remote_name)
                        .file_name()
                        .expect("a host name is always a bare file name")
                        .to_owned();
                    let path = self.directory.join(name);
                    self.current = Some(std::fs::File::create(&path).unwrap());
                    self.received.push(path);
                }
                PeerStep::FileCompleted => self.current = None,
                PeerStep::SessionCompleted => self.session_completed = true,
                PeerStep::Idle => {
                    if !self.pending.is_empty() {
                        let consumed = self.receiver.submit_wire(&self.pending).unwrap();
                        if consumed > 0 {
                            self.pending.drain(..consumed);
                            continue;
                        }
                    }
                    if !self.session_completed && self.handshake_retries < 2 {
                        self.handshake_retries += 1;
                        self.receiver.timeout().unwrap();
                        continue;
                    }
                    break;
                }
                other => panic!("a receiving peer has nothing to do with {other:?}"),
            }
        }
        out
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[test]
fn download_from_a_sending_peer() {
    let destination = temp_dir("download-destination");
    let source = temp_dir("download-source").join("report.bin");
    let contents = payload(300 * 1024, 251);
    std::fs::write(&source, &contents).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    host.set_destination(&destination);
    let mut peer = SendingPeer::new(std::slice::from_ref(&source));
    let mut stripper = TriggerStripper::new();
    let answers = HostAnswers {
        host: &host,
        upload_paths: Vec::new(),
        download_dir: Some(destination.clone()),
    };

    let saved = exchange(
        ZmodemProvider.start_session(&download_offer()),
        &answers,
        |input| peer.pump(stripper.strip(&input)),
    )
    .expect("download must complete");

    assert_eq!(saved, vec![destination.join("report.bin")]);
    assert_eq!(std::fs::read(&saved[0]).unwrap(), contents);
    let _ = std::fs::remove_dir_all(&destination);
}

/// A batch exercises the ordering a single file cannot: the receiver must
/// close and commit each staged file before it opens the next one, while the
/// `ZRINIT` that unlocks the next file still goes out first (a disk sync must
/// not sit in front of it). The empty file covers a completion that arrives
/// without a single data frame.
#[test]
fn download_of_a_multi_file_batch() {
    let destination = temp_dir("download-batch");
    let sources = temp_dir("download-batch-source");
    let first = sources.join("first.bin");
    let empty = sources.join("empty.bin");
    let second = sources.join("second.txt");
    let first_contents = payload(150 * 1024, 251);
    let second_contents = b"second file\n".to_vec();
    std::fs::write(&first, &first_contents).unwrap();
    std::fs::write(&empty, b"").unwrap();
    std::fs::write(&second, &second_contents).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    host.set_destination(&destination);
    let mut peer = SendingPeer::new(&[first, empty, second]);
    let mut stripper = TriggerStripper::new();
    let answers = HostAnswers {
        host: &host,
        upload_paths: Vec::new(),
        download_dir: Some(destination.clone()),
    };

    let saved = exchange(
        ZmodemProvider.start_session(&download_offer()),
        &answers,
        |input| peer.pump(stripper.strip(&input)),
    )
    .expect("batch download must complete");

    assert_eq!(
        saved,
        vec![
            destination.join("first.bin"),
            destination.join("empty.bin"),
            destination.join("second.txt"),
        ]
    );
    assert_eq!(std::fs::read(&saved[0]).unwrap(), first_contents);
    assert_eq!(std::fs::read(&saved[1]).unwrap(), b"");
    assert_eq!(std::fs::read(&saved[2]).unwrap(), second_contents);
    let _ = std::fs::remove_dir_all(&destination);
    let _ = std::fs::remove_dir_all(&sources);
}

#[test]
fn upload_of_a_multi_file_batch() {
    let workdir = temp_dir("upload-batch");
    let destination = workdir.join("received");
    std::fs::create_dir_all(&destination).unwrap();
    let first = workdir.join("one.dat");
    let empty = workdir.join("two.dat");
    let third = workdir.join("three.dat");
    let first_contents = payload(120 * 1024, 241);
    let third_contents = b"third file\n".to_vec();
    std::fs::write(&first, &first_contents).unwrap();
    std::fs::write(&empty, b"").unwrap();
    std::fs::write(&third, &third_contents).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let mut peer = ReceivingPeer::new(&destination);
    let mut stripper = TriggerStripper::new();
    let answers = HostAnswers {
        host: &host,
        upload_paths: vec![first.clone(), empty.clone(), third.clone()],
        download_dir: None,
    };

    let sent = exchange(
        ZmodemProvider.start_session(&upload_offer()),
        &answers,
        |input| peer.pump(stripper.strip(&input)),
    )
    .expect("batch upload must complete");

    assert_eq!(sent, vec![first, empty, third]);
    assert_eq!(peer.received.len(), 3);
    assert_eq!(
        std::fs::read(destination.join("one.dat")).unwrap(),
        first_contents
    );
    assert_eq!(std::fs::read(destination.join("two.dat")).unwrap(), b"");
    assert_eq!(
        std::fs::read(destination.join("three.dat")).unwrap(),
        third_contents
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn upload_to_a_receiving_peer() {
    let workdir = temp_dir("upload");
    let destination = workdir.join("received");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("uploaded.bin");
    let contents = payload(200 * 1024, 253);
    std::fs::write(&source, &contents).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let mut peer = ReceivingPeer::new(&destination);
    let mut stripper = TriggerStripper::new();
    let answers = HostAnswers {
        host: &host,
        upload_paths: vec![source.clone()],
        download_dir: None,
    };

    let sent = exchange(
        ZmodemProvider.start_session(&upload_offer()),
        &answers,
        |input| peer.pump(stripper.strip(&input)),
    )
    .expect("upload must complete");

    assert_eq!(sent, vec![source.clone()]);
    let received = destination.join("uploaded.bin");
    assert_eq!(peer.received, vec![received.clone()]);
    assert_eq!(std::fs::read(&received).unwrap(), contents);
    let _ = std::fs::remove_dir_all(&workdir);
}

/// A `ZRPOS` rewind (error recovery resends from the last good byte) must
/// make the session re-read from the requested offset and keep going. The
/// resend used to pile onto the monotonic byte count and fail the file with
/// "the file changed while it was being sent".
#[test]
fn upload_resends_from_a_zrpos_rewind() {
    // The peer's half of this test is six hex headers, hand-encoded.
    fn hex_header(frame: u8, count: u32) -> Vec<u8> {
        fn crc16(data: &[u8]) -> u16 {
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
        let payload = [
            frame,
            count as u8,
            (count >> 8) as u8,
            (count >> 16) as u8,
            (count >> 24) as u8,
        ];
        let mut header = b"**\x18B".to_vec();
        for byte in payload {
            header.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        let crc = crc16(&payload);
        header.extend_from_slice(format!("{crc:04x}").as_bytes());
        header.extend_from_slice(b"\r\n");
        header
    }

    let workdir = temp_dir("upload-rewind");
    let source = workdir.join("rewind.bin");
    let contents = payload(8 * 1024, 251);
    std::fs::write(&source, &contents).unwrap();
    let total = contents.len() as u32;

    let host = AppTransferHost::new(None, u64::MAX);
    let answers = HostAnswers {
        host: &host,
        upload_paths: vec![source.clone()],
        download_dir: None,
    };
    let mut session = ZmodemProvider.start_session(&upload_offer());
    // One frame per time the session runs out of things to say: ZRINIT
    // (zero buffer length with ZF3 = CANFDX|CANOVIO|CANFC32, so the sender
    // streams the whole file per window), the initial ZRPOS at 0, the rewind
    // to 1 KiB after the file went out once, the ZRPOS at the size that ends
    // the data, the ZRINIT that confirms the ZEOF, and ZFIN.
    let mut peer_frames: VecDeque<Vec<u8>> = vec![
        hex_header(1, 0x23 << 24),
        hex_header(9, 0),
        hex_header(9, 1024),
        hex_header(9, total),
        hex_header(1, 0x23 << 24),
        hex_header(8, 0),
    ]
    .into();

    let mut actions: VecDeque<SessionAction> = session.start().into();
    let mut read_offsets: Vec<u64> = Vec::new();
    loop {
        if let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(_) | SessionAction::Progress { .. } => {}
                SessionAction::Failed(reason) => {
                    panic!("a rewind must not fail the file: {reason}")
                }
                SessionAction::Cancelled => panic!("unexpected cancel"),
                SessionAction::Done { paths } => {
                    assert_eq!(paths, vec![source]);
                    break;
                }
                SessionAction::ReadFile { offset, max_len } => {
                    read_offsets.push(offset);
                    actions.extend(session.submit(HostEvent::FileData {
                        offset,
                        result: host.read_chunk(offset, max_len),
                    }));
                }
                other => {
                    if let Some(follow) = answers.run(other, &mut session) {
                        actions.extend(follow);
                    }
                }
            }
            continue;
        }
        let frame = peer_frames
            .pop_front()
            .expect("the peer still owes a frame but the session went idle");
        actions.extend(session.feed_wire(&frame));
    }

    // The rewind re-read the tail a second time; without it the test proves
    // nothing.
    assert_eq!(
        read_offsets
            .iter()
            .filter(|&&offset| offset == 1024)
            .count(),
        2,
        "the resend must re-read from the rewound offset"
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

/// Both directions have to survive the mux swallowing the peer's autostart
/// header (the session never sees those bytes). For an upload that means the
/// session must still be able to answer the peer's *next* `ZRINIT`.
#[test]
fn upload_recovers_when_the_trigger_header_is_swallowed() {
    let workdir = temp_dir("upload-swallowed");
    let destination = workdir.join("received");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("payload.txt");
    std::fs::write(&source, b"swallowed trigger\n").unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let mut peer = ReceivingPeer::new(&destination);
    let answers = HostAnswers {
        host: &host,
        upload_paths: vec![source.clone()],
        download_dir: None,
    };
    let mut stripper = TriggerStripper::new();
    let mut first = true;

    let sent = exchange(
        ZmodemProvider.start_session(&upload_offer()),
        &answers,
        |input| {
            let stripped = stripper.strip(&input);
            let out = peer.pump(stripped);
            // The very first bytes the peer emits are the swallowed trigger.
            if first {
                first = false;
                assert!(out.starts_with(b"**\x18B"), "peer must open with a header");
            }
            out
        },
    )
    .expect("upload must complete");

    assert_eq!(sent, vec![source]);
    assert_eq!(
        std::fs::read(destination.join("payload.txt")).unwrap(),
        b"swallowed trigger\n"
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

/// §7.1: a download must not start before the user confirmed where it lands.
/// `ZRINIT` is what makes the peer send, so it is withheld until then.
#[test]
fn download_handshake_waits_for_the_confirmed_destination() {
    let mut session = ZmodemProvider.start_session(&download_offer());
    let actions = session.start();
    assert_eq!(actions, vec![SessionAction::NeedDownloadDir]);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, SessionAction::WriteWire(_))),
        "no protocol bytes may go out before the user confirms"
    );

    let destination = temp_dir("withheld-destination");
    let actions = session.submit(HostEvent::DownloadDir(Some(destination.clone())));
    assert!(
        actions.iter().any(
            |action| matches!(action, SessionAction::WriteWire(bytes) if bytes.starts_with(b"**\x18B"))
        ),
        "confirming the destination must send the handshake: {actions:?}"
    );
    let _ = std::fs::remove_dir_all(&destination);
}

#[test]
fn declining_a_picker_cancels_with_an_abort_burst() {
    for offer in [download_offer(), upload_offer()] {
        let mut session = ZmodemProvider.start_session(&offer);
        let _ = session.start();
        let declined = if offer.direction == Some(Direction::Upload) {
            session.submit(HostEvent::UploadPaths(None))
        } else {
            session.submit(HostEvent::DownloadDir(None))
        };
        assert!(
            matches!(declined.last(), Some(SessionAction::Cancelled)),
            "a declined picker is a cancel, not a completion: {declined:?}"
        );
        assert!(
            declined.iter().any(|action| matches!(
                action,
                SessionAction::WriteWire(bytes)
                    if bytes.iter().filter(|byte| **byte == ZDLE).count() >= 5
            )),
            "the peer must be told the transfer is off: {declined:?}"
        );
    }
}

/// A peer that cancels mid-transfer with the classic `CAN` burst ends the
/// session with a failure instead of hanging until the idle watchdog.
#[test]
fn remote_abort_burst_fails_the_session() {
    let destination = temp_dir("abort-destination");
    let mut session = ZmodemProvider.start_session(&download_offer());
    let _ = session.start();
    let _ = session.submit(HostEvent::DownloadDir(Some(destination.clone())));

    let actions = session.feed_wire(&[ZDLE; 8]);
    assert!(
        matches!(actions.last(), Some(SessionAction::Failed(reason)) if reason.contains("cancel")),
        "an abort burst must fail the session: {actions:?}"
    );
    let _ = std::fs::remove_dir_all(&destination);
}

/// The detector is what turns the peer's real handshake into an offer, so the
/// provider must expose one that reports the direction the peer asked for.
#[test]
fn provider_detector_reports_the_direction_of_a_real_handshake() {
    let provider = ZmodemProvider;
    let capabilities = provider.capabilities();
    assert!(capabilities.auto_detect && capabilities.upload && capabilities.download);

    let mut detector: Box<dyn TransferDetector> = provider.new_detector();
    let verdict = detector.feed_bytes(b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11");
    match verdict {
        DetectorVerdict::Matched { offer, .. } => {
            assert_eq!(offer.direction, Some(Direction::Upload));
            assert_eq!(&*offer.provider_id, "zmodem");
        }
        other => panic!("expected a match, got {other:?}"),
    }
    assert!(ZmodemDetector::new().feed_bytes(b"plain output\n") == DetectorVerdict::NoMatch);
}

// ─── Interoperability with real lrzsz ──────────────────────────────────────

/// Runs `peer` (a real `sz`/`rz` process) against a session, in the same
/// fashion as the `trzsz` interop tests: the process talks over pipes and the
/// mux's trigger handling is reproduced by `TriggerStripper`.
fn run_lrzsz(
    mut command: Command,
    mut session: Box<dyn TransferSession>,
    host: &AppTransferHost,
    answers: &HostAnswers,
) -> Result<Vec<PathBuf>, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the peer");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    let mut stripper = TriggerStripper::new();
    let mut buffer = [0u8; 4096];
    let mut loops = 0;

    // In production the tap starts a session only after it has swallowed the
    // peer's autostart header, which is also what makes the withheld
    // handshake answer safe. Read up to that point before starting, so the
    // peer never sees our first bytes "too early" in a way production cannot
    // produce.
    let mut buffered = Vec::new();
    while !stripper.done() {
        let count = stdout.read(&mut buffer).expect("read from the peer");
        assert!(count > 0, "the peer closed its output before its handshake");
        buffered.extend_from_slice(&stripper.strip(&buffer[..count]));
    }

    let mut actions: VecDeque<SessionAction> = session.start().into();
    if !buffered.is_empty() {
        actions.extend(session.feed_wire(&buffered));
    }

    let result = loop {
        loops += 1;
        assert!(loops < 100_000, "interop did not terminate");

        let Some(action) = actions.pop_front() else {
            // Nothing left to do locally: the peer owes us bytes now.
            let count = stdout.read(&mut buffer).expect("read from the peer");
            assert!(count > 0, "the peer closed its output before finishing");
            actions.extend(session.feed_wire(&stripper.strip(&buffer[..count])));
            continue;
        };
        match action {
            SessionAction::WriteWire(bytes) => {
                stdin.write_all(&bytes).unwrap();
                stdin.flush().unwrap();
            }
            SessionAction::Progress { .. } => {}
            SessionAction::Done { paths } => break Ok(paths),
            SessionAction::Failed(reason) => break Err(reason),
            SessionAction::Cancelled => break Err("cancelled".to_string()),
            other => {
                if let Some(follow) = answers.run(other, &mut session) {
                    actions.extend(follow);
                }
            }
        }
    };

    let _ = child.kill();
    let _ = child.wait();
    let _ = host;
    result
}

#[test]
fn download_interop_with_real_sz() {
    let Ok(sz) = std::env::var("ZEDTERM_SZ_BIN") else {
        return;
    };
    let workdir = temp_dir("interop-sz");
    let destination = workdir.join("out");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("interop-source.bin");
    let empty = workdir.join("interop-empty.bin");
    let contents = payload(300 * 1024, 249);
    std::fs::write(&source, &contents).unwrap();
    std::fs::write(&empty, b"").unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    host.set_destination(&destination);
    let answers = HostAnswers {
        host: &host,
        upload_paths: Vec::new(),
        download_dir: Some(destination.clone()),
    };
    // A batch, so the receiver's close/commit/re-handshake ordering is
    // exercised against the real `sz` as well.
    let mut command = Command::new(&sz);
    command.arg(&source).arg(&empty).current_dir(&workdir);

    let saved = run_lrzsz(
        command,
        ZmodemProvider.start_session(&download_offer()),
        &host,
        &answers,
    )
    .expect("sz interop must complete");

    assert_eq!(
        saved,
        vec![
            destination.join("interop-source.bin"),
            destination.join("interop-empty.bin"),
        ]
    );
    assert_eq!(std::fs::read(&saved[0]).unwrap(), contents);
    assert_eq!(std::fs::read(&saved[1]).unwrap(), b"");
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn upload_interop_with_real_rz() {
    let Ok(rz) = std::env::var("ZEDTERM_RZ_BIN") else {
        return;
    };
    let workdir = temp_dir("interop-rz");
    let destination = workdir.join("received");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("interop-upload.bin");
    let contents = payload(200 * 1024, 247);
    std::fs::write(&source, &contents).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let answers = HostAnswers {
        host: &host,
        upload_paths: vec![source.clone()],
        download_dir: None,
    };
    let mut command = Command::new(&rz);
    command.current_dir(&destination);

    let sent = run_lrzsz(
        command,
        ZmodemProvider.start_session(&upload_offer()),
        &host,
        &answers,
    )
    .expect("rz interop must complete");

    assert_eq!(sent, vec![source]);
    assert_eq!(
        std::fs::read(destination.join("interop-upload.bin")).unwrap(),
        contents
    );
    let _ = std::fs::remove_dir_all(&workdir);
}
