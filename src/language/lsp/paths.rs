//! LSP document paths and URI conversion.

use std::path::Path;
use std::path::PathBuf;

#[must_use]
pub fn language_id_for_path(path: &Path) -> Option<&'static str> {
    let filename = path.file_name().and_then(|name| name.to_str());
    if filename.is_some_and(|name| name == "Dockerfile" || name.starts_with("Dockerfile.")) {
        return Some("dockerfile");
    }
    match filename {
        Some("go.mod") => return Some("gomod"),
        Some("go.work") => return Some("gowork"),
        _ => {}
    }
    let extension = path.extension().and_then(|extension| extension.to_str());
    if matches!(extension, Some("C" | "H")) {
        return Some("cpp");
    }
    let extension = extension.map(str::to_ascii_lowercase);
    let builtin = match extension.as_deref().unwrap_or_default() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "py" | "pyi" | "pyw" => Some("python"),
        "go" => Some("go"),
        "gotmpl" => Some("gotmpl"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
        "json" => Some("json"),
        "jsonc" => Some("jsonc"),
        "toml" => Some("toml"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "sass" => Some("sass"),
        "less" => Some("less"),
        "yaml" | "yml" => Some("yaml"),
        "md" | "markdown" => Some("markdown"),
        "mdx" => Some("markdown.mdx"),
        "sh" | "bash" | "zsh" => Some("shellscript"),
        "fish" => Some("fish"),
        "lua" => Some("lua"),
        "php" => Some("php"),
        "svelte" => Some("svelte"),
        "vue" => Some("vue"),
        "astro" => Some("astro"),
        "tf" => Some("terraform"),
        "tfvars" => Some("terraform-vars"),
        "asm" => Some("asm"),
        _ => None,
    };
    builtin.or_else(|| crate::tools::mason_filetype_for_path(path))
}

#[must_use]
pub fn file_uri(path: &Path) -> String {
    let mut encoded = String::new();
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(windows)]
    let path_text = {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = if let Some(unc) = text.strip_prefix("//?/UNC/") {
            format!("//{unc}")
        } else {
            text.strip_prefix("//?/").unwrap_or(&text).to_owned()
        };
        if text.as_bytes().get(1) == Some(&b':') {
            format!("/{text}")
        } else {
            text
        }
    };
    #[cfg(windows)]
    let bytes = path_text.as_bytes();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
            || (cfg!(windows) && byte == b':')
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    #[cfg(windows)]
    if encoded.starts_with("//") {
        return format!("file:{encoded}");
    }
    format!("file://{encoded}")
}

#[must_use]
pub fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let source = uri.strip_prefix("file://")?;
    let (authority, source) = source.split_at(source.find('/')?);
    if authority
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, ':' | '@' | '\\' | '%'))
    {
        return None;
    }
    #[cfg(unix)]
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let bytes = source.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let value = u8::from_str_radix(
                std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?,
                16,
            )
            .ok()?;
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
    }
    #[cfg(windows)]
    {
        let text = String::from_utf8(decoded).ok()?;
        // Open buffers use canonical paths, which keep the verbatim prefix only past the
        // ordinary Windows length limit (see `files::canonicalize`).
        let (plain, verbatim) =
            if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
                let share = format!(r"{authority}{}", text.replace('/', r"\"));
                (format!(r"\\{share}"), format!(r"\\?\UNC\{share}"))
            } else if text.as_bytes().get(2) == Some(&b':') {
                let disk = text.strip_prefix('/')?.replace('/', r"\");
                (disk.clone(), format!(r"\\?\{disk}"))
            } else {
                return Some(PathBuf::from(text));
            };
        Some(PathBuf::from(if plain.len() < 260 {
            plain
        } else {
            verbatim
        }))
    }
}
