//! End-to-end transfer tests through the *real* tap: a real PTY running a real
//! lrzsz binary, driven by the real mux, registry and host the app installs
//! (docs/TRANSFER_EXTENSION.md Phase 3).
//!
//! The session-level tests in `transfer_zmodem.rs` skip the tap and answer the
//! host by hand; this file is what proves the wiring the app actually uses —
//! detector → hold/divert → driver → session → staging — against `rz` and `sz`
//! themselves. Real binaries are required, so both tests are enabled by
//! pointing `ZEDTERM_RZ_BIN`/`ZEDTERM_SZ_BIN` at built lrzsz (`rz` and `sz`
//! are `lrz`/`lsz` symlinks in its source tree).

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use terminal_app::providers_zmodem::ZmodemProvider;
use terminal_app::transfer_io::AppTransferHost;
use terminal_core::transfer_mux::{
    TapReader, TransferPolicy, TransferRegistry, TransferRuntime, TransferUiEvent,
};
use transfer_core::{TransferHost, TransferProvider};

/// Test binaries can hang; failing loudly beats wedging the suite.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zedterm-pty-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn payload(length: usize, modulus: u32) -> Vec<u8> {
    (0..length as u32)
        .map(|index| (index % modulus) as u8)
        .collect()
}

/// A real PTY with `command` on the slave end, the way the app hands a shell
/// its terminal: its own session, the slave as controlling terminal, and a
/// non-blocking master the tap reads.
struct PseudoTerminal {
    master: std::fs::File,
    child: Child,
}

impl PseudoTerminal {
    fn spawn(command: &mut Command) -> Self {
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            opened,
            0,
            "openpty failed: {}",
            std::io::Error::last_os_error()
        );
        // Both fds are owned from here on; the slave copies go to the child.
        let master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };

        command
            .stdin(Stdio::from(slave.try_clone().expect("clone slave")))
            .stdout(Stdio::from(slave.try_clone().expect("clone slave")))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                // std duplicates the slave onto 0/1/2 before this runs.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn on the pty");

        let fd = master.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed");
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0,
            "F_SETFL failed"
        );
        PseudoTerminal { master, child }
    }

    /// The transfer tap over this terminal's master, wired to the real mux.
    fn tap(
        &self,
        shared: &Arc<terminal_core::transfer_mux::TransferShared>,
    ) -> TapReader<std::fs::File> {
        TapReader::new(
            self.master.try_clone().expect("clone master"),
            shared.clone(),
            Some(self.master.as_raw_fd()),
        )
    }
}

/// The dialogs the app's UI answers, in the order the session asks for them.
struct Dialogs {
    upload_paths: Option<Vec<PathBuf>>,
    download_dir: Option<PathBuf>,
}

/// Runs a transfer to completion through the real tap, answering dialogs the
/// way `TransferUiState` does, and returns the completed paths.
fn run_transfer(terminal: &PseudoTerminal, dialogs: Dialogs) -> Result<Vec<PathBuf>, String> {
    let mut provider = ZmodemProvider;
    provider
        .configure(&serde_json::json!({ "enabled": true }))
        .expect("the provider accepts its own default configuration");
    let provider: Arc<dyn TransferProvider> = Arc::new(provider);
    let registry = Arc::new(TransferRegistry::new(vec![provider], Vec::new()));
    let host: Arc<dyn TransferHost> = Arc::new(AppTransferHost::new(None, u64::MAX));
    // Kept alive for the whole transfer: dropping the runtime shuts the driver
    // down.
    let runtime = TransferRuntime::new(registry, host, TransferPolicy::default());
    let shared = runtime.shared();

    let writer = std::sync::Mutex::new(terminal.master.try_clone().expect("clone master"));
    shared.set_wire_writer(Arc::new(move |bytes: &[u8]| {
        let mut writer = writer.lock().expect("wire writer");
        let mut rest = bytes;
        while !rest.is_empty() {
            match writer.write(rest) {
                Ok(0) => {
                    eprintln!("pty write made no progress");
                    return;
                }
                Ok(written) => rest = &rest[written..],
                // The master is non-blocking, and a full buffer must not drop
                // protocol bytes (the app's event loop queues and retries);
                // wait for space instead.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                // A closed slave (the peer exited) is the end of the session, not
                // a test failure.
                Err(error) => {
                    eprintln!("pty write failed: {error}");
                    return;
                }
            }
        }
    }));

    let ui_events = runtime.ui_events();
    let mut tap = terminal.tap(&shared);
    let mut buffer = [0u8; 8192];
    let deadline = Instant::now() + TEST_TIMEOUT;

    loop {
        assert!(
            Instant::now() < deadline,
            "the transfer did not finish within {TEST_TIMEOUT:?}"
        );
        while let Ok(event) = ui_events.try_recv() {
            match event {
                TransferUiEvent::AwaitingUploadPaths { .. } => {
                    let paths = dialogs
                        .upload_paths
                        .clone()
                        .expect("the test must provide upload paths");
                    shared.answer_upload_paths(Some(paths));
                }
                TransferUiEvent::AwaitingDownloadDir { .. } => {
                    let directory = dialogs
                        .download_dir
                        .clone()
                        .expect("the test must provide a download directory");
                    shared.answer_download_dir(Some(directory));
                }
                TransferUiEvent::Completed { paths } => return Ok(paths),
                TransferUiEvent::Failed { reason } => return Err(reason),
                TransferUiEvent::Cancelled => return Err("cancelled".to_string()),
                TransferUiEvent::Detected {
                    provider_id,
                    direction,
                    ..
                } => {
                    eprintln!("detected {provider_id} {direction:?}");
                }
                _ => {}
            }
        }
        match tap.read(&mut buffer) {
            // Parser bytes are not what this test checks; the terminal state
            // around a transfer is `terminal_core`'s own concern.
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("tap read failed: {error}"),
        }
    }
}

