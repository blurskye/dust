//! Package registry requests, downloads and metadata caching.

use crate::plugins::PluginManifest;
use crate::tools::PackageManifest;
use crate::tools::PackageSource;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::blocking::Response;
use reqwest::header::ACCEPT;
use reqwest::header::CONTENT_LENGTH;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;
use std::time::Duration;
use thiserror::Error;

pub const OFFICIAL_REGISTRY: &str = "https://dust.blurskye.dev/registry/v1";

/// Network and cache limits for the official package registry.
#[derive(Clone, Debug)]
pub struct RegistryPolicy {
    pub base_url: String,
    pub offline: bool,
    pub redirect_limit: u8,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub maximum_metadata_bytes: usize,
    pub maximum_artifact_bytes: usize,
    pub maximum_cache_bytes: u64,
    pub maximum_cache_entries: usize,
    pub maximum_search_results: usize,
    pub queue_capacity: usize,
}

impl Default for RegistryPolicy {
    fn default() -> Self {
        Self {
            base_url: OFFICIAL_REGISTRY.into(),
            offline: false,
            redirect_limit: 0,
            connect_timeout_ms: 5_000,
            request_timeout_ms: 30_000,
            maximum_metadata_bytes: 1024 * 1024,
            maximum_artifact_bytes: 64 * 1024 * 1024,
            maximum_cache_bytes: 512 * 1024 * 1024,
            maximum_cache_entries: 4096,
            maximum_search_results: 100,
            queue_capacity: 8,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageSummary {
    pub publisher: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub yanked: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySearchResponse {
    pub schema: u32,
    pub packages: Vec<RegistryPackageSummary>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageMetadata {
    pub schema: u32,
    pub publisher: String,
    pub name: String,
    pub version: String,
    pub yanked: bool,
    pub artifact_path: String,
    pub artifact_sha256: String,
    pub package: PackageManifest,
    pub plugin: PluginManifest,
}
#[derive(Clone, Debug)]
pub enum RegistryRequest {
    Search {
        query: String,
    },
    Show {
        publisher: String,
        name: String,
        version: String,
    },
    Fetch {
        metadata: Box<RegistryPackageMetadata>,
    },
}
#[derive(Clone, Debug)]
pub enum RegistryResult {
    Search(RegistrySearchResponse),
    Show(RegistryPackageMetadata),
    Artifact {
        metadata: RegistryPackageMetadata,
        bytes: Vec<u8>,
    },
}
#[derive(Clone, Debug)]
pub struct RegistryEvent {
    pub id: u64,
    pub result: Result<RegistryResult, RegistryError>,
}
pub struct RegistryWorker {
    sender: SyncSender<(u64, RegistryRequest)>,
    receiver: Receiver<RegistryEvent>,
    ids: Arc<dyn crate::clock::IdSource>,
}
impl RegistryWorker {
    pub fn start(policy: RegistryPolicy, cache: PathBuf) -> Result<Self, RegistryError> {
        Self::start_with_ids(policy, cache, Arc::new(crate::clock::AtomicIds::new(1)))
    }
    pub fn start_with_ids(
        policy: RegistryPolicy,
        cache: PathBuf,
        ids: Arc<dyn crate::clock::IdSource>,
    ) -> Result<Self, RegistryError> {
        validate_policy(&policy)?;
        let capacity = policy.queue_capacity;
        let (tx, rx) = mpsc::sync_channel::<(u64, RegistryRequest)>(capacity);
        let (etx, erx) = mpsc::sync_channel(capacity);
        thread::Builder::new()
            .name("dust-registry".into())
            .spawn(move || {
                let client = RegistryClient::new(policy, cache);
                while let Ok((id, request)) = rx.recv() {
                    let result = client
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|client| client.execute(request));
                    if etx.send(RegistryEvent { id, result }).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| RegistryError::Thread(Arc::new(error)))?;
        Ok(Self {
            sender: tx,
            receiver: erx,
            ids,
        })
    }
    pub fn request(&self, request: RegistryRequest) -> Result<u64, RegistryError> {
        let id = self.ids.next();
        self.sender
            .try_send((id, request))
            .map_err(|error| match error {
                TrySendError::Full(_) => RegistryError::QueueFull,
                TrySendError::Disconnected(_) => RegistryError::Stopped,
            })?;
        Ok(id)
    }
    pub fn poll(&self) -> Result<Vec<RegistryEvent>, RegistryError> {
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(RegistryError::Stopped),
            }
        }
        Ok(events)
    }
}
struct RegistryClient {
    policy: RegistryPolicy,
    base: Url,
    cache: PathBuf,
    http: Client,
}
impl RegistryClient {
    fn new(policy: RegistryPolicy, cache: PathBuf) -> Result<Self, RegistryError> {
        validate_policy(&policy)?;
        let base = Url::parse(&format!("{}/", policy.base_url.trim_end_matches('/')))
            .map_err(|_| RegistryError::Policy)?;
        if !cache.is_absolute() {
            return Err(RegistryError::Policy);
        }
        fs::create_dir_all(&cache).map_err(io_error)?;
        let metadata = fs::symlink_metadata(&cache).map_err(io_error)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(RegistryError::Cache);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        let http = Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(policy.connect_timeout_ms))
            .timeout(Duration::from_millis(policy.request_timeout_ms))
            .build()
            .map_err(http_error)?;
        Ok(Self {
            policy,
            base,
            cache,
            http,
        })
    }
    fn execute(&self, request: RegistryRequest) -> Result<RegistryResult, RegistryError> {
        match request {
            RegistryRequest::Search { query } => {
                if query.is_empty() || query.len() > 256 || query.chars().any(char::is_control) {
                    return Err(RegistryError::InvalidMetadata);
                }
                let mut url = self.official_url("packages/search")?;
                url.query_pairs_mut()
                    .append_pair("q", &query)
                    .append_pair("limit", &self.policy.maximum_search_results.to_string());
                let value: RegistrySearchResponse = self.fetch_json(url)?;
                validate_search(&value, self.policy.maximum_search_results)?;
                Ok(RegistryResult::Search(value))
            }
            RegistryRequest::Show {
                publisher,
                name,
                version,
            } => {
                validate_segment(&publisher)?;
                validate_segment(&name)?;
                validate_version(&version)?;
                let url = self.official_url(&format!("packages/{publisher}/{name}/{version}"))?;
                let value: RegistryPackageMetadata = self.fetch_json(url)?;
                validate_metadata(&value, Some((&publisher, &name, &version)))?;
                Ok(RegistryResult::Show(value))
            }
            RegistryRequest::Fetch { metadata } => {
                validate_metadata(&metadata, None)?;
                let url = self.official_url(metadata.artifact_path.trim_start_matches('/'))?;
                let bytes =
                    self.fetch(url, "application/wasm", self.policy.maximum_artifact_bytes)?;
                let actual = format!("{:x}", Sha256::digest(&bytes));
                if !actual.eq_ignore_ascii_case(&metadata.artifact_sha256) {
                    return Err(RegistryError::Checksum);
                }
                Ok(RegistryResult::Artifact {
                    metadata: *metadata,
                    bytes,
                })
            }
        }
    }
    fn official_url(&self, path: &str) -> Result<Url, RegistryError> {
        if path.contains("..") || path.starts_with('/') || path.contains('\\') {
            return Err(RegistryError::Origin);
        }
        let url = self.base.join(path).map_err(|_| RegistryError::Origin)?;
        if url.scheme() != "https"
            || url.host_str() != self.base.host_str()
            || url.port_or_known_default() != self.base.port_or_known_default()
            || url.username() != ""
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with(self.base.path())
        {
            return Err(RegistryError::Origin);
        }
        Ok(url)
    }
    fn fetch_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, RegistryError> {
        let bytes = self.fetch(url, "application/json", self.policy.maximum_metadata_bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| RegistryError::InvalidMetadata)
    }
    fn fetch(&self, url: Url, accept: &str, maximum: usize) -> Result<Vec<u8>, RegistryError> {
        let key = format!("{:x}", Sha256::digest(url.as_str().as_bytes()));
        let path = self.cache.join(format!("{key}.body"));
        if self.policy.offline {
            let _lock = self.lock_cache()?;
            return read_cache(&path, maximum);
        }
        let response = self
            .http
            .get(url)
            .header(ACCEPT, accept)
            .send()
            .map_err(http_error)?;
        let bytes = read_response(response, accept, maximum)?;
        let _lock = self.lock_cache()?;
        write_cache(&path, &bytes)?;
        self.prune_cache()?;
        Ok(bytes)
    }
    // Keep the body/checksum pair and pruning under one cross-process lock.
    fn lock_cache(&self) -> Result<crate::files::FileLock, RegistryError> {
        crate::files::lock_file(&self.cache.join("cache.lock")).map_err(io_error)
    }
    fn prune_cache(&self) -> Result<(), RegistryError> {
        let mut entries = fs::read_dir(&self.cache)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                (metadata.is_file()
                    && entry.path().extension().and_then(|v| v.to_str()) == Some("body"))
                .then_some((entry.path(), metadata.len(), metadata.modified().ok()))
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(path, _, modified)| (*modified, path.clone()));
        let mut bytes = entries.iter().map(|(_, len, _)| *len).sum::<u64>();
        while entries.len() > self.policy.maximum_cache_entries
            || bytes > self.policy.maximum_cache_bytes
        {
            let (path, len, _) = entries.remove(0);
            fs::remove_file(&path).map_err(io_error)?;
            let _ = fs::remove_file(path.with_extension("sha256"));
            bytes = bytes.saturating_sub(len)
        }
        Ok(())
    }
}
fn validate_policy(policy: &RegistryPolicy) -> Result<(), RegistryError> {
    if policy.base_url != OFFICIAL_REGISTRY
        || policy.redirect_limit != 0
        || policy.offline && policy.maximum_cache_entries == 0
        || policy.maximum_metadata_bytes == 0
        || policy.maximum_artifact_bytes == 0
    {
        return Err(RegistryError::Policy);
    }
    Ok(())
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn validate_segment(value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RegistryError::InvalidMetadata);
    }
    Ok(())
}
fn validate_version(value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(RegistryError::InvalidMetadata);
    }
    Ok(())
}
fn validate_search(value: &RegistrySearchResponse, maximum: usize) -> Result<(), RegistryError> {
    if value.schema != 1 || value.packages.len() > maximum {
        return Err(RegistryError::InvalidMetadata);
    }
    for package in &value.packages {
        validate_segment(&package.publisher)?;
        validate_segment(&package.name)?;
        validate_version(&package.version)?;
        if package.description.len() > 4096 || package.description.chars().any(char::is_control) {
            return Err(RegistryError::InvalidMetadata);
        }
    }
    Ok(())
}
fn validate_metadata(
    value: &RegistryPackageMetadata,
    expected: Option<(&str, &str, &str)>,
) -> Result<(), RegistryError> {
    if value.schema != 1 || value.yanked || !valid_digest(&value.artifact_sha256) {
        return Err(RegistryError::InvalidMetadata);
    }
    validate_segment(&value.publisher)?;
    validate_segment(&value.name)?;
    validate_version(&value.version)?;
    if let Some((publisher, name, version)) = expected
        && (value.publisher != publisher || value.name != name || value.version != version)
    {
        return Err(RegistryError::InvalidMetadata);
    }
    let identity = format!("{}/{}", value.publisher, value.name);
    match &value.package.source {
        PackageSource::Registry {
            registry,
            version,
            archive_sha256,
        } if registry == OFFICIAL_REGISTRY
            && version == &value.version
            && archive_sha256.eq_ignore_ascii_case(&value.artifact_sha256) => {}
        _ => return Err(RegistryError::InvalidMetadata),
    }
    if value.package.name != identity
        || value.package.version != value.version
        || !value
            .plugin
            .component_sha256
            .eq_ignore_ascii_case(&value.artifact_sha256)
    {
        return Err(RegistryError::InvalidMetadata);
    }
    let prefix = format!(
        "packages/{}/{}/{}/artifacts/",
        value.publisher, value.name, value.version
    );
    let artifact_path = value.artifact_path.trim_start_matches('/');
    let artifact_url = Url::parse(&format!("{OFFICIAL_REGISTRY}/{artifact_path}"))
        .map_err(|_| RegistryError::Origin)?;
    // URL parsing removes encoded parent segments too; validate the final scope.
    if !artifact_path.starts_with(&prefix)
        || !artifact_url
            .as_str()
            .starts_with(&format!("{OFFICIAL_REGISTRY}/{prefix}"))
    {
        return Err(RegistryError::Origin);
    }
    Ok(())
}
#[cfg(test)]
mod metadata_tests {
    use crate::tools::registry::*;

