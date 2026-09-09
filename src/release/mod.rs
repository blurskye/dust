#![forbid(unsafe_code)]
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseManifest {
    pub format: u32,
    pub dust_version: String,
    pub target: String,
    pub rustc: String,
    pub command_api: u32,
    pub wit_api: String,
    pub license: String,
    pub distribution: String,
    pub signing_policy: String,
    pub binary: ReleaseFile,
    pub cargo_lock: ReleaseFile,
    pub assets: Vec<ReleaseFile>,
}
pub fn build(binary: &Path, root: &Path) -> Result<ReleaseManifest, ReleaseError> {
    let binary = file_record(
        binary,
        binary
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("dust"),
    )?;
    let cargo_lock = file_record(&root.join("Cargo.lock"), "Cargo.lock")?;
    let mut paths = Vec::new();
    collect(&root.join("assets"), &mut paths)?;
    paths.sort();
    let assets = paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ReleaseError::OutsideRoot)?;
            file_record(&path, &relative.to_string_lossy())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rustc = Command::new("rustc")
        .arg("-Vv")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .chars()
                .take(4096)
                .collect()
        })
        .unwrap_or_else(|| "unknown".into());
    Ok(ReleaseManifest {
        format: 2,
        dust_version: env!("CARGO_PKG_VERSION").into(),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        rustc,
        command_api: crate::api::COMMAND_API_VERSION,
        wit_api: crate::api::WIT_API_VERSION.into(),
        license: "GPL-3.0-or-later".into(),
        distribution: "deterministic-tar-zst-v1".into(),
        signing_policy: "sigstore-keyless".into(),
        binary,
        cargo_lock,
        assets,
    })
}
pub fn write_canonical(path: &Path, manifest: &ReleaseManifest) -> Result<(), ReleaseError> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(ReleaseError::Json)?;
    let parent = path.parent().ok_or(ReleaseError::OutsideRoot)?;
    fs::create_dir_all(parent).map_err(ReleaseError::Io)?;
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(ReleaseError::Io)?;
    file.write_all(&bytes).map_err(ReleaseError::Io)?;
    file.sync_all().map_err(ReleaseError::Io)?;
    fs::rename(temp, path).map_err(ReleaseError::Io)?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(ReleaseError::Io)
}
fn file_record(path: &Path, label: &str) -> Result<ReleaseFile, ReleaseError> {
    let metadata = fs::symlink_metadata(path).map_err(ReleaseError::Io)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ReleaseError::InvalidFile(path.into()));
    }
    let mut file = fs::File::open(path).map_err(ReleaseError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(ReleaseError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ReleaseFile {
        path: label.into(),
        bytes: metadata.len(),
        sha256: format!("{:x}", hasher.finalize()),
    })
}
fn collect(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), ReleaseError> {
    for entry in fs::read_dir(directory).map_err(ReleaseError::Io)? {
        let entry = entry.map_err(ReleaseError::Io)?;
        let metadata = entry.file_type().map_err(ReleaseError::Io)?;
        if metadata.is_symlink() {
            return Err(ReleaseError::InvalidFile(entry.path()));
        }
        if metadata.is_dir() {
            collect(&entry.path(), out)?
        } else if metadata.is_file() {
            out.push(entry.path())
        }
    }
    Ok(())
}
#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("release I/O failed: {0}")]
    Io(#[source] io::Error),
    #[error("release JSON failed: {0}")]
    Json(#[source] serde_json::Error),
    #[error("invalid release file {0}")]
    InvalidFile(PathBuf),
    #[error("release path escaped root")]
    OutsideRoot,
    #[error("invalid archive path {0}")]
    InvalidArchivePath(String),
    #[error("release checksum mismatch")]
    ChecksumMismatch,
    #[error("no installed rollback is available")]
    NoRollback,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReleaseArchive {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub manifest: ReleaseManifest,
}
#[derive(Clone, Debug, Serialize)]
struct Provenance<'a> {
    format: u32,
    version: &'a str,
    target: String,
    builder: &'a str,
    command_api: u32,
    wit_api: &'a str,
    license: &'a str,
    distribution: &'a str,
    signing_policy: &'a str,
    reproducible_inputs: bool,
}
#[derive(Clone, Debug, Serialize)]
struct Sbom {
    bom_format: &'static str,
    spec_version: &'static str,
    components: Vec<SbomComponent>,
}
#[derive(Clone, Debug, Serialize)]
struct SbomComponent {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}
pub fn write_deterministic_archive(
    binary: &Path,
    root: &Path,
    output: &Path,
) -> Result<ReleaseArchive, ReleaseError> {
    if !output.is_absolute() {
        return Err(ReleaseError::OutsideRoot);
    }
    if !output.to_string_lossy().ends_with(".tar.zst") {
        return Err(ReleaseError::InvalidArchivePath(
            output.display().to_string(),
        ));
    }
    let manifest = build(binary, root)?;
    let mut entries = Vec::<(String, Vec<u8>, u32)>::new();
    entries.push((
        "bin/dust".into(),
        read_regular_bounded(binary, 1024 * 1024 * 1024)?,
        0o755,
    ));
    entries.push((
        "Cargo.lock".into(),
        read_regular_bounded(&root.join("Cargo.lock"), 64 * 1024 * 1024)?,
        0o644,
    ));
    for (relative, maximum) in [
        ("LICENSE", 1024 * 1024),
        ("NOTICE", 1024 * 1024),
        ("LICENSES/Apache-2.0.txt", 1024 * 1024),
        ("LICENSES/MIT-mini.icons.txt", 1024 * 1024),
        ("LICENSES/MIT-catppuccin.txt", 1024 * 1024),
        ("LICENSES/MIT-nord.txt", 1024 * 1024),
        ("LICENSES/MIT-gruvbox.txt", 1024 * 1024),
        ("wit/dust-plugin.wit", 1024 * 1024),
        ("docs/plugin-sdk.md", 4 * 1024 * 1024),
    ] {
        entries.push((
            relative.into(),
            read_regular_bounded(&root.join(relative), maximum)?,
            0o644,
        ));
    }
    let mut assets = Vec::new();
    collect(&root.join("assets"), &mut assets)?;
    assets.sort();
    for path in assets {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ReleaseError::OutsideRoot)?
            .to_string_lossy()
            .replace('\\', "/");
        entries.push((
            relative,
            read_regular_bounded(&path, 256 * 1024 * 1024)?,
            0o644,
        ));
    }
    entries.push((
        "release-manifest.json".into(),
        serde_json::to_vec_pretty(&manifest).map_err(ReleaseError::Json)?,
        0o644,
    ));
    let provenance = Provenance {
        format: 2,
        version: env!("CARGO_PKG_VERSION"),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        builder: "dust release owner",
        command_api: crate::api::COMMAND_API_VERSION,
        wit_api: crate::api::WIT_API_VERSION,
        license: "GPL-3.0-or-later",
        distribution: "deterministic-tar-zst-v1",
        signing_policy: "sigstore-keyless",
        reproducible_inputs: true,
    };
    entries.push((
        "provenance.json".into(),
        serde_json::to_vec_pretty(&provenance).map_err(ReleaseError::Json)?,
        0o644,
    ));
    entries.push((
        "sbom.cdx.json".into(),
        serde_json::to_vec_pretty(&cargo_lock_sbom(root)?).map_err(ReleaseError::Json)?,
        0o644,
    ));
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let parent = output.parent().ok_or(ReleaseError::OutsideRoot)?;
    fs::create_dir_all(parent).map_err(ReleaseError::Io)?;
    let temp = output.with_extension(format!("tmp.{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&temp).map_err(ReleaseError::Io)?;
    let mut encoder = zstd::stream::write::Encoder::new(file, 19).map_err(ReleaseError::Io)?;
    encoder.include_checksum(true).map_err(ReleaseError::Io)?;
    for (name, bytes, mode) in entries {
        write_tar_entry(&mut encoder, &name, &bytes, mode)?;
    }
    encoder.write_all(&[0; 1024]).map_err(ReleaseError::Io)?;
    let file = encoder.finish().map_err(ReleaseError::Io)?;
    file.sync_all().map_err(ReleaseError::Io)?;
    fs::rename(&temp, output).map_err(ReleaseError::Io)?;
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(ReleaseError::Io)?;
    let record = file_record(
        output,
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dust.tar.zst"),
    )?;
    Ok(ReleaseArchive {
        path: output.into(),
        bytes: record.bytes,
        sha256: record.sha256,
        manifest,
    })
}
fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ReleaseError> {
    let metadata = fs::symlink_metadata(path).map_err(ReleaseError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum
    {
        return Err(ReleaseError::InvalidFile(path.into()));
    }
    fs::read(path).map_err(ReleaseError::Io)
}
fn octal(field: &mut [u8], value: u64) {
    field.fill(b'0');
    let text = format!("{value:o}");
    let start = field.len().saturating_sub(text.len() + 1);
    field[start..start + text.len()].copy_from_slice(text.as_bytes());
    field[field.len() - 1] = 0;
}
fn write_tar_entry(
    writer: &mut impl Write,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), ReleaseError> {
    if name.is_empty()
        || name.len() > 100
        || name.starts_with('/')
        || name.split('/').any(|part| part == "..")
        || name.bytes().any(|byte| byte == 0)
    {
        return Err(ReleaseError::InvalidArchivePath(name.into()));
    }
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut header[100..108], u64::from(mode));
    octal(&mut header[108..116], 0);
    octal(&mut header[116..124], 0);
    octal(&mut header[124..136], bytes.len() as u64);
    octal(&mut header[136..148], 0);
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum = header.iter().map(|byte| u64::from(*byte)).sum();
    octal(&mut header[148..156], sum);
    writer
        .write_all(&header)
        .and_then(|()| writer.write_all(bytes))
        .map_err(ReleaseError::Io)?;
    let padding = (512 - bytes.len() % 512) % 512;
    if padding > 0 {
        writer
            .write_all(&vec![0; padding])
            .map_err(ReleaseError::Io)?
    }
    Ok(())
}
fn cargo_lock_sbom(root: &Path) -> Result<Sbom, ReleaseError> {
    let bytes = read_regular_bounded(&root.join("Cargo.lock"), 64 * 1024 * 1024)?;
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| ReleaseError::InvalidFile(root.join("Cargo.lock")))?;
    let value: toml::Value =
        toml::from_str(source).map_err(|_| ReleaseError::InvalidFile(root.join("Cargo.lock")))?;
    let mut components = value
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .take(100_000)
        .filter_map(|package| {
            Some(SbomComponent {
                name: package.get("name")?.as_str()?.into(),
                version: package.get("version")?.as_str()?.into(),
                source: package
                    .get("source")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
                checksum: package
                    .get("checksum")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect::<Vec<_>>();
    components.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(Sbom {
        bom_format: "CycloneDX",
        spec_version: "1.5",
        components,
    })
}
pub fn install_binary(
    source: &Path,
    prefix: &Path,
    expected_sha256: Option<&str>,
) -> Result<PathBuf, ReleaseError> {
    if !prefix.is_absolute() {
        return Err(ReleaseError::OutsideRoot);
    }
    let bytes = read_regular_bounded(source, 1024 * 1024 * 1024)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if expected_sha256.is_some_and(|expected| expected.to_ascii_lowercase() != digest) {
        return Err(ReleaseError::ChecksumMismatch);
    }
    fs::create_dir_all(prefix).map_err(ReleaseError::Io)?;
    let prefix = fs::canonicalize(prefix).map_err(ReleaseError::Io)?;
    let bin = prefix.join("bin");
    if bin.exists()
        && fs::symlink_metadata(&bin)
            .map_err(ReleaseError::Io)?
            .file_type()
            .is_symlink()
    {
        return Err(ReleaseError::InvalidFile(bin));
    }
    fs::create_dir_all(&bin).map_err(ReleaseError::Io)?;
    let target = bin.join("dust");
    if target.exists()
        && fs::symlink_metadata(&target)
            .map_err(ReleaseError::Io)?
            .file_type()
            .is_symlink()
    {
        return Err(ReleaseError::InvalidFile(target));
    }
    let backup = bin.join("dust.previous");
    if target.exists() {
        let _ = fs::remove_file(&backup);
        fs::rename(&target, &backup).map_err(ReleaseError::Io)?
    }
    let temp = bin.join(format!(".dust.{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options.open(&temp).map_err(ReleaseError::Io)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(ReleaseError::Io)?;
    fs::rename(temp, &target).map_err(ReleaseError::Io)?;
    fs::File::open(&bin)
        .and_then(|file| file.sync_all())
        .map_err(ReleaseError::Io)?;
    Ok(target)
}
pub fn rollback_install(prefix: &Path) -> Result<PathBuf, ReleaseError> {
    let bin = canonical_bin(prefix)?;
    let target = bin.join("dust");
    let backup = bin.join("dust.previous");
    if !backup.is_file() {
        return Err(ReleaseError::NoRollback);
    }
    if target.exists() {
        fs::remove_file(&target).map_err(ReleaseError::Io)?
    }
    fs::rename(backup, &target).map_err(ReleaseError::Io)?;
    Ok(target)
}
pub fn uninstall(prefix: &Path) -> Result<(), ReleaseError> {
    let bin = canonical_bin(prefix)?;
    for name in ["dust", "dust.previous"] {
        let path = bin.join(name);
        if path.exists() {
            let metadata = fs::symlink_metadata(&path).map_err(ReleaseError::Io)?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(ReleaseError::InvalidFile(path));
            }
            fs::remove_file(path).map_err(ReleaseError::Io)?
        }
    }
    Ok(())
}
fn canonical_bin(prefix: &Path) -> Result<PathBuf, ReleaseError> {
    if !prefix.is_absolute() {
        return Err(ReleaseError::OutsideRoot);
    }
    let prefix = fs::canonicalize(prefix).map_err(ReleaseError::Io)?;
    let bin = prefix.join("bin");
    let metadata = fs::symlink_metadata(&bin).map_err(ReleaseError::Io)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ReleaseError::InvalidFile(bin));
    }
    Ok(bin)
}
