//! Theme extension catalog, download, install, update, and uninstall.
//!
//! ZedTerm only consumes one kind of Zed extension: the themes it ships. The
//! store therefore talks to the public extension API directly instead of
//! vendoring Zed's `extension_host`, which brings a WASM runtime together with
//! language, grammar, and LSP support that this fork does not have.
//!
//! Extensions are laid out on disk the same way Zed lays them out, so the two
//! can share a data directory: one directory per extension id containing the
//! unpacked archive (`extension.toml` plus a `themes/` directory).

use std::{collections::HashSet, path::Path, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result};
use futures::AsyncReadExt as _;
use gpui::{App, AppContext as _, Context, Task, WeakEntity};
use http_client::{AsyncBody, HttpClient, Request};
use semver::Version;
use serde::Deserialize;

/// Extension schema version ZedTerm's theme loader is compatible with.
///
/// Themes are read with `theme_settings::deserialize_user_theme`, so this is
/// the schema the bundled theme assets use.
const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Host of the extension API.
///
/// `HttpClientWithUrl` rewrites Zed's site base to the API host, but the default
/// gpui client has no base at all, so the host is named here.
const API_BASE_URL: &str = "https://api.zed.dev";

/// Directory holding in-progress downloads. Excluded from the installed list so
/// a partially written extension is never loaded.
const STAGING_DIRECTORY: &str = "staging";

const MANIFEST_FILE: &str = "extension.toml";

/// Upper bound on a downloaded archive, so a hostile or broken response cannot
/// exhaust memory.
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// One entry from the extension catalog.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ExtensionMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub download_count: u64,
}

#[derive(Deserialize)]
struct GetExtensionsResponse {
    data: Vec<ExtensionMetadata>,
}

/// The subset of `extension.toml` the theme store needs.
#[derive(Clone, Debug, Deserialize)]
struct ExtensionManifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    themes: Vec<String>,
}

/// A theme family an installed extension provides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledThemeFamily {
    /// Family name from the theme file's `name`.
    pub family_name: String,
    /// Theme names exactly as the registry knows them, so the Terminal tab's
    /// picker and the Themes tab agree on identity.
    pub theme_names: Vec<String>,
}

/// An installed theme extension, reconstructed from its on-disk manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub repository: String,
    pub theme_families: Vec<InstalledThemeFamily>,
}

impl InstalledExtension {
    /// Number of selectable themes this extension contributes.
    pub fn theme_count(&self) -> usize {
        self.theme_families
            .iter()
            .map(|family| family.theme_names.len())
            .sum()
    }
}

/// A catalog entry paired with the locally installed version, when any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub metadata: ExtensionMetadata,
    pub installed_version: Option<String>,
    pub update_available: bool,
    /// Themes the installed copy contributes; zero when it is not installed.
    pub installed_theme_count: usize,
}

impl CatalogEntry {
    pub fn is_installed(&self) -> bool {
        self.installed_version.is_some()
    }
}

/// Where an operation failed, so the tab can explain the failure category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreErrorKind {
    Network,
    Api,
    Archive,
    Filesystem,
    Manifest,
}

impl StoreErrorKind {
    /// Short label prefixed to the user-facing message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "network error",
            Self::Api => "extension API error",
            Self::Archive => "archive error",
            Self::Filesystem => "file error",
            Self::Manifest => "manifest error",
        }
    }
}

#[derive(Debug)]
pub struct StoreError {
    pub kind: StoreErrorKind,
    pub message: String,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind.label(), self.message)
    }
}

impl std::error::Error for StoreError {}

