//! Adapter between trzsz-rs and the terminal transfer runtime.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use transfer_core::{
    Capabilities, DetectorVerdict, Direction, HostEvent, ProviderManifest, SessionAction,
    TransferDetector, TransferOffer, TransferProvider, TransferSession,
};
use trzsz_rs::comm::{self, FileWriter, TrzszError, check_paths_readable};
use trzsz_rs::filter::parse_trzsz_trigger;
use trzsz_rs::progress::ProgressCallback;
use trzsz_rs::transfer::TrzszTransfer;
use trzsz_rs::version::TrzszVersion;

const TRIGGER_MARKER: &[u8] = b"::TRZSZ:TRANSFER:";
const MAX_TRIGGER_LINE: usize = 4096;
const MAX_SESSION_BUFFER: usize = 4 * 1024 * 1024;
const MAX_TRANSFER_BUF_SIZE: i64 = 512 * 1024;
const PROGRESS_REPORT_INTERVAL: Duration = Duration::from_millis(50);
const REPLY_POLL: Duration = Duration::from_millis(50);

fn progress_report_due(
    bytes_done: u64,
    last_reported: u64,
    last_reported_at: Instant,
    now: Instant,
) -> bool {
    bytes_done != last_reported
        && now.saturating_duration_since(last_reported_at) >= PROGRESS_REPORT_INTERVAL
}

/// Trigger detector for trzsz's handshake line. The crate owns parsing and
/// validation; this adapter only tracks the streaming range required by the mux.
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

