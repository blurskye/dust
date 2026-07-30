use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArchitectureAudit {
    pub files: usize,
    pub bytes: u64,
    pub rules: Vec<&'static str>,
}
pub fn audit_source(root: &Path) -> Result<ArchitectureAudit, ArchitectureError> {
    let source = root.join("src");
    let mut paths = Vec::new();
    collect(&source, &mut paths)?;
    paths.sort();
    if paths.len() > 10_000 {
        return Err(ArchitectureError::Ceiling);
    }
    let mut bytes = 0u64;
    for path in &paths {
        let metadata = fs::symlink_metadata(path).map_err(ArchitectureError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 4 * 1024 * 1024
        {
            return Err(ArchitectureError::Invalid(path.clone()));
        }
        bytes = bytes.saturating_add(metadata.len());
        if bytes > 64 * 1024 * 1024 {
            return Err(ArchitectureError::Ceiling);
        }
        let source = fs::read_to_string(path).map_err(ArchitectureError::Io)?;
        if path.file_name().and_then(|name| name.to_str()) == Some("architecture.rs") {
            continue;
        }
        for (pattern, rule) in [
            ("Command::new(\"sh\")", "implicit shell execution"),
            ("Command::new(\"bash\")", "implicit shell execution"),
            ("Command::new(\"git\")", "Git CLI package fetching"),
            ("wasmtime_wasi", "ambient WASI linkage"),
            ("libloading::", "native plugin/parser dynamic loading"),
        ] {
            if source.contains(pattern) {
                return Err(ArchitectureError::Violation {
                    path: path.clone(),
                    rule,
                });
            }
        }
    }
    let interactive = fs::read_to_string(source.join("application/interactive.rs"))
        .map_err(ArchitectureError::Io)?;
    let languages =
        fs::read_to_string(source.join("languages/mod.rs")).map_err(ArchitectureError::Io)?;
    let debugging =
        fs::read_to_string(source.join("debugging/mod.rs")).map_err(ArchitectureError::Io)?;
    let packages =
        fs::read_to_string(source.join("packages/mod.rs")).map_err(ArchitectureError::Io)?;
    let registry =
        fs::read_to_string(source.join("packages/registry.rs")).map_err(ArchitectureError::Io)?;
    if !interactive.contains("PendingExecutionPreflight::ToolInstall")
        || !interactive.contains("PendingExecutionPreflight::Lsp")
        || !interactive.contains("PendingExecutionPreflight::DapToolAdapter")
        || !languages.contains("start_session_absolute_with_policy")
        || !languages.contains("PreflightRequired")
        || debugging.contains("discover_executable")
        || !packages.contains("install_absolute")
        || !packages.contains("env_clear()")
        || !registry.contains("https_only(true)")
        || !registry.contains("Policy::none()")
        || !registry.contains("OFFICIAL_REGISTRY")
        || !registry.contains("take(maximum as u64 + 1)")
    {
        return Err(ArchitectureError::Violation {
            path: source.clone(),
            rule: "process launch paths must retain absolute execution-preflight ownership",
        });
    }
    if let Err(diagnostic) = crate::syntax::validate_builtin_queries() {
        return Err(ArchitectureError::Syntax(diagnostic));
    }
    let plugin =
        fs::read_to_string(source.join("plugins/mod.rs")).map_err(ArchitectureError::Io)?;
    if !plugin.contains("wasmtime::component::bindgen!")
        || !plugin.contains("ComponentHostState")
        || !plugin.contains("PluginLoadId")
        || !plugin.contains("unload_generation")
        || !plugin.contains("pending_disables")
        || !plugin.contains("AlreadyActive")
        || !plugin.contains("maximum_loads")
        || !plugin.contains("fail_stopped_loads")
    {
        return Err(ArchitectureError::Violation {
            path: source.join("plugins/mod.rs"),
            rule: "plugins must use generated Component Model host bindings",
        });
    }
    Ok(ArchitectureAudit {
        files: paths.len(),
        bytes,
        rules: vec![
            "no implicit shell",
            "no Git CLI fetch",
            "no ambient WASI",
            "no native dynamic plugin/parser loading",
            "generated Component host bindings",
            "absolute execution-preflight ownership",
            "official bounded no-redirect HTTPS registry",
            "compiled pinned built-in syntax registry",
            "generation-owned Component loads",
        ],
    })
}
fn collect(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), ArchitectureError> {
    for entry in fs::read_dir(directory).map_err(ArchitectureError::Io)? {
        let entry = entry.map_err(ArchitectureError::Io)?;
        let kind = entry.file_type().map_err(ArchitectureError::Io)?;
        if kind.is_symlink() {
            return Err(ArchitectureError::Invalid(entry.path()));
        }
        if kind.is_dir() {
            collect(&entry.path(), out)?
        } else if kind.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
        {
            out.push(entry.path())
        }
    }
    Ok(())
}
#[derive(Debug, Error)]
pub enum ArchitectureError {
    #[error("architecture audit I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("invalid architecture source path {0}")]
    Invalid(PathBuf),
    #[error("architecture audit ceiling exceeded")]
    Ceiling,
    #[error("built-in syntax registry is invalid: {0}")]
    Syntax(String),
    #[error("architecture violation in {path}: {rule}")]
    Violation { path: PathBuf, rule: &'static str },
}