impl StoreError {
    fn new(kind: StoreErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn network(error: impl std::fmt::Display) -> Self {
        Self::new(StoreErrorKind::Network, error.to_string())
    }

    fn api(status: u16, body: &str) -> Self {
        let body = body.trim();
        // The API reports failures as a short JSON or text body; trim it so a
        // large HTML error page cannot flood the status line.
        let detail: String = body.chars().take(200).collect();
        let detail = detail.trim();
        if detail.is_empty() {
            Self::new(StoreErrorKind::Api, format!("HTTP {status}"))
        } else {
            Self::new(StoreErrorKind::Api, format!("HTTP {status}: {detail}"))
        }
    }

    fn archive(error: impl std::fmt::Display) -> Self {
        Self::new(StoreErrorKind::Archive, error.to_string())
    }

    fn filesystem(error: impl std::fmt::Display) -> Self {
        Self::new(StoreErrorKind::Filesystem, error.to_string())
    }

    fn manifest(error: impl std::fmt::Display) -> Self {
        Self::new(StoreErrorKind::Manifest, error.to_string())
    }
}

type StoreResult<T> = std::result::Result<T, StoreError>;

fn extensions_dir() -> PathBuf {
    paths::extensions_dir().clone()
}

fn installed_extension_dir(extensions_dir: &Path, extension_id: &str) -> PathBuf {
    extensions_dir.join(extension_id)
}
/// Rejects an extension id that is unusable as a single path component.
///
/// Ids come from the extension API and from manifests, so a hostile or buggy
/// response must not be able to escape the extensions directory.
fn validate_extension_id(extension_id: &str) -> StoreResult<()> {
    if extension_id.is_empty() {
        return Err(StoreError::new(
            StoreErrorKind::Api,
            "the extension id is empty",
        ));
    }
    if !extension_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(StoreError::new(
            StoreErrorKind::Api,
            format!("the extension id {extension_id:?} contains unsupported characters"),
        ));
    }
    Ok(())
}

fn extension_api_url(path: &str, query: &[(&str, String)]) -> StoreResult<String> {
    let mut url = url::Url::parse(&format!("{API_BASE_URL}{path}")).map_err(StoreError::network)?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url.into())
}

/// Performs a GET, returning the body plus the announced content length.
async fn fetch_bytes(
    http_client: &dyn HttpClient,
    url: &str,
) -> StoreResult<(Vec<u8>, Option<usize>)> {
    let request = Request::builder()
        .uri(url)
        .body(AsyncBody::empty())
        .map_err(StoreError::network)?;
    let mut response = http_client
        .send(request)
        .await
        .map_err(StoreError::network)?;
    let status = response.status();
    let content_length = response
        .headers()
        .get(http_client::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if let Some(content_length) = content_length
        && content_length > MAX_ARCHIVE_BYTES
    {
        return Err(StoreError::new(
            StoreErrorKind::Network,
            format!(
                "the download is {content_length} bytes, which exceeds the {MAX_ARCHIVE_BYTES} byte limit"
            ),
        ));
    }
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .map_err(StoreError::network)?;
    if status.is_client_error() || status.is_server_error() {
        return Err(StoreError::api(
            status.as_u16(),
            &String::from_utf8_lossy(&body),
        ));
    }
    if body.len() > MAX_ARCHIVE_BYTES {
        return Err(StoreError::new(
            StoreErrorKind::Network,
            format!("the download exceeded the {MAX_ARCHIVE_BYTES} byte limit"),
        ));
    }
    Ok((body, content_length))
}

/// Fetches the theme extension catalog, optionally filtered by a search term.
pub async fn fetch_catalog(
    http_client: Arc<dyn HttpClient>,
    search: Option<String>,
) -> StoreResult<Vec<ExtensionMetadata>> {
    let mut query = vec![("max_schema_version", CURRENT_SCHEMA_VERSION.to_string())];
    if let Some(search) = search.filter(|search| !search.trim().is_empty()) {
        query.push(("filter", search.trim().to_string()));
    }
    query.push(("provides", "themes".to_string()));
    let url = extension_api_url("/extensions", &query)?;
    let (body, _) = fetch_bytes(http_client.as_ref(), &url).await?;
    let response: GetExtensionsResponse = serde_json::from_slice(&body)
        .map_err(|error| StoreError::new(StoreErrorKind::Api, error.to_string()))?;
    Ok(response.data)
}

/// Downloads and unpacks an extension into `extensions_dir`, replacing any
/// installed copy.
///
/// The archive is staged in a sibling directory and only moved into place once
/// it unpacks cleanly and its manifest matches the requested id, so a failed or
/// mismatched download never destroys a working installation.
pub async fn install_extension_into(
    extensions_dir: &Path,
    http_client: &dyn HttpClient,
    extension_id: &str,
    version: Option<&str>,
) -> StoreResult<String> {
    validate_extension_id(extension_id)?;
    let path = match version {
        Some(version) => format!("/extensions/{extension_id}/{version}/download"),
        None => format!("/extensions/{extension_id}/download"),
    };
    let url = extension_api_url(
        &path,
        &[("max_schema_version", CURRENT_SCHEMA_VERSION.to_string())],
    )?;
    let (bytes, content_length) = fetch_bytes(http_client, &url).await?;
    if let Some(content_length) = content_length
        && content_length != bytes.len()
    {
        return Err(StoreError::new(
            StoreErrorKind::Network,
            format!(
                "the download was {} bytes but the server announced {content_length}",
                bytes.len()
            ),
        ));
    }

    install_archive(extensions_dir, extension_id, bytes)
}

/// Unpacks an already-downloaded archive, validating it before it replaces the
/// installed copy.
///
/// Split out from [`install_extension_into`] so the filesystem behavior can be
/// tested without a network round trip.
pub(crate) fn install_archive(
    extensions_dir: &Path,
    extension_id: &str,
    bytes: Vec<u8>,
) -> StoreResult<String> {
    let extension_dir = installed_extension_dir(extensions_dir, extension_id);
    let staging_dir = extensions_dir.join(STAGING_DIRECTORY);
    // The staging path is named after the process so two ZedTerm instances
    // unpacking the same extension cannot collide.
    let staging_path = staging_dir.join(format!("{extension_id}-{}", std::process::id()));
    remove_dir_if_present(&staging_path)?;

    let manifest = (|| {
        unpack_tar_gz(&bytes, &staging_path)?;
        let manifest = read_manifest(&staging_path).ok_or_else(|| {
            StoreError::manifest(format!("the archive has no readable {MANIFEST_FILE}"))
        })?;
        if manifest.id != extension_id {
            return Err(StoreError::manifest(format!(
                "the archive contains the extension {:?} but {extension_id:?} was requested",
                manifest.id
            )));
        }
        Ok(manifest)
    })();

    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(error) => {
            // Discard the staging directory so a bad download leaves no trace
            // and never becomes visible as an installed extension.
            if let Err(cleanup_error) = remove_dir_if_present(&staging_path) {
                log::error!("Could not clean up {staging_path:?}: {cleanup_error}");
            }
            return Err(error);
        }
    };

    remove_dir_if_present(&extension_dir)?;
    if let Err(error) = rename_into_place(&staging_path, &extension_dir) {
        log::error!("Could not move the staged extension into {extension_dir:?}: {error}");
        return Err(StoreError::filesystem(error));
    }

    Ok(manifest.version)
}

