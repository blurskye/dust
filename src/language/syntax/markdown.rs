//! What a markdown line draws in place of its markup, as render-markdown.nvim does in Neovim:
//! heading bars, bullets, checkboxes, quote bars, rules, code blocks and links without their
//! brackets. The marks come from the same trees that colour the text.
use tree_sitter::{Node, Tree};

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
    /// A quote's `>`.
    Quote,
    /// Where a link starts, its `[` or `<`, drawn as a link icon.
    Link,
    /// Where an image starts, its `![`.
    Image,
    /// A code block, drawn on its own background to the window's edge.
    Code,
    /// A `---` break, drawn as a line across the window.
    Rule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownSpan {
    pub start: usize,
    pub end: usize,
    pub mark: MarkdownMark,
}

/// Marks from the block grammar's tree: headings, lists, quotes, rules and code blocks.
pub(super) fn block_marks(tree: &Tree, source: &str, maximum: usize, marks: &mut Vec<MarkdownSpan>) {
    let mut push = |start: usize, end: usize, mark| {
        if marks.len() < maximum && start < end && source.is_char_boundary(start) && source.is_char_boundary(end) {
            marks.push(MarkdownSpan { start, end, mark });
        }
    };
    let mut walk = tree.walk();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "atx_heading" => {
                let marker = node.child(0).filter(|marker| marker.kind().starts_with("atx_h"));
                if let Some(marker) = marker {
                    let level = marker.kind().as_bytes().get(5).map_or(1, |digit| digit.saturating_sub(b'0'));
                    let end = marker.end_byte() + usize::from(source.as_bytes().get(marker.end_byte()) == Some(&b' '));
                    push(marker.start_byte(), end, MarkdownMark::Heading(level));
                    push(node.start_byte(), line_end(source, node.start_byte()), MarkdownMark::HeadingLine(level));
                }
            }
            "list_marker_minus" | "list_marker_plus" | "list_marker_star" => {
                let task = node.next_named_sibling().filter(|next| next.kind().starts_with("task_list_marker"));
                if let Some(task) = task {
                    // A task shows only its checkbox, as a bullet before it would be noise.
                    push(node.start_byte(), task.start_byte(), MarkdownMark::Hide);
                } else {
                    let start = node.start_byte() + leading_spaces(source, node.start_byte(), node.end_byte());
                    push(start, start + 1, MarkdownMark::Bullet(list_depth(node)));
                }
            }
            "task_list_marker_unchecked" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Checkbox { checked: false });
            }
            "task_list_marker_checked" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Checkbox { checked: true });
            }
            "block_quote_marker" | "block_continuation" => {
                let text = source.get(node.byte_range()).unwrap_or_default();
                for (at, _) in text.match_indices('>') {
                    push(node.start_byte() + at, node.start_byte() + at + 1, MarkdownMark::Quote);
                }
            }
            "thematic_break" => push(node.start_byte(), line_end(source, node.start_byte()), MarkdownMark::Rule),
            "fenced_code_block" | "indented_code_block" => {
                push(node.start_byte(), node.end_byte(), MarkdownMark::Code);
            }
            "fenced_code_block_delimiter" => push(node.start_byte(), node.end_byte(), MarkdownMark::Hide),
            _ => {}
        }
        nodes.extend(node.named_children(&mut walk));
    }
}

/// Marks from an inline tree: emphasis and code markers, links and escapes.
pub(super) fn inline_marks(tree: &Tree, source: &str, maximum: usize, marks: &mut Vec<MarkdownSpan>) {
    let mut push = |start: usize, end: usize, mark| {
        if marks.len() < maximum && start < end && source.is_char_boundary(start) && source.is_char_boundary(end) {
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
            "backslash_escape" => push(node.start_byte(), node.start_byte() + 1, MarkdownMark::Hide),
            "inline_link" | "full_reference_link" | "image" => {
                let text = named_child(node, "link_text").or_else(|| named_child(node, "image_description"));
                if let Some(text) = text {
                    let mark = if node.kind() == "image" { MarkdownMark::Image } else { MarkdownMark::Link };
                    push(node.start_byte(), text.start_byte(), mark);
                    push(text.end_byte(), node.end_byte(), MarkdownMark::Hide);
                }
            }
            "uri_autolink" | "email_autolink" if node.end_byte() > node.start_byte() + 2 => {
                push(node.start_byte(), node.start_byte() + 1, MarkdownMark::Link);
                push(node.end_byte() - 1, node.end_byte(), MarkdownMark::Hide);
            }
            _ => {}
        }
        nodes.extend(node.named_children(&mut walk));
    }
}

fn named_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut walk = node.walk();
    node.named_children(&mut walk).find(|child| child.kind() == kind)
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
    source.get(start..end).map_or(0, |text| text.len() - text.trim_start().len())
}

fn line_end(source: &str, start: usize) -> usize {
    source[start..].find('\n').map_or(source.len(), |at| start + at)
}
