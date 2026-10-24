use std::{
    ffi::OsString,
    fs::{File, OpenOptions, TryLockError},
    io::{ErrorKind, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use net::async_net::{UnixListener, UnixStream};
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DIRECTORY_BYTES: usize = 64 * 1024;

pub(crate) struct OpenListener {
    listener: UnixListener,
    _socket_directory: tempfile::TempDir,
    _lock: File,
}

impl OpenListener {
    pub(crate) fn bind_or_forward(directory: Option<&Path>) -> Result<Option<Self>> {
        smol::block_on(Self::bind_or_forward_in(paths::data_dir(), directory))
    }

    async fn bind_or_forward_in(
        data_directory: &Path,
        directory: Option<&Path>,
    ) -> Result<Option<Self>> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data_directory.join("open.lock"))
            .context("could not open the ZedTerm instance lock")?;
        let started = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    // Unix socket paths have a small fixed limit, independent of PATH_MAX.
                    #[cfg(unix)]
                    let socket_parent = Path::new("/tmp");
                    #[cfg(windows)]
                    let socket_parent = std::env::temp_dir();
                    let socket_directory = tempfile::Builder::new()
                        .prefix("zedterm-open-")
                        .tempdir_in(socket_parent)
                        .context("could not create the ZedTerm socket directory")?;
                    let socket_path = socket_directory.path().join("open.sock");
                    let listener = UnixListener::bind(&socket_path)
                        .context("could not listen for ZedTerm open requests")?;
                    let mut endpoint = tempfile::NamedTempFile::new_in(data_directory)?;
                    endpoint.write_all(&encode_directory(&socket_path))?;
                    endpoint
                        .persist(data_directory.join("open.socket"))
                        .context("could not save the ZedTerm socket path")?;
                    return Ok(Some(Self {
                        listener,
                        _socket_directory: socket_directory,
                        _lock: lock,
                    }));
                }
                Err(TryLockError::Error(error)) => {
                    return Err(error).context("could not lock the ZedTerm instance");
                }
                Err(TryLockError::WouldBlock) => {}
            }

            match connect_to_instance(data_directory).await {
                Ok(mut stream) => {
                    with_timeout(async {
                        write_request(&mut stream, directory).await?;
                        let mut acknowledged = [0];
                        stream.read_exact(&mut acknowledged).await?;
                        if acknowledged != [1] {
                            bail!("the running ZedTerm could not open the directory");
                        }
                        Ok(())
                    })
                    .await
                    .context("could not deliver the open request to the running ZedTerm")?;
                    return Ok(None);
                }
                Err(error) if started.elapsed() >= REQUEST_TIMEOUT => {
                    return Err(error).context("could not connect to the running ZedTerm");
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) => {}
                Err(error) => {
                    return Err(error).context("could not connect to the running ZedTerm");
                }
            }
            smol::Timer::after(Duration::from_millis(50)).await;
        }
    }

    pub(crate) fn listen(self, cx: &mut gpui::App) {
        cx.spawn(async move |cx| {
            loop {
                let (mut stream, _) = match self.listener.accept().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        log::error!("Could not accept ZedTerm open request: {error}");
                        break;
                    }
                };
                let result = with_timeout(async {
                    let directory = read_request(&mut stream).await?;
                    cx.update(|cx| terminal_app::open_external_directory(directory, cx));
                    stream.write_all(&[1]).await?;
                    stream.flush().await?;
                    Ok(())
                })
                .await;
                if let Err(error) = result {
                    log::error!("Could not handle ZedTerm open request: {error:#}");
                }
            }
            drop(self);
        })
        .detach();
    }
}

async fn connect_to_instance(data_directory: &Path) -> std::io::Result<UnixStream> {
    let bytes = std::fs::read(data_directory.join("open.socket"))?;
    UnixStream::connect(decode_directory(bytes)?).await
}

async fn with_timeout<T>(operation: impl Future<Output = Result<T>>) -> Result<T> {
    smol::future::race(operation, async {
        smol::Timer::after(REQUEST_TIMEOUT).await;
        bail!("ZedTerm open request timed out")
    })
    .await
}