/// Removes an installed extension from disk.
pub fn uninstall_extension_from(extensions_dir: &Path, extension_id: &str) -> StoreResult<()> {
    validate_extension_id(extension_id)?;
    remove_dir_if_present(&installed_extension_dir(extensions_dir, extension_id))
}

/// Lists installed theme extensions by reading their manifests.
///
/// The extension directories are the source of truth rather than an index file,
/// so an extension installed by hand — Zed's dev flow writes into the same data
/// directory — is still shown and can be removed from the UI.
pub fn load_installed_extensions_in(extensions_dir: &Path) -> Vec<InstalledExtension> {
    let Ok(entries) = std::fs::read_dir(extensions_dir) else {
        return Vec::new();
    };
    let mut installed = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name() == STAGING_DIRECTORY {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&path) else {
            continue;
        };
        // The directory name is the id the store installs into, so it wins over
        // the manifest: otherwise a disagreeing manifest would make the
        // extension impossible to uninstall.
        let id = match entry.file_name().into_string() {
            Ok(id) => id,
            Err(name) => {
                log::warn!("Ignoring extension directory with a non-UTF-8 name: {name:?}");
                continue;
            }
        };
        installed.push(InstalledExtension {
            id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            repository: manifest.repository,
            theme_families: load_extension_theme_families(&path, &manifest.themes),
        });
    }
    installed.sort_by(|left, right| left.id.cmp(&right.id));
    installed
}

/// Lists installed extensions from ZedTerm's data directory.
pub fn load_installed_extensions() -> Vec<InstalledExtension> {
    load_installed_extensions_in(&extensions_dir())
}

/// Reads the raw theme files contributed by every installed extension.
///
/// The theme registry takes theme *bytes*, while [`InstalledExtension`] only
/// carries the parsed names the UI needs, so the files are read separately.
pub fn installed_theme_files() -> Vec<Vec<u8>> {
    installed_theme_paths()
        .into_iter()
        .filter_map(|path| match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                log::warn!("Could not read the theme file {path:?}: {error}");
                None
            }
        })
        .collect()
}

