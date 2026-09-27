//! trzsz session tests: a scripted remote peer drives the full protocol v1
//! base64 flows (download from a remote `tsz`, upload to a remote `trz`),
//! plus trigger-detection edge cases. The scripted peers speak the same wire
//! format as the real trzsz servers (see the trzsz-rs reference).
//!
//! When a real `tsz` binary is available, `ZEDTERM_TSZ_BIN` additionally runs
//! the detector against its actual handshake output.

use std::collections::VecDeque;
use std::io::Read as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use md5::{Digest, Md5};

use terminal_app::providers_trzsz::{TrzszDetector, TrzszProvider};
use terminal_app::transfer_io::AppTransferHost;
use transfer_core::{
    DetectorVerdict, Direction, HostEvent, OpenedFile, SessionAction, TransferDetector,
    TransferHost, TransferOffer, TransferProvider as _, TransferSession,
};

fn encode_bytes(bytes: &[u8]) -> String {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = encoder.write_all(bytes);
    BASE64.encode(encoder.finish().unwrap_or_default())
}

fn decode_value(value: &str) -> Vec<u8> {
    let compressed = BASE64.decode(value).unwrap();
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).unwrap();
    decoded
}

fn string_line(kind: &str, value: impl AsRef<[u8]>) -> Vec<u8> {
    format!("#{}:{}\n", kind, encode_bytes(value.as_ref())).into_bytes()
}

fn integer_line(kind: &str, value: u64) -> Vec<u8> {
    format!("#{}:{}\n", kind, value).into_bytes()
}

fn download_offer() -> TransferOffer {
    TransferOffer {
        provider_id: "trzsz".into(),
        direction: Some(Direction::Download),
        trigger_mode: Some('S'),
        trigger_version: Some("1.2.0".into()),
        remote_names: Vec::new(),
    }
}

