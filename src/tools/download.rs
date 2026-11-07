//! Bounded tool downloads and artifact verification.

use std::fs;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;

use reqwest::blocking::Client;

use reqwest::redirect::Policy;
use sha2::Digest;
use sha2::Sha256;

use crate::tools::catalog::ToolArchiveFormat;

use crate::tools::install_prepare::MAXIMUM_TOOL_ARCHIVE_ENTRIES;

use crate::tools::install_prepare::MAXIMUM_TOOL_DOWNLOAD_BYTES;

use crate::tools::install_prepare::MAXIMUM_TOOL_UNPACKED_BYTES;

use crate::tools::install_prepare::archive_entry_matches;

use crate::tools::install_prepare::checked_https_url;

use crate::tools::install_prepare::relative_catalog_path;

use crate::tools::package_lock::next_temp_suffix;

use crate::tools::package_lock::valid_digest;

pub(super) type ToolProgressReporter = dyn FnMut(&str, u64, Option<u64>);

pub(super) fn download_checked(
    url: &str,
    expected_sha256: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
) -> Result<Vec<u8>, String> {
    if cancellations.try_recv().is_ok() {
        return Err("tool installation cancelled".into());
    }
    let url = url.to_owned();
    let expected = expected_sha256.to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (progress, updates) = mpsc::sync_channel(8);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    thread::Builder::new()
        .name("dust-tool-download".into())
        .spawn(move || {
            let result = download_artifact(
                &url,
                &expected,
                &worker_cancelled,
                &mut move |_, completed, total| {
                    let _ = progress.try_send((completed, total));
                },
            );
            let _ = sender.send(result);
        })
        .map_err(|error| format!("cannot start tool download: {error}"))?;
    let result = receive_download(&receiver, &updates, cancellations, report);
    cancelled.store(true, Ordering::Release);
    result
}

fn receive_download(
    receiver: &Receiver<Result<Vec<u8>, String>>,
    progress: &Receiver<(u64, Option<u64>)>,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
) -> Result<Vec<u8>, String> {
    loop {
        if cancellations.try_recv().is_ok() {
            return Err("tool installation cancelled".into());
        }
        for (completed, total) in progress.try_iter() {
            report("downloading", completed, total);
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("tool download stopped".into()),
        }
    }
}

fn download_artifact(
    url: &str,
    expected_sha256: &str,
    cancelled: &AtomicBool,
    report: &mut ToolProgressReporter,
) -> Result<Vec<u8>, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("tool installation cancelled".into());
    }
    if !valid_digest(expected_sha256) {
        return Err("tool catalog has an invalid artifact checksum".into());
    }
    let url = checked_https_url(url)?;
    let client = Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(Policy::limited(3))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| format!("cannot create HTTPS client: {error}"))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("tool artifact download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "tool artifact download returned {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_TOOL_DOWNLOAD_BYTES as u64)
    {
        return Err("tool artifact exceeds the download limit".into());
    }
    let total = response.content_length();
    report("downloading", 0, total);
    let mut response = response.take(MAXIMUM_TOOL_DOWNLOAD_BYTES.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("tool installation cancelled".into());
        }
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("cannot read tool artifact: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        report(
            "downloading",
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            total,
        );
    }
    if bytes.len() > MAXIMUM_TOOL_DOWNLOAD_BYTES {
        return Err("tool artifact exceeds the download limit".into());
    }
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err("tool artifact checksum did not match the bundled catalog".into());
    }
    Ok(bytes)
}

pub(super) fn copy_limited_with_budget(
    mut reader: impl Read,
    mut writer: impl Write,
    remaining: &mut u64,
) -> Result<(), String> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot read tool archive: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        if read > *remaining {
            return Err("tool archive exceeds the unpacked-size limit".into());
        }
        writer
            .write_all(&buffer[..usize::try_from(read).unwrap_or(0)])
            .map_err(|error| format!("cannot write installed tool: {error}"))?;
        *remaining = remaining.saturating_sub(read);
    }
}

pub(super) fn copy_limited(reader: impl Read, writer: impl Write) -> Result<(), String> {
    let mut remaining = MAXIMUM_TOOL_UNPACKED_BYTES;
    copy_limited_with_budget(reader, writer, &mut remaining)
}

pub(super) fn prepare_executable(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect installed tool: {error}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("tool archive did not contain a regular executable".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot mark installed tool executable: {error}"))?;
    }
    Ok(())
}

pub(super) fn extract_one(
    bytes: &[u8],
    archive: ToolArchiveFormat,
    expected_path: &str,
    output: &Path,
) -> Result<(), String> {
    let expected_path = relative_catalog_path(expected_path)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|error| format!("cannot create installed tool: {error}"))?;
    let found = match archive {
        ToolArchiveFormat::Raw => {
            if expected_path.components().count() != 1 {
                return Err("a raw tool artifact must contain one executable name".into());
            }
            copy_limited(Cursor::new(bytes), &mut file)?;
            true
        }
        ToolArchiveFormat::Gzip => {
            if expected_path.components().count() != 1 {
                return Err("a gzip tool artifact must contain one executable name".into());
            }
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            copy_limited(decoder, &mut file)?;
            true
        }
        ToolArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|error| format!("cannot open ZIP tool artifact: {error}"))?;
            if zip.len() > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
                return Err("tool archive has too many entries".into());
            }
            let mut entry = zip
                .by_name(expected_path.to_str().unwrap_or_default())
                .map_err(|_| "tool archive is missing its expected executable")?;
            if entry.is_dir() {
                return Err("tool archive executable is a directory".into());
            }
            copy_limited(&mut entry, &mut file)?;
            true
        }
        ToolArchiveFormat::TarGzip => {
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            let mut tar = tar::Archive::new(decoder);
            let mut found = false;
            for entry in tar
                .entries()
                .map_err(|error| format!("cannot read tar tool artifact: {error}"))?
                .take(MAXIMUM_TOOL_ARCHIVE_ENTRIES.saturating_add(1))
            {
                let mut entry = entry.map_err(|error| format!("cannot read tar entry: {error}"))?;
                let path = entry
                    .path()
                    .map_err(|error| format!("cannot read tar path: {error}"))?;
                if archive_entry_matches(&path, &expected_path) {
                    if !entry.header().entry_type().is_file() {
                        return Err("tool archive executable is not a regular file".into());
                    }
                    copy_limited(&mut entry, &mut file)?;
                    found = true;
                    break;
                }
            }
            found
        }
    };
    if !found {
        return Err("tool archive is missing its expected executable".into());
    }
    drop(file);
    prepare_executable(output)
}

