//! trzsz (trz/tsz) provider: line-based trigger detection plus a protocol v1
//! base64 client for upload and download (Phase 1,
//! docs/TRANSFER_EXTENSION.md).
//!
//! Protocol reference: the trzsz-rs checkout (transfer state machine,
//! `#TYPE:value` lines, base64(zlib) encoding, stop-and-wait per-chunk SUCC
//! acknowledgements) and the upstream trzsz protocol. Only base64 mode is
//! implemented: the client declares `binary: false`, so the remote never
//! sends escaped-binary chunks.

use std::io::Read as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use md5::{Digest, Md5};
use serde_json::json;

use transfer_core::{
    Capabilities, DetectorVerdict, Direction, HostEvent, OpenedFile, ProviderManifest,
    SessionAction, TransferDetector, TransferOffer, TransferProvider, TransferSession,
};

const MARKER: &[u8] = b"::TRZSZ:TRANSFER:";
const MAX_LINE_BYTES: usize = 4096;
/// Upper bound on wire bytes buffered waiting for a protocol line (§3.5).
const MAX_SESSION_BUFFER: usize = 4 * 1024 * 1024;
/// Upper bound for upload read chunks; the remote's CFG bufsize may lower it.
const MAX_UPLOAD_CHUNK: usize = 512 * 1024;
const CLIENT_VERSION: &str = "1.2.0";

/// Compress and base64 a protocol value. Compression into an in-memory
/// buffer is effectively infallible, but the error is propagated rather than
/// dropped: a silently empty payload would corrupt the transfer.
fn try_encode_bytes(bytes: &[u8]) -> std::io::Result<String> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    Ok(BASE64.encode(encoder.finish()?))
}

fn decode_value(value: &str) -> std::io::Result<Vec<u8>> {
    let compressed = BASE64
        .decode(value)
        .map_err(|error| std::io::Error::other(format!("base64 decode failed: {error}")))?;
    let mut decoder = ZlibDecoder::new(&compressed[..]);
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .map_err(|error| std::io::Error::other(format!("zlib decode failed: {error}")))?;
    Ok(decoded)
}

// ─── Trigger detection ─────────────────────────────────────────────────────

/// Trigger detector for the trzsz handshake line
/// `::TRZSZ:TRANSFER:<mode>:<version>:<id>[:<tunnel>]`.
///
/// The marker is matched with a sliding hold window (candidates survive
/// arbitrary read chunking); the mode is only trusted once the complete line
/// arrived, so binary junk ending in the marker cannot start a session.
pub struct TrzszDetector {
    tail: Vec<u8>,
    candidate_start: Option<u64>,
    line: Vec<u8>,
    position: u64,
}

impl Default for TrzszDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrzszTrigger {
    pub mode: char,
    pub version: String,
}

fn parse_trigger_line(line: &[u8]) -> Option<TrzszTrigger> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("::TRZSZ:TRANSFER:")?;
    let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
    let parts: Vec<&str> = rest[..end].split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let mode = parts[0].chars().next()?;
    if !matches!(mode, 'S' | 'R' | 'D') {
        return None;
    }
    Some(TrzszTrigger {
        mode,
        version: parts[1].to_string(),
    })
}

impl TrzszDetector {
    pub fn new() -> Self {
        TrzszDetector {
            tail: Vec::new(),
            candidate_start: None,
            line: Vec::new(),
            position: 0,
        }
    }

    /// Drop an open candidate without touching `position`: the mux measures
    /// trigger ranges from the last `reset`, so a false alarm in the middle of
    /// a batch must not rebase them.
    fn clear_candidate(&mut self) {
        self.tail.clear();
        self.candidate_start = None;
        self.line.clear();
    }
}

