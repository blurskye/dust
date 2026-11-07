//! Syntax parsing and highlight results.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use std::collections::HashSet;

use std::path::Path;

use std::sync::Arc;

use std::sync::Mutex;

use std::sync::mpsc;

use std::sync::mpsc::Receiver;

use std::sync::mpsc::SyncSender;

use std::sync::mpsc::TryRecvError;

use std::sync::mpsc::TrySendError;

use std::thread;
use thiserror::Error;
use tree_sitter::Language;
use tree_sitter::Parser;
use tree_sitter::Query;
use tree_sitter::QueryCursor;
use tree_sitter::StreamingIterator;

mod markdown;
mod objects;
use crate::config::Highlight;
use crate::editor::SyntaxObject;
pub(crate) mod query;
pub use markdown::MarkdownMark;
pub use markdown::MarkdownSpan;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SyntaxDocumentId(pub u64);
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SyntaxLanguage {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
    Json,
    Toml,
    Markdown,
    Bash,
    C,
    Cpp,
    CSharp,
    Css,
    Html,
    Python,
    Yaml,
    Lua,
    Nix,
    Java,
    Ruby,
    Php,
    Zig,
    Haskell,
    Elixir,
    OCaml,
    Scala,
    Swift,
    Kotlin,
    Dart,
    Make,
    CMake,
    Xml,
    Diff,
    Ini,
    Proto,
    Sql,
    Erlang,
    R,
    Julia,
    Vue,
}
impl SyntaxLanguage {
    /// The name people know the language by, as shown in `:health`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript / JSX",
            Self::Go => "Go",
            Self::Json => "JSON / JSONC",
            Self::Toml => "TOML",
            Self::Markdown => "Markdown",
            Self::Bash => "Bash / Shell",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::CSharp => "C#",
            Self::Css => "CSS",
            Self::Html => "HTML",
            Self::Python => "Python",
            Self::Yaml => "YAML",
            Self::Lua => "Lua",
            Self::Nix => "Nix",
            Self::Java => "Java",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Zig => "Zig",
            Self::Haskell => "Haskell",
            Self::Elixir => "Elixir",
            Self::OCaml => "OCaml",
            Self::Scala => "Scala",
            Self::Swift => "Swift",
            Self::Kotlin => "Kotlin",
            Self::Dart => "Dart",
            Self::Make => "Make",
            Self::CMake => "CMake",
            Self::Xml => "XML",
            Self::Diff => "Diff",
            Self::Ini => "INI",
            Self::Proto => "Protocol Buffers",
            Self::Sql => "SQL",
            Self::Erlang => "Erlang",
            Self::R => "R",
            Self::Julia => "Julia",
            Self::Vue => "Vue",
        }
    }
    pub const fn id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Go => "go",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Markdown => "markdown",
            Self::Bash => "bash",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "c_sharp",
            Self::Css => "css",
            Self::Html => "html",
            Self::Python => "python",
            Self::Yaml => "yaml",
            Self::Lua => "lua",
            Self::Nix => "nix",
            Self::Java => "java",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Zig => "zig",
            Self::Haskell => "haskell",
            Self::Elixir => "elixir",
            Self::OCaml => "ocaml",
            Self::Scala => "scala",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Dart => "dart",
            Self::Make => "make",
            Self::CMake => "cmake",
            Self::Xml => "xml",
            Self::Diff => "diff",
            Self::Ini => "ini",
            Self::Proto => "proto",
            Self::Sql => "sql",
            Self::Erlang => "erlang",
            Self::R => "r",
            Self::Julia => "julia",
            Self::Vue => "vue",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxSupport {
    Full,
    Partial,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxCapabilities {
    pub highlights: SyntaxSupport,
    pub folds: SyntaxSupport,
    pub indents: SyntaxSupport,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxLanguageInfo {
    pub language: SyntaxLanguage,
    pub id: &'static str,
    pub capabilities: SyntaxCapabilities,
}
const BUILTIN_LANGUAGES: [SyntaxLanguage; 40] = [
    SyntaxLanguage::Rust,
    SyntaxLanguage::TypeScript,
    SyntaxLanguage::Tsx,
    SyntaxLanguage::JavaScript,
    SyntaxLanguage::Go,
    SyntaxLanguage::Json,
    SyntaxLanguage::Toml,
    SyntaxLanguage::Markdown,
    SyntaxLanguage::Bash,
    SyntaxLanguage::C,
    SyntaxLanguage::Cpp,
    SyntaxLanguage::CSharp,
    SyntaxLanguage::Css,
    SyntaxLanguage::Html,
    SyntaxLanguage::Python,
    SyntaxLanguage::Yaml,
    SyntaxLanguage::Lua,
    SyntaxLanguage::Nix,
    SyntaxLanguage::Java,
    SyntaxLanguage::Ruby,
    SyntaxLanguage::Php,
    SyntaxLanguage::Zig,
    SyntaxLanguage::Haskell,
    SyntaxLanguage::Elixir,
    SyntaxLanguage::OCaml,
    SyntaxLanguage::Scala,
    SyntaxLanguage::Swift,
    SyntaxLanguage::Kotlin,
    SyntaxLanguage::Dart,
    SyntaxLanguage::Make,
    SyntaxLanguage::CMake,
    SyntaxLanguage::Xml,
    SyntaxLanguage::Diff,
    SyntaxLanguage::Ini,
    SyntaxLanguage::Proto,
    SyntaxLanguage::Sql,
    SyntaxLanguage::Erlang,
    SyntaxLanguage::R,
    SyntaxLanguage::Julia,
    SyntaxLanguage::Vue,
];
pub fn validate_builtin_queries() -> Result<(), String> {
    for info in builtin_languages() {
        let descriptor = descriptor(info.language);
        let mut parser = Parser::new();
        parser
            .set_language(&descriptor.language)
            .map_err(|error| format!("{} parser: {error}", info.id))?;
        let highlights = Query::new(&descriptor.language, descriptor.highlights)
            .map_err(|error| format!("{} highlights: {error}", info.id))?;
        for capture in highlights.capture_names() {
            if !uncoloured_capture(capture) && Highlight::for_capture(capture).is_none() {
                return Err(format!(
                    "{} highlights: unsupported capture {capture}",
                    info.id
                ));
            }
        }
        for (name, query) in [("folds", descriptor.folds), ("indents", descriptor.indents)] {
            if let Some(query) = query {
                Query::new(&descriptor.language, query)
                    .map_err(|error| format!("{} {name}: {error}", info.id))?;
            }
        }
    }
    let inline = Query::new(
        &tree_sitter_md_025::INLINE_LANGUAGE.into(),
        MARKDOWN_INLINE_HIGHLIGHTS,
    )
    .map_err(|error| format!("markdown_inline highlights: {error}"))?;
    if let Some(capture) = inline
        .capture_names()
        .iter()
        .find(|capture| !uncoloured_capture(capture) && Highlight::for_capture(capture).is_none())
    {
        return Err(format!(
            "markdown_inline highlights: unsupported capture {capture}"
        ));
    }
    Ok(())
}
pub fn builtin_languages() -> impl Iterator<Item = SyntaxLanguageInfo> {
    BUILTIN_LANGUAGES.into_iter().map(language_info)
}
pub const fn language_info(language: SyntaxLanguage) -> SyntaxLanguageInfo {
    let capabilities = match language {
        SyntaxLanguage::Markdown => SyntaxCapabilities {
            highlights: SyntaxSupport::Partial,
            folds: SyntaxSupport::Full,
            indents: SyntaxSupport::Unsupported,
        },
        SyntaxLanguage::Toml | SyntaxLanguage::Html => SyntaxCapabilities {
            highlights: SyntaxSupport::Full,
            folds: SyntaxSupport::Full,
            indents: SyntaxSupport::Unsupported,
        },
        SyntaxLanguage::Rust
        | SyntaxLanguage::TypeScript
        | SyntaxLanguage::Tsx
        | SyntaxLanguage::JavaScript
        | SyntaxLanguage::Go
        | SyntaxLanguage::Json => SyntaxCapabilities {
            highlights: SyntaxSupport::Full,
            folds: SyntaxSupport::Full,
            indents: SyntaxSupport::Full,
        },
        // The other grammars bring their upstream highlight query only, and fold by indentation.
        _ => SyntaxCapabilities {
            highlights: SyntaxSupport::Full,
            folds: SyntaxSupport::Partial,
            indents: SyntaxSupport::Unsupported,
        },
    };
    SyntaxLanguageInfo {
        language,
        id: language.id(),
        capabilities,
    }
}
const MARKDOWN_INLINE_HIGHLIGHTS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/syntax/markdown_inline/highlights.scm"
));