    #[test]
    fn artifact_scope_is_checked_after_url_normalization() {
        let (mut package, plugin, _) = crate::tools::tests::package_fixture("publisher/demo");
        if let PackageSource::Registry { registry, .. } = &mut package.source {
            *registry = OFFICIAL_REGISTRY.into();
        }
        let mut metadata = RegistryPackageMetadata {
            schema: 1,
            publisher: "publisher".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            yanked: false,
            artifact_path: "packages/publisher/demo/1.0.0/artifacts/x.wasm".into(),
            artifact_sha256: plugin.component_sha256.clone(),
            package,
            plugin,
        };
        assert!(validate_metadata(&metadata, Some(("publisher", "demo", "1.0.0"))).is_ok());
        for traversal in ["%2e%2e/%2e%2e", ".%2e/%2e.", "../.."] {
            metadata.artifact_path = format!(
                "packages/publisher/demo/1.0.0/artifacts/{traversal}/2.0.0/artifacts/x.wasm"
            );
            assert!(matches!(
                validate_metadata(&metadata, None),
                Err(RegistryError::Origin)
            ));
            assert!(matches!(
                validate_metadata(&metadata, Some(("publisher", "demo", "1.0.0"))),
                Err(RegistryError::Origin)
            ));
        }
    }
}