impl TransferDetector for TrzszDetector {
    fn feed(&mut self, byte: u8) -> DetectorVerdict {
        let index = self.position;
        self.position += 1;

        if let Some(start) = self.candidate_start {
            self.line.push(byte);
            if byte == b'\n' {
                self.candidate_start = None;
                return match parse_trigger_line(&self.line) {
                    Some(trigger) => {
                        let direction = match trigger.mode {
                            'S' | 'D' => Direction::Download,
                            _ => Direction::Upload,
                        };
                        DetectorVerdict::Matched {
                            offer: TransferOffer {
                                provider_id: "trzsz".into(),
                                direction: Some(direction),
                                remote_names: Vec::new(),
                            },
                            trigger: start..index + 1,
                        }
                    }
                    // False alarm: drop the candidate and let the held bytes
                    // flush, but keep counting for the next trigger.
                    None => {
                        self.clear_candidate();
                        DetectorVerdict::NoMatch
                    }
                };
            }
            if self.line.len() > MAX_LINE_BYTES {
                self.clear_candidate();
                return DetectorVerdict::NoMatch;
            }
            return DetectorVerdict::NeedMore;
        }

        self.tail.push(byte);
        let max = self.tail.len().min(MARKER.len());
        let mut keep = 0;
        for length in (0..=max).rev() {
            let suffix = &self.tail[self.tail.len() - length..];
            if suffix == &MARKER[..length] {
                keep = length;
                break;
            }
        }
        self.tail.drain(..self.tail.len() - keep);
        if keep == MARKER.len() {
            self.candidate_start = Some(index + 1 - MARKER.len() as u64);
            self.line = MARKER.to_vec();
            self.tail.clear();
        }
        if self.candidate_start.is_some() || keep > 0 {
            DetectorVerdict::NeedMore
        } else {
            DetectorVerdict::NoMatch
        }
    }

    fn reset(&mut self) {
        self.clear_candidate();
        self.position = 0;
    }
}

// ─── Session ───────────────────────────────────────────────────────────────

fn dotted_version(candidate: &str) -> (u32, u32, u32) {
    let mut parts = [0u32; 3];
    for (index, part) in candidate.split('.').take(3).enumerate() {
        parts[index] = part.parse().unwrap_or(0);
    }
    (parts[0], parts[1], parts[2])
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Download: the remote is sending (remote runs `tsz`).
    DownloadAwaitDir,
    DownloadAwaitCfg,
    DownloadAwaitNum,
    DownloadAwaitName,
    DownloadAwaitOpen,
    DownloadAwaitSize,
    DownloadAwaitData {
        remaining: u64,
        offset: u64,
    },
    DownloadAwaitWrite {
        remaining: u64,
        offset: u64,
        length: u64,
    },
    DownloadAwaitMd5,
    DownloadAwaitClose,
    DownloadAwaitCommit,
    /// Upload: the remote is receiving (remote runs `trz`).
    UploadAwaitPaths,
    UploadAwaitCfg,
    UploadAwaitOpen,
    UploadAwaitSuccNum,
    UploadAwaitSuccName,
    UploadAwaitSuccSize {
        size: u64,
    },
    UploadAwaitFileData {
        size: u64,
        offset: u64,
        chunk: usize,
    },
    UploadAwaitSuccData {
        size: u64,
        offset: u64,
        length: usize,
        chunk: usize,
    },
    UploadAwaitSuccMd5,
    /// Waiting for nothing; the session is over.
    Finished,
}

/// The trzsz protocol session. One instance per transfer.
pub struct TrzszSession {
    direction: Direction,
    remote_version: String,
    state: State,
    /// Upload: local files chosen by the user.
    paths: Vec<PathBuf>,
    /// Completed local paths (committed downloads / uploaded files).
    completed: Vec<PathBuf>,
    file_count: usize,
    file_index: usize,
    bytes_done: u64,
    hasher: Md5,
    upload_digest: Option<Vec<u8>>,
    bufsize: usize,
    buffer: Vec<u8>,
    finished: bool,
}

