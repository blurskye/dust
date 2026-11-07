//! Host compatibility checks and tool launch environments.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::sync::mpsc::Receiver;

use crate::tools::catalog::NixCompatibilityRuntime;

use crate::tools::catalog::ToolArchiveFormat;

use crate::tools::install_prepare::BootstrapArtifact;

use crate::tools::install_prepare::MAXIMUM_TOOL_ARCHIVE_ENTRIES;

use crate::tools::install_prepare::MAXIMUM_TOOL_UNPACKED_BYTES;

use crate::tools::runtimes::run_managed;

pub(super) const MANAGED_RUNTIME_MARKER: &str = ".dust-runtime-sha256";
pub(super) const MAXIMUM_ELF_INTERPRETER_BYTES: u64 = 4096;
pub(super) const MAXIMUM_ELF_PROGRAM_HEADERS: u16 = 256;

pub(super) fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

pub(super) fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

pub(super) fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// Reads PT_INTERP without executing an untrusted artifact or loading the whole file.
pub(super) fn elf_interpreter(path: &Path) -> Result<Option<String>, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("cannot inspect ELF artifact: {error}"))?;
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open ELF artifact: {error}"))?;
    let mut header = [0_u8; 64];
    let read = file
        .read(&mut header)
        .map_err(|error| format!("cannot read ELF header: {error}"))?;
    if read < 4 || &header[..4] != b"\x7fELF" {
        return Ok(None);
    }
    if read < 52 || header[5] != 1 {
        return Err("managed ELF uses an unsupported class or byte order".into());
    }
    let (program_offset, entry_size, entry_count, offset_field, size_field) = match header[4] {
        1 => (
            u64::from(read_u32_le(&header, 28).ok_or("invalid ELF32 header")?),
            read_u16_le(&header, 42).ok_or("invalid ELF32 header")?,
            read_u16_le(&header, 44).ok_or("invalid ELF32 header")?,
            4_usize,
            16_usize,
        ),
        2 if read >= 64 => (
            read_u64_le(&header, 32).ok_or("invalid ELF64 header")?,
            read_u16_le(&header, 54).ok_or("invalid ELF64 header")?,
            read_u16_le(&header, 56).ok_or("invalid ELF64 header")?,
            8_usize,
            32_usize,
        ),
        _ => return Err("managed ELF uses an unsupported class or truncated header".into()),
    };
    if entry_count > MAXIMUM_ELF_PROGRAM_HEADERS
        || entry_size < u16::try_from(size_field + 8).unwrap_or(u16::MAX)
    {
        return Err("managed ELF program-header table is invalid".into());
    }
    let table_bytes = u64::from(entry_size)
        .checked_mul(u64::from(entry_count))
        .and_then(|size| program_offset.checked_add(size))
        .ok_or("managed ELF program-header table overflows")?;
    if table_bytes > metadata.len() {
        return Err("managed ELF program-header table exceeds the file".into());
    }
    let mut entry = vec![0_u8; usize::from(entry_size)];
    for index in 0..entry_count {
        let position = program_offset + u64::from(index) * u64::from(entry_size);
        file.seek(SeekFrom::Start(position))
            .and_then(|_| file.read_exact(&mut entry))
            .map_err(|error| format!("cannot read ELF program header: {error}"))?;
        if read_u32_le(&entry, 0) != Some(3) {
            continue;
        }
        let (offset, size) = if header[4] == 1 {
            (
                u64::from(read_u32_le(&entry, offset_field).ok_or("invalid ELF interpreter")?),
                u64::from(read_u32_le(&entry, size_field).ok_or("invalid ELF interpreter")?),
            )
        } else {
            (
                read_u64_le(&entry, offset_field).ok_or("invalid ELF interpreter")?,
                read_u64_le(&entry, size_field).ok_or("invalid ELF interpreter")?,
            )
        };
        if size == 0
            || size > MAXIMUM_ELF_INTERPRETER_BYTES
            || offset
                .checked_add(size)
                .is_none_or(|end| end > metadata.len())
        {
            return Err("managed ELF interpreter is invalid".into());
        }
        let mut bytes = vec![0_u8; usize::try_from(size).map_err(|_| "invalid ELF interpreter")?];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|error| format!("cannot read ELF interpreter: {error}"))?;
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        let interpreter =
            String::from_utf8(bytes).map_err(|_| "managed ELF interpreter is not UTF-8")?;
        if interpreter.is_empty() || interpreter.chars().any(char::is_control) {
            return Err("managed ELF interpreter is invalid".into());
        }
        return Ok(Some(interpreter));
    }
    Ok(None)
}