fn upload_offer() -> TransferOffer {
    TransferOffer {
        provider_id: "trzsz".into(),
        direction: Some(Direction::Upload),
        trigger_mode: Some('R'),
        trigger_version: Some("1.2.0".into()),
        remote_names: Vec::new(),
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("zedterm-trzsz-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ─── Scripted download server (mirrors remote `tsz`) ───────────────────────

#[derive(PartialEq)]
enum DownloadStage {
    Start,
    NumAcked,
    NameAcked,
    SizeAcked,
    DigestSent,
    Finished,
}

struct DownloadServer {
    files: Vec<(String, Vec<u8>)>,
    file_index: usize,
    chunk: usize,
    hasher: Md5,
    sent: usize,
    stage: DownloadStage,
    out: VecDeque<Vec<u8>>,
}

impl DownloadServer {
    fn new(file: Vec<u8>, file_name: String) -> Self {
        Self::with_files(vec![(file_name, file)])
    }

    fn with_files(files: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            files,
            file_index: 0,
            chunk: 16 * 1024,
            hasher: Md5::new(),
            sent: 0,
            stage: DownloadStage::Start,
            out: VecDeque::new(),
        }
    }

    fn send_data_chunk(&mut self) {
        let Some((_, file)) = self.files.get(self.file_index) else {
            return;
        };
        let end = (self.sent + self.chunk).min(file.len());
        let chunk = file[self.sent..end].to_vec();
        self.hasher.update(&chunk);
        self.sent = end;
        self.out.push_back(string_line("DATA", chunk));
    }

    fn on_client_line(&mut self, kind: &str, value: &str) {
        match kind {
            "ACT" => {
                self.out.push_back(string_line(
                    "CFG",
                    br#"{"quiet":true,"binary":false,"bufsize":1048576,"timeout":20}"#,
                ));
                self.out
                    .push_back(integer_line("NUM", self.files.len() as u64));
                self.stage = DownloadStage::Start;
            }
            "SUCC" => match self.stage {
                DownloadStage::Start => {
                    if let Some((name, _)) = self.files.get(self.file_index) {
                        self.out.push_back(string_line("NAME", name.as_bytes()));
                        self.stage = DownloadStage::NumAcked;
                    }
                }
                DownloadStage::NumAcked => {
                    if let Some((_, file)) = self.files.get(self.file_index) {
                        self.out.push_back(integer_line("SIZE", file.len() as u64));
                        self.stage = DownloadStage::NameAcked;
                    }
                }
                DownloadStage::NameAcked => {
                    self.send_data_chunk();
                    self.stage = DownloadStage::SizeAcked;
                }
                DownloadStage::SizeAcked => {
                    if self
                        .files
                        .get(self.file_index)
                        .is_some_and(|(_, file)| self.sent < file.len())
                    {
                        self.send_data_chunk();
                    } else {
                        let digest: [u8; 16] = self.hasher.clone().finalize().into();
                        self.out.push_back(string_line("MD5", digest));
                        self.stage = DownloadStage::DigestSent;
                    }
                }
                DownloadStage::DigestSent => {
                    if self.file_index + 1 < self.files.len() {
                        self.file_index += 1;
                        self.sent = 0;
                        self.hasher = Md5::new();
                        if let Some((name, _)) = self.files.get(self.file_index) {
                            self.out.push_back(string_line("NAME", name.as_bytes()));
                            self.stage = DownloadStage::NumAcked;
                        }
                    } else {
                        self.out.push_back(string_line("EXIT", "done"));
                        self.stage = DownloadStage::Finished;
                    }
                }
                DownloadStage::Finished => {}
            },
            "MD5" => {
                let digest: [u8; 16] = self.hasher.clone().finalize().into();
                assert_eq!(
                    decode_value(value),
                    digest,
                    "client echoed the wrong digest"
                );
            }
            "EXIT" => {}
            other => panic!("download server got unexpected {other} line"),
        }
    }
}

// ─── Scripted upload receiver (mirrors remote `trz`) ───────────────────────

struct UploadReceiver {
    current_file_name: String,
    current_file: Vec<u8>,
    received_files: Vec<(String, Vec<u8>)>,
    hasher: Md5,
    out: VecDeque<Vec<u8>>,
}

impl UploadReceiver {
    fn new() -> Self {
        Self {
            current_file_name: String::new(),
            current_file: Vec::new(),
            received_files: Vec::new(),
            hasher: Md5::new(),
            out: VecDeque::new(),
        }
    }

    fn on_client_line(&mut self, kind: &str, value: &str) {
        match kind {
            "ACT" => {
                let action: serde_json::Value =
                    serde_json::from_slice(&decode_value(value)).unwrap();
                assert_eq!(action["lang"], "rust");
                assert_eq!(action["protocol"], 1);
                assert_eq!(action["binary"], false);
                assert_eq!(action["support_dir"], false);
                assert_eq!(action["confirm"], true);
                self.out.push_back(string_line(
                    "CFG",
                    br#"{"quiet":true,"binary":false,"overwrite":true,"bufsize":1048576,"timeout":20}"#,
                ));
            }
            "NUM" => self
                .out
                .push_back(integer_line("SUCC", value.parse().unwrap())),
            "NAME" => {
                self.current_file_name = String::from_utf8(decode_value(value)).unwrap();
                self.current_file.clear();
                self.out
                    .push_back(string_line("SUCC", self.current_file_name.as_bytes()));
            }
            "SIZE" => self
                .out
                .push_back(integer_line("SUCC", value.parse().unwrap())),
            "DATA" => {
                let chunk = decode_value(value);
                self.hasher.update(&chunk);
                self.current_file.extend_from_slice(&chunk);
                self.out.push_back(integer_line("SUCC", chunk.len() as u64));
            }
            "MD5" => {
                let digest: [u8; 16] = self.hasher.clone().finalize().into();
                assert_eq!(
                    decode_value(value),
                    digest,
                    "uploaded data checksum mismatch"
                );
                self.received_files.push((
                    self.current_file_name.clone(),
                    std::mem::take(&mut self.current_file),
                ));
                self.hasher = Md5::new();
                self.out.push_back(string_line("SUCC", digest));
            }
            "EXIT" => {}
            other => panic!("upload receiver got unexpected {other} line"),
        }
    }
}

// ─── Pump ──────────────────────────────────────────────────────────────────

struct Dialogs {
    upload_paths: Option<Vec<PathBuf>>,
    download_destination: Option<PathBuf>,
}

/// Drive `session` against a scripted peer, performing host IO through a
/// real `AppTransferHost`. Returns the paths reported by `Done`.
fn pump(
    mut session: Box<dyn TransferSession>,
    mut peer: impl FnMut(&str, &str) -> VecDeque<Vec<u8>>,
    dialogs: Dialogs,
    host: &AppTransferHost,
) -> Vec<PathBuf> {
    let mut server_lines: VecDeque<Vec<u8>> = VecDeque::new();
    let mut client_lines: VecDeque<Vec<u8>> = VecDeque::new();
    let mut done: Option<Vec<PathBuf>> = None;

    let mut actions: VecDeque<SessionAction> = session.start().into();

    loop {
        if let Some(paths) = done {
            return paths;
        }

        if let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(bytes) => client_lines.push_back(bytes),
                SessionAction::OpenRead { path } => {
                    let size = host.open_read(&path).unwrap();
                    actions.extend(session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                        size,
                        local_name: None,
                    }))));
                }
                SessionAction::ReadFile { offset, max_len } => {
                    let result = host.read_chunk(offset, max_len);
                    actions.extend(session.submit(HostEvent::FileData { offset, result }));
                }
                SessionAction::OpenWrite { remote_name, size } => {
                    let local_name = host.open_write(&remote_name, size).unwrap();
                    actions.extend(session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                        size: 0,
                        local_name: Some(local_name),
                    }))));
                }
                SessionAction::WriteFile { offset, data } => {
                    let result = host.write_chunk(offset, &data);
                    actions.extend(session.submit(HostEvent::FileWritten { offset, result }));
                }
                SessionAction::CloseFile => {
                    let result = host.close_write();
                    actions.extend(session.submit(HostEvent::FileClosed(result)));
                }
                SessionAction::CommitFile => {
                    let result = host.commit();
                    actions.extend(session.submit(HostEvent::FileCommitted(result)));
                }
                SessionAction::Progress { .. } => {}
                SessionAction::Done { paths } => done = Some(paths),
                SessionAction::Failed(reason) => panic!("session failed: {reason}"),
                SessionAction::NeedUploadPaths | SessionAction::NeedUploadPathsWithDirectories => {
                    let paths = dialogs
                        .upload_paths
                        .clone()
                        .expect("test must provide upload paths");
                    actions.extend(session.submit(HostEvent::UploadPaths(Some(paths))));
                }
                SessionAction::NeedDownloadDir => {
                    let destination = dialogs
                        .download_destination
                        .clone()
                        .expect("test must provide a download destination");
                    host.set_destination(&destination);
                    actions.extend(session.submit(HostEvent::DownloadDir(Some(destination))));
                }
                SessionAction::Cancelled => {
                    // Peer-driven runs only end through Done; cancellation is
                    // exercised in its own test.
                    panic!("unexpected cancellation")
                }
            }
            continue;
        }

        if let Some(bytes) = client_lines.pop_front() {
            let text = String::from_utf8(bytes).unwrap();
            let text = text.trim_end_matches(['\n', '\r', '!']);
            let colon = text.find(':').expect("client line must have a colon");
            let kind = &text[1..colon];
            let value = &text[colon + 1..];
            server_lines.extend(peer(kind, value));
            continue;
        }

        if let Some(line) = server_lines.pop_front() {
            actions.extend(session.feed_wire(&line));
            continue;
        }

        panic!("no progress: session is stuck");
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[test]
fn download_flow_against_scripted_server() {
    let payload: Vec<u8> = (0..100 * 1024u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let destination = temp_dir("download-destination");
    let host = AppTransferHost::new(None, u64::MAX);

    let paths = pump(
        TrzszProvider::default().start_session(&download_offer()),
        {
            let mut server = DownloadServer::new(payload.clone(), "downloaded.bin".into());
            move |kind, value| {
                server.on_client_line(kind, value);
                std::mem::take(&mut server.out)
            }
        },
        Dialogs {
            upload_paths: None,
            download_destination: Some(destination.clone()),
        },
        &host,
    );

    let saved = &paths[0];
    assert_eq!(saved.parent().unwrap(), destination);
    // The picker chooses the directory only; the name comes from the remote.
    assert_eq!(
        saved.file_name().unwrap().to_string_lossy(),
        "downloaded.bin"
    );
    assert_eq!(std::fs::read(saved).unwrap(), payload);
    let _ = std::fs::remove_dir_all(&destination);
}

#[test]
fn download_multiple_files_with_trzsz_library_protocol() {
    let first_payload: Vec<u8> = (0..5 * 1024u32).map(|index| (index % 241) as u8).collect();
    let second_payload: Vec<u8> = (0..9 * 1024u32)
        .map(|index| (index * 3 % 251) as u8)
        .collect();
    let destination = temp_dir("multi-download-destination");
    let host = AppTransferHost::new(None, u64::MAX);
    let paths = pump(
        TrzszProvider::default().start_session(&download_offer()),
        {
            let mut server = DownloadServer::with_files(vec![
                ("first.txt".into(), first_payload.clone()),
                ("second.bin".into(), second_payload.clone()),
            ]);
            move |kind, value| {
                server.on_client_line(kind, value);
                std::mem::take(&mut server.out)
            }
        },
        Dialogs {
            upload_paths: None,
            download_destination: Some(destination.clone()),
        },
        &host,
    );

    assert_eq!(
        paths,
        vec![
            destination.join("first.txt"),
            destination.join("second.bin")
        ]
    );
    assert_eq!(std::fs::read(&paths[0]).unwrap(), first_payload);
    assert_eq!(std::fs::read(&paths[1]).unwrap(), second_payload);
    let _ = std::fs::remove_dir_all(&destination);
}

#[test]
fn upload_flow_against_scripted_receiver() {
    let workdir = temp_dir("upload-source");
    let source = workdir.join("uploaded.bin");
    let payload: Vec<u8> = (0..50 * 1024u32)
        .map(|index| (index * 7 % 256) as u8)
        .collect();
    std::fs::write(&source, &payload).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let receiver = Arc::new(Mutex::new(UploadReceiver::new()));
    let paths = pump(
        TrzszProvider::default().start_session(&upload_offer()),
        {
            let receiver = receiver.clone();
            move |kind, value| {
                let mut receiver = receiver.lock().unwrap();
                receiver.on_client_line(kind, value);
                std::mem::take(&mut receiver.out)
            }
        },
        Dialogs {
            upload_paths: Some(vec![source.clone()]),
            download_destination: None,
        },
        &host,
    );

    assert_eq!(paths, vec![source.clone()]);
    assert_eq!(
        receiver.lock().unwrap().received_files,
        vec![("uploaded.bin".to_string(), payload)]
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn upload_multiple_files_with_trzsz_library_protocol() {
    let workdir = temp_dir("multi-upload-source");
    let first = workdir.join("first.txt");
    let second = workdir.join("second.bin");
    let first_payload: Vec<u8> = (0..8 * 1024u32).map(|index| (index % 239) as u8).collect();
    let second_payload: Vec<u8> = (0..3 * 1024u32)
        .map(|index| (index * 7 % 251) as u8)
        .collect();
    std::fs::write(&first, &first_payload).unwrap();
    std::fs::write(&second, &second_payload).unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let receiver = Arc::new(Mutex::new(UploadReceiver::new()));
    let paths = vec![first.clone(), second.clone()];
    let completed = pump(
        TrzszProvider::default().start_session(&upload_offer()),
        {
            let receiver = receiver.clone();
            move |kind, value| {
                let mut receiver = receiver.lock().unwrap();
                receiver.on_client_line(kind, value);
                std::mem::take(&mut receiver.out)
            }
        },
        Dialogs {
            upload_paths: Some(paths.clone()),
            download_destination: None,
        },
        &host,
    );

    assert_eq!(completed, paths);
    assert_eq!(
        receiver.lock().unwrap().received_files,
        vec![
            ("first.txt".to_string(), first_payload),
            ("second.bin".to_string(), second_payload),
        ]
    );
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn user_cancelled_upload_sends_unconfirmed_act() {
    let mut session = TrzszProvider::default().start_session(&upload_offer());
    let actions = session.start();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::NeedUploadPaths))
    );

    let actions = session.submit(HostEvent::UploadPaths(None));
    // The remote is told the transfer is not confirmed, and the result is a
    // cancellation rather than an empty "completed".
    assert!(actions
        .iter()
        .any(|action| matches!(action, SessionAction::WriteWire(bytes) if bytes.windows(4).any(|window| window == b"ACT:"))));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::Cancelled)),
        "cancelling the picker must not report completion: {actions:?}"
    );
}

