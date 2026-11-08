//! Package the example core modules as installable components using the test toolchain.

use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: plugin_pack OUTPUT [CORE_WASM MANIFEST]")?,
    );
    if let Some(core) = arguments.next() {
        let manifest = arguments
            .next()
            .ok_or("MANIFEST is required after CORE_WASM")?;
        if arguments.next().is_some() {
            return Err("usage: plugin_pack OUTPUT [CORE_WASM MANIFEST]".into());
        }
        return pack(&output, &PathBuf::from(core), &PathBuf::from(manifest));
    }
    fs::create_dir_all(&output)?;
    for name in [
        "autopairs",
        "surround",
        "oil",
        "completion",
        "wrappers",
        "mode",
    ] {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("examples/plugins")
            .join(name);
        let core = fs::read(source.join("guest.core.wasm"))?;
        let component = wit_component::ComponentEncoder::default()
            .module(&core)?
            .validate(true)
            .encode()?;
        let digest = format!("{:x}", Sha256::digest(&component));
        let owners: &[&str] = if name == "wrappers" {
            &["wrappersa", "wrappersb"]
        } else {
            &[name]
        };
        for owner in owners {
            let directory = output.join(owner);
            fs::create_dir(&directory)?;
            fs::write(directory.join("guest.wasm"), &component)?;
            let manifest = fs::read_to_string(source.join("manifest.toml"))?
                .replace(&"0".repeat(64), &digest)
                .replacen(&format!("id = \"{name}\""), &format!("id = \"{owner}\""), 1);
            fs::write(directory.join("manifest.toml"), manifest)?;
        }
    }
    Ok(())
}

fn pack(
    output: &std::path::Path,
    core: &std::path::Path,
    manifest: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    let component = wit_component::ComponentEncoder::default()
        .module(&fs::read(core)?)?
        .validate(true)
        .encode()?;
    let mut manifest: dust::plugins::PluginManifest =
        toml::from_str(&fs::read_to_string(manifest)?)?;
    manifest.component_sha256 = format!("{:x}", Sha256::digest(&component));
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    fs::write(output.join("guest.wasm"), component)?;
    fs::write(
        output.join("manifest.toml"),
        toml::to_string_pretty(&manifest)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_a_custom_guest_with_its_component_digest_without_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/completion");
        let output = root.path().join("packaged/words");
        pack(
            &output,
            &source.join("guest.core.wasm"),
            &source.join("manifest.toml"),
        )
        .unwrap();
        let bytes = fs::read(output.join("guest.wasm")).unwrap();
        let manifest: dust::plugins::PluginManifest =
            toml::from_str(&fs::read_to_string(output.join("manifest.toml")).unwrap()).unwrap();
        assert_eq!(
            manifest.component_sha256,
            format!("{:x}", Sha256::digest(bytes))
        );
        assert_eq!(manifest.id, "completion");
        assert!(
            pack(
                &output,
                &source.join("guest.core.wasm"),
                &source.join("manifest.toml")
            )
            .is_err()
        );
    }
}
