//! Tool installation records and installed executables.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use thiserror::Error;

use crate::tools::catalog::NixCompatibilityRuntime;

use crate::tools::catalog::ToolDefinition;

use crate::tools::catalog::ToolInstall;

use crate::tools::catalog::write_managed_tool_version;

use crate::tools::compatibility::patch_nix_compatibility_tree;

use crate::tools::download::ToolProgressReporter;

use crate::tools::download::download_checked;

use crate::tools::download::extract_one;

use crate::tools::download::extract_zip_tree;

use crate::tools::download::prepare_executable;

use crate::tools::download::replace_managed_directory;

use crate::tools::install_prepare::ensure_private_directory;

use crate::tools::install_prepare::validate_tool_id;

use crate::tools::package_lock::next_temp_suffix;

use crate::tools::runtimes::install_javascript_package;

use crate::tools::runtimes::install_rustup_component;

use crate::tools::runtimes::install_uv_tool;

use crate::tools::runtimes::managed_runtime;

use crate::tools::runtimes::probe_downloaded_tool;

use crate::tools::runtimes::run_managed;

pub(super) fn install_tool(
    definition: &ToolDefinition,
    root: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    validate_tool_id(&definition.id)?;
    if let Some(error) = crate::tools::catalog::installation_platform_error(definition) {
        return Err(error);
    }
    ensure_private_directory(root)?;
    let install = definition
        .install
        .as_ref()
        .ok_or("this tool has no portable bundled installer")?;
    if let ToolInstall::UvTool { packages } = install {
        return install_uv_tool(
            definition,
            root,
            packages,
            cancellations,
            report,
            compatibility,
        );
    }
    if let ToolInstall::RustupComponent { component } = install {
        return install_rustup_component(
            definition,
            root,
            component,
            cancellations,
            report,
            compatibility,
        );
    }
    let staging = root.join(format!(".tool-{}-{}", definition.id, next_temp_suffix()));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage tool installation: {error}"))?;
    let result = (|| {
        let bin = staging.join("bin");
        fs::create_dir(&bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        let output = bin.join(&definition.executable);
        match install {
            ToolInstall::Archive {
                url,
                sha256,
                archive,
                executable_path,
                tree,
            } => {
                let bytes = download_checked(url, sha256, cancellations, report)?;
                report("extracting", 0, None);
                if let Some(tree) = tree {
                    extract_zip_tree(&bytes, tree, &staging)?;
                    // The tool finds its libraries from its real path, which a link keeps.
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(Path::new("..").join(executable_path), &output)
                        .map_err(|error| format!("cannot link installed tool: {error}"))?;
                    #[cfg(windows)]
                    return Err(
                        "this archive needs a Unix symlink layout; install the tool on PATH".into(),
                    );
                } else {
                    extract_one(&bytes, *archive, executable_path, &output)?;
                }
            }
            ToolInstall::JavascriptArchive {
                url,
                sha256,
                prefix,
                entrypoint,
            } => {
                let bytes = download_checked(url, sha256, cancellations, report)?;
                let runtime = compatibility
                    .and_then(|runtime| runtime.executable("node"))
                    .or_else(|| crate::tools::discover_executable("node"));
                let runtime = match runtime {
                    Some(runtime) => runtime,
                    None => crate::tools::runtimes::managed_bun_runtime(
                        root,
                        cancellations,
                        report,
                        compatibility,
                    )?,
                };
                report("extracting JavaScript adapter", 0, None);
                prepare_javascript_archive(
                    &bytes, prefix, entrypoint, &runtime, &staging, &output,
                )?;
            }
            ToolInstall::NpmPackage { packages } => {
                install_javascript_package(
                    root,
                    packages,
                    &staging,
                    &output,
                    cancellations,
                    report,
                    compatibility,
                )?;
            }
            ToolInstall::GoPackage { package } => {
                let go = managed_runtime(root, "go", cancellations, report, compatibility)?;
                let arguments = vec!["install".into(), package.clone()];
                let environment = BTreeMap::from([
                    ("HOME".into(), staging.as_os_str().to_owned()),
                    ("GOBIN".into(), bin.into_os_string()),
                    ("GOPATH".into(), staging.join("gopath").into_os_string()),
                    ("GOCACHE".into(), staging.join("gocache").into_os_string()),
                    ("GOPROXY".into(), "https://proxy.golang.org,direct".into()),
                    ("GOSUMDB".into(), "sum.golang.org".into()),
                ]);
                report("building Go tool", 0, None);
                run_managed(
                    &go,
                    &arguments,
                    &staging,
                    &environment,
                    cancellations,
                    compatibility,
                )?;
                prepare_executable(&output)?;
            }
            ToolInstall::UvTool { .. } | ToolInstall::RustupComponent { .. } => {
                unreachable!("handled before staging")
            }
        }
        if let Some(compatibility) = compatibility {
            report("applying Nix compatibility", 0, None);
            patch_nix_compatibility_tree(&staging, compatibility, cancellations)?;
        }
        if !matches!(install, ToolInstall::NpmPackage { .. }) {
            report("verifying", 0, None);
            probe_downloaded_tool(
                &output,
                &definition.probe_args,
                &staging,
                cancellations,
                compatibility,
            )?;
        }
        write_managed_tool_version(&staging, definition)?;
        let destination = root.join(&definition.id);
        replace_managed_directory(&staging, &destination)?;
        Ok(Some(destination.join("bin").join(&definition.executable)))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn prepare_javascript_archive(
    bytes: &[u8],
    prefix: &str,
    entrypoint: &str,
    runtime: &Path,
    staging: &Path,
    output: &Path,
) -> Result<(), String> {
    let entrypoint = crate::tools::install_prepare::relative_catalog_path(entrypoint)?;
    if cfg!(windows) {
        return Err("this adapter's launcher is a POSIX shell script; install it on PATH".into());
    }
    crate::tools::download::extract_tar_gzip_tree(bytes, prefix, staging)?;
    if !staging.join(&entrypoint).is_file() {
        return Err("JavaScript archive does not contain its entrypoint".into());
    }
    // Quote paths literally; a tool-store path may contain spaces or shell metacharacters.
    let quote = |path: &Path| -> Result<String, String> {
        let path = path
            .to_str()
            .ok_or("JavaScript runtime path is not UTF-8")?;
        Ok(format!("'{}'", path.replace('\'', "'\\''")))
    };
    let script = format!(
        "#!/bin/sh\nexec {} \"$(dirname \"$0\")/../\"{} \"$@\"\n",
        quote(runtime)?,
        quote(&entrypoint)?
    );
    fs::write(output, script)
        .map_err(|error| format!("cannot write JavaScript launcher: {error}"))?;
    prepare_executable(output)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn javascript_archive_launcher_keeps_support_files_and_literal_paths() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("adapter's files");
        fs::create_dir_all(staging.join("bin")).unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (name, data) in [
            ("adapter/src/main.js", "entrypoint"),
            ("adapter/src/support.js", "support"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive
                .append_data(&mut header, name, data.as_bytes())
                .unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let runtime = root.path().join("runtime's shell");
        fs::File::create(&runtime)
            .unwrap()
            .write_all(b"#!/bin/sh\nprintf '%s\\n' \"$@\"\n")
            .unwrap();
        prepare_executable(&runtime).unwrap();
        let output = staging.join("bin/adapter");
        prepare_javascript_archive(
            &bytes,
            "adapter",
            "src/main.js",
            &runtime,
            &staging,
            &output,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(staging.join("src/support.js")).unwrap(),
            "support"
        );
        let result = std::process::Command::new(&output)
            .arg("literal argument")
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            format!(
                "{}/bin/../src/main.js\nliteral argument\n",
                staging.display()
            )
        );
        assert!(
            prepare_javascript_archive(&bytes, "adapter", "../escape", &runtime, &staging, &output)
                .is_err()
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolInstallerError {
    #[error("tool installer stopped")]
    Stopped,
    #[error("this tool has no portable bundled installer")]
    Unavailable,
}