struct Descriptor {
    language: Language,
    highlights: &'static str,
    folds: Option<&'static str>,
    indents: Option<&'static str>,
}
fn descriptor(language: SyntaxLanguage) -> Descriptor {
    match language {
        SyntaxLanguage::Rust => Descriptor {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/rust/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/rust/folds.scm"
            ))),
            indents: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/rust/indents.scm"
            ))),
        },
        SyntaxLanguage::TypeScript => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/typescript/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/typescript/folds.scm"
            ))),
            indents: Some(include_str!(
                "../../../assets/syntax/typescript/indents.scm"
            )),
        },
        SyntaxLanguage::Tsx => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/tsx/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/tsx/folds.scm"
            ))),
            indents: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/tsx/indents.scm"
            ))),
        },
        SyntaxLanguage::JavaScript => Descriptor {
            language: tree_sitter_javascript::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/javascript/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/javascript/folds.scm"
            ))),
            indents: Some(include_str!(
                "../../../assets/syntax/javascript/indents.scm"
            )),
        },
        SyntaxLanguage::Go => Descriptor {
            language: tree_sitter_go::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/go/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/go/folds.scm"
            ))),
            indents: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/go/indents.scm"
            ))),
        },
        SyntaxLanguage::Json => Descriptor {
            language: tree_sitter_json::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/json/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/json/folds.scm"
            ))),
            indents: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/json/indents.scm"
            ))),
        },
        SyntaxLanguage::Toml => Descriptor {
            language: tree_sitter_toml_ng::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/toml/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/toml/folds.scm"
            ))),
            indents: None,
        },
        SyntaxLanguage::Markdown => Descriptor {
            language: tree_sitter_md_025::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/markdown/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/markdown/folds.scm"
            ))),
            indents: None,
        },
        SyntaxLanguage::Bash => Descriptor {
            language: tree_sitter_bash::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/bash/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::C => Descriptor {
            language: tree_sitter_c::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/c/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Cpp => Descriptor {
            language: tree_sitter_cpp::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/cpp/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::CSharp => Descriptor {
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/c_sharp/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Css => Descriptor {
            language: tree_sitter_css::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/css/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Html => Descriptor {
            language: tree_sitter_html::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/html/highlights.scm"
            )),
            folds: Some(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/html/folds.scm"
            ))),
            indents: None,
        },
        SyntaxLanguage::Python => Descriptor {
            language: tree_sitter_python::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/python/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Yaml => Descriptor {
            language: tree_sitter_yaml::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/yaml/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Lua => Descriptor {
            language: tree_sitter_lua::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/lua/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Nix => Descriptor {
            language: tree_sitter_nix::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/nix/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Java => Descriptor {
            language: tree_sitter_java::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/java/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Ruby => Descriptor {
            language: tree_sitter_ruby::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/ruby/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Php => Descriptor {
            language: tree_sitter_php::LANGUAGE_PHP.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/php/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Zig => Descriptor {
            language: tree_sitter_zig::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/zig/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Haskell => Descriptor {
            language: tree_sitter_haskell::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/haskell/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Elixir => Descriptor {
            language: tree_sitter_elixir::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/elixir/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::OCaml => Descriptor {
            language: tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/ocaml/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Scala => Descriptor {
            language: tree_sitter_scala::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/scala/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Swift => Descriptor {
            language: tree_sitter_swift::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/swift/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Kotlin => Descriptor {
            language: tree_sitter_kotlin_sg::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/kotlin/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Dart => Descriptor {
            language: tree_sitter_dart::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/dart/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Make => Descriptor {
            language: tree_sitter_make::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/make/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::CMake => Descriptor {
            language: tree_sitter_cmake::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/cmake/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Xml => Descriptor {
            language: tree_sitter_xml::LANGUAGE_XML.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/xml/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Diff => Descriptor {
            language: tree_sitter_diff::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/diff/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Ini => Descriptor {
            language: tree_sitter_ini::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/ini/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Proto => Descriptor {
            language: tree_sitter_proto::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/proto/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Sql => Descriptor {
            language: tree_sitter_sequel::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/sql/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Erlang => Descriptor {
            language: tree_sitter_erlang::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/erlang/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::R => Descriptor {
            language: tree_sitter_r::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/r/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Julia => Descriptor {
            language: tree_sitter_julia::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/julia/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Vue => Descriptor {
            language: tree_sitter_vue_next::LANGUAGE.into(),
            highlights: include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/vue/highlights.scm"
            )),
            folds: None,
            indents: None,
        },
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxSpan {
    pub start: usize,
    pub end: usize,
    pub role: Highlight,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SyntaxFold {
    pub start_row: usize,
    pub end_row: usize,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SyntaxIndent {
    pub start_row: usize,
    pub end_row: usize,
}
#[derive(Clone, Debug)]
pub struct SyntaxResult {
    pub(crate) tree: Arc<tree_sitter::Tree>,
    pub(crate) source: Arc<str>,
    pub document: SyntaxDocumentId,
    pub request_id: u64,
    pub revision: u64,
    pub language: SyntaxLanguage,
    pub capabilities: SyntaxCapabilities,
    pub spans: Vec<SyntaxSpan>,
    pub folds: Vec<SyntaxFold>,
    pub indents: Vec<SyntaxIndent>,
    /// Functions, classes and arguments for the tree-sitter text objects.
    pub objects: Arc<Vec<SyntaxObject>>,
    /// What a markdown file draws in place of its markup; empty for other languages.
    pub markdown: Vec<MarkdownSpan>,
    pub highlights_truncated: bool,
    pub folds_truncated: bool,
    pub indents_truncated: bool,
    pub truncated: bool,
}
#[derive(Clone, Debug)]
pub struct SyntaxPolicy {
    pub maximum_source_bytes: usize,
    pub maximum_spans: usize,
    pub maximum_folds: usize,
    pub maximum_indent_rows: usize,
    pub tab_width: usize,
}
impl Default for SyntaxPolicy {
    fn default() -> Self {
        Self {
            maximum_source_bytes: 8 * 1024 * 1024,
            maximum_spans: 100_000,
            maximum_folds: 10_000,
            maximum_indent_rows: 100_000,
            tab_width: 4,
        }
    }
}
#[derive(Debug)]
pub enum SyntaxEvent {
    Parsed(SyntaxResult),
    Failed {
        document: SyntaxDocumentId,
        request_id: u64,
        revision: u64,
        language: SyntaxLanguage,
        diagnostic: String,
    },
    Skipped {
        document: SyntaxDocumentId,
        request_id: u64,
        revision: u64,
        language: SyntaxLanguage,
    },
}
#[derive(Debug)]
struct Request {
    document: SyntaxDocumentId,
    request_id: u64,
    revision: u64,
    language: SyntaxLanguage,
    source: String,
    policy: SyntaxPolicy,
}
#[derive(Debug, Error)]
pub enum SyntaxError {
    #[error("syntax worker queue is full")]
    QueueFull,
    #[error("syntax worker stopped")]
    Stopped,
}
pub struct SyntaxWorker {
    sender: SyncSender<Request>,
    receiver: Receiver<SyntaxEvent>,
    slots: Receiver<()>,
    release_slot: SyncSender<()>,
    latest: Arc<Mutex<HashMap<SyntaxDocumentId, u64>>>,
}
impl SyntaxWorker {
    pub fn start(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity.max(1));
        let (etx, erx) = mpsc::sync_channel(capacity.max(1));
        let (release_slot, slots) = mpsc::sync_channel(capacity.max(1));
        for _ in 0..capacity.max(1) {
            release_slot.try_send(()).expect("initial parser capacity");
        }
        let worker_slot = release_slot.clone();
        let latest = Arc::new(Mutex::new(HashMap::new()));
        let worker = latest.clone();
        thread::Builder::new()
            .name("dust-syntax".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    let _ = worker_slot.try_send(());
                    if worker
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&request.document).copied())
                        != Some(request.request_id)
                    {
                        continue;
                    }
                    let document = request.document;
                    let request_id = request.request_id;
                    let event = parse(request);
                    if worker
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&document).copied())
                        == Some(request_id)
                        && etx.send(event).is_err()
                    {
                        break;
                    }
                }
            })
            .expect("syntax worker");
        Self {
            sender: tx,
            receiver: erx,
            slots,
            release_slot,
            latest,
        }
    }
    pub fn submit(
        &self,
        document: SyntaxDocumentId,
        request_id: u64,
        revision: u64,
        language: SyntaxLanguage,
        source: impl FnOnce() -> String,
        policy: SyntaxPolicy,
    ) -> Result<(), SyntaxError> {
        // Reserve a queue slot before copying a potentially large document.
        self.slots.try_recv().map_err(|error| match error {
            TryRecvError::Empty => SyntaxError::QueueFull,
            TryRecvError::Disconnected => SyntaxError::Stopped,
        })?;
        let request = Request {
            document,
            request_id,
            revision,
            language,
            source: source(),
            policy,
        };
        let mut latest = self.latest.lock().map_err(|_| SyntaxError::Stopped)?;
        match self.sender.try_send(request) {
            Ok(()) => {
                latest.insert(document, request_id);
                Ok(())
            }
            Err(error) => {
                let _ = self.release_slot.try_send(());
                Err(match error {
                    TrySendError::Full(_) => SyntaxError::QueueFull,
                    TrySendError::Disconnected(_) => SyntaxError::Stopped,
                })
            }
        }
    }
    pub fn cancel(&self, document: SyntaxDocumentId) {
        if let Ok(mut latest) = self.latest.lock() {
            latest.remove(&document);
        }
    }
    pub fn try_event(&self) -> Result<Option<SyntaxEvent>, SyntaxError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(SyntaxError::Stopped),
        }
    }
}
/// Spans for a query's captures. A group for this language, such as `@markup.heading.1.markdown`,
/// comes before the general one, as in Neovim.
fn highlight_spans(
    query: &Query,
    tree: &tree_sitter::Tree,
    source: &str,
    maximum: usize,
    language: &str,
) -> (Vec<SyntaxSpan>, bool) {
    let mut cursor = QueryCursor::new();
    let groups: Vec<Option<Highlight>> = query
        .capture_names()
        .iter()
        .map(|name| {
            (!uncoloured_capture(name))
                .then(|| {
                    // Keep the language even when its theme is loaded after this parse.
                    Highlight::register(&format!("@{name}.{language}"))
                        .or_else(|| Highlight::for_capture(name))
                })
                .flatten()
        })
        .collect();
    let locals = local_definitions(tree, source, language);
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut spans = Vec::new();
    while let Some(item) = matches.next() {
        if !highlight_predicates_match(query, item.pattern_index, item.captures, source, &locals) {
            continue;
        }
        for capture in item.captures {
            if let Some(role) = groups[capture.index as usize] {
                let range = capture.node.byte_range();
                if range.start < range.end
                    && source.is_char_boundary(range.start)
                    && source.is_char_boundary(range.end)
                {
                    if spans.len() == maximum {
                        return (spans, true);
                    }
                    spans.push(SyntaxSpan {
                        start: range.start,
                        end: range.end,
                        role,
                    });
                }
            }
        }
    }
    (spans, false)
}

#[derive(Default)]
struct LocalScope<'a> {
    inherits: bool,
    definitions: HashSet<&'a str>,
}

/// Definitions and scope boundaries for queries using `#is-not? local`.
fn local_definitions<'a>(
    tree: &tree_sitter::Tree,
    source: &'a str,
    language: &str,
) -> HashMap<usize, LocalScope<'a>> {
    let (grammar, query) = match language {
        "javascript" => (
            descriptor(SyntaxLanguage::JavaScript).language,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        "typescript" | "tsx" => (
            descriptor(if language == "tsx" {
                SyntaxLanguage::Tsx
            } else {
                SyntaxLanguage::TypeScript
            })
            .language,
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "ruby" => (
            descriptor(SyntaxLanguage::Ruby).language,
            tree_sitter_ruby::LOCALS_QUERY,
        ),
        "nix" => (
            descriptor(SyntaxLanguage::Nix).language,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/syntax/nix/locals.scm"
            )),
        ),
        _ => return HashMap::new(),
    };
    thread_local! {
        static QUERIES: std::cell::RefCell<HashMap<String, Query>> = Default::default();
    }
    QUERIES.with_borrow_mut(|queries| {
        let query = queries.entry(language.into()).or_insert_with(|| {
            // TypeScript's query adds typed parameters to JavaScript's scopes and bindings.
            let query = if matches!(language, "typescript" | "tsx") {
                format!("{}\n{query}", tree_sitter_javascript::LOCALS_QUERY)
            } else {
                query.to_owned()
            };
            Query::new(&grammar, &query).expect("bundled locals query")
        });
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        let mut scopes = HashMap::from([(tree.root_node().id(), LocalScope::default())]);
        let mut definitions = Vec::new();
        while let Some(item) = matches.next() {
            for capture in item.captures {
                match query.capture_names()[capture.index as usize] {
                    "local.scope" => {
                        let inherits =
                            !query
                                .property_settings(item.pattern_index)
                                .iter()
                                .any(|property| {
                                    property.key.as_ref() == "local.scope-inherits"
                                        && property.value.as_deref() == Some("false")
                                });
                        scopes.insert(
                            capture.node.id(),
                            LocalScope {
                                inherits,
                                ..Default::default()
                            },
                        );
                    }
                    "local.definition" => definitions.push(capture.node),
                    _ => {}
                }
            }
        }
        for node in definitions {
            let mut scope = node;
            while !scopes.contains_key(&scope.id()) {
                let Some(parent) = scope.parent() else { break };
                scope = parent;
            }
            if let Some(name) = source.get(node.byte_range()) {
                scopes
                    .get_mut(&scope.id())
                    .expect("enclosing local scope")
                    .definitions
                    .insert(name);
            }
        }
        scopes
    })
}

fn highlight_predicates_match(
    query: &Query,
    pattern: usize,
    captures: &[tree_sitter::QueryCapture<'_>],
    source: &str,
    locals: &HashMap<usize, LocalScope<'_>>,
) -> bool {
    for (property, positive) in query.property_predicates(pattern) {
        if property.key.as_ref() != "local" {
            return false;
        }
        let local = captures
            .iter()
            .filter(|capture| {
                property
                    .capture_id
                    .is_none_or(|id| id == capture.index as usize)
            })
            .any(|capture| {
                let Some(name) = source.get(capture.node.byte_range()) else {
                    return false;
                };
                let mut node = Some(capture.node);
                while let Some(current) = node {
                    if let Some(scope) = locals.get(&current.id()) {
                        if scope.definitions.contains(name) {
                            return true;
                        }
                        if !scope.inherits {
                            break;
                        }
                    }
                    node = current.parent();
                }
                false
            });
        if local != *positive {
            return false;
        }
    }
    for predicate in query.general_predicates(pattern) {
        match predicate.operator.as_ref() {
            "has-ancestor?" => {
                let Some(tree_sitter::QueryPredicateArg::Capture(id)) = predicate.args.first()
                else {
                    return false;
                };
                if !captures.iter().filter(|capture| capture.index == *id).all(|capture| {
                    let mut parent = capture.node.parent();
                    while let Some(node) = parent {
                        if predicate.args[1..].iter().any(|arg| matches!(arg,
                            tree_sitter::QueryPredicateArg::String(kind) if kind.as_ref() == node.kind())) {
                            return true;
                        }
                        parent = node.parent();
                    }
                    false
                }) {
                    return false;
                }
            }
            // Unknown predicates must not silently broaden a capture's matches.
            _ => return false,
        }
    }
    true
}

/// Markdown's inline text, such as `code`, *emphasis* and links, has its own grammar. Each
/// paragraph, heading and table cell is parsed with it, as Neovim injects `markdown_inline`.
fn markdown_inline_spans(
    tree: &tree_sitter::Tree,
    source: &str,
    maximum: usize,
    maximum_marks: usize,
    marks: &mut Vec<MarkdownSpan>,
) -> (Vec<SyntaxSpan>, bool) {
    thread_local! {
        static QUERY: Option<Query> = Query::new(
            &tree_sitter_md_025::INLINE_LANGUAGE.into(),
            MARKDOWN_INLINE_HIGHLIGHTS,
        )
        .ok();
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_md_025::INLINE_LANGUAGE.into())
        .is_err()
    {
        return (Vec::new(), false);
    }
    let references = markdown::references(tree, source);
    let mut inline = Vec::new();
    let mut nodes = vec![tree.root_node()];
    let mut walk = tree.walk();
    while let Some(node) = nodes.pop() {
        if matches!(node.kind(), "inline" | "pipe_table_cell") {
            inline.push(node);
        } else {
            nodes.extend(node.named_children(&mut walk));
        }
    }
    QUERY.with(|query| {
        let Some(query) = query else {
            return (Vec::new(), false);
        };
        let mut spans = Vec::new();
        let mut truncated = false;
        for node in inline {
            // Leave out what the block grammar owns inside the text, such as a quote's `>`.
            let ranges = markdown_content_ranges(node);
            if ranges.is_empty() || parser.set_included_ranges(&ranges).is_err() {
                continue;
            }
            let Some(tree) = parser.parse(source, None) else {
                continue;
            };
            let room = maximum.saturating_sub(spans.len());
            markdown::inline_marks(&tree, source, &references, maximum_marks, marks);
            let (inline_spans, overflow) =
                highlight_spans(query, &tree, source, room, "markdown_inline");
            spans.extend(inline_spans);
            truncated |= overflow;
        }
        (spans, truncated)
    })
}

/// Exclude block-continuation prefixes without changing the source coordinates.
fn markdown_content_ranges(node: tree_sitter::Node<'_>) -> Vec<tree_sitter::Range> {
    let mut ranges = Vec::new();
    let mut start = (node.start_byte(), node.start_position());
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        if child.start_byte() > start.0 {
            ranges.push(tree_sitter::Range {
                start_byte: start.0,
                end_byte: child.start_byte(),
                start_point: start.1,
                end_point: child.start_position(),
            });
        }
        start = (child.end_byte(), child.end_position());
    }
    if node.end_byte() > start.0 {
        ranges.push(tree_sitter::Range {
            start_byte: start.0,
            end_byte: node.end_byte(),
            start_point: start.1,
            end_point: node.end_position(),
        });
    }
    ranges
}

/// A fenced code block that names its language is coloured as that language, as Neovim
/// injects it.
fn markdown_fence_spans(
    tree: &tree_sitter::Tree,
    source: &str,
    maximum: usize,
) -> (Vec<SyntaxSpan>, bool) {
    thread_local! {
        static QUERIES: std::cell::RefCell<HashMap<SyntaxLanguage, Query>> = Default::default();
    }
    let mut truncated = false;
    let mut spans = Vec::new();
    let mut nodes = vec![tree.root_node()];
    let mut walk = tree.walk();
    while let Some(node) = nodes.pop() {
        if node.kind() != "fenced_code_block" {
            nodes.extend(node.named_children(&mut walk));
            continue;
        }
        let children = node.named_children(&mut walk).collect::<Vec<_>>();
        let language = children
            .iter()
            .find(|child| child.kind() == "info_string")
            .and_then(|info| info.named_child(0))
            .and_then(|name| source.get(name.byte_range()))
            .and_then(language_for_fence)
            .filter(|language| *language != SyntaxLanguage::Markdown);
        let content = children
            .iter()
            .find(|child| child.kind() == "code_fence_content");
        let (Some(language), Some(content)) = (language, content) else {
            continue;
        };
        let room = maximum.saturating_sub(spans.len());
        if room == 0 {
            return (
                spans,
                truncated || content.start_byte() < content.end_byte(),
            );
        }
        let descriptor = descriptor(language);
        let mut parser = Parser::new();
        let ranges = markdown_content_ranges(*content);
        if parser.set_language(&descriptor.language).is_err()
            || ranges.is_empty()
            || parser.set_included_ranges(&ranges).is_err()
        {
            continue;
        }
        let Some(tree) = parser.parse(source, None) else {
            continue;
        };
        let (injected, overflow) = QUERIES.with_borrow_mut(|queries| {
            let query = match queries.entry(language) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    match Query::new(&descriptor.language, descriptor.highlights) {
                        Ok(query) => entry.insert(query),
                        Err(_) => return (Vec::new(), false),
                    }
                }
            };
            highlight_spans(query, &tree, source, room, language.id())
        });
        spans.extend(injected);
        truncated |= overflow;
    }
    (spans, truncated)
}

/// Nested captures, such as an escape inside a string or code inside a heading, become
/// side-by-side spans where the inner one wins, as Neovim draws them.
fn flatten_spans(mut spans: Vec<SyntaxSpan>) -> Vec<SyntaxSpan> {
    // Outer spans first; a stable sort keeps a later capture of the same text on top.
    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    let mut flat: Vec<SyntaxSpan> = Vec::with_capacity(spans.len());
    let mut emit = |role, start: usize, end: usize| {
        if start >= end {
            return;
        }
        match flat.last_mut() {
            Some(last) if last.role == role && last.end == start => last.end = end,
            _ => flat.push(SyntaxSpan { start, end, role }),
        }
    };
    let mut open: Vec<SyntaxSpan> = Vec::new();
    let mut at = 0;
    for span in spans {
        while let Some(top) = open.last().copied().filter(|top| top.end <= span.start) {
            emit(top.role, at.max(top.start), top.end);
            at = at.max(top.end);
            open.pop();
        }
        if let Some(top) = open.last() {
            emit(top.role, at.max(top.start), span.start);
        }
        at = at.max(span.start);
        open.push(span);
    }
    while let Some(top) = open.pop() {
        emit(top.role, at.max(top.start), top.end);
        at = at.max(top.end);
    }
    flat
}

/// Highlight a short snippet right away, such as a code block in a chat reply. The
/// compiled query is kept per thread, since compiling it costs more than the parse.
#[must_use]
pub fn highlight(language: SyntaxLanguage, source: &str) -> Vec<SyntaxSpan> {
    thread_local! {
        static QUERIES: std::cell::RefCell<HashMap<SyntaxLanguage, Query>> = Default::default();
    }
    let descriptor = descriptor(language);
    let mut parser = Parser::new();
    if parser.set_language(&descriptor.language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let (mut spans, _) = QUERIES.with_borrow_mut(|queries| {
        let query = match queries.entry(language) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                match Query::new(&descriptor.language, descriptor.highlights) {
                    Ok(query) => entry.insert(query),
                    Err(_) => return (Vec::new(), false),
                }
            }
        };
        highlight_spans(query, &tree, source, usize::MAX, language.id())
    });
    // Inline and fenced languages use their own query caches.
    if language == SyntaxLanguage::Markdown {
        spans.extend(
            markdown_inline_spans(&tree, source, usize::MAX, usize::MAX, &mut Vec::new()).0,
        );
        spans.extend(markdown_fence_spans(&tree, source, usize::MAX).0);
    }
    flatten_spans(spans)
}

/// The language a code fence names, such as `rust` or `ts`.
#[must_use]
pub fn language_for_fence(name: &str) -> Option<SyntaxLanguage> {
    match name.to_ascii_lowercase().as_str() {
        "rust" => Some(SyntaxLanguage::Rust),
        "typescript" => Some(SyntaxLanguage::TypeScript),
        "javascript" | "node" => Some(SyntaxLanguage::JavaScript),
        "golang" => Some(SyntaxLanguage::Go),
        "markdown" => Some(SyntaxLanguage::Markdown),
        "shell" | "console" | "shellsession" => Some(SyntaxLanguage::Bash),
        "python" | "python3" => Some(SyntaxLanguage::Python),
        "csharp" | "c#" => Some(SyntaxLanguage::CSharp),
        "kotlin" => Some(SyntaxLanguage::Kotlin),
        "ruby" => Some(SyntaxLanguage::Ruby),
        "haskell" => Some(SyntaxLanguage::Haskell),
        "elixir" => Some(SyntaxLanguage::Elixir),
        "ocaml" => Some(SyntaxLanguage::OCaml),
        "erlang" => Some(SyntaxLanguage::Erlang),
        "julia" => Some(SyntaxLanguage::Julia),
        "makefile" => Some(SyntaxLanguage::Make),
        other => language_for_extension(other),
    }
}

/// The language of a document: by its file name, else by a `#!` line, as Neovim does for
/// scripts without an extension.
#[must_use]
pub fn language_for_document(path: Option<&Path>, start: &str) -> Option<SyntaxLanguage> {
    path.and_then(language_for_path).or_else(|| {
        let line = start.lines().next()?.strip_prefix("#!")?;
        let mut words = line.split_whitespace();
        let mut program = words.next()?.rsplit('/').next()?;
        if program == "env" {
            loop {
                let word = words.next()?;
                match word {
                    "-u" | "--unset" | "-C" | "--chdir" | "-a" | "--argv0" => {
                        words.next()?;
                    }
                    // -S splits the rest of the shebang into arguments.
                    "-S" | "--split-string" | "--" => {}
                    _ if word.starts_with('-') || word.contains('=') => {}
                    _ => {
                        program = word.rsplit('/').next()?;
                        break;
                    }
                }
            }
        }
        match program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
            "sh" | "bash" | "zsh" | "ksh" | "dash" => Some(SyntaxLanguage::Bash),
            "python" => Some(SyntaxLanguage::Python),
            "node" | "deno" | "bun" => Some(SyntaxLanguage::JavaScript),
            "ruby" => Some(SyntaxLanguage::Ruby),
            "lua" => Some(SyntaxLanguage::Lua),
            "make" => Some(SyntaxLanguage::Make),
            _ => None,
        }
    })
}

#[must_use]
pub fn language_for_path(path: &Path) -> Option<SyntaxLanguage> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(".bashrc")
        | Some(".bash_profile")
        | Some(".zshrc")
        | Some(".profile")
        | Some("PKGBUILD") => return Some(SyntaxLanguage::Bash),
        Some("Gemfile") | Some("Rakefile") => return Some(SyntaxLanguage::Ruby),
        Some("Makefile") | Some("makefile") | Some("GNUmakefile") => {
            return Some(SyntaxLanguage::Make);
        }
        Some("CMakeLists.txt") => return Some(SyntaxLanguage::CMake),
        Some(".gitconfig") | Some(".editorconfig") => return Some(SyntaxLanguage::Ini),
        _ => {}
    }
    let extension = path.extension().and_then(|extension| extension.to_str());
    if extension == Some("C") {
        return Some(SyntaxLanguage::Cpp);
    }
    // Neovim's filetype table names the rest, such as `flake.lock` as JSON.
    extension
        .and_then(language_for_extension)
        .or_else(|| crate::tools::mason_filetype_for_path(path).and_then(language_for_fence))
}

fn language_for_extension(extension: &str) -> Option<SyntaxLanguage> {
    match extension.to_ascii_lowercase().as_str() {
        "rs" => Some(SyntaxLanguage::Rust),
        "ts" | "mts" | "cts" => Some(SyntaxLanguage::TypeScript),
        "tsx" => Some(SyntaxLanguage::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(SyntaxLanguage::JavaScript),
        "go" => Some(SyntaxLanguage::Go),
        "json" | "jsonc" => Some(SyntaxLanguage::Json),
        "toml" => Some(SyntaxLanguage::Toml),
        "md" | "markdown" => Some(SyntaxLanguage::Markdown),
        "sh" | "bash" | "zsh" | "ksh" => Some(SyntaxLanguage::Bash),
        "c" | "h" => Some(SyntaxLanguage::C),
        "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "ipp" => Some(SyntaxLanguage::Cpp),
        "cs" | "csx" => Some(SyntaxLanguage::CSharp),
        "css" | "scss" | "less" | "pcss" | "postcss" => Some(SyntaxLanguage::Css),
        "html" | "htm" | "xhtml" => Some(SyntaxLanguage::Html),
        "py" | "pyi" | "pyw" => Some(SyntaxLanguage::Python),
        "yaml" | "yml" => Some(SyntaxLanguage::Yaml),
        "lua" => Some(SyntaxLanguage::Lua),
        "nix" => Some(SyntaxLanguage::Nix),
        "java" => Some(SyntaxLanguage::Java),
        "rb" | "rake" | "gemspec" => Some(SyntaxLanguage::Ruby),
        "php" => Some(SyntaxLanguage::Php),
        "zig" | "zon" => Some(SyntaxLanguage::Zig),
        "hs" => Some(SyntaxLanguage::Haskell),
        "ex" | "exs" => Some(SyntaxLanguage::Elixir),
        "ml" => Some(SyntaxLanguage::OCaml),
        "scala" | "sc" | "sbt" => Some(SyntaxLanguage::Scala),
        "swift" => Some(SyntaxLanguage::Swift),
        "kt" | "kts" => Some(SyntaxLanguage::Kotlin),
        "dart" => Some(SyntaxLanguage::Dart),
        "mk" | "mak" => Some(SyntaxLanguage::Make),
        "cmake" => Some(SyntaxLanguage::CMake),
        "xml" | "svg" | "xsd" | "xsl" | "plist" | "csproj" => Some(SyntaxLanguage::Xml),
        "diff" | "patch" => Some(SyntaxLanguage::Diff),
        "ini" | "cfg" | "desktop" | "service" => Some(SyntaxLanguage::Ini),
        "proto" => Some(SyntaxLanguage::Proto),
        "sql" => Some(SyntaxLanguage::Sql),
        "erl" | "hrl" => Some(SyntaxLanguage::Erlang),
        "r" => Some(SyntaxLanguage::R),
        "jl" => Some(SyntaxLanguage::Julia),
        "vue" => Some(SyntaxLanguage::Vue),
        _ => None,
    }
}
/// Captures upstream queries use for predicates, spelling or errors, which take no colour.
fn uncoloured_capture(name: &str) -> bool {
    name.starts_with('_')
        || matches!(
            name.split('.').next(),
            Some("none" | "spell" | "nospell" | "error" | "conceal" | "local")
        )
}
fn parse(request: Request) -> SyntaxEvent {
    if request.source.len() > request.policy.maximum_source_bytes {
        return SyntaxEvent::Skipped {
            document: request.document,
            request_id: request.request_id,
            revision: request.revision,
            language: request.language,
        };
    }
    let descriptor = descriptor(request.language);
    let mut parser = Parser::new();
    if let Err(error) = parser.set_language(&descriptor.language) {
        return failed(&request, error.to_string());
    }
    let Some(tree) = parser.parse(&request.source, None) else {
        return failed(&request, "parser returned no tree".into());
    };
    // Compiling a large query costs more than the parse, so each worker keeps its own.
    thread_local! {
        static QUERIES: std::cell::RefCell<HashMap<SyntaxLanguage, Query>> = Default::default();
    }
    let mut marks = Vec::new();
    let spans = QUERIES.with_borrow_mut(|queries| {
        let query = match queries.entry(request.language) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Query::new(&descriptor.language, descriptor.highlights)?)
            }
        };
        let maximum = request.policy.maximum_spans;
        let (mut spans, mut truncated) = highlight_spans(
            query,
            &tree,
            &request.source,
            maximum,
            request.language.id(),
        );
        if request.language == SyntaxLanguage::Markdown {
            // Keep one extra mark to detect actual overflow, independently of highlights.
            let maximum_marks = maximum.saturating_add(1);
            markdown::block_marks(&tree, &request.source, maximum_marks, &mut marks);
            let room = maximum.saturating_sub(spans.len());
            let (inline, overflow) =
                markdown_inline_spans(&tree, &request.source, room, maximum_marks, &mut marks);
            spans.extend(inline);
            truncated |= overflow || marks.len() > maximum;
            marks.truncate(maximum);
            let room = maximum.saturating_sub(spans.len());
            let (fenced, overflow) = markdown_fence_spans(&tree, &request.source, room);
            spans.extend(fenced);
            truncated |= overflow;
        }
        let mut spans = flatten_spans(spans);
        truncated |= spans.len() > maximum;
        spans.truncate(maximum);
        Ok((spans, truncated))
    });
    let (spans, highlights_truncated) = match spans {
        Ok(spans) => spans,
        Err(error) => {
            let error: tree_sitter::QueryError = error;
            return failed(&request, format!("highlight query: {error}"));
        }
    };
    let (folds, folds_truncated) = match descriptor.folds.map_or_else(
        || {
            Ok(indentation_folds(
                &request.source,
                request.policy.maximum_folds,
                request.policy.tab_width,
            ))
        },
        |query| {
            query_rows(
                &descriptor.language,
                Some(query),
                &tree,
                &request.source,
                request.policy.maximum_folds,
            )
        },
    ) {
        Ok(rows) => rows,
        Err(error) => return failed(&request, format!("fold query: {error}")),
    };
    let (indents, indents_truncated) = match query_rows(
        &descriptor.language,
        descriptor.indents,
        &tree,
        &request.source,
        request.policy.maximum_indent_rows,
    ) {
        Ok((ranges, truncated)) => (
            ranges
                .into_iter()
                .map(|range| SyntaxIndent {
                    start_row: range.start_row,
                    end_row: range.end_row,
                })
                .collect::<Vec<_>>(),
            truncated,
        ),
        Err(error) => return failed(&request, format!("indent query: {error}")),
    };
    let objects = Arc::new(objects::collect(&tree, &request.source, 50_000));
    SyntaxEvent::Parsed(SyntaxResult {
        tree: Arc::new(tree),
        source: request.source.into(),
        document: request.document,
        request_id: request.request_id,
        revision: request.revision,
        language: request.language,
        capabilities: language_info(request.language).capabilities,
        highlights_truncated,
        folds_truncated,
        indents_truncated,
        truncated: highlights_truncated || folds_truncated || indents_truncated,
        spans,
        folds,
        indents,
        objects,
        markdown: marks,
    })
}
fn query_rows(
    language: &Language,
    source_query: Option<&str>,
    tree: &tree_sitter::Tree,
    source: &str,
    limit: usize,
) -> Result<(Vec<SyntaxFold>, bool), String> {
    let Some(source_query) = source_query else {
        return Ok((Vec::new(), false));
    };
    let query = Query::new(language, source_query).map_err(|error| error.to_string())?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    while let Some(item) = matches.next() {
        for capture in item.captures {
            let start = capture.node.start_position().row;
            let end_position = capture.node.end_position();
            // A node that swallows its last newline, like a TOML table, ends at the start of
            // the next line, and that line isn't part of the fold.
            let mut end = end_position
                .row
                .saturating_sub(usize::from(end_position.column == 0));
            if query.capture_names()[capture.index as usize] == "indent"
                && let Some(closing) = capture
                    .node
                    .child(capture.node.child_count().saturating_sub(1))
                && closing.start_position().row == end
                && source[..closing.start_byte()]
                    .rsplit('\n')
                    .next()
                    .is_some_and(|prefix| !prefix.trim().is_empty())
            {
                // A statement before the closing delimiter still belongs to this block.
                end += 1;
            }
            if end > start && seen.insert((start, end)) {
                if output.len() == limit {
                    return Ok((output, true));
                }
                output.push(SyntaxFold {
                    start_row: start,
                    end_row: end,
                });
            }
        }
    }
    Ok((output, false))
}
/// Folds what is indented under a line, as Vim's `foldmethod=indent`, for grammars with no fold
/// query. The line closing a block at its own indentation folds with it.
fn indentation_folds(source: &str, limit: usize, tab_width: usize) -> (Vec<SyntaxFold>, bool) {
    let mut folds = Vec::new();
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut previous = 0;
    for (row, line) in source.lines().enumerate() {
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let tab_width = tab_width.max(1);
        let indent = line[..line.len() - body.len()]
            .chars()
            .fold(0, |column, ch| {
                column
                    + if ch == '\t' {
                        tab_width - column % tab_width
                    } else {
                        1
                    }
            });
        let closes = body.starts_with(['}', ')', ']'])
            || body.starts_with("</")
            || body.split(|c: char| !c.is_alphanumeric()).next() == Some("end");
        while let Some(&(opened, start)) = open.last().filter(|(opened, _)| *opened >= indent) {
            open.pop();
            let end = if opened == indent && closes {
                row
            } else {
                previous
            };
            if end > start {
                folds.push(SyntaxFold {
                    start_row: start,
                    end_row: end,
                });
            }
        }
        open.push((indent, row));
        previous = row;
    }
    for (_, start) in open {
        if previous > start {
            folds.push(SyntaxFold {
                start_row: start,
                end_row: previous,
            });
        }
    }
    folds.sort_by_key(|fold| fold.start_row);
    let truncated = folds.len() > limit;
    folds.truncate(limit);
    (folds, truncated)
}
fn failed(request: &Request, diagnostic: String) -> SyntaxEvent {
    SyntaxEvent::Failed {
        document: request.document,
        request_id: request.request_id,
        revision: request.revision,
        language: request.language,
        diagnostic,
    }
}

#[cfg(test)]
mod tests {
    use crate::language::syntax::*;
    #[test]
    fn full_queue_does_not_materialize_source_or_replace_accepted_request() {
        let (sender, _requests) = mpsc::sync_channel(1);
        let (_events, receiver) = mpsc::sync_channel(1);
        let (release_slot, slots) = mpsc::sync_channel(1);
        release_slot.try_send(()).unwrap();
        let service = SyntaxWorker {
            sender,
            receiver,
            slots,
            release_slot,
            latest: Arc::new(Mutex::new(HashMap::new())),
        };
        let document = SyntaxDocumentId(1);
        service
            .submit(
                document,
                1,
                0,
                SyntaxLanguage::Rust,
                || "fn main() {}".into(),
                SyntaxPolicy::default(),
            )
            .unwrap();
        for _ in 0..10 {
            assert!(matches!(
                service.submit(
                    document,
                    2,
                    1,
                    SyntaxLanguage::Rust,
                    || panic!("full queue must not copy source"),
                    SyntaxPolicy::default()
                ),
                Err(SyntaxError::QueueFull)
            ));
        }
        assert_eq!(service.latest.lock().unwrap().get(&document), Some(&1));
    }

    #[test]
    fn saturated_result_queue_retains_completed_parses() {
        let service = SyntaxWorker::start(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for id in 1..=3 {
            loop {
                match service.submit(
                    SyntaxDocumentId(id),
                    1,
                    0,
                    SyntaxLanguage::Rust,
                    || "fn main() {}".into(),
                    SyntaxPolicy::default(),
                ) {
                    Ok(()) => break,
                    Err(SyntaxError::QueueFull) => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut completed = 0;
        while completed < 3 {
            if service.try_event().unwrap().is_some() {
                completed += 1;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn languages_without_a_fold_query_fold_by_indentation() {
        let rows = |source: &str| {
            indentation_folds(source, 10, 4)
                .0
                .into_iter()
                .map(|fold| (fold.start_row, fold.end_row))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows("def main():\n    a = 1\n\n    if a:\n        b\n    return a\n\nrest\n"),
            [(0, 5), (3, 4)]
        );
        assert_eq!(rows("void f() {\n  x;\n}\nnext\n"), [(0, 2)]);
        assert_eq!(rows("if a then\n  b\nend\n"), [(0, 2)]);
        assert_eq!(rows("<div>\n  <p>\n</div>\n"), [(0, 2)]);
        assert!(rows("one\ntwo\n").is_empty());
        assert_eq!(rows("class A:\n  def b():\n    c\n"), [(0, 2), (1, 2)]);
        let (folds, truncated) = indentation_folds("a\n b\nc\n d\n", 1, 4);
        assert_eq!(folds.len(), 1);
        assert!(truncated);
    }
    #[test]
    fn indentation_folds_use_configured_tab_stops() {
        let source = "void f() {\n\tx;\n    y;\n}\n";
        for tab_width in [4, 8] {
            let result = parse_with_policy(
                SyntaxLanguage::C,
                source,
                SyntaxPolicy {
                    tab_width,
                    ..Default::default()
                },
            );
            assert_eq!(
                result
                    .folds
                    .iter()
                    .map(|fold| (fold.start_row, fold.end_row))
                    .collect::<Vec<_>>(),
                [(0, 3)]
            );
        }
        let source = "void f() {\n    x;\n\ty;\n}\n";
        assert_eq!(indentation_folds(source, 10, 4).0.len(), 1);
        assert_eq!(indentation_folds(source, 10, 8).0.len(), 2);
        // A tab advances to its next stop, not by a fixed number of cells.
        assert_eq!(indentation_folds("f\n \tx\n    y\n", 10, 4).0.len(), 1);
    }
    #[test]
    fn an_html_element_folds_even_when_its_children_share_its_indentation() {
        let source = "<!doctype html>\n<html>\n<head>\n<title>x</title>\n</head>\n</html>\n";
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 2,
            revision: 3,
            language: SyntaxLanguage::Html,
            source: source.into(),
            policy: SyntaxPolicy::default(),
        };
        let SyntaxEvent::Parsed(result) = parse(request) else {
            panic!("parse html")
        };
        let folds = result.folds;
        assert!(
            folds
                .iter()
                .any(|fold| (fold.start_row, fold.end_row) == (1, 5)),
            "{folds:?}"
        );
        assert!(
            folds
                .iter()
                .any(|fold| (fold.start_row, fold.end_row) == (2, 4)),
            "{folds:?}"
        );
    }
    #[test]
    fn a_toml_table_fold_stops_before_the_next_table() {
        let source = "[app]\na = 1\nb = 2\n[app.flags]\nenabled = true\n";
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 2,
            revision: 3,
            language: SyntaxLanguage::Toml,
            source: source.into(),
            policy: SyntaxPolicy::default(),
        };
        let SyntaxEvent::Parsed(result) = parse(request) else {
            panic!("parse toml")
        };
        let folds = result.folds;
        assert!(
            folds
                .iter()
                .any(|fold| (fold.start_row, fold.end_row) == (0, 2)),
            "{folds:?}"
        );
        assert!(folds.iter().all(|fold| fold.end_row != 3), "{folds:?}");
    }
    #[test]
    fn builtins_parse_unicode_and_queries() {
        for (language, source) in [
            (SyntaxLanguage::Rust, "fn café() { let x = \"é\"; }"),
            (
                SyntaxLanguage::TypeScript,
                "function café(): string { return \"é\"; }",
            ),
            (SyntaxLanguage::Tsx, "const x = <div>é</div>;"),
            (
                SyntaxLanguage::JavaScript,
                "function café(){ return \"é\"; }",
            ),
            (
                SyntaxLanguage::Go,
                "package main\nfunc café() { println(\"é\") }",
            ),
            (SyntaxLanguage::Json, "{\"café\":[1,2]}"),
            (SyntaxLanguage::Toml, "[café]\nvalue = \"é\""),
            (SyntaxLanguage::Markdown, "# café\n\n```\né\n```"),
        ] {
            let request = Request {
                document: SyntaxDocumentId(1),
                request_id: 2,
                revision: 3,
                language,
                source: source.into(),
                policy: SyntaxPolicy::default(),
            };
            let SyntaxEvent::Parsed(result) = parse(request) else {
                panic!("parse {language:?}")
            };
            assert_eq!(result.language, language);
            assert!(result.spans.iter().all(
                |span| source.is_char_boundary(span.start) && source.is_char_boundary(span.end)
            ));
        }
    }
    #[test]
    fn highlight_queries_enforce_text_local_and_ancestor_predicates() {
        for (language, source, ordinary, special) in [
            (
                SyntaxLanguage::Zig,
                "var count: u32 = 0; const VALUE = 1;",
                "count",
                "VALUE",
            ),
            (
                SyntaxLanguage::CMake,
                "set(lower value)\nset(UPPER value)\n",
                "lower",
                "UPPER",
            ),
        ] {
            let spans = highlight(language, source);
            let role = |word: &str| {
                let at = source.find(word).unwrap();
                spans
                    .iter()
                    .find(|span| span.start <= at && at < span.end)
                    .map(|span| span.role.name())
            };
            let expected = match language {
                SyntaxLanguage::Zig => "@constant.zig",
                SyntaxLanguage::CMake => "@constant.cmake",
                _ => unreachable!(),
            };
            assert_ne!(role(ordinary), Some(expected), "{language:?}: {spans:?}");
            assert_eq!(role(special), Some(expected), "{language:?}: {spans:?}");
        }
        for (language, source, word) in [
            (
                SyntaxLanguage::JavaScript,
                "console.log(1); function f(console) { console.log(2); }",
                "console",
            ),
            (
                SyntaxLanguage::TypeScript,
                "console.log(1); function f(console: any) { console.log(2); }",
                "console",
            ),
            (
                SyntaxLanguage::Ruby,
                "value\ndef f\n  value = 1\n  value\nend\n",
                "value",
            ),
            (
                SyntaxLanguage::Nix,
                "[ map (let map = x: x; in map 1) ]",
                "map",
            ),
        ] {
            let descriptor = descriptor(language);
            let mut parser = Parser::new();
            parser.set_language(&descriptor.language).unwrap();
            let tree = parser.parse(source, None).unwrap();
            let query = Query::new(&descriptor.language, &format!("((identifier) @variable.builtin (#eq? @variable.builtin \"{word}\") (#is-not? local))")).unwrap();
            let (spans, _) = highlight_spans(&query, &tree, source, 100, language.id());
            assert_eq!(spans.len(), 1, "{language:?}: {spans:?}");
            assert_eq!(spans[0].start, source.find(word).unwrap());
        }
        let ruby = descriptor(SyntaxLanguage::Ruby);
        let mut parser = Parser::new();
        parser.set_language(&ruby.language).unwrap();
        let source = "value = 1\nvalue\ndef f\n  value\nend\n";
        let tree = parser.parse(source, None).unwrap();
        let query = Query::new(
            &ruby.language,
            "((identifier) @function.method (#eq? @function.method \"value\") (#is-not? local))",
        )
        .unwrap();
        let (spans, _) = highlight_spans(&query, &tree, source, 100, "ruby");
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].start, source.rfind("value").unwrap());

        let descriptor = descriptor(SyntaxLanguage::Rust);
        let mut parser = Parser::new();
        parser.set_language(&descriptor.language).unwrap();
        let source = "fn f() { outside; foo(inside); }";
        let tree = parser.parse(source, None).unwrap();
        let query = Query::new(
            &descriptor.language,
            "((identifier) @variable.builtin (#has-ancestor? @variable.builtin call_expression))",
        )
        .unwrap();
        let (spans, _) = highlight_spans(&query, &tree, source, 100, "rust");
        let words = spans
            .iter()
            .map(|span| &source[span.start..span.end])
            .collect::<Vec<_>>();
        assert_eq!(words, ["foo", "inside"]);
    }

    #[test]
    fn markdown_colours_headings_by_level_and_inline_code_emphasis_and_links() {
        let source = "# Title `code`\n\n## Sub\n\n> quote **bold** and *it* [link](https://x.y)\n";
        let spans = highlight(SyntaxLanguage::Markdown, source);
        let group = |word: &str| {
            let at = source.find(word).expect(word);
            spans
                .iter()
                .find(|span| span.start <= at && at < span.end)
                .map(|span| span.role.name())
        };
        assert_eq!(group("Title"), Some("@markup.heading.1.markdown"));
        assert_eq!(group("code"), Some("@markup.raw.markdown_inline"));
        assert_eq!(group("Sub"), Some("@markup.heading.2.markdown"));
        assert_eq!(group("quote"), Some("@markup.quote.markdown"));
        assert_eq!(group("bold"), Some("@markup.strong.markdown_inline"));
        assert_eq!(group("it* "), Some("@markup.italic.markdown_inline"));
        assert_eq!(group("link"), Some("@markup.link.label.markdown_inline"));
        assert_eq!(group("https"), Some("@markup.link.url.markdown_inline"));
        assert!(spans.windows(2).all(|pair| pair[0].end <= pair[1].start));
    }
    fn parse_with_policy(
        language: SyntaxLanguage,
        source: &str,
        policy: SyntaxPolicy,
    ) -> SyntaxResult {
        let event = parse(Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language,
            source: source.into(),
            policy,
        });
        let SyntaxEvent::Parsed(result) = event else {
            panic!("{event:?}")
        };
        result
    }

    #[test]
    fn markdown_mark_budget_is_independent_of_remaining_highlight_capacity() {
        let source = "# h\n\n**b**\n";
        let result = parse_with_policy(
            SyntaxLanguage::Markdown,
            source,
            SyntaxPolicy {
                maximum_spans: 6,
                ..Default::default()
            },
        );
        assert_eq!(result.markdown.len(), 6);
        assert_eq!(
            result
                .markdown
                .iter()
                .filter(|span| span.mark == MarkdownMark::Hide)
                .count(),
            4
        );
        assert!(!result.highlights_truncated);
    }

    #[test]
    fn markdown_marks_what_it_draws_in_place_of_its_markup() {
        let source = "## Sub\n\n- one\n  * two\n- [x] done\n\n> quote **bold** `code` [link](https://x.y) ![alt](a.png)\n\n---\n\n```rust\nfn main() {}\n```\n\n> [!NOTE]\n> noted\n\n| a | b |\n|---|:-:|\n| c | d |\n";
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language: SyntaxLanguage::Markdown,
            source: source.into(),
            policy: SyntaxPolicy::default(),
        };
        let SyntaxEvent::Parsed(result) = parse(request) else {
            panic!("parse markdown")
        };
        let marked = |text: &str| {
            let mut marks = result
                .markdown
                .iter()
                .filter(|span| &source[span.start..span.end] == text)
                .map(|span| span.mark)
                .collect::<Vec<_>>();
            marks.sort_by_key(|mark| format!("{mark:?}"));
            marks
        };
        assert_eq!(marked("## "), [MarkdownMark::Heading(2)]);
        assert_eq!(marked("## Sub"), [MarkdownMark::HeadingLine(2)]);
        assert_eq!(marked("-")[0], MarkdownMark::Bullet(0));
        // Each `*` of `**bold**` is a delimiter of its own.
        assert_eq!(
            marked("*"),
            [
                MarkdownMark::Bullet(1),
                MarkdownMark::Hide,
                MarkdownMark::Hide,
                MarkdownMark::Hide,
                MarkdownMark::Hide
            ]
        );
        assert_eq!(marked("[x]"), [MarkdownMark::Checkbox { checked: true }]);
        assert_eq!(marked("- "), [MarkdownMark::Hide]);
        let note = source.find("[!NOTE]").expect("callout");
        assert_eq!(
            marked(">"),
            [
                MarkdownMark::Quote { callout: None },
                MarkdownMark::Quote {
                    callout: Some(note)
                },
                MarkdownMark::Quote {
                    callout: Some(note)
                },
            ]
        );
        assert_eq!(marked("[!NOTE]"), [MarkdownMark::Callout]);
        assert_eq!(marked("`code`"), [MarkdownMark::InlineCode]);
        assert_eq!(marked("rust"), [MarkdownMark::CodeLanguage]);
        let url = source.find("https://x.y").expect("url");
        assert_eq!(
            marked("["),
            [MarkdownMark::Link {
                url: Some((url, url + "https://x.y".len()))
            }]
        );
        assert_eq!(
            result
                .markdown
                .iter()
                .filter(|span| span.mark == MarkdownMark::TablePipe)
                .count(),
            9
        );
        let table = source.find("| a").expect("table");
        assert!(
            result
                .markdown
                .iter()
                .any(|span| span.mark == MarkdownMark::Table
                    && span.start == table
                    && source[span.end..].trim().is_empty())
        );
        assert_eq!(marked("`"), [MarkdownMark::Hide, MarkdownMark::Hide]);
        assert_eq!(marked("](https://x.y)"), [MarkdownMark::Hide]);
        assert_eq!(marked("!["), [MarkdownMark::Image]);
        assert_eq!(marked("---"), [MarkdownMark::Rule]);
        assert_eq!(marked("```"), [MarkdownMark::Hide, MarkdownMark::Hide]);
        let code = source.find("```rust").expect("fence");
        assert!(
            result
                .markdown
                .iter()
                .any(|span| span.mark == MarkdownMark::Code
                    && span.start == code
                    && source[span.start..span.end].contains("fn main"))
        );
    }
    #[test]
    fn markdown_reference_marks_require_a_definition() {
        let source = "[a][missing]\n\n![a][missing]\n\n[missing][]\n\n[missing]\n\n![missing][]\n\n![missing]\n\n```markdown\n[missing]: https://example.com\n```\n";
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language: SyntaxLanguage::Markdown,
            source: source.into(),
            policy: SyntaxPolicy::default(),
        };
        let SyntaxEvent::Parsed(result) = parse(request) else {
            panic!("parse markdown")
        };
        let fence = source.find("```markdown").unwrap();
        assert!(
            result.markdown.iter().all(|span| span.start >= fence),
            "{:?}",
            result.markdown
        );
    }

    #[test]
    fn markdown_resolves_full_collapsed_and_shortcut_references() {
        let links = [
            "[full][label]",
            "[LaBeL][]",
            "[label]",
            "![full][label]",
            "![LaBeL][]",
            "![label]",
            "[spaced][two  words]",
        ];
        let source = format!(
            "{}\n\n[label]: https://first.example\n[label]: https://ignored.example\n[two words]: https://spaced.example\n",
            links.join("\n\n")
        );
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language: SyntaxLanguage::Markdown,
            source: source.clone(),
            policy: SyntaxPolicy::default(),
        };
        let SyntaxEvent::Parsed(result) = parse(request) else {
            panic!("parse markdown")
        };
        let mut start = 0;
        for link in links {
            let url = if link.contains("two ") {
                "https://spaced.example"
            } else {
                "https://first.example"
            };
            let url_start = source.find(url).unwrap();
            let mark = if link.starts_with('!') {
                MarkdownMark::Image
            } else {
                MarkdownMark::Link {
                    url: Some((url_start, url_start + url.len())),
                }
            };
            assert!(
                result
                    .markdown
                    .iter()
                    .any(|span| span.start == start && span.mark == mark),
                "{link}: {:?}",
                result.markdown
            );
            assert!(
                result
                    .markdown
                    .iter()
                    .any(|span| span.end == start + link.len() && span.mark == MarkdownMark::Hide),
                "{link}: {:?}",
                result.markdown
            );
            start += link.len() + 2;
        }
    }

    #[test]
    fn a_fenced_block_is_coloured_as_the_language_it_names() {
        let source = "# Example\n\n```python\ndef area(r: float):\n    return \"x\"\n```\n";
        let spans = highlight(SyntaxLanguage::Markdown, source);
        let role = |word: &str| {
            let at = source.find(word).expect(word);
            spans
                .iter()
                .find(|span| span.start <= at && at < span.end)
                .map_or("", |span| span.role.name())
        };
        assert!(role("def").starts_with("@keyword"), "{}", role("def"));
        assert!(role("\"x\"").starts_with("@string"), "{}", role("\"x\""));
        assert!(spans.windows(2).all(|pair| pair[0].end <= pair[1].start));
    }
    #[test]
    fn quoted_fences_exclude_markdown_continuations_from_injected_code() {
        let source = "> ```bash\n> cat <<EOF\n> hello\n> EOF\n> echo done\n> ```\n";
        let plain = "cat <<EOF\nhello\nEOF\necho done\n";
        let spans = highlight(SyntaxLanguage::Markdown, source);
        let plain_spans = highlight(SyntaxLanguage::Bash, plain);
        let at = source.find("echo").unwrap();
        let plain_at = plain.find("echo").unwrap();
        let expected = plain_spans
            .iter()
            .find(|span| span.start <= plain_at && plain_at < span.end)
            .unwrap()
            .role;
        assert!(!expected.name().starts_with("@string"));
        assert_eq!(
            spans
                .iter()
                .find(|span| span.start <= at && at < span.end)
                .map(|span| span.role),
            Some(expected)
        );
    }

    #[test]
    fn fenced_highlights_obey_the_remaining_capture_budget() {
        let source = format!("```json\n[{}]\n```\n", vec!["1"; 10_000].join(","));
        let mut parser = Parser::new();
        parser
            .set_language(&descriptor(SyntaxLanguage::Markdown).language)
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        for maximum in [0, 1, 3] {
            let (spans, truncated) = markdown_fence_spans(&tree, &source, maximum);
            assert!(spans.len() <= maximum);
            assert!(truncated);
        }
    }

    #[test]
    fn collectors_report_only_actual_overflow_including_before_flattening() {
        let source = "fn main() {\n    foo();\n}\n";
        let policy = SyntaxPolicy {
            maximum_folds: 1,
            maximum_indent_rows: 1,
            ..Default::default()
        };
        let result = parse_with_policy(SyntaxLanguage::Rust, source, policy.clone());
        assert_eq!(result.folds.len(), 1);
        assert_eq!(result.indents.len(), 1);
        assert!(!result.truncated);
        let result = parse_with_policy(SyntaxLanguage::Rust, &format!("{source}{source}"), policy);
        assert!(result.folds_truncated && result.indents_truncated);
        let zero = SyntaxPolicy {
            maximum_spans: 0,
            maximum_folds: 0,
            maximum_indent_rows: 0,
            ..Default::default()
        };
        let result = parse_with_policy(SyntaxLanguage::Rust, "", zero.clone());
        assert!(!result.truncated);
        let result = parse_with_policy(SyntaxLanguage::Rust, source, zero);
        assert!(result.spans.is_empty() && result.folds.is_empty() && result.indents.is_empty());
        assert!(result.highlights_truncated && result.folds_truncated && result.indents_truncated);
        let descriptor = descriptor(SyntaxLanguage::Rust);
        let mut parser = Parser::new();
        parser.set_language(&descriptor.language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let query = Query::new(&descriptor.language, descriptor.highlights).unwrap();
        let (all, overflow) = highlight_spans(&query, &tree, source, usize::MAX, "rust");
        assert!(!overflow);
        let exact = parse_with_policy(
            SyntaxLanguage::Rust,
            source,
            SyntaxPolicy {
                maximum_spans: all.len(),
                ..Default::default()
            },
        );
        assert!(!exact.highlights_truncated);
        let limited = parse_with_policy(
            SyntaxLanguage::Rust,
            source,
            SyntaxPolicy {
                maximum_spans: all.len() - 1,
                ..Default::default()
            },
        );
        assert!(limited.highlights_truncated);
        let query = Query::new(
            &descriptor.language,
            "(identifier) @variable (identifier) @variable (identifier) @variable",
        )
        .unwrap();
        let (spans, overflow) = highlight_spans(&query, &tree, source, 2, "rust");
        assert!(overflow);
        assert!(flatten_spans(spans).len() < 2);
        assert!(!indentation_folds("a\n b\n", 1, 4).1);
        assert!(indentation_folds("a\n b\nc\n d\n", 1, 4).1);
    }

    #[test]
    fn nested_spans_flatten_with_the_inner_one_on_top() {
        let span = |start, end, role| SyntaxSpan { start, end, role };
        let flat = flatten_spans(vec![
            span(0, 10, Highlight::STRING),
            span(3, 5, Highlight::KEYWORD),
            span(12, 14, Highlight::COMMENT),
        ]);
        assert_eq!(
            flat,
            vec![
                span(0, 3, Highlight::STRING),
                span(3, 5, Highlight::KEYWORD),
                span(5, 10, Highlight::STRING),
                span(12, 14, Highlight::COMMENT),
            ]
        );
    }
    #[test]
    fn every_bundled_query_compiles_and_colours_its_language() {
        validate_builtin_queries().expect("bundled queries");
        for (path, word) in [
            ("run.sh", "echo"),
            ("main.py", "def"),
            ("index.html", "div"),
            ("site.css", "color"),
            ("ci.yaml", "name"),
            ("main.c", "return"),
            ("main.cpp", "return"),
            ("flake.nix", "inputs"),
            ("Makefile", "all"),
        ] {
            let source = match path {
                "run.sh" => "echo \"hi\" # note\n",
                "main.py" => "def f():\n    return 1  # note\n",
                "index.html" => "<div class=\"a\">hi</div>\n",
                "site.css" => "a { color: red; }\n",
                "ci.yaml" => "name: build # note\n",
                "main.c" | "main.cpp" => "int main() { return 0; }\n",
                "flake.nix" => "{ inputs = { }; }\n",
                _ => "all:\n\techo hi\n",
            };
            let language = language_for_path(Path::new(path)).expect(path);
            let at = source.find(word).expect(word);
            assert!(
                highlight(language, source)
                    .iter()
                    .any(|span| span.start <= at && at < span.end),
                "{path}: `{word}` is coloured"
            );
        }
    }
    #[test]
    fn toml_values_keep_their_own_colour_next_to_keys() {
        let source = "[package]\nname = \"dust\"\nversion = 2\n";
        let spans = highlight(SyntaxLanguage::Toml, source);
        let role_at = |word: &str| {
            let at = source.find(word).expect(word);
            spans
                .iter()
                .find(|span| span.start <= at && at < span.end)
                .map(|span| span.role.name())
        };
        assert_eq!(role_at("package"), Some("@keyword.toml"));
        assert_eq!(role_at("name"), Some("@type.toml"));
        assert_eq!(role_at("\"dust\""), Some("@string.toml"));
        assert_eq!(role_at("2"), Some("@number.toml"));
    }
    #[test]
    fn scripts_without_an_extension_are_known_by_their_first_line() {
        let script = Path::new("scripts/ui-shot");
        assert_eq!(
            language_for_document(Some(script), "#!/usr/bin/env bash\nset -e\n"),
            Some(SyntaxLanguage::Bash)
        );
        assert_eq!(
            language_for_document(Some(script), "#!/usr/bin/python3 -u\n"),
            Some(SyntaxLanguage::Python)
        );
        assert_eq!(
            language_for_document(Some(script), "#!/usr/bin/env -S node --flag\n"),
            Some(SyntaxLanguage::JavaScript)
        );
        for shebang in [
            "#!/usr/bin/env -S FOO=bar python3",
            "#!/usr/bin/env -u OLD -C /tmp FOO=bar python3",
            "#!/usr/bin/env --unset OLD --chdir /tmp python3",
            "#!/usr/bin/env --unset=OLD --chdir=/tmp FOO=bar python3",
        ] {
            assert_eq!(
                language_for_document(Some(script), shebang),
                Some(SyntaxLanguage::Python),
                "{shebang}"
            );
        }
        assert_eq!(language_for_document(Some(script), "echo hi\n"), None);
        // The file name wins over the first line.
        assert_eq!(
            language_for_document(Some(Path::new("a.py")), "#!/bin/sh\n"),
            Some(SyntaxLanguage::Python)
        );
    }
    #[test]
    fn lock_files_use_the_language_neovim_gives_their_name() {
        for (path, language) in [
            ("flake.lock", SyntaxLanguage::Json),
            ("composer.lock", SyntaxLanguage::Json),
            ("Cargo.lock", SyntaxLanguage::Toml),
            ("uv.lock", SyntaxLanguage::Toml),
            ("yarn.lock", SyntaxLanguage::Yaml),
        ] {
            assert_eq!(language_for_path(Path::new(path)), Some(language), "{path}");
        }
        assert_eq!(language_for_path(Path::new("notes.lock")), None);
    }

    #[test]
    fn uppercase_c_extension_selects_cpp_before_case_folding() {
        assert_eq!(
            language_for_path(Path::new("main.C")),
            Some(SyntaxLanguage::Cpp)
        );
        assert_eq!(
            language_for_path(Path::new("main.c")),
            Some(SyntaxLanguage::C)
        );
        assert_eq!(
            language_for_document(Some(Path::new("main.C")), "class Example {};"),
            Some(SyntaxLanguage::Cpp)
        );
    }

    #[test]
    fn oversized_source_falls_back() {
        let request = Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language: SyntaxLanguage::Rust,
            source: "12345".into(),
            policy: SyntaxPolicy {
                maximum_source_bytes: 2,
                ..SyntaxPolicy::default()
            },
        };
        assert!(matches!(parse(request), SyntaxEvent::Skipped { .. }));
    }
}
