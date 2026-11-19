//! What a markdown line draws in place of its markup, as render-markdown.nvim does in Neovim:
//! heading bars, bullets, checkboxes, quote bars, rules, code blocks and links without their
//! brackets. The marks come from the same trees that colour the text.
use std::collections::BTreeMap;

use tree_sitter::Node;

use tree_sitter::Tree;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkdownMark {
    /// Markup that is not drawn, such as `**`, a code span's backticks or a link's `(url)`.
    Hide,
    /// A heading's `#` marker and the space after it, drawn as the level's icon.
    Heading(u8),
    /// A whole heading, drawn on a bar of the level's colour to the window's edge.
    HeadingLine(u8),
    /// A `-`, `+` or `*` list marker, drawn as the bullet for its nesting depth.
    Bullet(usize),
    /// A task's `[ ]` or `[x]`.
    Checkbox { checked: bool },
    /// A quote's `>`. In a callout, such as `> [!NOTE]`, `callout` is where its `[!NOTE]` starts.
    Quote { callout: Option<usize> },
    /// A callout's `[!NOTE]`, drawn as its icon and title.
    Callout,
    /// Where a link starts, its `[` or `<`, drawn as the icon for where `url` points.
    Link { url: Option<(usize, usize)> },
    /// Where an image starts, its `![`.
    Image,
    /// A code span, `` `code` ``, drawn on the code background.
    InlineCode,
    /// A code block, drawn on its own background to the window's edge.
    Code,
    /// A fenced code block's language name, drawn after the language's icon.
    CodeLanguage,
    /// A `---` break, drawn as a line across the window.
    Rule,
    /// A whole table, whose cells are padded to their column's width.
    Table,
    /// A `|` between a table's cells, in any of its rows.
    TablePipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSpan {
    pub start: usize,
    pub end: usize,
    pub mark: MarkdownMark,
}

/// Marks from the block grammar's tree: headings, lists, quotes, rules and code blocks.
pub(super) fn block_marks(
    tree: &Tree,
    source: &str,
    maximum: usize,
    marks: &mut Vec<MarkdownSpan>,
) {
    let mut push = |start: usize, end: usize, mark| {
        if marks.len() < maximum
            && start < end
            && source.is_char_boundary(start)
            && source.is_char_boundary(end)
        {
            marks.push(MarkdownSpan { start, end, mark });
        }
    };
    let mut walk = tree.walk();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "atx_heading" => {
                let marker = node
                    .child(0)
                    .filter(|marker| marker.kind().starts_with("atx_h"));
                if let Some(marker) = marker {
                    let level = marker
                        .kind()
                        .as_bytes()
                        .get(5)
                        .map_or(1, |digit| digit.saturating_sub(b'0'));
                    let end = marker.end_byte()
                        + usize::from(source.as_bytes().get(marker.end_byte()) == Some(&b' '));
                    push(marker.start_byte(), end, MarkdownMark::Heading(level));
                    push(
                        node.start_byte(),
                        line_end(source, node.start_byte()),
                        MarkdownMark::HeadingLine(level),
                    );
                }
            }
            "list_marker_minus" | "list_marker_plus" | "list_marker_star" => {
                let task = node
                    .next_named_sibling()
                    .filter(|next| next.kind().starts_with("task_list_marker"));
                if let Some(task) = task {
                    // A task shows only its checkbox, as a bullet before it would be noise.
                    push(node.start_byte(), task.start_byte(), MarkdownMark::Hide);
                } else {
                    let start = node.start_byte()
                        + leading_spaces(source, node.start_byte(), node.end_byte());
                    push(start, start + 1, MarkdownMark::Bullet(list_depth(node)));
                }
            }
            "task_list_marker_unchecked" => {
                push(
                    node.start_byte(),
                    node.end_byte(),
                    MarkdownMark::Checkbox { checked: false },
                );
            }
            "task_list_marker_checked" => {
                push(
                    node.start_byte(),
                    node.end_byte(),
                    MarkdownMark::Checkbox { checked: true },
                );
            }
            "block_quote" => {
                if let Some(callout) = callout(node, source) {
                    let end = source[callout..]
                        .find(']')
                        .map_or(callout, |at| callout + at + 1);
                    push(callout, end, MarkdownMark::Callout);
                }
            }
            "block_quote_marker" | "block_continuation" => {
                let quote = ancestor(node, "block_quote");
                let callout = quote.and_then(|quote| callout(quote, source));
                let text = source.get(node.byte_range()).unwrap_or_default();
                for (at, _) in text.match_indices('>') {
                    let at = node.start_byte() + at;
                    push(at, at + 1, MarkdownMark::Quote { callout });
                }
            }
            "pipe_table" => push(node.start_byte(), node.end_byte(), MarkdownMark::Table),
            "pipe_table_header" | "pipe_table_delimiter_row" | "pipe_table_row" => {
                let mut walk = node.walk();
                for pipe in node.children(&mut walk).filter(|child| child.kind() == "|") {
                    push(pipe.start_byte(), pipe.end_byte(), MarkdownMark::TablePipe);
                }
            }
            "thematic_break" => push(
                node.start_byte(),
                line_end(source, node.start_byte()),
                MarkdownMark::Rule,
            ),
            "fenced_code_block" | "indented_code_block" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Code);
            }
            "fenced_code_block_delimiter" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Hide)
            }
            "info_string" => {
                if let Some(language) = node
                    .named_child(0)
                    .filter(|child| child.kind() == "language")
                {
                    push(
                        language.start_byte(),
                        language.end_byte(),
                        MarkdownMark::CodeLanguage,
                    );
                }
            }
            _ => {}
        }
        nodes.extend(node.named_children(&mut walk));
    }
}