#[test]
fn user_cancelled_download_dir_sends_unconfirmed_act() {
    let mut session = TrzszProvider::default().start_session(&download_offer());
    let actions = session.start();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::NeedDownloadDir))
    );

    let actions = session.submit(HostEvent::DownloadDir(None));
    // Without this the remote `tsz` is left waiting for an ACT until its own
    // timeout, and the UI shows "completed" for a transfer that never ran.
    assert!(actions
        .iter()
        .any(|action| matches!(action, SessionAction::WriteWire(bytes) if bytes.windows(4).any(|window| window == b"ACT:"))));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::Cancelled)),
        "cancelling the directory picker must report a cancel: {actions:?}"
    );
}

/// A remote that never sends a newline must not be able to grow the session
/// line buffer without bound (§3.5).
#[test]
fn unterminated_line_fails_the_session() {
    let mut session = TrzszProvider::default().start_session(&download_offer());
    let actions = session.start();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::NeedDownloadDir))
    );
    let _ = session.submit(HostEvent::DownloadDir(Some(PathBuf::from("/tmp"))));

    let huge = vec![b'a'; 4 * 1024 * 1024 + 1];
    let actions = session.feed_wire(&huge);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::Failed(_))),
        "an unterminated line must fail the session, got {actions:?}"
    );
}

