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
}
impl SyntaxLanguage {
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
const BUILTIN_LANGUAGES: [SyntaxLanguage; 8] = [
    SyntaxLanguage::Rust,
    SyntaxLanguage::TypeScript,
    SyntaxLanguage::Tsx,
    SyntaxLanguage::JavaScript,
    SyntaxLanguage::Go,
    SyntaxLanguage::Json,
    SyntaxLanguage::Toml,
    SyntaxLanguage::Markdown,
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
            if role(capture).is_none() && capture.split('.').next() != Some("none") {
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
        _ => SyntaxCapabilities {
            highlights: SyntaxSupport::Full,
            folds: SyntaxSupport::Full,
            indents: SyntaxSupport::Full,
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
            highlights: include_str!("../../assets/syntax/rust/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/rust/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/rust/indents.scm")),
        },
        SyntaxLanguage::TypeScript => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            highlights: include_str!("../../assets/syntax/typescript/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/typescript/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/typescript/indents.scm")),
        },
        SyntaxLanguage::Tsx => Descriptor {
            language: tree_sitter_typescript::LANGUAGE_TSX.into(),
            highlights: include_str!("../../assets/syntax/tsx/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/tsx/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/tsx/indents.scm")),
        },
        SyntaxLanguage::JavaScript => Descriptor {
            language: tree_sitter_javascript::LANGUAGE.into(),
            highlights: include_str!("../../assets/syntax/javascript/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/javascript/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/javascript/indents.scm")),
        },
        SyntaxLanguage::Go => Descriptor {
            language: tree_sitter_go::LANGUAGE.into(),
            highlights: include_str!("../../assets/syntax/go/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/go/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/go/indents.scm")),
        },
        SyntaxLanguage::Json => Descriptor {
            language: tree_sitter_json::LANGUAGE.into(),
            highlights: include_str!("../../assets/syntax/json/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/json/folds.scm")),
            indents: Some(include_str!("../../assets/syntax/json/indents.scm")),
        },
        SyntaxLanguage::Toml => Descriptor {
            language: tree_sitter_toml_ng::LANGUAGE.into(),
            highlights: include_str!("../../assets/syntax/toml/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/toml/folds.scm")),
            indents: None,
        },
        SyntaxLanguage::Markdown => Descriptor {
            language: tree_sitter_md_025::LANGUAGE.into(),
            highlights: include_str!("../../assets/syntax/markdown/highlights.scm"),
            folds: Some(include_str!("../../assets/syntax/markdown/folds.scm")),
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
#[must_use]
pub fn language_for_path(path: &Path) -> Option<SyntaxLanguage> {
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
        _ => None,
    }
}
fn role(name: &str) -> Option<SyntaxRole> {
    let root = name.split('.').next().unwrap_or(name);
    match root {
        "none" => None,
        "comment" => Some(SyntaxRole::Comment),
        "string" | "character" | "text" | "escape" => Some(SyntaxRole::String),
        "number" | "float" => Some(SyntaxRole::Number),
        "keyword" | "operator" | "punctuation" | "tag" | "label" | "boolean" => {
            Some(SyntaxRole::Keyword)
        }
        "type" | "constructor" | "module" | "namespace" => Some(SyntaxRole::Type),
        "variable" | "function" | "method" | "property" | "constant" | "attribute" | "embedded" => {
            Some(SyntaxRole::Variable)
        }
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
    let query = match Query::new(&descriptor.language, descriptor.highlights) {
        Ok(query) => query,
        Err(error) => return failed(&request, format!("highlight query: {error}")),
    };
    let mut cursor = QueryCursor::new();
    let names = query.capture_names();
    let mut matches = cursor.matches(&query, tree.root_node(), request.source.as_bytes());
    let mut spans = Vec::new();
    while let Some(item) = matches.next() {
        for capture in item.captures {
            if spans.len() >= request.policy.maximum_spans {
                break;
            }
            if let Some(role) = role(names[capture.index as usize]) {
                let range = capture.node.byte_range();
                if request.source.is_char_boundary(range.start)
                    && request.source.is_char_boundary(range.end)
                {
                    spans.push(SyntaxSpan {
                        start: range.start,
                        end: range.end,
                        role,
                    });
                }
            }
        }
    }
    let folds = match query_rows(
        &descriptor.language,
        descriptor.folds,
        &tree,
        &request.source,
        request.policy.maximum_folds,
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
            let end = capture.node.end_position().row;
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
