//! Queries and node ancestry on the same parsed trees used for highlighting.

use super::SyntaxResult;
use std::ops::Range;
use thiserror::Error;
use tree_sitter::StreamingIterator;

#[derive(Debug, Error)]
pub(crate) enum SyntaxQueryError {
    #[error("invalid syntax byte range")]
    Range,
    #[error("syntax query limit exceeded")]
    Limit,
    #[error("invalid syntax query: {0}")]
    Query(#[from] tree_sitter::QueryError),
    #[error("no syntax node at position")]
    NoNode,
}

#[derive(Debug)]
pub(crate) struct Capture {
    pub name: String,
    pub range: Range<usize>,
}

#[derive(Debug)]
pub(crate) struct Node {
    pub kind: String,
    pub range: Range<usize>,
}

impl SyntaxResult {
    pub(crate) fn query(
        &self,
        source: &str,
        range: Option<Range<usize>>,
    ) -> Result<Vec<Capture>, SyntaxQueryError> {
        if source.len() > 16 * 1024 {
            return Err(SyntaxQueryError::Limit);
        }
        let query = tree_sitter::Query::new(&self.tree.language(), source)?;
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_match_limit(4096);
        if let Some(range) = range {
            if range.start > range.end
                || range.end > self.source.len()
                || !self.source.is_char_boundary(range.start)
                || !self.source.is_char_boundary(range.end)
            {
                return Err(SyntaxQueryError::Range);
            }
            cursor.set_byte_range(range);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(20);
        let mut expired = false;
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            expired = std::time::Instant::now() >= deadline;
            expired
        };
        let options = tree_sitter::QueryCursorOptions::new().progress_callback(&mut progress);
        let mut captures = cursor.captures_with_options(
            &query,
            self.tree.root_node(),
            self.source.as_bytes(),
            options,
        );
        let mut result = Vec::new();
        while let Some((matched, index)) = captures.next() {
            if result.len() >= 4096 {
                return Err(SyntaxQueryError::Limit);
            }
            let capture = matched.captures[*index];
            result.push(Capture {
                name: query.capture_names()[capture.index as usize].into(),
                range: capture.node.byte_range(),
            });
        }
        drop(captures);
        if expired || cursor.did_exceed_match_limit() {
            return Err(SyntaxQueryError::Limit);
        }
        Ok(result)
    }

    pub(crate) fn node_at(&self, position: usize) -> Result<Vec<Node>, SyntaxQueryError> {
        if position > self.source.len() || !self.source.is_char_boundary(position) {
            return Err(SyntaxQueryError::Range);
        }
        let mut node = self
            .tree
            .root_node()
            .descendant_for_byte_range(position, position)
            .ok_or(SyntaxQueryError::NoNode)?;
        let mut nodes = Vec::new();
        loop {
            if nodes.len() >= 256 {
                return Err(SyntaxQueryError::Limit);
            }
            nodes.push(Node {
                kind: node.kind().into(),
                range: node.byte_range(),
            });
            let Some(parent) = node.parent() else {
                break;
            };
            node = parent;
        }
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use crate::language::syntax::{
        Request, SyntaxDocumentId, SyntaxEvent, SyntaxLanguage, SyntaxPolicy, parse,
    };

    #[test]
    fn queries_share_the_highlighting_tree_and_return_byte_ranges_and_parents() {
        let event = parse(Request {
            document: SyntaxDocumentId(1),
            request_id: 1,
            revision: 1,
            language: SyntaxLanguage::Rust,
            source: "fn hello() {}".into(),
            policy: SyntaxPolicy::default(),
        });
        let SyntaxEvent::Parsed(parsed) = event else {
            panic!("parse failed")
        };
        let captures = parsed.query("(identifier) @name", None).unwrap();
        assert_eq!(captures[0].name, "name");
        assert_eq!(captures[0].range, 3..8);
        assert!(
            parsed
                .query("(identifier) @name", Some(9..13))
                .unwrap()
                .is_empty()
        );
        let nodes = parsed.node_at(4).unwrap();
        assert_eq!(nodes[0].kind, "identifier");
        assert_eq!(nodes.last().unwrap().kind, "source_file");
        assert!(parsed.query("(", None).is_err());
        assert!(parsed.node_at(100).is_err());
    }
}