/// Resolves the theme files of every installed extension.
///
/// Manifest theme paths are archive-relative; a traversal component would let a
/// malicious manifest read arbitrary files, so those entries are skipped.
fn installed_theme_paths() -> Vec<PathBuf> {
    let extensions_dir = extensions_dir();
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&extensions_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.file_name() == STAGING_DIRECTORY || !entry.path().is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&entry.path()) else {
            continue;
        };
        for relative in &manifest.themes {
            let Some(path) = resolve_theme_path(&entry.path(), relative) else {
                continue;
            };
            paths.push(path);
        }
    }
    paths
}

/// Joins a manifest theme path onto an extension directory.
///
/// Manifest theme paths are archive-relative, so a traversal component would
/// let a malicious manifest read arbitrary files. Such an entry is skipped.
fn resolve_theme_path(extension_dir: &Path, relative: &str) -> Option<PathBuf> {
    if Path::new(relative)
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        log::warn!("Ignoring the theme path {relative:?}: it is not a plain relative path");
        return None;
    }
    Some(extension_dir.join(relative))
}

fn load_extension_theme_families(
    extension_dir: &std::path::Path,
    theme_paths: &[String],
) -> Vec<InstalledThemeFamily> {
    let mut families = Vec::new();
    for relative in theme_paths {
        let Some(path) = resolve_theme_path(extension_dir, relative) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            log::warn!("Could not read the theme file {path:?}");
            continue;
        };
        match theme_settings::deserialize_user_theme(&bytes) {
            Ok(family) => families.push(InstalledThemeFamily {
                family_name: family.name,
                theme_names: family.themes.into_iter().map(|theme| theme.name).collect(),
            }),
            Err(error) => {
                log::warn!("Could not parse the theme file {path:?}: {error:#}");
            }
        }
    }
    families
}

fn read_manifest(extension_dir: &Path) -> Option<ExtensionManifest> {
    let path = extension_dir.join(MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            log::debug!("Could not read the extension manifest {path:?}: {error}");
            return None;
        }
    };
    match toml::from_str::<ExtensionManifest>(&text) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            log::warn!("Could not parse the extension manifest {path:?}: {error:#}");
            None
        }
    }
}

fn remove_dir_if_present(path: &std::path::Path) -> StoreResult<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::filesystem(error)),
    }
}

/// Moves the staged extension into place, falling back to a copy when the two
/// directories are on different filesystems.
fn rename_into_place(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            log::debug!("Falling back to copying {from:?} into {to:?}: {rename_error}");
            copy_directory(from, to)?;
            remove_dir_if_present(from).map_err(|error| anyhow::anyhow!(error))?;
            Ok(())
        }
    }
}

fn copy_directory(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {to:?}"))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {from:?}"))? {
        let entry = entry.with_context(|| format!("reading an entry of {from:?}"))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspecting {:?}", entry.path()))?;
        let target = to.join(entry.file_name());
        // Extension archives hold only files and directories; anything else is
        // skipped rather than followed, so a symlink cannot escape `to`.
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {:?} to {target:?}", entry.path()))?;
        } else {
            log::warn!("Skipping the unsupported archive entry {:?}", entry.path());
        }
    }
    Ok(())
}

/// Unpacks a `.tar.gz` extension archive into `destination`.
///
/// `async_tar` validates every entry path against `destination`, so a malicious
/// archive cannot write outside it. Only regular files and directories are
/// materialized, which is all Zed extension archives contain.
fn unpack_tar_gz(bytes: &[u8], destination: &Path) -> StoreResult<()> {
    std::fs::create_dir_all(destination).map_err(StoreError::filesystem)?;
    // `async-compression` and `async-tar` both speak `futures::io`, so the
    // reader must be the async `BufReader`, not `std::io`'s.
    let reader = futures::io::BufReader::new(futures::io::Cursor::new(bytes));
    let decoder = async_compression::futures::bufread::GzipDecoder::new(reader);
    let archive = async_tar::Archive::new(decoder);
    smol::block_on(archive.unpack(destination)).map_err(StoreError::archive)
}

