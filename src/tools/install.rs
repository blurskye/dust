use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc::Receiver},
};

use thiserror::Error;

use super::{
    catalog::{NixCompatibilityRuntime, ToolDefinition, ToolInstall, write_managed_tool_version},
    compatibility::patch_nix_compatibility_tree,
    download::{
        ToolProgressReporter, download_checked, extract_one, extract_zip_tree, prepare_executable,
        replace_managed_directory,
    },
    install_prepare::{ensure_private_directory, validate_tool_id},
    package_lock::NEXT_PACKAGE_TEMP,
    runtimes::{
        install_javascript_package, install_rustup_component, install_uv_tool, managed_runtime,
        probe_downloaded_tool, run_managed,
    },
};

pub(super) fn install_tool(
    definition: &ToolDefinition,
    root: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    validate_tool_id(&definition.id)?;
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
    let staging = root.join(format!(
        ".tool-{}-{}",
        definition.id,
        NEXT_PACKAGE_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
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
                let bytes = download_checked(url, sha256, report)?;
                report("extracting", 0, None);
                if let Some(tree) = tree {
                    extract_zip_tree(&bytes, tree, &staging)?;
                    // The tool finds its libraries from its real path, which a link keeps.
                    std::os::unix::fs::symlink(Path::new("..").join(executable_path), &output)
                        .map_err(|error| format!("cannot link installed tool: {error}"))?;
                } else {
                    extract_one(&bytes, *archive, executable_path, &output)?;
                }
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
                    ("HOME".into(), staging.display().to_string()),
                    ("GOBIN".into(), bin.display().to_string()),
                    (
                        "GOPATH".into(),
                        staging.join("gopath").display().to_string(),
                    ),
                    (
                        "GOCACHE".into(),
                        staging.join("gocache").display().to_string(),
                    ),
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
            if !matches!(install, ToolInstall::NpmPackage { .. }) {
                report("applying Nix compatibility", 0, None);
                patch_nix_compatibility_tree(&staging, compatibility, cancellations)?;
            }
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

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolInstallerError {
    #[error("another tool installation is already running")]
    Busy,
    #[error("tool installer stopped")]
    Stopped,
    #[error("this tool has no portable bundled installer")]
    Unavailable,
}
