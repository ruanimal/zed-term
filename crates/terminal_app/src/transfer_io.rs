//! Host-side file IO for transfers: staging, name sanitizing and commits
//! (§7 of docs/TRANSFER_EXTENSION.md). This is the [`TransferHost`]
//! implementation backing the in-process providers.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use transfer_core::TransferHost;

pub struct AppTransferHost {
    inner: Mutex<HostInner>,
}

struct HostInner {
    /// Default download directory from settings; used when the UI saves one.
    default_download_dir: Option<PathBuf>,
    max_file_size: u64,
    read_file: Option<(File, u64)>,
    write_file: Option<(File, PathBuf)>,
    destination: Option<PathBuf>,
    staged: Vec<StagedRecord>,
    committed: Vec<PathBuf>,
    /// Temp directory created for a session that opened a file before a
    /// destination was chosen. Tracked explicitly so cleanup only ever
    /// removes a directory this host created, never one the user picked.
    fallback_session_dir: Option<PathBuf>,
}

/// A staged (uncommitted) download file, kept until commit or discard.
struct StagedRecord {
    path: PathBuf,
    local_name: String,
}

impl AppTransferHost {
    pub fn new(default_download_dir: Option<PathBuf>, max_file_size: u64) -> Self {
        AppTransferHost {
            inner: Mutex::new(HostInner {
                default_download_dir,
                max_file_size,
                read_file: None,
                write_file: None,
                destination: None,
                staged: Vec::new(),
                committed: Vec::new(),
                fallback_session_dir: None,
            }),
        }
    }

    /// Local paths that completed a transfer (for display and logging).
    pub fn committed_paths(&self) -> Vec<PathBuf> {
        self.inner.lock().committed.clone()
    }
}

/// Strip everything dangerous from a remote-supplied file name (§7.2):
/// directory components, `..`, NUL and control characters. The result is a
/// bare file name or an error when nothing safe remains.
pub fn sanitize_remote_name(remote_name: &str) -> io::Result<String> {
    if remote_name.chars().any(|character| character.is_control()) {
        return Err(io::Error::other("file name contains control characters"));
    }
    let stripped = remote_name.to_string();
    let base = stripped
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if base.is_empty() || base == "." || base == ".." {
        return Err(io::Error::other(format!(
            "unusable remote file name {remote_name:?}"
        )));
    }
    Ok(base)
}

fn staging_root() -> PathBuf {
    std::env::temp_dir().join("zedterm-transfer-staging")
}

/// Sequence making hidden staging names unique, so two concurrent downloads
/// in one process can never share a temporary file.
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Hidden temporary name for staging inside `directory`. Downloads stage in
/// the directory the user chose: the final rename then stays on one
/// filesystem and is atomic, and an existing file is never touched until
/// commit (§7.3).
fn staging_path_in(directory: &Path, name: &str) -> PathBuf {
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(".{name}.{}.{sequence}.part", std::process::id()))
}

/// The directory a chosen destination writes into: the destination itself
/// when it is a directory, otherwise its parent (a full path from the save
/// dialog).
fn destination_directory(destination: &Path) -> io::Result<PathBuf> {
    if destination.is_dir() {
        return Ok(destination.to_path_buf());
    }
    destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("download destination has no directory"))
}

