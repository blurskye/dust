//! Stable explorer icon metadata derived from the MIT-licensed `mini.icons`
//! project. Icons are presentation metadata only; file detection and editor
//! behavior do not depend on Nerd Font availability.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorerIconColor {
    Azure,
    Blue,
    Cyan,
    Green,
    Grey,
    Orange,
    Purple,
    Red,
    Yellow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerIcon {
    pub glyph: &'static str,
    pub color: ExplorerIconColor,
}

const fn icon(glyph: &'static str, color: ExplorerIconColor) -> ExplorerIcon {
    ExplorerIcon { glyph, color }
}

/// Resolves popular language-agnostic directory names before the generic
/// folder fallback. Matching intentionally remains case-sensitive, like a
/// filesystem basename lookup.
#[must_use]
pub fn directory_icon(name: &str) -> Option<ExplorerIcon> {
    use ExplorerIconColor::Azure;
    use ExplorerIconColor::Blue;
    use ExplorerIconColor::Cyan;
    use ExplorerIconColor::Green;
    use ExplorerIconColor::Grey;
    use ExplorerIconColor::Orange;
    use ExplorerIconColor::Purple;
    use ExplorerIconColor::Yellow;
    Some(match name {
        ".cache" | ".local" => icon("󰪺", Cyan),
        ".config" => icon("󱁿", Cyan),
        ".git" => icon("", Orange),
        ".github" => icon("", Azure),
        "bin" => icon("󱧺", Yellow),
        "build" => icon("󱧼", Grey),
        "doc" | "docs" => icon("󱂷", Purple),
        "home" => icon("󱂵", Yellow),
        "lib" => icon("󰲂", Yellow),
        "lua" => icon("󰉋", Blue),
        "node_modules" => icon("", Green),
        "src" => icon("󰴉", Purple),
        "test" | "tests" => icon("󱞊", Blue),
        "tmp" => icon("󰪺", Yellow),
        _ => return None,
    })
}

