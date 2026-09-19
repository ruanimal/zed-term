//! Protocol-agnostic traits and types for pluggable terminal file-transfer
//! providers. See `docs/TRANSFER_EXTENSION.md`.
//!
//! This crate is deliberately dependency-poor: no gpui, no terminal state, no
//! filesystem access. Provider implementations (in-process or as external
//! helper adapters) and the mux runtime build on top of it.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Direction of a transfer, always from the local user's point of view:
/// `Upload` moves bytes from this machine to the remote, `Download` the
/// other way around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// What a provider supports. Advertised to the UI and used to skip work
/// (e.g. no detector is built for a provider without `auto_detect`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub upload: bool,
    pub download: bool,
    /// Whether the provider can recognize a transfer trigger from raw PTY
    /// output, enabling automatic session start.
    pub auto_detect: bool,
    /// Reserved for drag & drop upload; drag upload is not designed yet, so
    /// providers keep this `false`.
    pub drag_upload: bool,
    pub multi_file: bool,
    /// Whether the protocol is designed to survive tmux passthrough. Protocols
    /// whose frames are plain text (trzsz) qualify; classic binary ZMODEM does
    /// not.
    pub tmux_compatible: bool,
}

/// What a detector decided about the bytes fed to it so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectorVerdict {
    /// The bytes so far neither match nor refute a trigger; the mux must keep
    /// holding them instead of handing them to the terminal parser.
    NeedMore,
    /// No transfer trigger is in progress; held bytes (if any) can be released
    /// to the parser.
    NoMatch,
    /// A transfer trigger was recognized. `trigger` is the byte range of the
    /// trigger sequence in the detector's own coordinate space (bytes fed
    /// since the last `reset`). The mux uses it to split the byte stream:
    /// bytes before the range go to the parser, the range itself is swallowed,
    /// and everything after it is diverted to the new session.
    ///
    /// Invariant: a candidate is only ever accumulated while the detector is
    /// undecided, which is exactly while the mux holds the bytes, so a matched
    /// trigger can never have been flushed to the parser beforehand.
    Matched {
        offer: TransferOffer,
        trigger: Range<u64>,
    },
}

/// The information known about a transfer at match time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferOffer {
    pub provider_id: Arc<str>,
    /// `None` when the direction cannot be determined at match time (e.g.
    /// ZMODEM's ZRQINIT is sent by both `rz` and `sz`). The UI shows neutral
    /// copy in that case; the direction is revealed later by which dialog the
    /// session asks for (`NeedUploadPaths` vs `NeedDownloadDir`).
    pub direction: Option<Direction>,
    /// File names declared by the remote, when the trigger carries them.
    /// Display/suggestion only: they are attacker-controlled and must be
    /// sanitized before anything touches the filesystem.
    pub remote_names: Vec<String>,
}

/// Streaming trigger detector, called on the PTY IO thread. Implementations
/// must be cheap, non-blocking and use bounded memory.
pub trait TransferDetector: Send {
    /// Feed a single byte. Batch callers should prefer [`feed_bytes`].
    fn feed(&mut self, byte: u8) -> DetectorVerdict;

    /// Feed a batch of bytes. Semantically equivalent to feeding the bytes one
    /// by one (for any chunking; asserted by tests), returning the verdict of
    /// the last consumed byte. On `Matched` the detector stops early: the
    /// bytes after the trigger sequence belong to the session, not the
    /// detector.
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

    /// Forget all accumulated state. Called by the mux when held bytes are
    /// released to the parser, so a trigger can never straddle a flush.
    fn reset(&mut self);
}

