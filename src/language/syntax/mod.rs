#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};
use thiserror::Error;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

mod objects;
pub use objects::{SyntaxObject, SyntaxObjectKind};

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
            if role(capture).is_none() && !uncoloured_capture(capture) {
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
        SyntaxLanguage::Toml => SyntaxCapabilities {
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
            highlights: include_str!("../../../assets/syntax/rust/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/rust/folds.scm")),
            indents: Some(include_str!("../../../assets/syntax/rust/indents.scm")),
        },
        SyntaxLanguage::TypeScript => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlights: include_str!("../../../assets/syntax/typescript/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/typescript/folds.scm")),
            indents: Some(include_str!(
                "../../../assets/syntax/typescript/indents.scm"
            )),
        },
        SyntaxLanguage::Tsx => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlights: include_str!("../../../assets/syntax/tsx/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/tsx/folds.scm")),
            indents: Some(include_str!("../../../assets/syntax/tsx/indents.scm")),
        },
        SyntaxLanguage::JavaScript => Descriptor {
            language: tree_sitter_javascript::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/javascript/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/javascript/folds.scm")),
            indents: Some(include_str!(
                "../../../assets/syntax/javascript/indents.scm"
            )),
        },
        SyntaxLanguage::Go => Descriptor {
            language: tree_sitter_go::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/go/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/go/folds.scm")),
            indents: Some(include_str!("../../../assets/syntax/go/indents.scm")),
        },
        SyntaxLanguage::Json => Descriptor {
            language: tree_sitter_json::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/json/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/json/folds.scm")),
            indents: Some(include_str!("../../../assets/syntax/json/indents.scm")),
        },
        SyntaxLanguage::Toml => Descriptor {
            language: tree_sitter_toml_ng::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/toml/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/toml/folds.scm")),
            indents: None,
        },
        SyntaxLanguage::Markdown => Descriptor {
            language: tree_sitter_md_025::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/markdown/highlights.scm"),
            folds: Some(include_str!("../../../assets/syntax/markdown/folds.scm")),
            indents: None,
        },
        SyntaxLanguage::Bash => Descriptor {
            language: tree_sitter_bash::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/bash/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::C => Descriptor {
            language: tree_sitter_c::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/c/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Cpp => Descriptor {
            language: tree_sitter_cpp::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/cpp/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::CSharp => Descriptor {
            language: tree_sitter_c_sharp::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/c_sharp/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Css => Descriptor {
            language: tree_sitter_css::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/css/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Html => Descriptor {
            language: tree_sitter_html::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/html/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Python => Descriptor {
            language: tree_sitter_python::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/python/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Yaml => Descriptor {
            language: tree_sitter_yaml::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/yaml/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Lua => Descriptor {
            language: tree_sitter_lua::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/lua/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Nix => Descriptor {
            language: tree_sitter_nix::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/nix/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Java => Descriptor {
            language: tree_sitter_java::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/java/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Ruby => Descriptor {
            language: tree_sitter_ruby::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/ruby/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Php => Descriptor {
            language: tree_sitter_php::LANGUAGE_PHP.into(),
            highlights: include_str!("../../../assets/syntax/php/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Zig => Descriptor {
            language: tree_sitter_zig::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/zig/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Haskell => Descriptor {
            language: tree_sitter_haskell::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/haskell/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Elixir => Descriptor {
            language: tree_sitter_elixir::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/elixir/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::OCaml => Descriptor {
            language: tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            highlights: include_str!("../../../assets/syntax/ocaml/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Scala => Descriptor {
            language: tree_sitter_scala::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/scala/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Swift => Descriptor {
            language: tree_sitter_swift::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/swift/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Kotlin => Descriptor {
            language: tree_sitter_kotlin_sg::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/kotlin/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Dart => Descriptor {
            language: tree_sitter_dart::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/dart/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Make => Descriptor {
            language: tree_sitter_make::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/make/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::CMake => Descriptor {
            language: tree_sitter_cmake::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/cmake/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Xml => Descriptor {
            language: tree_sitter_xml::LANGUAGE_XML.into(),
            highlights: include_str!("../../../assets/syntax/xml/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Diff => Descriptor {
            language: tree_sitter_diff::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/diff/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Ini => Descriptor {
            language: tree_sitter_ini::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/ini/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Proto => Descriptor {
            language: tree_sitter_proto::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/proto/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Sql => Descriptor {
            language: tree_sitter_sequel::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/sql/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Erlang => Descriptor {
            language: tree_sitter_erlang::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/erlang/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::R => Descriptor {
            language: tree_sitter_r::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/r/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Julia => Descriptor {
            language: tree_sitter_julia::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/julia/highlights.scm"),
            folds: None,
            indents: None,
        },
        SyntaxLanguage::Vue => Descriptor {
            language: tree_sitter_vue_next::LANGUAGE.into(),
            highlights: include_str!("../../../assets/syntax/vue/highlights.scm"),
            folds: None,
            indents: None,
        },
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxRole {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Variable,
    /// Markup: headings, bold and italic text, code, links and quotes.
    Heading,
    Strong,
    Emphasis,
    Code,
    Link,
    Quote,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxSpan {
    pub start: usize,
    pub end: usize,
    pub role: SyntaxRole,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxResult {
    pub document: SyntaxDocumentId,
    pub generation: u64,
    pub revision: u64,
    pub language: SyntaxLanguage,
    pub capabilities: SyntaxCapabilities,
    pub spans: Vec<SyntaxSpan>,
    pub folds: Vec<SyntaxFold>,
    pub indents: Vec<SyntaxIndent>,
    /// Functions, classes and arguments for the tree-sitter text objects.
    pub objects: Arc<Vec<SyntaxObject>>,
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
}
impl Default for SyntaxPolicy {
    fn default() -> Self {
        Self {
            maximum_source_bytes: 8 * 1024 * 1024,
            maximum_spans: 100_000,
            maximum_folds: 10_000,
            maximum_indent_rows: 100_000,
        }
    }
}
#[derive(Debug)]
pub enum SyntaxEvent {
    Parsed(SyntaxResult),
    Failed {
        document: SyntaxDocumentId,
        generation: u64,
        revision: u64,
        language: SyntaxLanguage,
        diagnostic: String,
    },
    Skipped {
        document: SyntaxDocumentId,
        generation: u64,
        revision: u64,
        language: SyntaxLanguage,
    },
}
#[derive(Debug)]
struct Request {
    document: SyntaxDocumentId,
    generation: u64,
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
pub struct SyntaxService {
    sender: SyncSender<Request>,
    receiver: Receiver<SyntaxEvent>,
    latest: Arc<Mutex<HashMap<SyntaxDocumentId, u64>>>,
}
impl SyntaxService {
    pub fn start(capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity.max(1));
        let (etx, erx) = mpsc::sync_channel(capacity.max(1));
        let latest = Arc::new(Mutex::new(HashMap::new()));
        let worker = latest.clone();
        thread::Builder::new()
            .name("dust-syntax".into())
            .spawn(move || {
                while let Ok(request) = rx.recv() {
                    if worker
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&request.document).copied())
                        != Some(request.generation)
                    {
                        continue;
                    }
                    let document = request.document;
                    let generation = request.generation;
                    let event = parse(request);
                    if worker
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&document).copied())
                        == Some(generation)
                    {
                        let _ = etx.try_send(event);
                    }
                }
            })
            .expect("syntax worker");
        Self {
            sender: tx,
            receiver: erx,
            latest,
        }
    }
    pub fn submit(
        &self,
        document: SyntaxDocumentId,
        generation: u64,
        revision: u64,
        language: SyntaxLanguage,
        source: String,
        policy: SyntaxPolicy,
    ) -> Result<(), SyntaxError> {
        if let Ok(mut latest) = self.latest.lock() {
            latest.insert(document, generation);
        }
        self.sender
            .try_send(Request {
                document,
                generation,
                revision,
                language,
                source,
                policy,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => SyntaxError::QueueFull,
                TrySendError::Disconnected(_) => SyntaxError::Stopped,
            })
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
fn highlight_spans(query: &Query, tree: &tree_sitter::Tree, source: &str, maximum: usize) -> Vec<SyntaxSpan> {
    let mut cursor = QueryCursor::new();
    let names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut spans = Vec::new();
    while let Some(item) = matches.next() {
        for capture in item.captures {
            if spans.len() >= maximum {
                break;
            }
            if let Some(role) = role(names[capture.index as usize]) {
                let range = capture.node.byte_range();
                if source.is_char_boundary(range.start) && source.is_char_boundary(range.end) {
                    spans.push(SyntaxSpan {
                        start: range.start,
                        end: range.end,
                        role,
                    });
                }
            }
        }
    }
    spans
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
    QUERIES.with_borrow_mut(|queries| {
        let query = match queries.entry(language) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                match Query::new(&descriptor.language, descriptor.highlights) {
                    Ok(query) => entry.insert(query),
                    Err(_) => return Vec::new(),
                }
            }
        };
        highlight_spans(query, &tree, source, usize::MAX)
    })
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
        other => language_for_path(Path::new(&format!("fence.{other}"))),
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
            program = words.find(|word| !word.starts_with('-'))?;
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
        Some(".bashrc") | Some(".bash_profile") | Some(".zshrc") | Some(".profile") | Some("PKGBUILD") => return Some(SyntaxLanguage::Bash),
        Some("Gemfile") | Some("Rakefile") => return Some(SyntaxLanguage::Ruby),
        Some("Makefile") | Some("makefile") | Some("GNUmakefile") => return Some(SyntaxLanguage::Make),
        Some("CMakeLists.txt") => return Some(SyntaxLanguage::CMake),
        Some(".gitconfig") | Some(".editorconfig") => return Some(SyntaxLanguage::Ini),
        _ => {}
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
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
fn role(name: &str) -> Option<SyntaxRole> {
    match name {
        "text.title" => return Some(SyntaxRole::Heading),
        "text.literal" => return Some(SyntaxRole::Code),
        "text.uri" | "text.reference" => return Some(SyntaxRole::Link),
        _ => {}
    }
    let root = name.split('.').next().unwrap_or(name);
    match root {
        "none" => None,
        "comment" => Some(SyntaxRole::Comment),
        "string" | "character" | "text" | "escape" => Some(SyntaxRole::String),
        "number" | "float" => Some(SyntaxRole::Number),
        "keyword" | "operator" | "punctuation" | "tag" | "label" | "boolean" | "conditional"
        | "repeat" | "exception" | "include" | "import" | "preproc" | "storageclass"
        | "delimiter" | "charset" | "keyframes" | "media" | "supports" => Some(SyntaxRole::Keyword),
        "type" | "constructor" | "module" | "namespace" => Some(SyntaxRole::Type),
        "variable" | "function" | "method" | "property" | "constant" | "attribute" | "embedded"
        | "field" | "parameter" => Some(SyntaxRole::Variable),
        "markup" => Some(match name {
            "markup.heading" => SyntaxRole::Heading,
            "markup.strong" | "markup.bold" => SyntaxRole::Strong,
            "markup.italic" | "markup.emphasis" => SyntaxRole::Emphasis,
            "markup.link" | "markup.link.url" => SyntaxRole::Link,
            "markup.quote" => SyntaxRole::Quote,
            _ => SyntaxRole::String,
        }),
        _ => None,
    }
}
fn parse(request: Request) -> SyntaxEvent {
    if request.source.len() > request.policy.maximum_source_bytes {
        return SyntaxEvent::Skipped {
            document: request.document,
            generation: request.generation,
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
    let spans = QUERIES.with_borrow_mut(|queries| {
        let query = match queries.entry(request.language) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Query::new(&descriptor.language, descriptor.highlights)?)
            }
        };
        Ok(highlight_spans(query, &tree, &request.source, request.policy.maximum_spans))
    });
    let spans = match spans {
        Ok(spans) => spans,
        Err(error) => {
            let error: tree_sitter::QueryError = error;
            return failed(&request, format!("highlight query: {error}"));
        }
    };
    let folds = match descriptor.folds.map_or_else(
        || Ok(indentation_folds(&request.source, request.policy.maximum_folds)),
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
    let indents = match query_rows(
        &descriptor.language,
        descriptor.indents,
        &tree,
        &request.source,
        request.policy.maximum_indent_rows,
    ) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|range| SyntaxIndent {
                start_row: range.start_row,
                end_row: range.end_row,
            })
            .collect::<Vec<_>>(),
        Err(error) => return failed(&request, format!("indent query: {error}")),
    };
    let objects = Arc::new(objects::collect(&tree, &request.source, 50_000));
    SyntaxEvent::Parsed(SyntaxResult {
        document: request.document,
        generation: request.generation,
        revision: request.revision,
        language: request.language,
        capabilities: language_info(request.language).capabilities,
        highlights_truncated: spans.len() >= request.policy.maximum_spans,
        folds_truncated: folds.len() >= request.policy.maximum_folds,
        indents_truncated: indents.len() >= request.policy.maximum_indent_rows,
        truncated: spans.len() >= request.policy.maximum_spans
            || folds.len() >= request.policy.maximum_folds
            || indents.len() >= request.policy.maximum_indent_rows,
        spans,
        folds,
        indents,
        objects,
    })
}
fn query_rows(
    language: &Language,
    source_query: Option<&str>,
    tree: &tree_sitter::Tree,
    source: &str,
    limit: usize,
) -> Result<Vec<SyntaxFold>, String> {
    let Some(source_query) = source_query else {
        return Ok(Vec::new());
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
            let end = end_position
                .row
                .saturating_sub(usize::from(end_position.column == 0));
            if end > start && seen.insert((start, end)) {
                output.push(SyntaxFold {
                    start_row: start,
                    end_row: end,
                });
                if output.len() >= limit {
                    return Ok(output);
                }
            }
        }
    }
    Ok(output)
}
/// Folds what is indented under a line, as Vim's `foldmethod=indent`, for grammars with no fold
/// query. The line closing a block at its own indentation folds with it.
fn indentation_folds(source: &str, limit: usize) -> Vec<SyntaxFold> {
    let mut folds = Vec::new();
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut previous = 0;
    for (row, line) in source.lines().enumerate() {
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let indent = line.len() - body.len();
        let closes = body.starts_with(['}', ')', ']'])
            || body.starts_with("</")
            || body.split(|c: char| !c.is_alphanumeric()).next() == Some("end");
        while let Some(&(opened, start)) = open.last().filter(|(opened, _)| *opened >= indent) {
            open.pop();
            let end = if opened == indent && closes { row } else { previous };
            if end > start {
                folds.push(SyntaxFold { start_row: start, end_row: end });
            }
        }
        open.push((indent, row));
        previous = row;
    }
    for (_, start) in open {
        if previous > start {
            folds.push(SyntaxFold { start_row: start, end_row: previous });
        }
    }
    folds.sort_by_key(|fold| fold.start_row);
    folds.truncate(limit);
    folds
}
fn failed(request: &Request, diagnostic: String) -> SyntaxEvent {
    SyntaxEvent::Failed {
        document: request.document,
        generation: request.generation,
        revision: request.revision,
        language: request.language,
        diagnostic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn languages_without_a_fold_query_fold_by_indentation() {
        let rows = |source: &str| {
            indentation_folds(source, 10)
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
        assert_eq!(indentation_folds("a\n b\nc\n d\n", 1).len(), 1);
    }
    #[test]
    fn a_toml_table_fold_stops_before_the_next_table() {
        let source = "[app]\na = 1\nb = 2\n[app.flags]\nenabled = true\n";
        let request = Request {
            document: SyntaxDocumentId(1),
            generation: 2,
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
            folds.iter().any(|fold| (fold.start_row, fold.end_row) == (0, 2)),
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
                generation: 2,
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
                highlight(language, source).iter().any(|span| span.start <= at && at < span.end),
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
            spans.iter().find(|span| span.start <= at && at < span.end).map(|span| span.role)
        };
        assert_eq!(role_at("package"), Some(SyntaxRole::Keyword));
        assert_eq!(role_at("name"), Some(SyntaxRole::Type));
        assert_eq!(role_at("\"dust\""), Some(SyntaxRole::String));
        assert_eq!(role_at("2"), Some(SyntaxRole::Number));
    }
    #[test]
    fn scripts_without_an_extension_are_known_by_their_first_line() {
        let script = Path::new("scripts/ui-shot");
        assert_eq!(language_for_document(Some(script), "#!/usr/bin/env bash\nset -e\n"), Some(SyntaxLanguage::Bash));
        assert_eq!(language_for_document(Some(script), "#!/usr/bin/python3 -u\n"), Some(SyntaxLanguage::Python));
        assert_eq!(language_for_document(Some(script), "#!/usr/bin/env -S node --flag\n"), Some(SyntaxLanguage::JavaScript));
        assert_eq!(language_for_document(Some(script), "echo hi\n"), None);
        // The file name wins over the first line.
        assert_eq!(language_for_document(Some(Path::new("a.py")), "#!/bin/sh\n"), Some(SyntaxLanguage::Python));
    }
    #[test]
    fn oversized_source_falls_back() {
        let request = Request {
            document: SyntaxDocumentId(1),
            generation: 1,
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
