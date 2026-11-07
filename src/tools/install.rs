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

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolInstallerError {
    #[error("tool installer stopped")]
    Stopped,
    #[error("this tool has no portable bundled installer")]
    Unavailable,
}