/// Fallback staging directory for a session that has no destination yet.
/// The sequence keeps concurrent sessions in one process from sharing a
/// directory (two downloads landing in the same millisecond otherwise did).
fn unique_session_dir() -> io::Result<PathBuf> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = staging_root().join(format!(
        "session-{millis}-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A target path that does not collide with existing files: `name (2)`,
/// `name (3)`, ... (§7.2 "自动 name (2)" fallback when the dialog did not
/// already confirm an exact path).
fn unique_destination(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    if !path.exists() {
        return path;
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let extension = Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());
    for counter in 2..1000 {
        let candidate = match &extension {
            Some(extension) => format!("{stem} ({counter}).{extension}"),
            None => format!("{stem} ({counter})"),
        };
        let path = directory.join(candidate);
        if !path.exists() {
            return path;
        }
    }
    path
}

impl TransferHost for AppTransferHost {
    fn open_read(&self, path: &Path) -> io::Result<u64> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        self.inner.lock().read_file = Some((file, size));
        Ok(size)
    }

    fn read_chunk(&self, offset: u64, max_len: usize) -> io::Result<Vec<u8>> {
        let mut inner = self.inner.lock();
        let (file, size) = inner
            .read_file
            .as_mut()
            .ok_or_else(|| io::Error::other("no file open for reading"))?;
        if offset >= *size {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; max_len];
        let mut filled = 0;
        while filled < max_len {
            let count = file.read(&mut buffer[filled..])?;
            if count == 0 {
                break;
            }
            filled += count;
        }
        buffer.truncate(filled);
        Ok(buffer)
    }

    fn open_write(&self, remote_name: &str, size: Option<u64>) -> io::Result<String> {
        let local_name = sanitize_remote_name(remote_name)?;
        let mut inner = self.inner.lock();
        if size.is_some_and(|size| size > inner.max_file_size) {
            return Err(io::Error::other("file size limit exceeded"));
        }
        // Stage next to where the file will land (§3.5). The download
        // directory is already known here — the session asks for it before
        // the remote sends any data — so the commit is a same-filesystem
        // rename. `unique_session_dir` only covers callers that opened a
        // file before a destination was chosen.
        let path = match inner.destination.as_deref() {
            Some(destination) => {
                let directory = destination_directory(destination)?;
                fs::create_dir_all(&directory)?;
                staging_path_in(&directory, &local_name)
            }
            None => {
                let directory = match inner.fallback_session_dir.clone() {
                    Some(directory) => directory,
                    None => {
                        let directory = unique_session_dir()?;
                        inner.fallback_session_dir = Some(directory.clone());
                        directory
                    }
                };
                directory.join(&local_name)
            }
        };
        let file = File::create(&path)?;
        inner.write_file = Some((file, path.clone()));
        inner.staged.push(StagedRecord {
            path: path.clone(),
            local_name: local_name.clone(),
        });
        Ok(local_name)
    }

    fn write_chunk(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut inner = self.inner.lock();
        let staged = inner
            .write_file
            .as_mut()
            .ok_or_else(|| io::Error::other("no staging file open"))?;
        staged.0.seek(SeekFrom::Start(offset))?;
        staged.0.write_all(data)
    }

    fn close_write(&self) -> io::Result<PathBuf> {
        let mut inner = self.inner.lock();
        let (file, path) = inner
            .write_file
            .take()
            .ok_or_else(|| io::Error::other("no staging file open"))?;
        file.sync_all()?;
        Ok(path)
    }

    fn request_upload_paths(&self) -> Option<Vec<PathBuf>> {
        // Dialogs are surfaced through `TransferUiEvent::AwaitingUploadPaths`
        // on the terminal, not through this hook.
        None
    }

    fn request_download_dir(&self, _suggested_name: Option<&str>) -> Option<PathBuf> {
        None
    }

    fn set_destination(&self, destination: &Path) {
        self.inner.lock().destination = Some(destination.to_path_buf());
    }

    fn commit(&self) -> io::Result<PathBuf> {
        let mut inner = self.inner.lock();
        let record = inner
            .staged
            .pop()
            .ok_or_else(|| io::Error::other("no staged file to commit"))?;

        let destination = inner
            .destination
            .clone()
            .or_else(|| inner.default_download_dir.clone())
            .ok_or_else(|| io::Error::other("no download destination chosen"))?;

        let target = if destination.is_dir() {
            unique_destination(&destination, &record.local_name)
        } else {
            // A full file path from the save dialog: the user already
            // confirmed overwriting through the dialog.
            destination
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        move_into_place(&record.path, &target)?;
        // A fallback staging directory only served this session; drop it once
        // its last file has moved out.
        if inner.fallback_session_dir.as_deref() == record.path.parent()
            && let Some(directory) = inner.fallback_session_dir.take()
        {
            remove_dir_if_empty_logged(&directory);
        }
        inner.committed.push(target.clone());
        Ok(target)
    }

    fn discard_staged(&self) {
        let mut inner = self.inner.lock();
        inner.write_file = None;
        inner.read_file = None;
        let staged = std::mem::take(&mut inner.staged);
        for record in staged {
            remove_staging_artifact(&record.path);
        }
        if let Some(directory) = inner.fallback_session_dir.take() {
            remove_dir_if_empty_logged(&directory);
        }
    }
}

/// Move a staged file onto its final path.
///
/// Staging normally sits in the destination directory, so this is a plain
/// same-filesystem rename. A session that opened its file before a
/// destination was chosen staged in temp instead, where the rename fails
/// with `EXDEV`; the bytes are then copied to a hidden sibling of the target
/// and renamed within that directory, so the visible path only ever appears
/// complete (never a truncated file) and that rename stays atomic.
fn move_into_place(staged: &Path, target: &Path) -> io::Result<()> {
    match fs::rename(staged, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_into_place(staged, target).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot save to {}: {error}", target.display()),
                )
            })
        }
        Err(error) => Err(error),
    }
}

