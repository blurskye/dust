//! Package the example core modules as installable components using the test toolchain.

use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: plugin_pack OUTPUT")?,
    );
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