pub(super) fn patch_nix_compatible_executable(
    path: &Path,
    runtime: &NixCompatibilityRuntime,
    cwd: &Path,
    cancellations: &Receiver<()>,
) -> Result<(), String> {
    let Some(interpreter) = elf_interpreter(path)? else {
        return Ok(());
    };
    if Path::new(&interpreter) == runtime.loader {
        return Ok(());
    }
    if interpreter.contains("ld-musl") {
        return Err(format!(
            "{} is dynamically linked against musl; Dust requires the verified GNU artifact on NixOS",
            path.display()
        ));
    }
    if interpreter.starts_with("/nix/store/") {
        return Err(format!(
            "{} references a Nix interpreter outside Dust's compatibility profile",
            path.display()
        ));
    }
    if !interpreter.contains("ld-linux") && !interpreter.ends_with("/ld.so") {
        return Err(format!(
            "{} uses unsupported ELF interpreter {interpreter}",
            path.display()
        ));
    }
    let arguments = vec![
        "--set-interpreter".into(),
        runtime.loader.display().to_string(),
        "--add-rpath".into(),
        runtime.library_path.clone(),
        path.display().to_string(),
    ];
    // A tool installed in place, like a uv tool, can be run by a probe for a moment while it
    // is patched; the kernel then refuses to open it for writing ("Text file busy").
    let mut attempt = 0;
    loop {
        attempt += 1;
        match run_managed(
            &runtime.patchelf,
            &arguments,
            cwd,
            &BTreeMap::new(),
            cancellations,
            None,
        ) {
            Err(error) if attempt < 5 && error.contains("Text file busy") => {
                std::thread::sleep(std::time::Duration::from_millis(200 * attempt));
            }
            result => break result,
        }
    }
    .map_err(|error| {
        format!(
            "cannot apply Dust Nix compatibility to {}: {error}",
            path.display()
        )
    })?;
    if elf_interpreter(path)?.as_deref() != Some(runtime.loader.to_string_lossy().as_ref()) {
        return Err(format!(
            "Dust Nix compatibility did not update {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn patch_nix_compatibility_tree(
    root: &Path,
    // Only Unix executables are patched.
    #[cfg_attr(not(unix), allow(unused_variables))] runtime: &NixCompatibilityRuntime,
    #[cfg_attr(not(unix), allow(unused_variables))] cancellations: &Receiver<()>,
) -> Result<(), String> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = 0usize;
    let mut bytes = 0u64;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot inspect managed tool tree: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("cannot inspect managed tool entry: {error}"))?;
            entries = entries.saturating_add(1);
            if entries > MAXIMUM_TOOL_ARCHIVE_ENTRIES {
                return Err("managed tool tree has too many entries".into());
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("cannot inspect managed tool entry: {error}"))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_dir() {
                stack.push(entry.path());
            } else if metadata.file_type().is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > MAXIMUM_TOOL_UNPACKED_BYTES {
                    return Err("managed tool tree exceeds its byte limit".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 != 0 {
                        patch_nix_compatible_executable(
                            &entry.path(),
                            runtime,
                            root,
                            cancellations,
                        )?;
                    }
                }
            } else {
                return Err("managed tool tree contains a special file".into());
            }
        }
    }
    Ok(())
}

pub(super) fn runtime_artifact(name: &str) -> Result<BootstrapArtifact, String> {
    if !cfg!(target_os = "linux") {
        return Err(format!(
            "Dust has no managed {name} download for {}; install it on PATH",
            std::env::consts::OS
        ));
    }
    match (name, std::env::consts::ARCH) {
        ("uv", "x86_64") => Ok(BootstrapArtifact {
            url: "https://github.com/astral-sh/uv/releases/download/0.12.5/uv-x86_64-unknown-linux-musl.tar.gz",
            sha256: "a4742988791c9aeae68c78150d6cba762062ad2a47e53738c2779d2b596bfcdb",
            archive: ToolArchiveFormat::TarGzip,
            executable_path: "uv-x86_64-unknown-linux-musl/uv",
        }),
        ("bun", "x86_64") => Ok(BootstrapArtifact {
            url: "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-linux-x64-baseline.zip",
            sha256: "a063908ae08b7852ca10939bbdc6ceed3ddabce8fb9402dce83d65d73b36e6c7",
            archive: ToolArchiveFormat::Zip,
            executable_path: "bun-linux-x64-baseline/bun",
        }),
        ("go", "x86_64") => Ok(BootstrapArtifact {
            url: "https://go.dev/dl/go1.24.12.linux-amd64.tar.gz",
            sha256: "bddf8e653c82429aea7aec2520774e79925d4bb929fe20e67ecc00dd5af44c50",
            archive: ToolArchiveFormat::TarGzip,
            executable_path: "go/bin/go",
        }),
        ("rustup", "x86_64") => Ok(BootstrapArtifact {
            url: "https://static.rust-lang.org/rustup/archive/1.28.0/x86_64-unknown-linux-gnu/rustup-init",
            sha256: "c8d03f559a2335693379e1d3eaee76622b2a6580807e63bcd61faea709b9f664",
            archive: ToolArchiveFormat::Raw,
            executable_path: "rustup-init",
        }),
        _ => Err(format!(
            "Dust has no managed {name} runtime for Linux {}",
            std::env::consts::ARCH
        )),
    }
}