fn action_line(kind: &str, value: &str) -> SessionAction {
    SessionAction::WriteWire(format!("#{}:{}\n", kind, value).into_bytes())
}

fn send_integer(kind: &str, value: u64) -> SessionAction {
    action_line(kind, &value.to_string())
}

/// An encoding failure cannot be expressed in the protocol, so it ends the
/// session instead of sending a corrupt frame.
fn encode_failure(kind: &str, error: std::io::Error) -> SessionAction {
    log::error!("trzsz: cannot encode the {kind} frame: {error}");
    SessionAction::Failed(format!("cannot encode the {kind} protocol frame: {error}"))
}

fn send_string(kind: &str, value: &str) -> SessionAction {
    match try_encode_bytes(value.as_bytes()) {
        Ok(encoded) => action_line(kind, &encoded),
        Err(error) => encode_failure(kind, error),
    }
}

fn send_bytes(kind: &str, value: &[u8]) -> SessionAction {
    match try_encode_bytes(value) {
        Ok(encoded) => action_line(kind, &encoded),
        Err(error) => encode_failure(kind, error),
    }
}

impl TrzszSession {
    pub fn new(direction: Direction, remote_version: String) -> Self {
        TrzszSession {
            direction,
            remote_version,
            state: State::Finished,
            paths: Vec::new(),
            completed: Vec::new(),
            file_count: 0,
            file_index: 0,
            bytes_done: 0,
            hasher: Md5::new(),
            upload_digest: None,
            bufsize: MAX_UPLOAD_CHUNK,
            buffer: Vec::new(),
            finished: false,
        }
    }

    pub fn new_manual() -> Self {
        Self::new(Direction::Upload, String::new())
    }

    fn fail(&mut self, reason: impl Into<String>) -> Vec<SessionAction> {
        self.finished = true;
        self.state = State::Finished;
        let reason = reason.into();
        let mut actions = Vec::new();
        match try_encode_bytes(reason.as_bytes()) {
            Ok(encoded) => actions.push(action_line("fail", &encoded)),
            // The reason frame is best effort; the failure itself still has to
            // reach the UI.
            Err(error) => log::error!("trzsz: cannot encode the failure reason: {error}"),
        }
        actions.push(SessionAction::Failed(reason));
        actions
    }

    fn progress(&self, bytes_total: Option<u64>) -> SessionAction {
        SessionAction::Progress {
            file_index: self.file_index,
            file_count: self.file_count,
            bytes_done: self.bytes_done,
            bytes_total,
        }
    }

    fn send_act(&self, confirm: bool) -> SessionAction {
        // Old go servers (1.1.0–1.1.3) only speak protocol 2.
        let (major, minor, patch) = dotted_version(&self.remote_version);
        let protocol = if (major, minor, patch) >= (1, 1, 0) && (major, minor, patch) <= (1, 1, 3) {
            2
        } else {
            1
        };
        let action = json!({
            "lang": "zedterm",
            "version": CLIENT_VERSION,
            "confirm": confirm,
            "newline": "\n",
            "protocol": protocol,
            "binary": false,
            "support_dir": false,
        });
        send_string("ACT", &action.to_string())
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.paths.get(self.file_index).cloned()
    }

