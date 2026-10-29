//! Function, class and argument ranges for the `af if ac ic aa ia` text objects, as
//! mini.ai provides them in LazyVim.

/// Node kinds across the built-in grammars. The names do not collide between languages.
const FUNCTIONS: &[&str] = &[
    "function_item",
    "closure_expression",
    "function_declaration",
    "function_expression",
    "function",
    "generator_function_declaration",
    "arrow_function",
    "method_definition",
    "method_declaration",
    "func_literal",
];
const CLASSES: &[&str] = &[
    "struct_item",
    "enum_item",
    "union_item",
    "impl_item",
    "trait_item",
    "mod_item",
    "class_declaration",
    "abstract_class_declaration",
    "class",
    "interface_declaration",
    "enum_declaration",
    "type_declaration",
];
const ARGUMENT_LISTS: &[&str] = &[
    "parameters",
    "arguments",
    "type_arguments",
    "type_parameters",
    "closure_parameters",
    "formal_parameters",
    "parameter_list",
    "argument_list",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxObjectKind {
    Function,
    Class,
    Argument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxObject {
    pub kind: SyntaxObjectKind,
    /// Byte range of the `a` object.
    pub outer: (usize, usize),
    /// Byte range of the `i` object.
    pub inner: (usize, usize),
}

pub(super) fn collect(tree: &tree_sitter::Tree, source: &str, limit: usize) -> Vec<SyntaxObject> {
    let mut objects = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        let kind = node.kind();
        if FUNCTIONS.contains(&kind) {
            objects.push(block(node, source, SyntaxObjectKind::Function));
        } else if CLASSES.contains(&kind) {
            objects.push(block(node, source, SyntaxObjectKind::Class));
        } else if ARGUMENT_LISTS.contains(&kind) {
            arguments(node, source, &mut objects);
        }
        if objects.len() >= limit {
            break;
        }
        if cursor.goto_first_child() || cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                break 'walk;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
    objects
}

/// The outer range is the whole node; the inner range is its body without the braces, the
/// newline after `{` and the indentation before `}`, so `dif` leaves an empty block.
fn block(node: tree_sitter::Node<'_>, source: &str, kind: SyntaxObjectKind) -> SyntaxObject {
    let outer = (node.start_byte(), node.end_byte());
    let body = node
        .child_by_field_name("body")
        .map_or(outer, |body| (body.start_byte(), body.end_byte()));
    let text = &source[body.0..body.1];
    let inner = if text.starts_with('{') && text.ends_with('}') && text.len() >= 2 {
        let mut start = body.0 + 1;
        let mut end = body.1 - 1;
        if source[start..end].starts_with('\n') {
            start += 1;
            let tail = &source[start..end];
            let trimmed = tail.trim_end_matches([' ', '\t']);
            if trimmed.ends_with('\n') {
                end = start + trimmed.len();
            }
        }
        (start, end.max(start))
    } else {
        body
    };
    SyntaxObject { kind, outer, inner }
}

/// Each named child of an argument list. The outer range takes the separating comma and
/// spaces after it, or before it for the last argument.
fn arguments(list: tree_sitter::Node<'_>, source: &str, objects: &mut Vec<SyntaxObject>) {
    let mut walker = list.walk();
    let items = list
        .named_children(&mut walker)
        .filter(|child| !child.kind().contains("comment"))
        .collect::<Vec<_>>();
    for (index, item) in items.iter().enumerate() {
        let inner = (item.start_byte(), item.end_byte());
        let outer = if let Some(next) = items.get(index + 1) {
            (inner.0, next.start_byte())
        } else if index > 0 {
            let previous = items[index - 1].end_byte();
            let between = &source[previous..inner.0];
            (previous + between.find(',').unwrap_or(0), inner.1)
        } else {
            inner
        };
        objects.push(SyntaxObject {
            kind: SyntaxObjectKind::Argument,
            outer,
            inner,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objects(source: &str) -> Vec<SyntaxObject> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        collect(&tree, source, 100)
    }

    #[test]
    fn functions_and_arguments_have_inner_and_outer_ranges() {
        let source = "fn add(x: u8, y: u8) {\n    x + y\n}\n";
        let found = objects(source);
        let function = found
            .iter()
            .find(|object| object.kind == SyntaxObjectKind::Function)
            .unwrap();
        assert_eq!(&source[function.outer.0..function.outer.1], source.trim_end());
        assert_eq!(&source[function.inner.0..function.inner.1], "    x + y\n");
        let arguments = found
            .iter()
            .filter(|object| object.kind == SyntaxObjectKind::Argument)
            .map(|object| {
                (
                    &source[object.inner.0..object.inner.1],
                    &source[object.outer.0..object.outer.1],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(arguments, vec![("x: u8", "x: u8, "), ("y: u8", ", y: u8")]);
    }
}