/// One step of a transfer session. Sessions are caller-driven: the runtime
/// feeds wire bytes in (`feed_wire`), and the session answers with actions
/// for the runtime to perform, feeding results back via `submit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionAction {
    /// Protocol bytes to write to the PTY, in order, through the same channel
    /// as user input.
    WriteWire(Vec<u8>),
    /// Open a local file for reading (upload). `path` comes from
    /// `HostEvent::UploadPaths`; the answer arrives as `HostEvent::FileOpened`.
    OpenRead { path: PathBuf },
    /// Read up to `max_len` bytes at `offset` from the file opened by the
    /// most recent `OpenRead`. Answered with `HostEvent::FileData`.
    ReadFile { offset: u64, max_len: usize },
    /// Create a staging file for a download. `remote_name` is attacker
    /// controlled; the host sanitizes it and picks the staging location.
    /// Answered with `HostEvent::FileOpened` (carrying the reported size, if
    /// any, is not needed here) — see `HostEvent::FileClosed` for the staged
    /// path.
    OpenWrite {
        remote_name: String,
        size: Option<u64>,
    },
    /// Write bytes at `offset` into the staging file opened by the most
    /// recent `OpenWrite`. Answered with `HostEvent::FileWritten`.
    WriteFile { offset: u64, data: Vec<u8> },
    /// Finish the current staging file. Answered with `HostEvent::FileClosed`,
    /// carrying the staging path.
    CloseFile,
    /// Move the staged file to its final location (inside the directory the
    /// user confirmed). May trigger an overwrite confirmation in the host.
    /// Answered with `HostEvent::FileCommitted`.
    CommitFile,
    /// Ask the user to pick file(s) to upload.
    NeedUploadPaths,
    /// Ask the user where to save a download.
    NeedDownloadDir { suggested_name: Option<String> },
    Progress {
        file_index: usize,
        file_count: usize,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    /// The transfer finished; `paths` are local files involved (for display).
    Done { paths: Vec<PathBuf> },
    /// The transfer failed with a user-displayable reason.
    Failed(String),
    /// The transfer ended because the user declined (picker cancelled,
    /// no files chosen). The session has already told the remote.
    Cancelled,
}

/// A running transfer session's protocol state machine. Caller-driven: the
/// runtime feeds PTY bytes and host results in, and collects the actions to
/// perform. Sessions must not block, touch the filesystem or the PTY
/// directly; everything goes through actions and host events.
pub trait TransferSession: Send {
    /// Called once when the session starts, before any wire bytes arrive.
    /// Sessions that must speak first (e.g. a download client sending its
    /// ACT line) return their initial actions here.
    fn start(&mut self) -> Vec<SessionAction> {
        Vec::new()
    }

    /// Bytes received from the remote (everything the mux diverted after the
    /// trigger). Returns the actions to perform, in order.
    fn feed_wire(&mut self, bytes: &[u8]) -> Vec<SessionAction>;

    /// Feed back the result of a previously issued action (file IO result,
    /// user dialog answer, cancel/timeout). Returns follow-up actions.
    fn submit(&mut self, event: HostEvent) -> Vec<SessionAction>;

    /// Provider-defined abort sequence to send to the remote when the user
    /// cancels (or a watchdog fires). The session is finished afterwards.
    fn abort_bytes(&mut self) -> Vec<u8>;
}

/// Information about a file opened by the host.
#[derive(Debug, Clone)]
pub struct OpenedFile {
    /// Size of the file in bytes (for reads); `0` for staged writes.
    pub size: u64,
    /// The file name the host actually used. For staged downloads this is
    /// the sanitized name chosen by the host; the session reports it back to
    /// the remote when the protocol asks for the local name.
    pub local_name: Option<String>,
}

/// Results of the host completing a [`SessionAction`], fed back to the
/// session via `submit`. File IO results use `io::Result`; an `Err` is the
/// session's cue to abort and report a failure.
#[derive(Debug)]
pub enum HostEvent {
    /// Answer to `SessionAction::OpenRead` / `OpenWrite`.
    FileOpened(io::Result<OpenedFile>),
    /// Answer to `SessionAction::ReadFile`.
    FileData {
        offset: u64,
        result: io::Result<Vec<u8>>,
    },
    /// Answer to `SessionAction::WriteFile`.
    FileWritten { offset: u64, result: io::Result<()> },
    /// Answer to `SessionAction::CloseFile`: `Ok` carries the staging path.
    FileClosed(io::Result<PathBuf>),
    /// Answer to `SessionAction::CommitFile`: `Ok` carries the final path.
    FileCommitted(io::Result<PathBuf>),
    /// Answer to `SessionAction::NeedUploadPaths`; `None` = user cancelled.
    UploadPaths(Option<Vec<PathBuf>>),
    /// Answer to `SessionAction::NeedDownloadDir`; `None` = user cancelled.
    DownloadDir(Option<PathBuf>),
    /// The user cancelled the transfer.
    Cancelled,
    /// A watchdog timeout expired (picker wait, idle, size cap, ...).
    TimedOut,
}