fn read_response(
    mut response: Response,
    accept: &str,
    maximum: usize,
) -> Result<Vec<u8>, RegistryError> {
    if response.status().is_redirection() {
        return Err(RegistryError::Redirect);
    }
    if !response.status().is_success() {
        return Err(RegistryError::Status(response.status().as_u16()));
    }
    let mut content_lengths = response.headers().get_all(CONTENT_LENGTH).iter();
    if let Some(value) = content_lengths.next() {
        if content_lengths.next().is_some() {
            return Err(RegistryError::ContentLength);
        }
        let value = value
            .to_str()
            .map_err(|_| RegistryError::ContentLength)?
            .parse::<u64>()
            .map_err(|_| RegistryError::ContentLength)?;
        if value > maximum as u64 {
            return Err(RegistryError::TooLarge);
        }
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(accept))
    {
        return Err(RegistryError::ContentType);
    }
    let mut bytes = Vec::with_capacity(maximum.min(1024 * 1024));
    response
        .by_ref()
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > maximum {
        return Err(RegistryError::TooLarge);
    }
    Ok(bytes)
}
fn read_cache(path: &Path, maximum: usize) -> Result<Vec<u8>, RegistryError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| RegistryError::OfflineMiss)?;
    let checksum_path = path.with_extension("sha256");
    let checksum_metadata =
        fs::symlink_metadata(&checksum_path).map_err(|_| RegistryError::OfflineMiss)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum as u64
        || !checksum_metadata.file_type().is_file()
        || checksum_metadata.file_type().is_symlink()
        || checksum_metadata.len() != 64
    {
        return Err(RegistryError::OfflineMiss);
    }
    let bytes = fs::read(path).map_err(io_error)?;
    let expected = fs::read_to_string(checksum_path).map_err(io_error)?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(RegistryError::Checksum);
    }
    Ok(bytes)
}
#[cfg(test)]
mod cache_tests {
    use crate::tools::registry::*;