impl TrzszDetector {
    pub fn new() -> Self {
        Self {
            tail: Vec::new(),
            candidate_start: None,
            line: Vec::new(),
            position: 0,
        }
    }

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
                return match parse_trzsz_trigger(&self.line) {
                    Some(trigger) => {
                        let direction = trigger_direction(trigger.mode);
                        DetectorVerdict::Matched {
                            offer: TransferOffer {
                                provider_id: "trzsz".into(),
                                direction: Some(direction),
                                trigger_mode: Some(trigger.mode),
                                trigger_version: trigger.version.as_ref().map(|version| {
                                    format!("{}.{}.{}", version.major, version.minor, version.patch)
                                }),
                                remote_names: Vec::new(),
                            },
                            trigger: start..index + 1,
                        }
                    }
                    None => {
                        self.clear_candidate();
                        DetectorVerdict::NoMatch
                    }
                };
            }
            if self.line.len() > MAX_TRIGGER_LINE {
                self.clear_candidate();
                return DetectorVerdict::NoMatch;
            }
            return DetectorVerdict::NeedMore;
        }

        self.tail.push(byte);
        let max = self.tail.len().min(TRIGGER_MARKER.len());
        let mut keep = 0;
        for length in (0..=max).rev() {
            let suffix = &self.tail[self.tail.len() - length..];
            if suffix == &TRIGGER_MARKER[..length] {
                keep = length;
                break;
            }
        }
        self.tail.drain(..self.tail.len() - keep);
        if keep == TRIGGER_MARKER.len() {
            self.candidate_start = Some(index + 1 - TRIGGER_MARKER.len() as u64);
            self.line = TRIGGER_MARKER.to_vec();
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

fn trigger_direction(mode: char) -> Direction {
    match mode {
        'S' => Direction::Download,
        'R' | 'D' => Direction::Upload,
        _ => Direction::Download,
    }
}

fn maximum_protocol_for(remote_version: Option<&TrzszVersion>) -> i32 {
    let oldest_protocol_two_version = TrzszVersion {
        major: 1,
        minor: 1,
        patch: 0,
    };
    let newest_protocol_two_version = TrzszVersion {
        major: 1,
        minor: 1,
        patch: 3,
    };
    if remote_version.is_some_and(|version| {
        version.compare(&oldest_protocol_two_version) >= 0
            && version.compare(&newest_protocol_two_version) <= 0
    }) {
        2
    } else {
        4
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionState {
    AwaitingPaths,
    AwaitingDirectory,
    Running,
    Finished,
}

type Reply<T> = SyncSender<Result<T, String>>;

enum WorkerEvent {
    Wire(Vec<u8>),
    WaitingForWire,
    OpenWrite {
        remote_name: String,
        reply: Reply<String>,
    },
    WriteFile {
        offset: u64,
        data: Vec<u8>,
        reply: Reply<()>,
    },
    CloseFile {
        reply: Reply<PathBuf>,
    },
    CommitFile {
        reply: Reply<PathBuf>,
    },
    Progress {
        file_index: usize,
        file_count: usize,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    Done,
    Failed(String),
    Cancelled,
}

struct Worker {
    input: SyncSender<Vec<u8>>,
    events: Receiver<WorkerEvent>,
    cancelled: Arc<AtomicBool>,
    buffer_stopped: Arc<AtomicBool>,
}

enum PendingHostReply {
    OpenWrite(Reply<String>),
    WriteFile(Reply<()>),
    CloseFile(Reply<PathBuf>),
    CommitFile(Reply<PathBuf>),
}

/// The protocol runs on its own thread because trzsz-rs exposes a synchronous
/// caller API. Channel-backed host writers preserve the transfer runtime's
/// staging and commit policy without blocking the PTY reader.
pub struct TrzszSession {
    direction: Direction,
    state: SessionState,
    directory_mode: bool,
    remote_version: Option<TrzszVersion>,
    max_file_size: u64,
    worker: Option<Worker>,
    pending_host_reply: Option<PendingHostReply>,
    completed: Vec<PathBuf>,
    selected_paths: Vec<PathBuf>,
    pending_line_bytes: usize,
}

impl TrzszSession {
    pub fn new(
        direction: Direction,
        directory_mode: bool,
        remote_version: Option<TrzszVersion>,
        max_file_size: u64,
    ) -> Self {
        Self {
            direction,
            directory_mode,
            remote_version,
            max_file_size,
            state: match direction {
                Direction::Upload => SessionState::AwaitingPaths,
                Direction::Download => SessionState::AwaitingDirectory,
            },
            worker: None,
            pending_host_reply: None,
            completed: Vec::new(),
            selected_paths: Vec::new(),
            pending_line_bytes: 0,
        }
    }

    fn start_worker(&mut self, confirmed: bool, paths: Option<Vec<PathBuf>>) -> Vec<SessionAction> {
        self.selected_paths = if self.direction == Direction::Upload {
            paths.clone().unwrap_or_default()
        } else {
            Vec::new()
        };
        let (event_sender, event_receiver) = mpsc::channel();
        let mut transfer = TrzszTransfer::new(Box::new(ProtocolWriter {
            events: event_sender.clone(),
        }));
        transfer.transfer_config.bufsize = MAX_TRANSFER_BUF_SIZE;
        let input = transfer.buffer.sender();
        let buffer_stopped = transfer.buffer.stop_handle();
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled = cancelled.clone();
        transfer.buffer.set_waiting_callback({
            let event_sender = event_sender.clone();
            move || {
                if let Err(error) = event_sender.send(WorkerEvent::WaitingForWire) {
                    log::debug!("trzsz adapter is no longer receiving protocol events: {error}");
                }
            }
        });
        let direction = self.direction;
        let directory_mode = self.directory_mode;
        let remote_version = self.remote_version.clone();
        let max_file_size = self.max_file_size;
        let spawn_result = thread::Builder::new()
            .name("zedterm-trzsz-session".to_string())
            .spawn(move || {
                run_protocol_worker(
                    transfer,
                    direction,
                    directory_mode,
                    remote_version,
                    max_file_size,
                    confirmed,
                    paths,
                    event_sender,
                    thread_cancelled,
                );
            });
        if let Err(error) = spawn_result {
            self.state = SessionState::Finished;
            return vec![SessionAction::Failed(format!(
                "cannot start trzsz session: {error}"
            ))];
        }

        self.worker = Some(Worker {
            input,
            events: event_receiver,
            cancelled,
            buffer_stopped,
        });
        self.state = SessionState::Running;
        self.drive_worker()
    }

    fn drive_worker(&mut self) -> Vec<SessionAction> {
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        loop {
            let event = match worker.events.recv() {
                Ok(event) => event,
                Err(error) => {
                    self.state = SessionState::Finished;
                    actions.push(SessionAction::Failed(format!(
                        "trzsz protocol worker stopped unexpectedly: {error}"
                    )));
                    return actions;
                }
            };
            match event {
                WorkerEvent::Wire(bytes) => actions.push(SessionAction::WriteWire(bytes)),
                WorkerEvent::WaitingForWire => return actions,
                WorkerEvent::OpenWrite { remote_name, reply } => {
                    self.pending_host_reply = Some(PendingHostReply::OpenWrite(reply));
                    actions.push(SessionAction::OpenWrite {
                        remote_name,
                        size: None,
                    });
                    return actions;
                }
                WorkerEvent::WriteFile {
                    offset,
                    data,
                    reply,
                } => {
                    self.pending_host_reply = Some(PendingHostReply::WriteFile(reply));
                    actions.push(SessionAction::WriteFile { offset, data });
                    return actions;
                }
                WorkerEvent::CloseFile { reply } => {
                    self.pending_host_reply = Some(PendingHostReply::CloseFile(reply));
                    actions.push(SessionAction::CloseFile);
                    return actions;
                }
                WorkerEvent::CommitFile { reply } => {
                    self.pending_host_reply = Some(PendingHostReply::CommitFile(reply));
                    actions.push(SessionAction::CommitFile);
                    return actions;
                }
                WorkerEvent::Progress {
                    file_index,
                    file_count,
                    bytes_done,
                    bytes_total,
                } => actions.push(SessionAction::Progress {
                    file_index,
                    file_count,
                    bytes_done,
                    bytes_total,
                }),
                WorkerEvent::Done => {
                    self.state = SessionState::Finished;
                    let paths = match self.direction {
                        Direction::Upload => self.selected_paths.clone(),
                        Direction::Download => self.completed.clone(),
                    };
                    actions.push(SessionAction::Done { paths });
                    return actions;
                }
                WorkerEvent::Failed(reason) => {
                    self.state = SessionState::Finished;
                    actions.push(SessionAction::Failed(reason));
                    return actions;
                }
                WorkerEvent::Cancelled => {
                    self.state = SessionState::Finished;
                    actions.push(SessionAction::Cancelled);
                    return actions;
                }
            }
        }
    }

    fn answer_host_request(&mut self, event: HostEvent) -> Vec<SessionAction> {
        let Some(pending) = self.pending_host_reply.take() else {
            return Vec::new();
        };
        match (pending, event) {
            (PendingHostReply::OpenWrite(reply), HostEvent::FileOpened(result)) => {
                let result = result
                    .and_then(|opened| {
                        opened.local_name.ok_or_else(|| {
                            io::Error::other("host did not report the sanitized file name")
                        })
                    })
                    .map_err(|error| error.to_string());
                send_reply(reply, result);
            }
            (PendingHostReply::WriteFile(reply), HostEvent::FileWritten { result, .. }) => {
                send_reply(reply, result.map_err(|error| error.to_string()))
            }
            (PendingHostReply::CloseFile(reply), HostEvent::FileClosed(result)) => {
                send_reply(reply, result.map_err(|error| error.to_string()));
            }
            (PendingHostReply::CommitFile(reply), HostEvent::FileCommitted(result)) => match result
            {
                Ok(path) => {
                    self.completed.push(path.clone());
                    send_reply(reply, Ok(path));
                }
                Err(error) => send_reply(reply, Err(error.to_string())),
            },
            (pending, _) => {
                self.pending_host_reply = Some(pending);
                return Vec::new();
            }
        }
        self.drive_worker()
    }
}

impl Drop for TrzszSession {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.as_ref() {
            worker.cancelled.store(true, Ordering::SeqCst);
            worker.buffer_stopped.store(true, Ordering::SeqCst);
        }
    }
}

impl TransferSession for TrzszSession {
    fn start(&mut self) -> Vec<SessionAction> {
        match self.direction {
            Direction::Upload if self.directory_mode => {
                vec![SessionAction::NeedUploadPathsWithDirectories]
            }
            Direction::Upload => vec![SessionAction::NeedUploadPaths],
            Direction::Download => vec![SessionAction::NeedDownloadDir],
        }
    }

    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction> {
        if self.state != SessionState::Running {
            return Vec::new();
        }
        let mut line_length = self.pending_line_bytes;
        for byte in bytes {
            if *byte == b'\n' {
                line_length = 0;
            } else {
                line_length = line_length.saturating_add(1);
                if line_length > MAX_SESSION_BUFFER {
                    self.state = SessionState::Finished;
                    return vec![SessionAction::Failed(
                        "trzsz protocol line exceeds the session buffer limit".into(),
                    )];
                }
            }
        }
        self.pending_line_bytes = line_length;
        let Some(worker) = self.worker.as_ref() else {
            return Vec::new();
        };
        if let Err(error) = worker.input.send(bytes.to_vec()) {
            self.state = SessionState::Finished;
            return vec![SessionAction::Failed(format!(
                "cannot deliver bytes to trzsz-rs: {error}"
            ))];
        }
        self.drive_worker()
    }

    fn submit(&mut self, event: HostEvent) -> Vec<SessionAction> {
        if self.state == SessionState::Finished {
            return Vec::new();
        }
        match event {
            HostEvent::UploadPaths(paths) if self.state == SessionState::AwaitingPaths => {
                let confirmed = paths.as_ref().is_some_and(|paths| !paths.is_empty());
                self.start_worker(confirmed, paths.filter(|paths| !paths.is_empty()))
            }
            HostEvent::DownloadDir(path) if self.state == SessionState::AwaitingDirectory => {
                self.start_worker(path.is_some(), None)
            }
            HostEvent::FileOpened(_)
            | HostEvent::FileData { .. }
            | HostEvent::FileWritten { .. }
            | HostEvent::FileClosed(_)
            | HostEvent::FileCommitted(_) => self.answer_host_request(event),
            HostEvent::Cancelled | HostEvent::TimedOut => {
                self.state = SessionState::Finished;
                if let Some(worker) = self.worker.as_ref() {
                    worker.cancelled.store(true, Ordering::SeqCst);
                    worker.buffer_stopped.store(true, Ordering::SeqCst);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn abort_bytes(&mut self) -> Vec<u8> {
        self.state = SessionState::Finished;
        if let Some(worker) = self.worker.as_ref() {
            worker.cancelled.store(true, Ordering::SeqCst);
            worker.buffer_stopped.store(true, Ordering::SeqCst);
        }
        TrzszTransfer::abort_bytes("canceled by user")
    }
}

fn send_reply<T>(reply: Reply<T>, value: Result<T, String>) {
    if let Err(error) = reply.send(value) {
        log::debug!("trzsz host request was abandoned: {error}");
    }
}

fn await_reply<T>(reply: Receiver<Result<T, String>>, cancelled: &AtomicBool) -> Result<T, String> {
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("transfer cancelled".into());
        }
        match reply.recv_timeout(REPLY_POLL) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("terminal transfer host disconnected".into());
            }
        }
    }
}

struct ProtocolWriter {
    events: mpsc::Sender<WorkerEvent>,
}

impl io::Write for ProtocolWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.events
            .send(WorkerEvent::Wire(bytes.to_vec()))
            .map_err(|error| {
                io::Error::other(format!("protocol output receiver stopped: {error}"))
            })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HostFileWriter {
    events: mpsc::Sender<WorkerEvent>,
    cancelled: Arc<AtomicBool>,
    offset: u64,
    size: u64,
}

impl FileWriter for HostFileWriter {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.events
            .send(WorkerEvent::WriteFile {
                offset: self.offset,
                data: bytes.to_vec(),
                reply,
            })
            .map_err(|error| io::Error::other(format!("transfer host stopped: {error}")))?;
        await_reply(response, &self.cancelled).map_err(io::Error::other)?;
        self.offset = self.offset.saturating_add(bytes.len() as u64);
        self.size = self.size.max(self.offset);
        Ok(())
    }

    // Downloads use fresh staging files, so v4 only inspects an empty local prefix.
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.offset == self.size {
            return Ok(0);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cannot read existing staged download data",
        ))
    }

    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        match position {
            io::SeekFrom::Start(offset) if offset <= self.size => {
                self.offset = offset;
                Ok(offset)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot seek outside staged download data",
            )),
        }
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn set_len(&mut self, size: u64) -> io::Result<()> {
        if size != self.size {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cannot resize staged download data",
            ));
        }
        self.offset = self.offset.min(size);
        Ok(())
    }

    fn close(&mut self) -> io::Result<()> {
        let (reply, response) = mpsc::sync_channel(1);
        self.events
            .send(WorkerEvent::CloseFile { reply })
            .map_err(|error| io::Error::other(format!("transfer host stopped: {error}")))?;
        await_reply(response, &self.cancelled)
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

fn run_protocol_worker(
    mut transfer: TrzszTransfer,
    direction: Direction,
    directory_mode: bool,
    remote_version: Option<TrzszVersion>,
    max_file_size: u64,
    confirmed: bool,
    paths: Option<Vec<PathBuf>>,
    events: mpsc::Sender<WorkerEvent>,
    cancelled: Arc<AtomicBool>,
) {
    let maximum_protocol = maximum_protocol_for(remote_version.as_ref());
    let outcome = (|| -> Result<(), TrzszError> {
        transfer.send_action_with_capabilities(
            confirmed,
            remote_version.as_ref(),
            false,
            false,
            directory_mode,
            maximum_protocol,
        )?;
        if !confirmed {
            transfer.client_exit("canceled")?;
            return Ok(());
        }
        match direction {
            Direction::Upload => upload_files(
                &mut transfer,
                paths.unwrap_or_default(),
                directory_mode,
                max_file_size,
                &events,
            )?,
            Direction::Download => download_files(&mut transfer, &events, cancelled.clone())?,
        }
        Ok(())
    })();

    match outcome {
        Ok(()) if confirmed => {
            if let Err(error) = events.send(WorkerEvent::Done) {
                log::debug!("trzsz session outcome was abandoned: {error}");
            }
        }
        Ok(()) => {
            if let Err(error) = events.send(WorkerEvent::Cancelled) {
                log::debug!("trzsz cancellation outcome was abandoned: {error}");
            }
        }
        Err(_error) if cancelled.load(Ordering::SeqCst) => {
            if let Err(send_error) = events.send(WorkerEvent::Cancelled) {
                log::debug!("trzsz cancellation outcome was abandoned: {send_error}");
            }
        }
        Err(error) => {
            transfer.client_error(&error);
            if let Err(send_error) = events.send(WorkerEvent::Failed(error.message)) {
                log::debug!("trzsz failure outcome was abandoned: {send_error}");
            }
        }
    }
}

fn upload_files(
    transfer: &mut TrzszTransfer,
    paths: Vec<PathBuf>,
    directory_mode: bool,
    max_file_size: u64,
    events: &mpsc::Sender<WorkerEvent>,
) -> Result<(), TrzszError> {
    let config = transfer.recv_config()?;
    if directory_mode && !config.directory {
        return Err(comm::simple_error(
            "Remote trzsz peer did not enable directory transfers",
        ));
    }
    let files = check_paths_readable(&paths, directory_mode)?;
    for file in &files {
        let file_size = u64::try_from(file.size)
            .map_err(|error| comm::simple_trzsz_error("Invalid file size", error))?;
        if file_size > max_file_size {
            return Err(comm::simple_error("file size limit exceeded"));
        }
    }
    if config.overwrite {
        comm::check_duplicate_names(&files)?;
    }
    let mut progress = TransferProgress::new(events.clone());
    let mut callback = Some(&mut progress as &mut dyn ProgressCallback);
    transfer.send_files(&files, &mut callback)?;
    transfer.client_exit("done")
}

fn download_files(
    transfer: &mut TrzszTransfer,
    events: &mpsc::Sender<WorkerEvent>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), TrzszError> {
    let config = transfer.recv_config()?;
    if config.directory {
        return Err(comm::simple_error(
            "Directory downloads are not supported by this terminal",
        ));
    }
    let buffer_stopped = transfer.buffer.stop_handle();
    let mut progress =
        TransferProgress::new_download(events.clone(), cancelled.clone(), buffer_stopped);
    let mut callback = Some(&mut progress as &mut dyn ProgressCallback);
    let receive_result =
        transfer.recv_files_with_writer(Path::new("."), &mut callback, |remote_name| {
            let (reply, response) = mpsc::sync_channel(1);
            events
                .send(WorkerEvent::OpenWrite {
                    remote_name: remote_name.to_string(),
                    reply,
                })
                .map_err(|error| comm::simple_trzsz_error("Transfer host stopped", error))?;
            let local_name = await_reply(response, &cancelled)
                .map_err(|error| comm::simple_trzsz_error("Open staged download failed", error))?;
            let writer: Box<dyn FileWriter> = Box::new(HostFileWriter {
                events: events.clone(),
                cancelled: cancelled.clone(),
                offset: 0,
                size: 0,
            });
            Ok((Some(writer), local_name))
        });
    if let Some(error) = progress.commit_error.take() {
        return Err(comm::simple_trzsz_error(
            "Commit downloaded file failed",
            error,
        ));
    }
    receive_result?;
    transfer.client_exit("done")
}

struct TransferProgress {
    events: mpsc::Sender<WorkerEvent>,
    file_index: usize,
    file_count: usize,
    bytes_done: u64,
    bytes_total: Option<u64>,
    last_reported: u64,
    last_reported_at: Instant,
    download: bool,
    cancelled: Arc<AtomicBool>,
    buffer_stopped: Arc<AtomicBool>,
    commit_error: Option<String>,
}

impl TransferProgress {
    fn new(events: mpsc::Sender<WorkerEvent>) -> Self {
        Self::with_download_commit(
            events,
            false,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn new_download(
        events: mpsc::Sender<WorkerEvent>,
        cancelled: Arc<AtomicBool>,
        buffer_stopped: Arc<AtomicBool>,
    ) -> Self {
        Self::with_download_commit(events, true, cancelled, buffer_stopped)
    }

    fn with_download_commit(
        events: mpsc::Sender<WorkerEvent>,
        download: bool,
        cancelled: Arc<AtomicBool>,
        buffer_stopped: Arc<AtomicBool>,
    ) -> Self {
        Self {
            events,
            file_index: 0,
            file_count: 0,
            bytes_done: 0,
            bytes_total: None,
            last_reported: 0,
            last_reported_at: Instant::now(),
            download,
            cancelled,
            buffer_stopped,
            commit_error: None,
        }
    }

    fn report(&self) {
        if let Err(error) = self.events.send(WorkerEvent::Progress {
            file_index: self.file_index,
            file_count: self.file_count,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
        }) {
            log::debug!("trzsz progress receiver stopped: {error}");
        }
    }

    fn report_progress(&mut self) {
        self.last_reported = self.bytes_done;
        self.last_reported_at = Instant::now();
        self.report();
    }

    fn commit_download(&mut self) {
        if !self.download {
            return;
        }
        let result = (|| {
            let (reply, response) = mpsc::sync_channel(1);
            self.events
                .send(WorkerEvent::CommitFile { reply })
                .map_err(|error| format!("transfer host stopped: {error}"))?;
            await_reply(response, &self.cancelled)
        })();
        if let Err(error) = result {
            self.commit_error = Some(error);
            self.buffer_stopped.store(true, Ordering::SeqCst);
        }
    }
}

impl ProgressCallback for TransferProgress {
    fn on_num(&mut self, count: i64) {
        self.file_count = usize::try_from(count).unwrap_or_default();
        self.report_progress();
    }

    fn on_name(&mut self, _name: &str) {
        self.bytes_done = 0;
        self.bytes_total = None;
        self.report_progress();
    }

    fn on_size(&mut self, size: i64) {
        self.bytes_total = u64::try_from(size).ok();
        self.report_progress();
    }

    fn on_step(&mut self, step: i64) {
        self.bytes_done = u64::try_from(step).unwrap_or_default();
        let now = Instant::now();
        if progress_report_due(
            self.bytes_done,
            self.last_reported,
            self.last_reported_at,
            now,
        ) {
            self.last_reported = self.bytes_done;
            self.last_reported_at = now;
            self.report();
        }
    }

    fn on_done(&mut self) {
        self.report_progress();
        self.commit_download();
        self.file_index = self.file_index.saturating_add(1);
    }

    fn set_pre_size(&mut self, size: i64) {
        self.bytes_total = u64::try_from(size).ok();
        self.report_progress();
    }

    fn set_pause(&mut self, _pausing: bool) {}
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn progress_updates_are_limited_to_twenty_per_second() {
        let last_reported_at = Instant::now();
        assert!(!progress_report_due(
            64 * 1024,
            0,
            last_reported_at,
            last_reported_at + Duration::from_millis(49),
        ));
        assert!(progress_report_due(
            64 * 1024,
            0,
            last_reported_at,
            last_reported_at + PROGRESS_REPORT_INTERVAL,
        ));
        assert!(!progress_report_due(
            0,
            0,
            last_reported_at,
            last_reported_at + Duration::from_secs(1),
        ));
    }
}

pub struct TrzszProvider {
    max_file_size: u64,
}

impl TrzszProvider {
    pub fn new(max_file_size: u64) -> Self {
        Self { max_file_size }
    }
}

impl Default for TrzszProvider {
    fn default() -> Self {
        Self::new(u64::MAX)
    }
}

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
        let remote_version = offer
            .trigger_version
            .as_deref()
            .and_then(TrzszVersion::parse);
        Box::new(TrzszSession::new(
            offer.direction.unwrap_or(Direction::Download),
            offer.trigger_mode == Some('D'),
            remote_version,
            self.max_file_size,
        ))
    }

    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>> {
        Some(Box::new(TrzszSession::new(
            Direction::Upload,
            false,
            None,
            self.max_file_size,
        )))
    }
}