    fn current_file_name(&self) -> String {
        self.current_path()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "file".to_string())
    }

    fn parse_config(&self, value: &str) -> std::io::Result<serde_json::Value> {
        let bytes = decode_value(value)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| std::io::Error::other(format!("invalid trzsz config: {error}")))
    }

    fn handle_line(&mut self, kind: &str, value: &str) -> Vec<SessionAction> {
        if self.finished {
            return Vec::new();
        }
        match (&self.state, kind) {
            // ── Download ──
            (State::DownloadAwaitCfg, "CFG") => match self.parse_config(value) {
                Ok(config) => {
                    if config
                        .get("binary")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                    {
                        return self.fail("binary transfer mode is not supported");
                    }
                    if let Some(bufsize) = config.get("bufsize").and_then(|value| value.as_i64()) {
                        self.bufsize = bufsize.clamp(1024, MAX_UPLOAD_CHUNK as i64) as usize;
                    }
                    self.state = State::DownloadAwaitNum;
                    Vec::new()
                }
                Err(error) => self.fail(error.to_string()),
            },
            (State::DownloadAwaitNum, "NUM") => match value.parse::<usize>() {
                Ok(count) => {
                    self.file_count = count;
                    self.file_index = 0;
                    self.state = State::DownloadAwaitName;
                    vec![send_integer("SUCC", count as u64), self.progress(None)]
                }
                Err(_) => self.fail("invalid file count"),
            },
            (State::DownloadAwaitName, "NAME") => match decode_value(value) {
                Ok(remote_name) => {
                    self.hasher = Md5::new();
                    self.bytes_done = 0;
                    self.state = State::DownloadAwaitOpen;
                    vec![
                        SessionAction::OpenWrite {
                            remote_name: String::from_utf8_lossy(&remote_name).into_owned(),
                            size: None,
                        },
                        self.progress(None),
                    ]
                }
                Err(_) => self.fail("invalid file name"),
            },
            (State::DownloadAwaitSize, "SIZE") => match value.parse::<u64>() {
                Ok(size) => {
                    self.bytes_done = 0;
                    self.state = State::DownloadAwaitData {
                        remaining: size,
                        offset: 0,
                    };
                    vec![send_integer("SUCC", size), self.progress(Some(size))]
                }
                Err(_) => self.fail("invalid file size"),
            },
            (State::DownloadAwaitData { remaining, offset }, "DATA") => match decode_value(value) {
                Ok(chunk) => {
                    let length = chunk.len() as u64;
                    if length > *remaining {
                        return self.fail("received more data than declared");
                    }
                    self.hasher.update(&chunk);
                    self.bytes_done += length;
                    let (remaining, offset) = (*remaining, *offset);
                    self.state = State::DownloadAwaitWrite {
                        remaining,
                        offset,
                        length,
                    };
                    vec![
                        SessionAction::WriteFile {
                            offset,
                            data: chunk,
                        },
                        self.progress(None),
                    ]
                }
                Err(_) => self.fail("invalid data chunk"),
            },
            (State::DownloadAwaitMd5, "MD5") => match decode_value(value) {
                Ok(digest) => {
                    let expected = self.hasher.clone().finalize();
                    if digest != expected.as_slice() {
                        return self.fail("MD5 checksum mismatch");
                    }
                    self.state = State::DownloadAwaitClose;
                    vec![send_bytes("SUCC", &digest), SessionAction::CloseFile]
                }
                Err(_) => self.fail("invalid checksum"),
            },
            // ── Upload ──
            (State::UploadAwaitCfg, "CFG") => match self.parse_config(value) {
                Ok(config) => {
                    if config
                        .get("binary")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                    {
                        return self.fail("binary transfer mode is not supported");
                    }
                    if let Some(bufsize) = config.get("bufsize").and_then(|value| value.as_i64()) {
                        self.bufsize = bufsize.clamp(1024, MAX_UPLOAD_CHUNK as i64) as usize;
                    }
                    self.state = State::UploadAwaitSuccNum;
                    vec![send_integer("NUM", self.file_count as u64)]
                }
                Err(error) => self.fail(error.to_string()),
            },
            (State::UploadAwaitSuccNum, "SUCC") => match value.parse::<u64>() {
                Ok(count) if count == self.file_count as u64 => {
                    self.state = State::UploadAwaitSuccName;
                    vec![send_string("NAME", &self.current_file_name())]
                }
                _ => self.fail("remote rejected the file count"),
            },
            (State::UploadAwaitSuccName, "SUCC") => match self.current_path() {
                Some(path) => {
                    self.hasher = Md5::new();
                    self.bytes_done = 0;
                    self.state = State::UploadAwaitOpen;
                    vec![SessionAction::OpenRead { path }]
                }
                None => self.fail("no upload file"),
            },
            (State::UploadAwaitSuccSize { size }, "SUCC") => match value.parse::<u64>() {
                Ok(acked) if acked == *size => {
                    self.state = State::UploadAwaitFileData {
                        size: *size,
                        offset: 0,
                        chunk: self.bufsize,
                    };
                    vec![SessionAction::ReadFile {
                        offset: 0,
                        max_len: self.bufsize,
                    }]
                }
                _ => self.fail("remote rejected the file size"),
            },
            (
                State::UploadAwaitSuccData {
                    size,
                    offset,
                    length,
                    chunk,
                },
                "SUCC",
            ) => {
                match value.parse::<u64>() {
                    Ok(acked) if acked == *length as u64 => {
                        let (size, offset, length, chunk) = (*size, *offset, *length, *chunk);
                        let offset = offset + length as u64;
                        self.bytes_done = offset;
                        if offset >= size || length < chunk {
                            // End of file: send the checksum.
                            let digest = self.hasher.clone().finalize().to_vec();
                            self.upload_digest = Some(digest.clone());
                            self.state = State::UploadAwaitSuccMd5;
                            return vec![send_bytes("MD5", &digest)];
                        }
                        self.state = State::UploadAwaitFileData {
                            size,
                            offset,
                            chunk,
                        };
                        vec![
                            SessionAction::ReadFile {
                                offset,
                                max_len: chunk,
                            },
                            self.progress(Some(size)),
                        ]
                    }
                    _ => self.fail("remote acknowledged the wrong chunk length"),
                }
            }
            (State::UploadAwaitSuccMd5, "SUCC") => match decode_value(value) {
                Ok(digest) if Some(&digest) == self.upload_digest.as_ref() => {
                    self.file_index += 1;
                    if self.file_index < self.file_count {
                        self.state = State::UploadAwaitSuccName;
                        vec![send_string("NAME", &self.current_file_name())]
                    } else {
                        self.finished = true;
                        self.state = State::Finished;
                        vec![
                            send_string("EXIT", "done"),
                            SessionAction::Done {
                                paths: self.paths.clone(),
                            },
                        ]
                    }
                }
                _ => self.fail("remote reported a different checksum"),
            },
            (_, "EXIT") => {
                // Remote-side graceful end before we finished: treat as a
                // failure so the user sees what happened.
                let reason = decode_value(value)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_else(|| "remote ended the transfer".into());
                self.fail(reason)
            }
            (_, "fail" | "FAIL") => {
                let reason = decode_value(value)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_else(|| "remote reported a transfer error".into());
                self.fail(reason)
            }
            _ => {
                log::debug!("trzsz: ignoring unexpected {} line", kind);
                Vec::new()
            }
        }
    }

    /// All files done (download): commit each staged file. Called after the
    /// last file's checksum verified.
    fn finish_download(&mut self) -> Vec<SessionAction> {
        self.finished = true;
        self.state = State::Finished;
        vec![
            send_string("EXIT", "done"),
            SessionAction::Done {
                paths: self.completed.clone(),
            },
        ]
    }
}

