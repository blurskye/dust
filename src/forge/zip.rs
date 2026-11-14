//! Just enough ZIP to browse a CI artifact (F6): list the central directory and inflate one
//! entry (stored or deflated; ZIP64 sizes are read from the extra field). Artifacts arrive as
//! one archive per artifact on every forge.

use crate::forge::model::ArtifactFile;
use crate::forge::request::ForgeError;
use std::io::Read as _;

const END_OF_DIRECTORY: u32 = 0x0605_4b50;
const DIRECTORY_ENTRY: u32 = 0x0201_4b50;
const LOCAL_HEADER: u32 = 0x0403_4b50;
/// Entries larger than this are not inflated into memory for browsing.
pub const ENTRY_CAP: u64 = 16 * 1024 * 1024;

struct Entry {
    path: String,
    method: u16,
    compressed: u64,
    size: u64,
    offset: u64,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn bad(message: &str) -> ForgeError {
    ForgeError::BadResponse(format!("artifact archive: {message}"))
}

fn entries(bytes: &[u8]) -> Result<Vec<Entry>, ForgeError> {
    // The end-of-directory record sits in the last 22 + 65535 (comment) bytes.
    let floor = bytes.len().saturating_sub(22 + 65_535);
    let end = (floor..bytes.len().saturating_sub(21))
        .rev()
        .find(|at| u32_at(bytes, *at) == Some(END_OF_DIRECTORY))
        .ok_or_else(|| bad("not a zip file"))?;
    let count = u16_at(bytes, end + 10).ok_or_else(|| bad("truncated"))? as usize;
    let mut at = u32_at(bytes, end + 16).ok_or_else(|| bad("truncated"))? as usize;
    let mut out = Vec::with_capacity(count);
    while u32_at(bytes, at) == Some(DIRECTORY_ENTRY) {
        let field = |offset: usize| u32_at(bytes, at + offset).ok_or_else(|| bad("truncated"));
        let method = u16_at(bytes, at + 10).ok_or_else(|| bad("truncated"))?;
        let mut compressed = field(20)? as u64;
        let mut size = field(24)? as u64;
        let name_len = u16_at(bytes, at + 28).ok_or_else(|| bad("truncated"))? as usize;
        let extra_len = u16_at(bytes, at + 30).ok_or_else(|| bad("truncated"))? as usize;
        let comment_len = u16_at(bytes, at + 32).ok_or_else(|| bad("truncated"))? as usize;
        let mut offset = field(42)? as u64;
        let name = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| bad("truncated"))?;
        let extra = bytes
            .get(at + 46 + name_len..at + 46 + name_len + extra_len)
            .ok_or_else(|| bad("truncated"))?;
        // ZIP64: the 0x0001 extra field holds whichever of the three sizes overflowed.
        let mut cursor = 0;
        while cursor + 4 <= extra.len() {
            let id = u16_at(extra, cursor).unwrap_or_default();
            let len = u16_at(extra, cursor + 2).unwrap_or_default() as usize;
            if id == 1 {
                let mut value = cursor + 4;
                for slot in [&mut size, &mut compressed, &mut offset] {
                    if *slot == u32::MAX as u64
                        && let Some(wide) = u64_at(extra, value)
                    {
                        *slot = wide;
                        value += 8;
                    }
                }
            }
            cursor += 4 + len;
        }
        out.push(Entry {
            path: String::from_utf8_lossy(name).into_owned(),
            method,
            compressed,
            size,
            offset,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The files of an archive, directories left out.
pub fn list(bytes: &[u8]) -> Result<Vec<ArtifactFile>, ForgeError> {
    Ok(entries(bytes)?
        .into_iter()
        .filter(|entry| !entry.path.ends_with('/'))
        .map(|entry| ArtifactFile {
            path: entry.path,
            size: entry.size,
            compressed: entry.compressed,
        })
        .collect())
}

/// One entry's bytes.
pub fn read(bytes: &[u8], path: &str) -> Result<Vec<u8>, ForgeError> {
    let entry = entries(bytes)?
        .into_iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| ForgeError::NotFound(format!("{path} in the artifact")))?;
    if entry.size > ENTRY_CAP {
        return Err(ForgeError::Unsupported {
            capability: "artifacts_browse",
            reason: format!(
                "{path} is larger than {} MiB · download the artifact",
                ENTRY_CAP / (1024 * 1024)
            ),
        });
    }
    let at = entry.offset as usize;
    if u32_at(bytes, at) != Some(LOCAL_HEADER) {
        return Err(bad("broken local header"));
    }
    let name_len = u16_at(bytes, at + 26).ok_or_else(|| bad("truncated"))? as usize;
    let extra_len = u16_at(bytes, at + 28).ok_or_else(|| bad("truncated"))? as usize;
    let start = at + 30 + name_len + extra_len;
    let data = bytes
        .get(start..start + entry.compressed as usize)
        .ok_or_else(|| bad("truncated entry"))?;
    match entry.method {
        0 => Ok(data.to_vec()),
        8 => {
            let mut out = Vec::with_capacity(entry.size as usize);
            flate2::read::DeflateDecoder::new(data)
                .take(ENTRY_CAP + 1)
                .read_to_end(&mut out)
                .map_err(|error| bad(&error.to_string()))?;
            Ok(out)
        }
        method => Err(ForgeError::Unsupported {
            capability: "artifacts_browse",
            reason: format!("{path} uses compression method {method}"),
        }),
    }
}

/// A small archive for tests: `(path, contents)` stored or deflated.
#[cfg(test)]
pub fn build(files: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    use std::io::Write as _;
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (path, contents) in files {
        let data = if deflate {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(contents).unwrap();
            encoder.finish().unwrap()
        } else {
            contents.to_vec()
        };
        let method: u16 = if deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend(LOCAL_HEADER.to_le_bytes());
        out.extend([20, 0, 0, 0]);
        out.extend(method.to_le_bytes());
        out.extend([0; 8]);
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((contents.len() as u32).to_le_bytes());
        out.extend((path.len() as u16).to_le_bytes());
        out.extend([0, 0]);
        out.extend(path.as_bytes());
        out.extend(&data);
        directory.extend(DIRECTORY_ENTRY.to_le_bytes());
        directory.extend([20, 0, 20, 0, 0, 0]);
        directory.extend(method.to_le_bytes());
        directory.extend([0; 8]);
        directory.extend((data.len() as u32).to_le_bytes());
        directory.extend((contents.len() as u32).to_le_bytes());
        directory.extend((path.len() as u16).to_le_bytes());
        directory.extend([0; 12]);
        directory.extend(offset.to_le_bytes());
        directory.extend(path.as_bytes());
    }
    let start = out.len() as u32;
    let size = directory.len() as u32;
    out.extend(directory);
    out.extend(END_OF_DIRECTORY.to_le_bytes());
    out.extend([0; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend([0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_stored_and_deflated_entries() {
        for deflate in [false, true] {
            let archive = build(
                &[
                    ("coverage/index.html", b"<html>ok</html>"),
                    ("lcov.info", b"TN:\nSF:src/lib.rs\n"),
                ],
                deflate,
            );
            let files = list(&archive).unwrap();
            assert_eq!(
                files
                    .iter()
                    .map(|file| (file.path.as_str(), file.size))
                    .collect::<Vec<_>>(),
                [("coverage/index.html", 15), ("lcov.info", 18)]
            );
            assert_eq!(
                read(&archive, "lcov.info").unwrap(),
                b"TN:\nSF:src/lib.rs\n"
            );
            assert!(matches!(
                read(&archive, "missing"),
                Err(ForgeError::NotFound(_))
            ));
        }
        assert!(list(b"not a zip").is_err());
    }
}