async fn write_request(stream: &mut UnixStream, directory: Option<&Path>) -> Result<()> {
    let bytes = directory.map(encode_directory).unwrap_or_default();
    if bytes.len() > MAX_DIRECTORY_BYTES {
        bail!("directory path is too long");
    }
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_request(stream: &mut UnixStream) -> Result<Option<PathBuf>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_DIRECTORY_BYTES {
        bail!("directory path is too long");
    }
    if length == 0 {
        return Ok(None);
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(Some(decode_directory(bytes)?))
}

#[cfg(unix)]
fn encode_directory(directory: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    directory.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_directory(bytes: Vec<u8>) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn encode_directory(directory: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    directory
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_directory(bytes: Vec<u8>) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    if !bytes.len().is_multiple_of(2) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "invalid directory encoding",
        ));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_long_data_directory_paths() -> Result<()> {
        smol::block_on(async {
            let temporary_directory = tempfile::tempdir()?;
            let data_directory = temporary_directory.path().join("directory".repeat(20));
            std::fs::create_dir(&data_directory)?;
            let owner = OpenListener::bind_or_forward_in(&data_directory, None)
                .await?
                .context("long data directory should still support opening ZedTerm")?;
            let (client, server) = smol::future::zip(
                OpenListener::bind_or_forward_in(&data_directory, Some(&data_directory)),
                async {
                    let (mut stream, _) = owner.listener.accept().await?;
                    assert_eq!(
                        read_request(&mut stream).await?.as_ref(),
                        Some(&data_directory)
                    );
                    stream.write_all(&[1]).await?;
                    anyhow::Ok(())
                },
            )
            .await;
            assert!(client?.is_none());
            server?;
            Ok(())
        })
    }

    #[test]
    fn forwards_directories_and_recovers_after_owner_exits() -> Result<()> {
        smol::block_on(async {
            let data_directory = tempfile::tempdir()?;
            let other_data_directory = tempfile::tempdir()?;
            let owner = OpenListener::bind_or_forward_in(data_directory.path(), None)
                .await?
                .context("first launch should own the socket")?;
            let other_owner = OpenListener::bind_or_forward_in(other_data_directory.path(), None)
                .await?
                .context("a different data directory should have its own instance")?;
            let directory = data_directory.path().join("目录 with spaces");
            for directory in [Some(directory.as_path()), None] {
                let (client, server) = smol::future::zip(
                    OpenListener::bind_or_forward_in(data_directory.path(), directory),
                    async {
                        let (mut stream, _) = owner.listener.accept().await?;
                        assert_eq!(read_request(&mut stream).await?.as_deref(), directory);
                        stream.write_all(&[1]).await?;
                        anyhow::Ok(())
                    },
                )
                .await;
                assert!(client?.is_none());
                server?;
            }
            drop(owner);
            let replacement = OpenListener::bind_or_forward_in(data_directory.path(), None).await?;
            assert!(replacement.is_some());
            drop(other_owner);
            Ok(())
        })
    }

    #[test]
    fn rejects_oversized_requests() -> Result<()> {
        smol::block_on(async {
            let data_directory = tempfile::tempdir()?;
            let owner = OpenListener::bind_or_forward_in(data_directory.path(), None)
                .await?
                .context("first launch should own the socket")?;
            let mut client = connect_to_instance(data_directory.path()).await?;
            client
                .write_all(&((MAX_DIRECTORY_BYTES + 1) as u32).to_le_bytes())
                .await?;
            let (mut stream, _) = owner.listener.accept().await?;
            assert!(read_request(&mut stream).await.is_err());
            Ok(())
        })
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_unicode_directories() -> Result<()> {
        use std::os::unix::ffi::OsStringExt as _;
        let directory = PathBuf::from(OsString::from_vec(b"/tmp/terminal-\xff".to_vec()));
        assert_eq!(decode_directory(encode_directory(&directory))?, directory);
        Ok(())
    }
}
