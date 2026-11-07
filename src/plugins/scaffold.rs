//! Create a standalone Rust guest from the maintained buffer-word completion example.

use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScaffoldError {
    #[error(
        "plugin names must start with a lowercase letter and contain only lowercase letters and digits (at most 64 characters)"
    )]
    InvalidName,
    #[error("could not create plugin: {0}")]
    Io(#[from] std::io::Error),
}

/// Create a new directory only; never replace an existing project or file.
pub fn create(parent: &Path, name: &str) -> Result<PathBuf, ScaffoldError> {
    if !super::index::valid_plugin_segment(name) {
        return Err(ScaffoldError::InvalidName);
    }
    let directory = parent.join(name);
    fs::create_dir(&directory)?;
    fs::create_dir(directory.join("src"))?;
    fs::create_dir(directory.join("wit"))?;
    let files = [
        (
            "Cargo.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/plugins/completion/Cargo.toml"
            ))
            .replace("dust-example-completion", &format!("dust-plugin-{name}")),
        ),
        (
            "Cargo.lock",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/plugins/completion/Cargo.lock"
            ))
            .replace("dust-example-completion", &format!("dust-plugin-{name}")),
        ),
        (
            "src/lib.rs",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/plugins/completion/src/lib.rs"
            ))
            .replace("../../../wit", "wit"),
        ),
        (
            "wit/dust-plugin.wit",
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/wit/dust-plugin.wit")).to_owned(),
        ),
        (
            "manifest.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/plugins/completion/manifest.toml"
            ))
            .replace("id = \"completion\"", &format!("id = \"{name}\""))
            .replace("completion:", &format!("{name}:")),
        ),
        (
            "README.md",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/plugin-template/README.md"
            ))
            .replace("{{name}}", name),
        ),
        (".gitignore", "/target/\n/packaged/\n".to_owned()),
    ];
    for (path, contents) in files {
        fs::write(directory.join(path), contents)?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_matches_the_buildable_completion_example() {
        let root = tempfile::tempdir().unwrap();
        let directory = create(root.path(), "mywords").unwrap();
        let cargo: toml::Value =
            toml::from_str(&fs::read_to_string(directory.join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            cargo["package"]["name"].as_str(),
            Some("dust-plugin-mywords")
        );
        assert_eq!(cargo["lib"]["crate-type"][0].as_str(), Some("cdylib"));
        let manifest: crate::plugins::PluginManifest =
            toml::from_str(&fs::read_to_string(directory.join("manifest.toml")).unwrap()).unwrap();
        assert_eq!(manifest.id, "mywords");
        assert!(
            manifest
                .capabilities
                .contains(&crate::plugins::PluginCapability::Completion)
        );
        assert!(manifest.commands.contains_key("complete"));
        assert_eq!(
            fs::read_to_string(directory.join("src/lib.rs")).unwrap(),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/plugins/completion/src/lib.rs"
            ))
            .replace("../../../wit", "wit")
        );
        assert_eq!(
            fs::read_to_string(directory.join("wit/dust-plugin.wit")).unwrap(),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/wit/dust-plugin.wit"))
        );
    }

    #[test]
    fn scaffold_rejects_paths_and_preserves_existing_projects() {
        let root = tempfile::tempdir().unwrap();
        for name in ["", "../escape", "foo/bar", "Upper", "two-words", "1word"] {
            assert!(matches!(
                create(root.path(), name),
                Err(ScaffoldError::InvalidName)
            ));
        }
        create(root.path(), "words").unwrap();
        fs::write(root.path().join("words/src/lib.rs"), "user draft").unwrap();
        assert!(create(root.path(), "words").is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("words/src/lib.rs")).unwrap(),
            "user draft"
        );
    }
}