/// Filesystem and dialog operations a session runtime performs on behalf of a
/// [`TransferSession`]. Implemented by the app layer (which owns the UI and
/// the staging/sanitizing policy); sessions never touch the filesystem or
/// dialogs directly.
///
/// All methods are synchronous and may block (dialogs wait for the user);
/// the runtime calls them from a dedicated session thread. File offsets are
/// absolute; the "current file" is the one from the most recent
/// `open_read`/`open_write`.
pub trait TransferHost: Send + Sync + 'static {
    /// Open `path` for reading (upload) and return its size in bytes.
    fn open_read(&self, path: &Path) -> io::Result<u64>;
    /// Read up to `max_len` bytes at `offset` from the current read file.
    /// Returning fewer bytes than requested (including `Ok(vec![])`) is fine
    /// and means end of file when at the reported size.
    fn read_chunk(&self, offset: u64, max_len: usize) -> io::Result<Vec<u8>>;
    /// Sanitize `remote_name` and create a staging file for a download,
    /// returning the sanitized local name.
    fn open_write(&self, remote_name: &str, size: Option<u64>) -> io::Result<String>;
    /// Write `data` at `offset` into the current staging file.
    fn write_chunk(&self, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Close the current staging file, returning its path.
    fn close_write(&self) -> io::Result<PathBuf>;
    /// Record the destination the user confirmed for the current download.
    /// Depending on the platform dialog this is either a directory or a full
    /// file path; `commit` interprets it.
    fn set_destination(&self, destination: &Path);
    /// Move the last staged file into the user-confirmed destination,
    /// resolving conflicts (overwrite confirmation or alternate name) as the
    /// app sees fit. Returns the final path.
    fn commit(&self) -> io::Result<PathBuf>;
    /// Ask the user which file(s) to upload. Blocks until the user answers or
    /// the picker times out; `None` means no files to upload.
    fn request_upload_paths(&self) -> Option<Vec<PathBuf>>;
    /// Ask the user where to save the download. `suggested_name` is display
    /// only. Blocks until the user answers or the picker times out; `None`
    /// means cancelled.
    fn request_download_dir(&self, suggested_name: Option<&str>) -> Option<PathBuf>;
    /// Remove staged-but-uncommitted files when a session aborts (§3.4).
    /// A no-op when nothing was staged or everything was committed.
    fn discard_staged(&self);
}

/// Static description of a provider: identity, capabilities and
/// configuration. Hosts use the manifest for settings validation and to
/// render provider settings without knowing individual protocols.
#[derive(Clone, Debug)]
pub struct ProviderManifest {
    pub id: Arc<str>,
    pub display_name: Arc<str>,
    pub capabilities: Capabilities,
    /// Default configuration applied when the user has not configured the
    /// provider. Must contain at least `{"enabled": bool}`.
    pub default_config: serde_json::Value,
    /// JSON-Schema subset describing the provider's configuration keys other
    /// than `enabled`. Empty object when the provider takes no parameters.
    pub config_schema: serde_json::Value,
}

/// A transfer protocol provider. One instance is shared across all terminals;
/// per-session state lives in the objects returned by the factory methods,
/// so they must not carry mutable session state themselves.
pub trait TransferProvider: Send + Sync {
    fn id(&self) -> Arc<str>;
    fn display_name(&self) -> Arc<str>;
    fn capabilities(&self) -> Capabilities;
    fn manifest(&self) -> ProviderManifest;

    /// Validate and apply the user's configuration for this provider (the
    /// whole settings subsection keyed by `id`). On `Err` the host disables
    /// the provider and logs a warning; other providers are unaffected.
    fn configure(&mut self, config: &serde_json::Value) -> anyhow::Result<()>;

    /// Build a detector for automatic trigger recognition. Only called when
    /// `capabilities().auto_detect` is true and the provider is enabled.
    fn new_detector(&self) -> Box<dyn TransferDetector>;

    /// Start a session for a detected trigger.
    fn start_session(&self, offer: &TransferOffer) -> Box<dyn TransferSession>;

    /// Start a session for a manual upload (menu "Send File…"). Providers
    /// that cannot push files return `None`.
    fn start_manual_upload(&self) -> Option<Box<dyn TransferSession>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    /// A sliding-window detector that matches the fixed trigger `GO:<name>\n`
    /// and reports the rest of the line as the remote name. Mirrors the
    /// structure the real line-based detectors use (cross-chunk candidate
    /// tracking, bounded line buffer) and exercises the
    /// `feed`/`feed_bytes` equivalence contract.
    struct LineDetector {
        /// Bytes fed since the last `reset`.
        position: u64,
        /// Last `TRIGGER.len()` bytes, used to find candidates across chunk
        /// boundaries.
        window: Vec<u8>,
        /// Set once a candidate started: absolute index of the candidate's
        /// first byte.
        candidate_start: Option<u64>,
        /// Line bytes after the candidate start (trigger included).
        line: Vec<u8>,
    }

    const TRIGGER: &[u8] = b"GO:";
    const MAX_LINE: usize = 128;