fn copy_into_place(staged: &Path, target: &Path) -> io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let temporary = staging_path_in(parent, &file_name);

    // `fs::copy` carries the staged file's permission bits, so the result
    // still has no execute bit (§7.5).
    let result = fs::copy(staged, &temporary).and_then(|_| fs::rename(&temporary, target));
    if let Err(error) = &result {
        remove_file_logged(&temporary);
        return Err(io::Error::new(error.kind(), error.to_string()));
    }
    remove_file_logged(staged);
    Ok(())
}

fn remove_file_logged(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("failed to remove {}: {error}", path.display());
    }
}

fn remove_staging_artifact(path: &Path) {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    if let Err(error) = &result
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!("failed to remove staged file {}: {error}", path.display());
    }
}

/// Remove the fallback staging directory this host created. Only ever called
/// with the tracked session directory, so a directory the user picked for
/// downloads can never be removed here.
fn remove_dir_if_empty_logged(path: &Path) {
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => log::warn!(
            "failed to remove transfer staging dir {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn sanitize_strips_directories_and_controls() {
        assert_eq!(sanitize_remote_name("hello.txt").unwrap(), "hello.txt");
        assert_eq!(sanitize_remote_name("a/b/c.txt").unwrap(), "c.txt");
        assert_eq!(sanitize_remote_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_remote_name("with\\back.txt").unwrap(), "back.txt");
        assert_eq!(
            sanitize_remote_name("trailing space ").unwrap(),
            "trailing space"
        );
        assert!(sanitize_remote_name("..").is_err());
        assert!(sanitize_remote_name(".").is_err());
        assert!(sanitize_remote_name("").is_err());
        assert!(sanitize_remote_name("a\0b").is_err());
        assert!(sanitize_remote_name("a\u{7}b").is_err());
    }

    #[test]
    fn staging_commit_round_trip() {
        let host = AppTransferHost::new(None, 1024);
        let destination = std::env::temp_dir().join("zedterm-transfer-test-commit");
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&destination).unwrap();
        // The download directory is known before the remote sends data, so
        // staging lands there under a hidden name (§3.5/§7.3).
        host.set_destination(&destination);
        let local_name = host.open_write("report.csv", None).unwrap();
        assert_eq!(local_name, "report.csv");
        host.write_chunk(0, b"column,column2\n1,2\n").unwrap();
        let staged = host.close_write().unwrap();
        assert_eq!(staged.parent().unwrap(), destination);
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with('.')
        );
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".part")
        );
        assert_ne!(staged.file_name().unwrap(), "report.csv");

        let final_path = host.commit().unwrap();
        assert_eq!(final_path.parent().unwrap(), destination);
        assert_eq!(fs::read(&final_path).unwrap(), b"column,column2\n1,2\n");
        assert!(!staged.exists());

        let committed = host.committed_paths();
        assert_eq!(committed, vec![final_path.clone()]);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn staging_falls_back_to_temp_without_a_destination() {
        let host = AppTransferHost::new(None, 1024);
        host.open_write("report.csv", None).unwrap();
        let staged = host.close_write().unwrap();
        assert!(
            staged.starts_with(staging_root()),
            "without a chosen directory the session stages in temp"
        );
        let session_dir = staged.parent().unwrap().to_path_buf();
        host.discard_staged();
        assert!(!staged.exists());
        assert!(
            !session_dir.exists(),
            "the temp session directory we created must be cleaned up"
        );
    }

    /// Regression: the download picker used to be a save-file dialog whose
    /// pre-filled name ("download") became the file name, so a remote
    /// `install_trzsz.sh` landed as `~/Downloads/download`. The file name
    /// must come from the remote, the picker only chooses the directory.
    #[test]
    fn download_is_named_by_the_remote_not_by_the_picker() {
        let host = AppTransferHost::new(None, u64::MAX);
        let destination = std::env::temp_dir().join("zedterm-transfer-test-remote-name");
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&destination).unwrap();
        host.set_destination(&destination);

        host.open_write("install_trzsz.sh", None).unwrap();
        host.write_chunk(0, b"#!/bin/sh\n").unwrap();
        host.close_write().unwrap();
        let saved = host.commit().unwrap();

        assert_eq!(saved, destination.join("install_trzsz.sh"));
        assert_eq!(fs::read(&saved).unwrap(), b"#!/bin/sh\n");
        let _ = fs::remove_dir_all(&destination);
    }

    /// A cancelled download must leave the directory the user picked intact.
    #[test]
    fn discard_never_removes_the_destination_directory() {
        let host = AppTransferHost::new(None, u64::MAX);
        let destination = std::env::temp_dir().join("zedterm-transfer-test-discard-dir");
        let _ = fs::remove_dir_all(&destination);
        fs::create_dir_all(&destination).unwrap();
        fs::write(destination.join("KEEP.txt"), b"keep").unwrap();
        host.set_destination(&destination);

        host.open_write("doomed.bin", None).unwrap();
        host.write_chunk(0, b"data").unwrap();
        let staged = host.close_write().unwrap();
        assert!(staged.starts_with(&destination));
        host.discard_staged();

        assert!(staged.parent().unwrap().exists(), "destination removed");
        assert_eq!(fs::read(destination.join("KEEP.txt")).unwrap(), b"keep");
        let _ = fs::remove_dir_all(&destination);
    }

    #[test]
    fn unique_destination_appends_counter() {
        let dir = std::env::temp_dir().join("zedterm-transfer-test-unique");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"").unwrap();
        let first = unique_destination(&dir, "a.txt");
        let second = unique_destination(&dir, "a.txt");
        assert_eq!(first, dir.join("a (2).txt"));
        assert_eq!(second, dir.join("a (2).txt"));
        assert_eq!(unique_destination(&dir, "b.txt"), dir.join("b.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_removes_staged_files() {
        let host = Arc::new(AppTransferHost::new(None, 1024));
        host.open_write("doomed.bin", None).unwrap();
        host.write_chunk(0, b"data").unwrap();
        let staged: Vec<PathBuf> = host
            .inner
            .lock()
            .staged
            .iter()
            .map(|record| record.path.clone())
            .collect();
        assert!(staged.iter().all(|path| path.exists()));
        host.discard_staged();
        assert!(staged.iter().all(|path| !path.exists()));
    }

    /// The copy fallback used when staging is on another filesystem (a
    /// session that opened its file before a destination was chosen) must
    /// land the bytes, leave no partial sibling behind, and clean up staging.
    #[test]
    fn cross_device_commit_copies_into_place() {
        let root = std::env::temp_dir().join("zedterm-transfer-test-cross-device");
        let _ = fs::remove_dir_all(&root);
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let staged = root.join("staged.bin");
        fs::write(&staged, b"downloaded payload").unwrap();

        let target = destination.join("report.bin");
        copy_into_place(&staged, &target).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"downloaded payload");
        assert!(!staged.exists(), "staging must be cleaned up");
        let leftovers: Vec<_> = fs::read_dir(&destination)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "no .part sibling may remain");
        let _ = fs::remove_dir_all(&root);
    }
}