impl TransferSession for TrzszSession {
    fn start(&mut self) -> Vec<SessionAction> {
        match self.direction {
            // Download: ask where to save before talking to the remote
            // (§7.1: downloads are never silent). The remote's own timeout
            // bounds how long the user may take here.
            Direction::Download => {
                self.state = State::DownloadAwaitDir;
                vec![SessionAction::NeedDownloadDir]
            }
            Direction::Upload => {
                self.state = State::UploadAwaitPaths;
                vec![SessionAction::NeedUploadPaths]
            }
        }
    }

    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction> {
        let mut actions = Vec::new();
        if self.finished {
            return actions;
        }
        self.buffer.extend_from_slice(bytes);
        // A remote that never sends a newline must not be able to grow the
        // session buffer without bound (§3.5).
        if self.buffer.len() > MAX_SESSION_BUFFER {
            return self.fail("protocol line exceeds the session buffer limit");
        }

        // Take every complete line in one move: draining one line at a time
        // shifts the tail repeatedly, which is quadratic for many short lines.
        let Some(last_newline) = self.buffer.iter().rposition(|&byte| byte == b'\n') else {
            return actions;
        };
        let consumed: Vec<u8> = self.buffer.drain(..=last_newline).collect();
        for raw in consumed.split_inclusive(|&byte| byte == b'\n') {
            let mut line = &raw[..raw.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.last() == Some(&b'!') {
                // Windows servers terminate protocol lines with '!'.
                line = &line[..line.len() - 1];
            }
            if line.contains(&0x03) {
                actions.extend(self.fail("interrupted by remote"));
                return actions;
            }
            let Some(hash) = line.iter().position(|&byte| byte == b'#') else {
                continue;
            };
            let line = &line[hash..];
            let Ok(line_text) = std::str::from_utf8(line) else {
                continue;
            };
            let Some(colon) = line_text.find(':') else {
                continue;
            };
            let (kind, value) = (&line_text[1..colon], &line_text[colon + 1..]);
            actions.extend(self.handle_line(kind, value));
            if self.finished {
                break;
            }
        }
        actions
    }

    fn submit(&mut self, event: HostEvent) -> Vec<SessionAction> {
        if self.finished {
            return Vec::new();
        }
        match event {
            HostEvent::FileOpened(Ok(OpenedFile { size, local_name })) => match &self.state {
                State::DownloadAwaitOpen => {
                    let Some(local_name) = local_name else {
                        return self.fail("host did not report the local file name");
                    };
                    self.state = State::DownloadAwaitSize;
                    vec![send_string("SUCC", &local_name)]
                }
                State::UploadAwaitOpen => {
                    self.state = State::UploadAwaitSuccSize { size };
                    vec![send_integer("SIZE", size), self.progress(Some(size))]
                }
                _ => Vec::new(),
            },
            HostEvent::FileOpened(Err(error)) => self.fail(format!("cannot open file: {error}")),
            HostEvent::FileData { offset: _, result } => match &self.state {
                State::UploadAwaitFileData {
                    size,
                    offset,
                    chunk,
                } => match result {
                    Ok(data) => {
                        let (size, offset, chunk) = (*size, *offset, *chunk);
                        if data.is_empty() {
                            return self.fail("file ended unexpectedly");
                        }
                        if offset + data.len() as u64 > size {
                            return self.fail("file grew during upload");
                        }
                        self.hasher.update(&data);
                        let length = data.len();
                        let encoded = match try_encode_bytes(&data) {
                            Ok(encoded) => encoded,
                            Err(error) => {
                                return self.fail(format!("cannot encode file data: {error}"));
                            }
                        };
                        self.state = State::UploadAwaitSuccData {
                            size,
                            offset,
                            length,
                            chunk,
                        };
                        vec![
                            SessionAction::WriteWire(format!("#DATA:{encoded}\n").into_bytes()),
                            self.progress(Some(size)),
                        ]
                    }
                    Err(error) => self.fail(format!("cannot read file: {error}")),
                },
                _ => Vec::new(),
            },
            HostEvent::FileWritten { offset: _, result } => match result {
                Ok(()) => {
                    if let State::DownloadAwaitWrite {
                        remaining,
                        offset,
                        length,
                    } = self.state
                    {
                        let remaining = remaining - length;
                        let offset = offset + length;
                        let actions = vec![send_integer("SUCC", length)];
                        if remaining == 0 {
                            self.state = State::DownloadAwaitMd5;
                        } else {
                            self.state = State::DownloadAwaitData { remaining, offset };
                        }
                        return actions;
                    }
                    Vec::new()
                }
                Err(error) => self.fail(format!("cannot write file: {error}")),
            },
            HostEvent::FileClosed(Ok(_staged_path)) => {
                if self.state == State::DownloadAwaitClose {
                    self.state = State::DownloadAwaitCommit;
                    vec![SessionAction::CommitFile]
                } else {
                    Vec::new()
                }
            }
            HostEvent::FileClosed(Err(error)) => self.fail(format!("cannot finish file: {error}")),
            HostEvent::FileCommitted(Ok(path)) => {
                if self.state == State::DownloadAwaitCommit {
                    self.completed.push(path);
                    self.file_index += 1;
                    if self.file_index < self.file_count {
                        self.state = State::DownloadAwaitName;
                        Vec::new()
                    } else {
                        self.finish_download()
                    }
                } else {
                    Vec::new()
                }
            }
            HostEvent::FileCommitted(Err(error)) => self.fail(format!("cannot save file: {error}")),
            HostEvent::UploadPaths(None) => {
                self.finished = true;
                self.state = State::Finished;
                // An unconfirmed ACT makes the remote exit; report a cancel
                // so the UI does not show "completed" for an empty transfer.
                vec![
                    self.send_act(false),
                    send_string("EXIT", "canceled"),
                    SessionAction::Cancelled,
                ]
            }
            HostEvent::UploadPaths(Some(paths)) => {
                if paths.is_empty() {
                    return self.submit(HostEvent::UploadPaths(None));
                }
                self.paths = paths;
                self.file_count = self.paths.len();
                self.file_index = 0;
                self.state = State::UploadAwaitCfg;
                vec![self.send_act(true)]
            }
            HostEvent::DownloadDir(None) => {
                self.finished = true;
                self.state = State::Finished;
                // The remote is already sending/waiting for our ACT; tell it
                // the transfer is off instead of leaving it to time out.
                vec![
                    self.send_act(false),
                    send_string("EXIT", "canceled"),
                    SessionAction::Cancelled,
                ]
            }
            HostEvent::DownloadDir(Some(_)) => {
                if self.state == State::DownloadAwaitDir {
                    self.state = State::DownloadAwaitCfg;
                    vec![self.send_act(true)]
                } else {
                    Vec::new()
                }
            }
            HostEvent::Cancelled | HostEvent::TimedOut => {
                self.finished = true;
                self.state = State::Finished;
                Vec::new()
            }
        }
    }

    fn abort_bytes(&mut self) -> Vec<u8> {
        self.finished = true;
        self.state = State::Finished;
        match try_encode_bytes(b"canceled by user") {
            Ok(encoded) => format!("#fail:{encoded}\n").into_bytes(),
            Err(error) => {
                // Nothing usable to send; the session is ending either way.
                log::error!("trzsz: cannot encode the abort frame: {error}");
                Vec::new()
            }
        }
    }
}

// ─── Provider ──────────────────────────────────────────────────────────────

pub struct TrzszProvider;

impl TransferProvider for TrzszProvider {
    fn id(&self) -> Arc<str> {
        "trzsz".into()
    }

    fn display_name(&self) -> Arc<str> {
        "trzsz".into()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            upload: true,
            download: true,
            auto_detect: true,
            drag_upload: false,
            multi_file: true,
            tmux_compatible: true,
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
            Some(other) => anyhow::bail!("trzsz: `enabled` must be a boolean, got {other}"),
        }
    }

    fn new_detector(&self) -> Box<dyn TransferDetector> {
        Box::new(TrzszDetector::new())
    }

    fn start_session(&self, offer: &TransferOffer) -> Box<dyn TransferSession> {
        Box::new(TrzszSession::new(
            offer.direction.unwrap_or(Direction::Download),
            String::new(),
        ))
    }

    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
        Some(Box::new(TrzszSession::new_manual()))
    }
}