    impl LineDetector {
        fn new() -> Self {
            LineDetector {
                position: 0,
                window: Vec::new(),
                candidate_start: None,
                line: Vec::new(),
            }
        }
    }

    impl TransferDetector for LineDetector {
        fn feed(&mut self, byte: u8) -> DetectorVerdict {
            let index = self.position;
            self.position += 1;

            if let Some(start) = self.candidate_start {
                self.line.push(byte);
                if byte == b'\n' {
                    let text = String::from_utf8_lossy(&self.line);
                    return DetectorVerdict::Matched {
                        offer: TransferOffer {
                            provider_id: "line".into(),
                            direction: Some(Direction::Download),
                            remote_names: vec![text[TRIGGER.len()..text.len() - 1].to_string()],
                        },
                        trigger: start..index + 1,
                    };
                }
                if self.line.len() > MAX_LINE {
                    // False alarm: the candidate was not a real trigger line.
                    self.reset();
                    return DetectorVerdict::NoMatch;
                }
                return DetectorVerdict::NeedMore;
            }

            self.window.push(byte);
            if self.window.len() > TRIGGER.len() {
                self.window.remove(0);
            }
            if self.window.as_slice() == TRIGGER {
                self.candidate_start = Some(index + 1 - TRIGGER.len() as u64);
                self.line = self.window.clone();
                return DetectorVerdict::NeedMore;
            }
            DetectorVerdict::NoMatch
        }

        fn reset(&mut self) {
            *self = LineDetector::new();
        }
    }

    #[test]
    fn feed_and_feed_bytes_agree_for_any_chunking() {
        let payload = b"hello GO:some-file.txt\n trailing";
        let single = {
            let mut detector = LineDetector::new();
            let mut verdict = DetectorVerdict::NoMatch;
            for &byte in payload {
                verdict = detector.feed(byte);
                if matches!(verdict, DetectorVerdict::Matched { .. }) {
                    break;
                }
            }
            verdict
        };

        let mut expected: Vec<DetectorVerdict> = Vec::new();
        {
            let mut detector = LineDetector::new();
            let mut rng = rand::rng();
            let mut rest: &[u8] = payload;
            while !rest.is_empty() {
                let take = rng.random_range(1..=5).min(rest.len());
                let (chunk, remainder) = rest.split_at(take);
                let verdict = detector.feed_bytes(chunk);
                let matched = matches!(verdict, DetectorVerdict::Matched { .. });
                expected.push(verdict);
                if matched {
                    break;
                }
                rest = remainder;
            }
        }

        let matched = expected
            .iter()
            .position(|verdict| matches!(verdict, DetectorVerdict::Matched { .. }))
            .expect("trigger must match");
        assert!(
            expected[..matched].iter().all(|verdict| matches!(
                verdict,
                DetectorVerdict::NoMatch | DetectorVerdict::NeedMore
            )),
            "verdicts before the match must not be Matched"
        );
        assert_eq!(expected[matched], single);
    }

    #[test]
    fn mutated_trigger_prefix_does_not_match() {
        let payload = b"hello GO:some-file.txt\n";
        let trigger_start = 6;
        // Corrupt the trigger itself (and drop each of its bytes); corrupting
        // bytes elsewhere may legitimately still match or not.
        for skip in 0..TRIGGER.len() {
            let mut truncated = payload.to_vec();
            truncated.remove(trigger_start + skip);
            let mut detector = LineDetector::new();
            let verdict = detector.feed_bytes(&truncated);
            assert!(
                !matches!(verdict, DetectorVerdict::Matched { .. }),
                "payload with trigger byte {skip} removed must not match"
            );

            let mut mutated = payload.to_vec();
            mutated[trigger_start + skip] = mutated[trigger_start + skip].wrapping_add(1);
            let mut detector = LineDetector::new();
            let verdict = detector.feed_bytes(&mutated);
            assert!(
                !matches!(verdict, DetectorVerdict::Matched { .. }),
                "payload with trigger byte {skip} mutated must not match"
            );
        }
    }

    #[test]
    fn shuffled_garbage_never_matches() {
        // Deterministic pseudo-random bytes; includes 'G', 'O', ':' and '\n'
        // so all partial trigger prefixes occur, but never "GO:\n".
        let mut state = 0x12345678u32;
        let payload: Vec<u8> = (0..4096)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        assert!(!payload.windows(3).any(|window| window == TRIGGER));
        let mut detector = LineDetector::new();
        assert!(matches!(
            detector.feed_bytes(&payload),
            DetectorVerdict::NoMatch
        ));
    }
}