/// Pairs the catalog with the locally installed extensions.
pub fn merge_catalog(
    catalog: Vec<ExtensionMetadata>,
    installed: &[InstalledExtension],
) -> Vec<CatalogEntry> {
    catalog
        .into_iter()
        .map(|metadata| {
            let installed_extension = installed
                .iter()
                .find(|extension| extension.id == metadata.id);
            let installed_version = installed_extension.map(|extension| extension.version.clone());
            let update_available = installed_version
                .as_deref()
                .is_some_and(|installed| version_is_newer(&metadata.version, installed));
            CatalogEntry {
                metadata,
                installed_version,
                update_available,
                installed_theme_count: installed_extension
                    .map_or(0, InstalledExtension::theme_count),
            }
        })
        .collect()
}

/// Whether `candidate` is strictly newer than `current`.
///
/// A version that does not parse as semver is treated as not newer, so a
/// malformed catalog version can only fail to offer an update and can never
/// offer a downgrade.
pub fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

/// Installed extensions that the catalog does not mention at all.
///
/// The catalog omits extensions published against an older schema, but the user
/// should still see them and be able to remove them.
pub fn installed_absent_from_catalog(
    catalog: &[ExtensionMetadata],
    installed: &[InstalledExtension],
) -> Vec<InstalledExtension> {
    let catalog_ids: HashSet<&str> = catalog
        .iter()
        .map(|metadata| metadata.id.as_str())
        .collect();
    installed
        .iter()
        .filter(|extension| !catalog_ids.contains(extension.id.as_str()))
        .cloned()
        .collect()
}

/// Spawns a catalog fetch, reporting the result to `this`.
pub fn spawn_fetch_catalog<This: 'static>(
    this: WeakEntity<This>,
    http_client: Arc<dyn HttpClient>,
    search: Option<String>,
    on_result: impl Fn(&mut This, StoreResult<Vec<ExtensionMetadata>>, &mut Context<This>)
    + Send
    + 'static,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx| {
        let result = fetch_catalog(http_client, search).await;
        this.update(cx, |this, cx| on_result(this, result, cx)).ok();
    })
}

/// Spawns an install, reporting the extension id and result to `this`.
pub fn spawn_install<This: 'static>(
    this: WeakEntity<This>,
    http_client: Arc<dyn HttpClient>,
    extension_id: String,
    version: Option<String>,
    on_result: impl Fn(&mut This, &str, StoreResult<String>, &mut Context<This>) + Send + 'static,
    cx: &mut App,
) -> Task<()> {
    let extensions_dir = extensions_dir();
    cx.spawn(async move |cx| {
        let result = install_extension_into(
            &extensions_dir,
            http_client.as_ref(),
            &extension_id,
            version.as_deref(),
        )
        .await;
        this.update(cx, |this, cx| on_result(this, &extension_id, result, cx))
            .ok();
    })
}