#[test]
fn download_through_the_real_tap_with_sz() {
    let Ok(sz) = std::env::var("ZEDTERM_SZ_BIN") else {
        return;
    };
    let workdir = temp_dir("sz-pty");
    let destination = workdir.join("out");
    std::fs::create_dir_all(&destination).unwrap();
    let source = workdir.join("through-tap.bin");
    let contents = payload(200 * 1024, 249);
    std::fs::write(&source, &contents).unwrap();

    let mut command = Command::new(&sz);
    command.arg(&source).current_dir(&workdir);
    let mut terminal = PseudoTerminal::spawn(&mut command);

    let saved = run_transfer(
        &terminal,
        Dialogs {
            upload_paths: None,
            download_dir: Some(destination.clone()),
        },
    )
    .expect("the download must complete");

    assert_eq!(saved, vec![destination.join("through-tap.bin")]);
    assert_eq!(std::fs::read(&saved[0]).unwrap(), contents);

    reap(&mut terminal);
    let _ = std::fs::remove_dir_all(&workdir);
}

#[test]
fn upload_through_the_real_tap_with_rz() {
    let Ok(rz) = std::env::var("ZEDTERM_RZ_BIN") else {
        return;
    };
    upload_through_the_real_tap(&rz, &[], "rz-pty");
}

/// `rz -e` announces ESCCTL in its ZRINIT and then decodes nothing that is
/// not escaped, so the sender must escape every control character once the
/// flag is seen (`rz -bye` is this case plus two wire-invisible options).
#[test]
fn upload_through_the_real_tap_with_rz_escape_controls() {
    let Ok(rz) = std::env::var("ZEDTERM_RZ_BIN") else {
        return;
    };
    upload_through_the_real_tap(&rz, &["-e"], "rz-pty-escape");
}

fn upload_through_the_real_tap(rz: &str, args: &[&str], workdir_name: &str) {
    let workdir = temp_dir(workdir_name);
    // `rz` writes into its working directory.
    let receiver = workdir.join("received");
    std::fs::create_dir_all(&receiver).unwrap();
    let source = workdir.join("through-tap-rz.bin");
    let contents = payload(150 * 1024, 251);
    std::fs::write(&source, &contents).unwrap();

    let mut command = Command::new(rz);
    command.args(args).current_dir(&receiver);
    let mut terminal = PseudoTerminal::spawn(&mut command);

    let sent = run_transfer(
        &terminal,
        Dialogs {
            upload_paths: Some(vec![source.clone()]),
            download_dir: None,
        },
    )
    .expect("the upload must complete");

    assert_eq!(sent, vec![source]);
    let received = receiver.join("through-tap-rz.bin");
    assert_eq!(std::fs::read(&received).unwrap(), contents);

    reap(&mut terminal);
    let _ = std::fs::remove_dir_all(&workdir);
}

/// lrzsz signs off with `tcdrain` and only exits once the master has read
/// its final output: a reader that stops wedges it in the drain past SIGKILL
/// (closing the master is what finally dislodges it). Keep pumping the master
/// until the peer is gone — its clean exit is also the proof that the session
/// closed on the wire, not just in the mux.
fn reap(terminal: &mut PseudoTerminal) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut buffer = [0u8; 8192];
    loop {
        match terminal.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => panic!("cannot reap the peer: {error}"),
        }
        assert!(Instant::now() < deadline, "the lrzsz peer did not exit");
        let drained = match terminal.master.read(&mut buffer) {
            Ok(count) => count > 0,
            // WouldBlock: nothing queued yet. EIO: the slave end is gone
            // and the exit is imminent. Either way just wait it out.
            Err(_) => false,
        };
        if !drained {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