/// Reference definitions belong to the block tree, not the separately parsed inline trees.
pub(super) fn references(tree: &Tree, source: &str) -> BTreeMap<String, (usize, usize)> {
    let mut references = BTreeMap::new();
    let mut nodes = vec![tree.root_node()];
    let mut walk = tree.walk();
    while let Some(node) = nodes.pop() {
        if node.kind() == "link_reference_definition" {
            if let (Some(label), Some(destination)) = (
                named_child(node, "link_label"),
                named_child(node, "link_destination"),
            ) {
                references
                    .entry(reference_label(
                        &source[label.start_byte() + 1..label.end_byte() - 1],
                    ))
                    .or_insert((destination.start_byte(), destination.end_byte()));
            }
        } else {
            // Preserve document order: the first definition of a label wins.
            let start = nodes.len();
            nodes.extend(node.named_children(&mut walk));
            nodes[start..].reverse();
        }
    }
    references
}

fn reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
        .to_lowercase()
}

/// Marks from an inline tree: emphasis and code markers, links and escapes.
pub(super) fn inline_marks(
    tree: &Tree,
    source: &str,
    references: &BTreeMap<String, (usize, usize)>,
    maximum: usize,
    marks: &mut Vec<MarkdownSpan>,
) {
    let mut push = |start: usize, end: usize, mark| {
        if marks.len() < maximum
            && start < end
            && source.is_char_boundary(start)
            && source.is_char_boundary(end)
        {
            marks.push(MarkdownSpan { start, end, mark });
        }
    };
    let mut walk = tree.walk();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "emphasis_delimiter" | "code_span_delimiter" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Hide);
            }
            "code_span" => push(node.start_byte(), node.end_byte(), MarkdownMark::InlineCode),
            "backslash_escape" => {
                push(node.start_byte(), node.start_byte() + 1, MarkdownMark::Hide)
            }
            "inline_link"
            | "full_reference_link"
            | "collapsed_reference_link"
            | "shortcut_link"
            | "image" => {
                let text = named_child(node, "link_text")
                    .or_else(|| named_child(node, "image_description"));
                if let Some(text) = text {
                    let mut url = named_child(node, "link_destination")
                        .map(|url| (url.start_byte(), url.end_byte()));
                    let inline = node.kind() == "inline_link"
                        || (node.kind() == "image"
                            && source[text.end_byte()..node.end_byte()].starts_with("]("));
                    if !inline {
                        let label = named_child(node, "link_label")
                            .map(|label| &source[label.start_byte() + 1..label.end_byte() - 1])
                            .unwrap_or(&source[text.byte_range()]);
                        url = references.get(&reference_label(label)).copied();
                        if url.is_none() {
                            nodes.extend(node.named_children(&mut walk));
                            continue;
                        }
                    }
                    let mark = if node.kind() == "image" {
                        MarkdownMark::Image
                    } else {
                        MarkdownMark::Link { url }
                    };
                    push(node.start_byte(), text.start_byte(), mark);
                    push(text.end_byte(), node.end_byte(), MarkdownMark::Hide);
                }
            }
            "uri_autolink" | "email_autolink" if node.end_byte() > node.start_byte() + 2 => {
                let url = Some((node.start_byte() + 1, node.end_byte() - 1));
                push(
                    node.start_byte(),
                    node.start_byte() + 1,
                    MarkdownMark::Link { url },
                );
                push(node.end_byte() - 1, node.end_byte(), MarkdownMark::Hide);
            }
            _ => {}
        }
        nodes.extend(node.named_children(&mut walk));
    }
}

/// Where a quote's `[!NOTE]` starts, when its first line is a callout as GitHub writes them.
fn callout(quote: Node<'_>, source: &str) -> Option<usize> {
    let text = source.get(quote.byte_range())?;
    let line = text.lines().next()?;
    let title = line.trim_start_matches(['>', ' ']);
    let name = title.strip_prefix("[!")?.split_once(']')?.0;
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphabetic()))
    .then(|| quote.start_byte() + (line.len() - title.len()))
}

fn ancestor<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut parent = node.parent();
    while let Some(node) = parent {
        if node.kind() == kind {
            return Some(node);
        }
        parent = node.parent();
    }
    None
}

fn named_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .find(|child| child.kind() == kind)
}

/// How many lists hold this marker's list, from zero.
fn list_depth(marker: Node<'_>) -> usize {
    let mut depth = 0usize;
    let mut parent = marker.parent();
    while let Some(node) = parent {
        if node.kind() == "list" {
            depth = depth.saturating_add(1);
        }
        parent = node.parent();
    }
    depth.saturating_sub(1)
}

fn leading_spaces(source: &str, start: usize, end: usize) -> usize {
    source
        .get(start..end)
        .map_or(0, |text| text.len() - text.trim_start().len())
}

fn line_end(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map_or(source.len(), |at| start + at)
}