#[test]
fn oversized_download_chunk_fails_the_session() {
    // A DATA chunk longer than the declared SIZE must abort the session.
    let destination = temp_dir("corrupt");
    let host = AppTransferHost::new(None, u64::MAX);
    let mut session = TrzszProvider::default().start_session(&download_offer());
    let mut actions: VecDeque<SessionAction> = session.start().into();
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::NeedDownloadDir))
    );
    host.set_destination(&destination);

    let mut failed = None;
    let mut wire: VecDeque<Vec<u8>> = VecDeque::from(vec![
        string_line("CFG", br#"{"binary":false}"#),
        integer_line("NUM", 1),
        string_line("NAME", b"corrupt.bin"),
        integer_line("SIZE", 4),
        // Valid encoding, but 16 bytes against a declared size of 4.
        string_line("DATA", [0u8; 16]),
    ]);

    loop {
        while let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(_) => {}
                SessionAction::OpenWrite { remote_name, .. } => {
                    actions.extend(session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                        size: 0,
                        local_name: Some(remote_name),
                    }))));
                }
                SessionAction::CloseFile => {
                    let result = host.close_write();
                    actions.extend(session.submit(HostEvent::FileClosed(result)));
                }
                SessionAction::CommitFile => {
                    let result = host.commit();
                    actions.extend(session.submit(HostEvent::FileCommitted(result)));
                }
                SessionAction::NeedDownloadDir => {
                    actions
                        .extend(session.submit(HostEvent::DownloadDir(Some(destination.clone()))));
                }
                SessionAction::Failed(reason) => {
                    failed = Some(reason);
                    break;
                }
                SessionAction::Done { .. } => panic!("completed despite corruption"),
                _ => {}
            }
        }
        if failed.is_some() {
            break;
        }
        match wire.pop_front() {
            Some(line) => actions.extend(session.feed_wire(&line)),
            None => break,
        }
    }

    assert!(
        failed
            .as_ref()
            .is_some_and(|reason| reason.contains("DATA exceeds negotiated file size")),
        "expected a size mismatch failure, got {failed:?}"
    );
    let _ = std::fs::remove_dir_all(&destination);
}