/// Uninstalls on a background thread, reporting the result to `this`.
pub fn spawn_uninstall<This: 'static>(
    this: WeakEntity<This>,
    extension_id: String,
    on_result: impl Fn(&mut This, &str, StoreResult<()>, &mut Context<This>) + Send + 'static,
    cx: &mut App,
) -> Task<()> {
    let extensions_dir = extensions_dir();
    cx.spawn(async move |cx| {
        let id = extension_id.clone();
        // Deleting a directory is blocking filesystem work, so it runs on the
        // background executor rather than stalling the foreground thread.
        let result = cx
            .background_spawn(async move { uninstall_extension_from(&extensions_dir, &id) })
            .await;
        this.update(cx, |this, cx| on_result(this, &extension_id, result, cx))
            .ok();
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    /// Builds a `.tar.gz` archive the way the extension API serves one.
    pub fn build_extension_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, std::io::Cursor::new(contents))
                .expect("a test archive entry should be writable");
        }
        let tar_bytes = builder
            .into_inner()
            .expect("the test archive should be finished");

        gzip(&tar_bytes)
    }

    /// Builds a `.tar.gz` whose entry path is written verbatim, including `..`.
    ///
    /// The `tar` crate refuses to *create* a traversal entry, so the header is
    /// assembled by hand: this is the only way to test that extraction rejects
    /// what a hostile server could actually send.
    pub fn build_traversal_archive(entry_path: &str, contents: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        let write = |header: &mut [u8; 512], offset: usize, value: &[u8]| {
            header[offset..offset + value.len()].copy_from_slice(value);
        };
        write(&mut header, 0, entry_path.as_bytes());
        write(&mut header, 100, b"0000644\0");
        write(&mut header, 108, b"0000000\0");
        write(&mut header, 116, b"0000000\0");
        write(
            &mut header,
            124,
            format!("{:011o}\0", contents.len()).as_bytes(),
        );
        write(&mut header, 136, b"00000000000\0");
        // Spaces in the checksum field are part of the checksum definition.
        write(&mut header, 148, b"        ");
        write(&mut header, 156, b"0");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        write(&mut header, 148, format!("{checksum:06o}\0 ").as_bytes());

        let mut tar_bytes = header.to_vec();
        tar_bytes.extend_from_slice(contents);
        let padding = (512 - contents.len() % 512) % 512;
        tar_bytes.extend(std::iter::repeat_n(0u8, padding));
        // The end-of-archive marker is two zero blocks.
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024));

        gzip(&tar_bytes)
    }

    fn gzip(tar_bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(tar_bytes)
            .expect("the test archive should be writable");
        encoder
            .finish()
            .expect("the test archive should be compressible")
    }

    /// A minimal theme family file with the given theme names.
    pub fn theme_family_json(family_name: &str, theme_names: &[&str]) -> Vec<u8> {
        let themes: Vec<String> = theme_names
            .iter()
            .map(|name| format!(r#"{{"name": "{name}", "appearance": "dark", "style": {{}}}}"#))
            .collect();
        format!(
            r#"{{"name": "{family_name}", "author": "Test", "themes": [{}]}}"#,
            themes.join(",")
        )
        .into_bytes()
    }

    /// A minimal `extension.toml`.
    pub fn extension_manifest(
        id: &str,
        name: &str,
        version: &str,
        theme_paths: &[&str],
    ) -> Vec<u8> {
        let themes = theme_paths
            .iter()
            .map(|path| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "id = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\n\
             description = \"Test theme extension\"\n\
             repository = \"https://example.invalid/{id}\"\n\
             themes = [{themes}]\n"
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    /// Creates a fresh extensions directory for one test.
    ///
    /// Each test gets its own directory so the store's filesystem behavior can
    /// be exercised without touching the developer's real data directory or
    /// racing other tests through process-wide path state.
    fn temporary_extensions_dir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("a temporary directory should be creatable");
        std::fs::create_dir_all(directory.path()).expect("the directory should exist");
        directory
    }

    #[test]
    fn version_comparison_follows_semver() {
        assert!(version_is_newer("0.2.0", "0.1.9"));
        assert!(version_is_newer("1.0.0", "0.9.9"));
        assert!(!version_is_newer("0.1.0", "0.1.0"));
        assert!(!version_is_newer("0.1.0", "0.2.0"));
        assert!(!version_is_newer("not-a-version", "0.1.0"));
        assert!(!version_is_newer("0.2.0", "not-a-version"));
    }

    #[test]
    fn extension_ids_cannot_escape_the_extensions_directory() {
        assert!(validate_extension_id("catppuccin").is_ok());
        assert!(validate_extension_id("one-dark-pro").is_ok());
        assert!(validate_extension_id("").is_err());
        assert!(validate_extension_id("../evil").is_err());
        assert!(validate_extension_id("a/b").is_err());
        assert!(validate_extension_id("..").is_err());
    }

    fn catalog_metadata(id: &str, version: &str) -> ExtensionMetadata {
        ExtensionMetadata {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            description: String::new(),
            authors: Vec::new(),
            repository: String::new(),
            provides: vec!["themes".to_string()],
            download_count: 0,
        }
    }

    fn installed(id: &str, version: &str) -> InstalledExtension {
        InstalledExtension {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            description: String::new(),
            repository: String::new(),
            theme_families: Vec::new(),
        }
    }

    #[test]
    fn merge_catalog_detects_installs_and_updates() {
        let catalog = vec![
            catalog_metadata("catppuccin", "0.2.1"),
            catalog_metadata("gruvbox", "1.0.0"),
        ];
        let installed = vec![installed("catppuccin", "0.2.0")];
        let merged = merge_catalog(catalog, &installed);

        assert!(merged[0].is_installed());
        assert!(merged[0].update_available);
        assert_eq!(merged[0].installed_version.as_deref(), Some("0.2.0"));
        assert!(!merged[1].is_installed());
        assert!(!merged[1].update_available);
    }

    #[test]
    fn a_newer_local_version_is_not_downgraded() {
        let catalog = vec![catalog_metadata("catppuccin", "0.2.0")];
        let installed = vec![installed("catppuccin", "0.3.0")];
        let merged = merge_catalog(catalog, &installed);

        assert!(merged[0].is_installed());
        assert!(!merged[0].update_available);
    }

    #[test]
    fn installed_but_absent_from_the_catalog_is_still_listed() {
        let installed = vec![installed("local-only", "1.0.0")];
        let extras = installed_absent_from_catalog(&[], &installed);

        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].id, "local-only");

        let catalog = vec![catalog_metadata("local-only", "1.0.0")];
        assert!(installed_absent_from_catalog(&catalog, &installed).is_empty());
    }

    /// Builds the archive the API would serve for one theme extension.
    fn theme_extension_archive(
        id: &str,
        name: &str,
        version: &str,
        family_name: &str,
        theme_names: &[&str],
    ) -> Vec<u8> {
        build_extension_archive(&[
            (
                "extension.toml",
                &extension_manifest(id, name, version, &[&format!("themes/{id}.json")]),
            ),
            (
                &format!("themes/{id}.json"),
                &theme_family_json(family_name, theme_names),
            ),
        ])
    }

    #[test]
    fn an_installed_extension_is_laid_out_like_zeds() {
        let directory = temporary_extensions_dir();
        let archive = theme_extension_archive(
            "catppuccin",
            "Catppuccin",
            "1.2.3",
            "Catppuccin",
            &["Catppuccin Mocha", "Catppuccin Latte"],
        );

        let version = install_archive(directory.path(), "catppuccin", archive)
            .expect("the archive should install");
        assert_eq!(version, "1.2.3");

        // The manifest sits directly in the extension directory, which is what
        // both Zed and ZedTerm's loaders expect.
        assert!(directory.path().join("catppuccin/extension.toml").is_file());

        let loaded = load_installed_extensions_in(directory.path());
        let extension = loaded
            .iter()
            .find(|extension| extension.id == "catppuccin")
            .expect("the extension should be listed");
        assert_eq!(extension.name, "Catppuccin");
        assert_eq!(extension.version, "1.2.3");
        assert_eq!(extension.theme_count(), 2);
        assert_eq!(
            extension.theme_families[0].theme_names,
            vec![
                "Catppuccin Mocha".to_string(),
                "Catppuccin Latte".to_string()
            ]
        );
    }

    #[test]
    fn a_reinstall_replaces_the_previous_version() {
        let directory = temporary_extensions_dir();
        let first = theme_extension_archive("theme", "Theme", "1.0.0", "Theme", &["Theme One"]);
        install_archive(directory.path(), "theme", first).expect("the first version installs");
        // A file from the old version must not survive the upgrade.
        std::fs::write(directory.path().join("theme/stale.txt"), b"stale").unwrap();

        let second = theme_extension_archive("theme", "Theme", "2.0.0", "Theme", &["Theme Two"]);
        install_archive(directory.path(), "theme", second).expect("the upgrade installs");

        let loaded = load_installed_extensions_in(directory.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].version, "2.0.0");
        assert!(
            !directory.path().join("theme/stale.txt").exists(),
            "the previous installation must be fully replaced"
        );
    }

    #[test]
    fn an_archive_for_another_extension_is_rejected_and_leaves_no_trace() {
        let directory = temporary_extensions_dir();
        let archive = theme_extension_archive("actual", "Actual", "1.0.0", "Actual", &["Actual"]);

        let error = install_archive(directory.path(), "requested", archive)
            .expect_err("a mismatched archive must be rejected");
        assert_eq!(error.kind, StoreErrorKind::Manifest);

        assert!(
            !directory.path().join("requested").exists(),
            "a rejected archive must not create an installation"
        );
        assert!(
            load_installed_extensions_in(directory.path()).is_empty(),
            "a rejected archive must not appear as installed"
        );
    }

    #[test]
    fn a_failed_install_does_not_destroy_the_installed_copy() {
        let directory = temporary_extensions_dir();
        let archive = theme_extension_archive("keep", "Keep", "1.0.0", "Keep", &["Keep"]);
        install_archive(directory.path(), "keep", archive).expect("the first install succeeds");

        // A corrupt download must be reported, not silently applied.
        let error = install_archive(directory.path(), "keep", b"not a gzip stream".to_vec())
            .expect_err("a corrupt archive must be rejected");
        assert_eq!(error.kind, StoreErrorKind::Archive);

        let loaded = load_installed_extensions_in(directory.path());
        assert_eq!(loaded.len(), 1, "the working installation must survive");
        assert_eq!(loaded[0].version, "1.0.0");
    }

    #[test]
    fn a_path_traversal_entry_cannot_escape_the_destination() {
        let directory = temporary_extensions_dir();
        let archive = build_traversal_archive("../../escaped.txt", b"should not be written");

        let result = install_archive(directory.path(), "evil", archive);

        assert!(result.is_err(), "a traversal entry must be rejected");
        // `tempdir()` lives directly under the system temp directory, so a
        // two-level escape would land beside it.
        let escaped = directory.path().join("../escaped.txt");
        assert!(
            !escaped.exists(),
            "the traversal target {escaped:?} must not be written"
        );
        assert!(
            load_installed_extensions_in(directory.path()).is_empty(),
            "a rejected archive must not be installed"
        );
    }

    #[test]
    fn the_staging_directory_is_never_listed_as_installed() {
        let directory = temporary_extensions_dir();
        let staging = directory.path().join(STAGING_DIRECTORY).join("partial");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(
            staging.join(MANIFEST_FILE),
            extension_manifest("partial", "Partial", "1.0.0", &[]),
        )
        .unwrap();

        let loaded = load_installed_extensions_in(directory.path());
        assert!(
            !loaded.iter().any(|extension| extension.id == "partial"),
            "an in-progress download must not appear as installed"
        );
    }

    #[test]
    fn install_cleans_up_its_staging_directory() {
        let directory = temporary_extensions_dir();
        let archive = theme_extension_archive("tidy", "Tidy", "1.0.0", "Tidy", &["Tidy"]);
        install_archive(directory.path(), "tidy", archive).expect("the archive should install");

        let staging = directory.path().join(STAGING_DIRECTORY);
        let leftovers = std::fs::read_dir(&staging)
            .map(|entries| entries.flatten().count())
            .unwrap_or(0);
        assert_eq!(
            leftovers, 0,
            "staging must be empty after a successful install"
        );
    }

    #[test]
    fn uninstall_removes_the_extension_directory() {
        let directory = temporary_extensions_dir();
        let target = directory.path().join("removable");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(MANIFEST_FILE),
            extension_manifest("removable", "Removable", "1.0.0", &[]),
        )
        .unwrap();
        assert!(
            load_installed_extensions_in(directory.path())
                .iter()
                .any(|extension| extension.id == "removable")
        );

        uninstall_extension_from(directory.path(), "removable").expect("uninstall should succeed");
        assert!(!target.exists());
        assert!(load_installed_extensions_in(directory.path()).is_empty());

        // Removing it twice is not an error: the UI may retry.
        uninstall_extension_from(directory.path(), "removable")
            .expect("a repeated uninstall should succeed");
    }

    #[test]
    fn uninstall_rejects_an_id_that_would_escape_the_directory() {
        let directory = temporary_extensions_dir();
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        assert!(uninstall_extension_from(directory.path(), "../outside").is_err());
        assert!(outside.exists(), "the traversal target must survive");
    }

    #[test]
    fn a_manifest_with_a_traversal_theme_path_is_ignored() {
        let directory = temporary_extensions_dir();
        let target = directory.path().join("traversal-theme");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(MANIFEST_FILE),
            extension_manifest(
                "traversal-theme",
                "Traversal",
                "1.0.0",
                &["../../../etc/passwd"],
            ),
        )
        .unwrap();

        let loaded = load_installed_extensions_in(directory.path());
        let extension = loaded
            .iter()
            .find(|extension| extension.id == "traversal-theme")
            .expect("the extension should be listed");
        assert!(
            extension.theme_families.is_empty(),
            "a traversal theme path must contribute no themes"
        );
    }

    #[test]
    fn a_directory_without_a_manifest_is_not_listed() {
        let directory = temporary_extensions_dir();
        std::fs::create_dir_all(directory.path().join("not-an-extension")).unwrap();
        std::fs::write(directory.path().join("a-file"), b"not a directory").unwrap();

        assert!(
            load_installed_extensions_in(directory.path()).is_empty(),
            "only directories with a readable manifest are installed extensions"
        );
    }
}