    #[test]
    fn independent_cache_clients_serialize_publication_reads_and_pruning() {
        let directory = tempfile::tempdir().unwrap();
        let policy = RegistryPolicy {
            offline: true,
            ..RegistryPolicy::default()
        };
        let client = RegistryClient::new(policy.clone(), directory.path().into()).unwrap();
        let url = client.official_url("packages/search?q=test").unwrap();
        let key = format!("{:x}", Sha256::digest(url.as_str().as_bytes()));
        let path = directory.path().join(format!("{key}.body"));
        {
            let _lock = client.lock_cache().unwrap();
            let file = fs::File::open(directory.path().join("cache.lock")).unwrap();
            assert!(matches!(
                crate::files::lock_exclusive(file, true),
                Err((_, error)) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
            write_cache(&path, b"initial").unwrap();
        }
        thread::scope(|scope| {
            for value in [b"first".as_slice(), b"second".as_slice()] {
                let client = RegistryClient::new(policy.clone(), directory.path().into()).unwrap();
                let path = path.clone();
                scope.spawn(move || {
                    for _ in 0..32 {
                        let _lock = client.lock_cache().unwrap();
                        write_cache(&path, value).unwrap();
                        client.prune_cache().unwrap();
                    }
                });
            }
            for _ in 0..64 {
                let bytes = client.fetch(url.clone(), "application/json", 1024).unwrap();
                assert!([b"initial".as_slice(), b"first", b"second"].contains(&bytes.as_slice()));
            }
        });
        assert!(read_cache(&path, 1024).is_ok());
    }
}

fn validate_cache_destination(path: &Path) -> Result<(), RegistryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(RegistryError::Cache),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}
fn write_cache_temp(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}
fn write_cache(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    let checksum_path = path.with_extension("sha256");
    validate_cache_destination(path)?;
    validate_cache_destination(&checksum_path)?;
    let nonce = std::process::id();
    let body_temp = path.with_extension(format!("body-tmp-{nonce}"));
    let checksum_temp = path.with_extension(format!("checksum-tmp-{nonce}"));
    let checksum = format!("{:x}", Sha256::digest(bytes));
    let result = (|| {
        write_cache_temp(&body_temp, bytes)?;
        write_cache_temp(&checksum_temp, checksum.as_bytes())?;
        fs::rename(&body_temp, path).map_err(io_error)?;
        fs::rename(&checksum_temp, &checksum_path).map_err(io_error)?;
        let parent = path.parent().ok_or(RegistryError::Cache)?;
        crate::files::sync_directory(parent).map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(body_temp);
        let _ = fs::remove_file(checksum_temp);
    }
    result
}
#[derive(Clone, Debug, Error)]
pub enum RegistryError {
    #[error("invalid official registry policy")]
    Policy,
    #[error("registry URL escaped the official origin")]
    Origin,
    #[error("registry redirects are forbidden")]
    Redirect,
    #[error("registry HTTP status {0}")]
    Status(u16),
    #[error("registry content type is invalid")]
    ContentType,
    #[error("registry Content-Length is malformed")]
    ContentLength,
    #[error("registry response exceeds its byte limit")]
    TooLarge,
    #[error("registry metadata is invalid or yanked")]
    InvalidMetadata,
    #[error("registry artifact checksum differs")]
    Checksum,
    #[error("registry offline cache miss")]
    OfflineMiss,
    #[error("registry cache path is unsafe")]
    Cache,
    #[error("registry request queue is full")]
    QueueFull,
    #[error("registry worker stopped")]
    Stopped,
    #[error("{0}")]
    Http(String),
    #[error("registry I/O failed: {0}")]
    Io(#[source] Arc<std::io::Error>),
    #[error("registry worker thread failed: {0}")]
    Thread(#[source] Arc<std::io::Error>),
}
/// reqwest's own text is mostly the URL and leaves the reason in its source chain.
fn http_error(error: reqwest::Error) -> RegistryError {
    let error = error.without_url();
    let mut cause: &dyn std::error::Error = &error;
    while let Some(source) = cause.source() {
        cause = source;
    }
    RegistryError::Http(cause.to_string())
}
fn io_error(value: std::io::Error) -> RegistryError {
    RegistryError::Io(Arc::new(value))
}
impl From<std::io::Error> for RegistryError {
    fn from(value: std::io::Error) -> Self {
        io_error(value)
    }
}