// ─── Detector ──────────────────────────────────────────────────────────────

fn feed_all(detector: &mut TrzszDetector, bytes: &[u8]) -> DetectorVerdict {
    let mut verdict = DetectorVerdict::NoMatch;
    for &byte in bytes {
        verdict = detector.feed(byte);
        if matches!(verdict, DetectorVerdict::Matched { .. }) {
            break;
        }
    }
    verdict
}

#[test]
fn detector_matches_download_trigger() {
    let mut detector = TrzszDetector::new();
    let payload = b"\x1b[s::TRZSZ:TRANSFER:S:1.2.4:1234567890123:0\r\n";
    let verdict = feed_all(&mut detector, payload);
    match verdict {
        DetectorVerdict::Matched { offer, trigger } => {
            assert_eq!(offer.direction, Some(Direction::Download));
            assert_eq!(trigger.start, 3);
            assert_eq!(trigger.end, payload.len() as u64);
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn detector_matches_screenshot_r_trigger_from_trzsz_1_1_8() {
    let mut detector = TrzszDetector::new();
    let header = b"\x1b[s::TRZSZ:TRANSFER:R:1.1.8:9047218932100:51087\r\r\n";
    match feed_all(&mut detector, header) {
        DetectorVerdict::Matched { offer, trigger } => {
            assert_eq!(offer.direction, Some(Direction::Upload));
            assert_eq!(offer.trigger_mode, Some('R'));
            assert_eq!(offer.trigger_version.as_deref(), Some("1.1.8"));
            assert_eq!(trigger.start, 3);
            assert_eq!(trigger.end, header.len() as u64);
        }
        other => panic!("expected the real R handshake to match, got {other:?}"),
    }
}
#[test]
fn detector_matches_upload_trigger_split_at_every_offset() {
    let payload: &[u8] = b"hello\x1b[s::TRZSZ:TRANSFER:R:1.1.0:42\n";
    for split in 1..payload.len() {
        let mut detector = TrzszDetector::new();
        let first = feed_all(&mut detector, &payload[..split]);
        assert!(
            !matches!(first, DetectorVerdict::Matched { .. }),
            "prefix must not match at split {split}"
        );
        let second = feed_all(&mut detector, &payload[split..]);
        match second {
            DetectorVerdict::Matched { offer, trigger } => {
                assert_eq!(offer.direction, Some(Direction::Upload));
                assert_eq!(trigger.start, 8);
                assert_eq!(trigger.end, payload.len() as u64);
            }
            other => panic!("expected match at split {split}, got {other:?}"),
        }
    }
}

#[test]
fn detector_treats_directory_mode_as_an_upload_and_requests_directory_picker() {
    let mut detector = TrzszDetector::new();
    let payload = b"::TRZSZ:TRANSFER:D:1.2.4:7:0\n";
    match feed_all(&mut detector, payload) {
        DetectorVerdict::Matched { offer, .. } => {
            assert_eq!(offer.direction, Some(Direction::Upload));
            assert_eq!(offer.trigger_mode, Some('D'));
            let mut session = TrzszProvider::default().start_session(&offer);
            assert!(
                session
                    .start()
                    .iter()
                    .any(|action| matches!(action, SessionAction::NeedUploadPathsWithDirectories))
            );
        }
        other => panic!("expected a directory upload trigger, got {other:?}"),
    }
}
#[test]
fn directory_mode_upload_advertises_support_directory_to_trz() {
    let workdir = temp_dir("directory-mode");
    let source = workdir.join("source.txt");
    std::fs::write(&source, b"directory-mode payload").unwrap();
    let offer = TransferOffer {
        provider_id: "trzsz".into(),
        direction: Some(Direction::Upload),
        trigger_mode: Some('D'),
        trigger_version: Some("1.2.4".into()),
        remote_names: Vec::new(),
    };
    let mut session = TrzszProvider::default().start_session(&offer);
    assert!(matches!(
        session.start().as_slice(),
        [SessionAction::NeedUploadPathsWithDirectories]
    ));

    let actions = session.submit(HostEvent::UploadPaths(Some(vec![source.clone()])));
    let frame = actions
        .iter()
        .find_map(|action| match action {
            SessionAction::WriteWire(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        })
        .expect("confirmed upload sends ACT");
    let (kind, value) = frame.trim_end().split_once(':').expect("ACT frame");
    assert_eq!(kind, "#ACT");
    let act: serde_json::Value = serde_json::from_slice(&decode_value(value)).unwrap();
    assert_eq!(act["support_dir"], true);
    assert_eq!(act["confirm"], true);

    drop(session);
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn legacy_remote_version_negotiates_protocol_two() {
    let offer = TransferOffer {
        provider_id: "trzsz".into(),
        direction: Some(Direction::Upload),
        trigger_mode: Some('R'),
        trigger_version: Some("1.1.3".into()),
        remote_names: Vec::new(),
    };
    let mut session = TrzszProvider::default().start_session(&offer);
    assert!(matches!(
        session.start().as_slice(),
        [SessionAction::NeedUploadPaths]
    ));

    let actions = session.submit(HostEvent::UploadPaths(Some(vec![PathBuf::from(
        "unused.bin",
    )])));
    let frame = actions
        .iter()
        .find_map(|action| match action {
            SessionAction::WriteWire(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        })
        .expect("confirmed upload sends ACT");
    let (_, value) = frame.trim_end().split_once(':').expect("ACT frame");
    let action: serde_json::Value = serde_json::from_slice(&decode_value(value)).unwrap();
    assert_eq!(action["protocol"], 2);

    drop(session);
}

#[test]
fn upload_rejects_files_over_configured_size_before_announcing_count() {
    let workdir = temp_dir("oversized-upload");
    let source = workdir.join("oversized.bin");
    std::fs::write(&source, b"0123456789").unwrap();
    let mut session = TrzszProvider::new(8).start_session(&upload_offer());
    assert!(matches!(
        session.start().as_slice(),
        [SessionAction::NeedUploadPaths]
    ));

    let actions = session.submit(HostEvent::UploadPaths(Some(vec![source.clone()])));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, SessionAction::WriteWire(_)))
    );
    let configuration =
        br#"{"quiet":true,"binary":false,"overwrite":true,"bufsize":1048576,"timeout":20}"#;
    let actions = session.feed_wire(&string_line("CFG", configuration));
    assert!(actions.iter().any(|action| {
        matches!(action, SessionAction::Failed(reason) if reason.contains("file size limit exceeded"))
    }));
    assert!(!actions.iter().any(|action| {
        matches!(action, SessionAction::WriteWire(bytes) if bytes.windows(b"#NUM:".len()).any(|window| window == b"#NUM:"))
    }));

    drop(session);
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn detector_ignores_junk_containing_marker() {
    let mut detector = TrzszDetector::new();
    let verdict = feed_all(&mut detector, b"junk::TRZSZ:TRANSFER:X:1.0.0:0\n");
    assert!(matches!(verdict, DetectorVerdict::NoMatch));
}

#[test]
fn detector_survives_garbage_without_matching() {
    let mut detector = TrzszDetector::new();
    let verdict = feed_all(&mut detector, b"plain terminal output;\nnothing to see.\n");
    assert!(matches!(verdict, DetectorVerdict::NoMatch));
}

/// Regression: a false alarm used to reset the detector's position, so a real
/// trigger later in the same batch reported a range relative to the reset.
/// The mux slices the stream with those coordinates, so it would flush and
/// divert the wrong bytes.
#[test]
fn detector_ranges_stay_absolute_across_a_false_alarm() {
    let chunk = b"::TRZSZ:TRANSFER:X:1.0.0:0\n::TRZSZ:TRANSFER:R:1.0.0:1\n";
    let marker = b"::TRZSZ:TRANSFER:";
    let absolute = chunk
        .windows(marker.len())
        .enumerate()
        .filter(|(_, window)| *window == marker)
        .nth(1)
        .map(|(index, _)| index)
        .expect("the chunk has two markers") as u64;

    let mut detector = TrzszDetector::new();
    match feed_all(&mut detector, chunk) {
        DetectorVerdict::Matched { offer, trigger } => {
            assert_eq!(offer.direction, Some(Direction::Upload));
            assert_eq!(trigger.start, absolute, "range must be absolute");
            assert_eq!(trigger.end, chunk.len() as u64);
        }
        other => panic!("expected the second marker to match, got {other:?}"),
    }
}

// ─── Interop (requires a real `tsz` binary) ────────────────────────────────

#[test]
fn detector_matches_real_tsz_handshake() {
    let Ok(tsz) = std::env::var("ZEDTERM_TSZ_BIN") else {
        return;
    };
    let workdir = temp_dir("interop");
    let source = workdir.join("interop.txt");
    std::fs::write(&source, b"interop\n").unwrap();

    let mut child = Command::new(&tsz)
        .arg("-y")
        .arg("-q")
        .arg(&source)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn tsz");
    let mut stdout = child.stdout.take().unwrap();

    let mut detector = TrzszDetector::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stdout.read(&mut buffer).unwrap();
        if count == 0 {
            panic!("tsz closed stdout before the trigger");
        }
        if matches!(
            feed_all(&mut detector, &buffer[..count]),
            DetectorVerdict::Matched { .. }
        ) {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn real_trz_file_trigger_requests_a_file_upload_picker() {
    let Ok(trz) = std::env::var("ZEDTERM_TRZ_BIN") else {
        return;
    };
    let workdir = temp_dir("trz-picker-trigger");
    let mut child = Command::new(&trz)
        .arg("-y")
        .arg("-q")
        .arg(&workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn trz");
    let mut stdout = child.stdout.take().unwrap();
    let mut detector = TrzszDetector::new();
    let mut buffer = [0u8; 4096];
    let offer = loop {
        let count = stdout.read(&mut buffer).unwrap();
        assert!(count > 0, "trz closed stdout before its trigger");
        if let DetectorVerdict::Matched { offer, .. } = feed_all(&mut detector, &buffer[..count]) {
            break offer;
        }
    };

    assert_eq!(offer.direction, Some(Direction::Upload));
    assert_eq!(offer.trigger_mode, Some('R'));
    let mut session = TrzszProvider::default().start_session(&offer);
    assert!(matches!(
        session.start().as_slice(),
        [SessionAction::NeedUploadPaths]
    ));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&workdir);
}
#[test]
fn real_trz_directory_trigger_requests_a_directory_upload_picker() {
    let Ok(trz) = std::env::var("ZEDTERM_TRZ_BIN") else {
        return;
    };
    let workdir = temp_dir("trz-directory-picker-trigger");
    let mut child = Command::new(&trz)
        .arg("-d")
        .arg("-y")
        .arg("-q")
        .arg(&workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn directory-mode trz");
    let mut stdout = child.stdout.take().unwrap();
    let mut detector = TrzszDetector::new();
    let mut buffer = [0u8; 4096];
    let offer = loop {
        let count = stdout.read(&mut buffer).unwrap();
        assert!(
            count > 0,
            "directory-mode trz closed stdout before its trigger"
        );
        if let DetectorVerdict::Matched { offer, .. } = feed_all(&mut detector, &buffer[..count]) {
            break offer;
        }
    };

    assert_eq!(offer.direction, Some(Direction::Upload));
    assert_eq!(offer.trigger_mode, Some('D'));
    let mut session = TrzszProvider::default().start_session(&offer);
    assert!(matches!(
        session.start().as_slice(),
        [SessionAction::NeedUploadPathsWithDirectories]
    ));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&workdir);
}
// ─── Full interop against a real `tsz` ─────────────────────────────────────

#[test]
fn download_interop_with_real_tsz() {
    let Ok(tsz) = std::env::var("ZEDTERM_TSZ_BIN") else {
        return;
    };
    let workdir = temp_dir("interop-e2e");
    let destination = workdir.join("out");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("interop-source.bin");
    // Enough bytes to cross several chunks and exercise stop-and-wait.
    let payload: Vec<u8> = (0..300 * 1024u32)
        .map(|index| (index % 253) as u8)
        .collect();
    std::fs::write(&source, &payload).unwrap();

    let mut child = Command::new(&tsz)
        .arg("-y")
        .arg("-q")
        .arg(&source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn tsz");
    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    host.set_destination(&destination);
    let mut session = TrzszProvider::default().start_session(&download_offer());

    let mut actions: VecDeque<SessionAction> = session.start().into();
    let mut done = None;
    let mut buffer = [0u8; 4096];
    let mut loops = 0;

    'outer: loop {
        loops += 1;
        assert!(loops < 100_000, "interop did not terminate");
        while let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(bytes) => {
                    stdin.write_all(&bytes).unwrap();
                    stdin.flush().unwrap();
                }
                SessionAction::OpenWrite { remote_name, size } => {
                    let local_name = host.open_write(&remote_name, size).unwrap();
                    actions.extend(session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                        size: 0,
                        local_name: Some(local_name),
                    }))));
                }
                SessionAction::WriteFile { offset, data } => {
                    let result = host.write_chunk(offset, &data);
                    actions.extend(session.submit(HostEvent::FileWritten { offset, result }));
                }
                SessionAction::CloseFile => {
                    let result = host.close_write();
                    actions.extend(session.submit(HostEvent::FileClosed(result)));
                }
                SessionAction::CommitFile => {
                    let result = host.commit();
                    actions.extend(session.submit(HostEvent::FileCommitted(result)));
                }
                SessionAction::NeedDownloadDir => {
                    actions
                        .extend(session.submit(HostEvent::DownloadDir(Some(destination.clone()))));
                }
                SessionAction::Progress { .. } => {}
                SessionAction::Done { paths } => {
                    done = Some(paths);
                    break 'outer;
                }
                SessionAction::Failed(reason) => panic!("interop transfer failed: {reason}"),
                SessionAction::OpenRead { .. } | SessionAction::ReadFile { .. } => {
                    panic!("download must not read local files")
                }
                other @ (SessionAction::NeedUploadPaths
                | SessionAction::NeedUploadPathsWithDirectories
                | SessionAction::Cancelled) => {
                    panic!("unexpected dialog request: {other:?}");
                }
            }
        }
        if done.is_some() {
            break;
        }
        let count = stdout.read(&mut buffer).expect("read from tsz");
        assert!(count > 0, "tsz closed stdout before completing");
        actions.extend(session.feed_wire(&buffer[..count]));
    }

    let saved = done.expect("transfer completed");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0], destination.join("interop-source.bin"));
    assert_eq!(std::fs::read(&saved[0]).unwrap(), payload);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn upload_interop_with_real_trz() {
    let Ok(trz) = std::env::var("ZEDTERM_TRZ_BIN") else {
        return;
    };
    let workdir = temp_dir("interop-upload");
    let destination = workdir.join("received");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("uploaded-interop.bin");
    let payload: Vec<u8> = (0..200 * 1024u32)
        .map(|index| (index * 3 % 256) as u8)
        .collect();
    std::fs::write(&source, &payload).unwrap();

    let mut child = Command::new(&trz)
        .arg("-y")
        .arg("-q")
        .arg(&destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn trz");
    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();

    let host = AppTransferHost::new(None, u64::MAX);
    let mut session = TrzszProvider::default().start_session(&upload_offer());

    let mut actions: VecDeque<SessionAction> = session.start().into();
    let mut done = None;
    let mut buffer = [0u8; 4096];
    let mut loops = 0;
    let mut upload_answered = false;

    'outer: loop {
        loops += 1;
        assert!(loops < 100_000, "interop did not terminate");
        while let Some(action) = actions.pop_front() {
            match action {
                SessionAction::WriteWire(bytes) => {
                    stdin.write_all(&bytes).unwrap();
                    stdin.flush().unwrap();
                }
                SessionAction::NeedUploadPaths | SessionAction::NeedUploadPathsWithDirectories => {
                    assert!(!upload_answered, "duplicate picker request");
                    upload_answered = true;
                    actions
                        .extend(session.submit(HostEvent::UploadPaths(Some(vec![source.clone()]))));
                }
                SessionAction::OpenRead { path } => {
                    let size = host.open_read(&path).unwrap();
                    actions.extend(session.submit(HostEvent::FileOpened(Ok(OpenedFile {
                        size,
                        local_name: None,
                    }))));
                }
                SessionAction::ReadFile { offset, max_len } => {
                    let result = host.read_chunk(offset, max_len);
                    actions.extend(session.submit(HostEvent::FileData { offset, result }));
                }
                SessionAction::Progress { .. } => {}
                SessionAction::Done { paths } => {
                    done = Some(paths);
                    break 'outer;
                }
                SessionAction::Failed(reason) => panic!("interop upload failed: {reason}"),
                other @ (SessionAction::OpenWrite { .. }
                | SessionAction::WriteFile { .. }
                | SessionAction::CloseFile
                | SessionAction::CommitFile
                | SessionAction::NeedDownloadDir
                | SessionAction::Cancelled) => {
                    panic!("upload must not write local files: {other:?}");
                }
            }
        }
        if done.is_some() {
            break;
        }
        let count = stdout.read(&mut buffer).expect("read from trz");
        assert!(count > 0, "trz closed stdout before completing");
        actions.extend(session.feed_wire(&buffer[..count]));
    }

    assert_eq!(done, Some(vec![source.clone()]));
    let received = destination.join("uploaded-interop.bin");
    assert_eq!(std::fs::read(&received).unwrap(), payload);

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&workdir);
}