/// Resolves a file basename using filename overrides followed by a
/// case-insensitive extension lookup. Multi-part archive extensions are
/// handled before the final extension.
#[must_use]
pub fn file_icon(name: &str) -> Option<ExplorerIcon> {
    use ExplorerIconColor::Azure;
    use ExplorerIconColor::Blue;
    use ExplorerIconColor::Cyan;
    use ExplorerIconColor::Green;
    use ExplorerIconColor::Grey;
    use ExplorerIconColor::Orange;
    use ExplorerIconColor::Purple;
    use ExplorerIconColor::Red;
    use ExplorerIconColor::Yellow;

    let named = match name {
        "Cargo.lock" | "Cargo.toml" => Some(icon("", Orange)),
        "CHANGELOG" | "CHANGELOG.md" => Some(icon("󰉻", Blue)),
        "CMakeLists.txt" => Some(icon("󱁤", Orange)),
        "Dockerfile" | ".dockerignore" => Some(icon("󰡨", Blue)),
        "Gemfile" => Some(icon("󰴭", Red)),
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => Some(icon("", Cyan)),
        "Makefile" | "GNUmakefile" => Some(icon("󱁤", Grey)),
        "README" | "README.md" | "README.txt" => Some(icon("", Yellow)),
        "TODO" | "TODO.md" => Some(icon("󰝖", Purple)),
        "build.zig" => Some(icon("", Orange)),
        "flake.nix" => Some(icon("󱄅", Azure)),
        "flake.lock" => Some(icon("󰘦", Yellow)),
        "go.mod" => Some(icon("󰫺", Cyan)),
        "go.sum" | "go.work.sum" => Some(icon("󰟓", Cyan)),
        "init.lua" => Some(icon("", Green)),
        "mix.exs" => Some(icon("", Purple)),
        "package.json" | "package-lock.json" | "tsconfig.json" => Some(icon("󰘦", Yellow)),
        ".git" | ".gitignore" | ".gitattributes" | ".gitmodules" => Some(icon("󰊢", Purple)),
        ".bash_profile" | ".bashrc" | ".nvmrc" | ".zshrc" => Some(icon("󰒓", Green)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tar.xz") || lower.ends_with(".tar.zst") {
        return Some(icon("󰗄", Grey));
    }
    let extension = lower.rsplit_once('.').map(|(_, extension)| extension)?;
    Some(match extension {
        // Documentation and data.
        "md" | "markdown" | "mdx" => icon("󰍔", Grey),
        "txt" => icon("󰦪", Yellow),
        "json" | "json5" | "jsonc" => icon("󰘦", Yellow),
        "toml" => icon("", Orange),
        "yaml" | "yml" => icon("", Purple),
        "xml" | "xsd" | "xsl" | "xslt" => icon("󰗀", Orange),
        "csv" => icon("", Green),
        "tsv" => icon("", Blue),
        "log" | "lock" => icon("󰈔", Grey),
        "pdf" => icon("󰈦", Red),

        // Systems and application languages.
        "rs" => icon("󱘗", Orange),
        "go" => icon("󰟓", Azure),
        "c" => icon("󰙱", Blue),
        "h" => icon("󰫵", Purple),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => icon("󰙲", Azure),
        "cs" => icon("󰌛", Green),
        "java" => icon("󰬷", Orange),
        "kt" | "kts" => icon("󱈙", Blue),
        "swift" => icon("󰛥", Orange),
        "zig" => icon("", Orange),
        "dart" => icon("", Blue),
        "scala" => icon("", Red),
        "sol" => icon("", Azure),
        "asm" | "s" => icon("", Purple),
        "wat" => icon("", Purple),
        "wasm" => icon("󰈔", Grey),

        // Scripting and functional languages.
        "py" | "pyi" | "pyw" => icon("󰌠", Yellow),
        "rb" => icon("󰴭", Red),
        "php" => icon("󰌟", Purple),
        "lua" => icon("󰢱", Azure),
        "vim" => icon("", Green),
        "sh" | "bash" | "fish" => icon("", Grey),
        "zsh" => icon("", Green),
        "ps1" | "psm1" => icon("󰨊", Blue),
        "pl" | "pm" => icon("", Blue),
        "r" => icon("󰟔", Blue),
        "jl" => icon("", Purple),
        "ex" => icon("", Purple),
        "exs" => icon("", Purple),
        "erl" | "hrl" => icon("", Red),
        "fs" | "fsi" | "fsx" => icon("", Blue),
        "hs" | "lhs" => icon("󰲒", Purple),
        "ml" | "mli" => icon("", Orange),
        "clj" | "cljs" | "cljc" | "edn" => icon("", Green),

        // Web and interface languages.
        "js" | "mjs" | "cjs" => icon("󰌞", Yellow),
        "jsx" => icon("", Azure),
        "ts" | "mts" | "cts" => icon("󰛦", Azure),
        "tsx" => icon("", Blue),
        "html" | "htm" => icon("󰌝", Orange),
        "css" => icon("󰌜", Azure),
        "scss" | "sass" => icon("󰟬", Red),
        "less" => icon("󰌜", Purple),
        "vue" => icon("󰡄", Green),
        "svelte" => icon("", Orange),
        "graphql" | "gql" => icon("󰡷", Red),

        // Build, infrastructure, and protocols.
        "nix" => icon("󱄅", Azure),
        "tf" | "tfvars" => icon("󱁢", Blue),
        "hcl" => icon("󰫵", Azure),
        "sql" => icon("󰆼", Grey),
        "proto" => icon("", Red),
        "ini" => icon("󰯂", Azure),
        "conf" | "cfg" | "env" | "editorconfig" => icon("󰒓", Grey),
        "ipynb" => icon("󰠮", Orange),

        // Media, archives, and office documents.
        "png" => icon("󰸭", Purple),
        "jpg" | "jpeg" => icon("󰈥", Orange),
        "gif" => icon("󰵸", Azure),
        "svg" => icon("󰜡", Yellow),
        "bmp" | "tif" | "tiff" | "webp" => icon("󰈟", Green),
        "mp3" | "m4a" => icon("󰈣", Azure),
        "wav" | "flac" | "ogg" => icon("󰈣", Green),
        "mp4" | "m4v" => icon("󰈫", Azure),
        "avi" | "mkv" | "mov" | "webm" => icon("󰈫", Grey),
        "zip" | "7z" => icon("󰗄", Azure),
        "tar" | "gz" | "bz" | "bz2" | "xz" | "zst" | "rar" => icon("󰗄", Grey),
        "doc" | "docx" => icon("󱎒", Azure),
        "xls" | "xlsx" => icon("󱎏", Green),
        "ppt" | "pptx" => icon("󱎐", Red),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::files::icons::*;

    #[test]
    fn filename_overrides_win_over_extensions() {
        assert_eq!(
            file_icon("README.md"),
            Some(icon("", ExplorerIconColor::Yellow))
        );
        assert_eq!(
            file_icon("notes.md"),
            Some(icon("󰍔", ExplorerIconColor::Grey))
        );
        assert_eq!(
            file_icon("Cargo.toml"),
            Some(icon("", ExplorerIconColor::Orange))
        );
    }

    #[test]
    fn extensions_are_case_insensitive_and_archives_are_compound() {
        assert_eq!(
            file_icon("main.RS"),
            Some(icon("󱘗", ExplorerIconColor::Orange))
        );
        assert_eq!(
            file_icon("source.tar.gz"),
            Some(icon("󰗄", ExplorerIconColor::Grey))
        );
    }

    #[test]
    fn unknown_names_defer_to_the_configured_fallback() {
        assert_eq!(file_icon("NOTICE"), None);
        assert_eq!(directory_icon("crates"), None);
    }
}