pub(super) fn extract_tar_gzip_tree(
    bytes: &[u8],
    prefix: &str,
    output: &Path,
) -> Result<(), String> {
    let prefix = relative_catalog_path(prefix)?;
    if prefix.components().count() != 1 {
        return Err("runtime archive prefix must contain one directory".into());
    }
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut tar = tar::Archive::new(decoder);
    let mut remaining = MAXIMUM_TOOL_UNPACKED_BYTES;
    let mut count = 0usize;
    for entry in tar
        .entries()
        .map_err(|error| format!("cannot read runtime tar archive: {error}"))?
    {
        count = count.saturating_add(1);
        if count > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
            return Err("runtime archive has too many entries".into());
        }
        let mut entry = entry.map_err(|error| format!("cannot read runtime tar entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("cannot read runtime tar path: {error}"))?;
        let Ok(relative) = path.strip_prefix(&prefix) else {
            return Err("runtime archive contains an entry outside its expected prefix".into());
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let relative_text = relative
            .to_str()
            .ok_or("runtime archive path is not UTF-8")?;
        let relative = relative_catalog_path(relative_text)?;
        let destination = output.join(relative);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("cannot create runtime directory: {error}"))?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err("runtime archive contains a non-regular entry".into());
        }
        let parent = destination
            .parent()
            .ok_or("runtime archive file has no parent")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create runtime directory: {error}"))?;
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options
            .open(&destination)
            .map_err(|error| format!("cannot create runtime file: {error}"))?;
        copy_limited_with_budget(&mut entry, &mut file, &mut remaining)?;
    }
    Ok(())
}

/// Unpacks one directory of a ZIP into `output` under the same name. The rest is left out.
pub(super) fn extract_zip_tree(bytes: &[u8], prefix: &str, output: &Path) -> Result<(), String> {
    let prefix = relative_catalog_path(prefix)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("cannot open ZIP tool artifact: {error}"))?;
    if zip.len() > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
        return Err("tool archive has too many entries".into());
    }
    let mut remaining = MAXIMUM_TOOL_UNPACKED_BYTES;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("cannot read ZIP entry: {error}"))?;
        let name = entry.name().to_owned();
        if !Path::new(&name).starts_with(&prefix) {
            continue;
        }
        let destination = output.join(relative_catalog_path(name.trim_end_matches('/'))?);
        if entry.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("cannot create tool directory: {error}"))?;
            continue;
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err("tool archive contains a non-regular entry".into());
        }
        let parent = destination
            .parent()
            .ok_or("tool archive file has no parent")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create tool directory: {error}"))?;
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = entry.unix_mode().unwrap_or(0) & 0o111 != 0;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options
            .open(&destination)
            .map_err(|error| format!("cannot create tool file: {error}"))?;
        copy_limited_with_budget(&mut entry, &mut file, &mut remaining)?;
    }
    Ok(())
}

pub(super) fn replace_managed_directory(staging: &Path, destination: &Path) -> Result<(), String> {
    let backup = destination.with_file_name(format!(
        ".replace-{}-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("tool"),
        next_temp_suffix()
    ));
    let had_previous = if destination.exists() {
        let metadata = fs::symlink_metadata(destination)
            .map_err(|error| format!("cannot inspect existing tool: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("managed tool destination is not a real directory".into());
        }
        fs::rename(destination, &backup)
            .map_err(|error| format!("cannot stage previous tool for replacement: {error}"))?;
        true
    } else {
        false
    };
    if let Err(error) = fs::rename(staging, destination) {
        if had_previous {
            let _ = fs::rename(&backup, destination);
        }
        return Err(format!("cannot activate tool: {error}"));
    }
    if had_previous {
        fs::remove_dir_all(&backup).map_err(|error| {
            format!("tool activated but previous version cleanup failed: {error}")
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::tools::download::*;

    #[test]
    fn stalled_download_does_not_hold_installer_cancellation() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let (_progress, updates) = mpsc::sync_channel(1);
        let (cancel, cancellations) = mpsc::channel();
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            cancel.send(()).unwrap();
        });
        let start = std::time::Instant::now();
        let result = receive_download(&receiver, &updates, &cancellations, &mut |_, _, _| {});
        assert!(result.unwrap_err().contains("cancelled"));
        // A stalled download never returns, so any prompt return proves the point;
        // timers on loaded CI machines can run hundreds of milliseconds late.
        assert!(start.elapsed() < Duration::from_secs(2));
        cancellation.join().unwrap();
    }
}
