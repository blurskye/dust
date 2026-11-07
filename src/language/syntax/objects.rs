//! Syntax node selections and definition, argument and comment text objects.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Kinds {
    functions: Vec<String>,
    classes: Vec<String>,
    argument_lists: Vec<String>,
    bodies: Vec<String>,
    comments: Vec<String>,
    comment_markers: Vec<(String, String)>,
}

static KINDS: LazyLock<Kinds> = LazyLock::new(|| {
    toml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/syntax-objects.toml"
    )))
    .expect("bundled syntax object kinds")
});

use crate::editor::{SyntaxObject, SyntaxObjectKind};

pub(super) fn collect(tree: &tree_sitter::Tree, source: &str, limit: usize) -> Vec<SyntaxObject> {
    let mut objects = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        if objects.len() >= limit {
            break;
        }
        let node = cursor.node();
        let kind = node.kind();
        // CMake's named `function` keyword is a leaf, not a definition.
        if node.is_named()
            && KINDS.functions.iter().any(|name| name == kind)
            && node.child_count() > 0
        {
            objects.push(block(node, source, SyntaxObjectKind::Function));
        } else if node.is_named()
            && KINDS.classes.iter().any(|name| name == kind)
            && !(kind == "type_declaration" && {
                let mut walk = node.walk();
                node.named_children(&mut walk)
                    .any(|child| child.kind() == "type_spec")
            })
        {
            objects.push(block(node, source, SyntaxObjectKind::Class));
        } else if KINDS.argument_lists.iter().any(|name| name == kind) {
            let remaining = limit - objects.len();
            arguments(node, &mut objects, remaining);
        } else if node.is_named() && KINDS.comments.iter().any(|name| name == kind) {
            objects.push(comment(node, source));
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
    // Preserve the text-object budget before spending the remaining space on node selection.
    let mut cursor = tree.walk();
    'nodes: loop {
        if objects.len() >= limit {
            break;
        }
        let node = cursor.node();
        if node.is_named() && node.start_byte() < node.end_byte() {
            objects.push(SyntaxObject {
                kind: SyntaxObjectKind::Node,
                outer: (node.start_byte(), node.end_byte()),
                inner: (node.start_byte(), node.end_byte()),
                parent: None,
            });
        }
        if cursor.goto_first_child() || cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                break 'nodes;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
    objects
}

fn comment(node: tree_sitter::Node<'_>, source: &str) -> SyntaxObject {
    let outer = (node.start_byte(), node.end_byte());
    let text = &source[outer.0..outer.1];
    let (prefix, content) = KINDS
        .comment_markers
        .iter()
        .find_map(|(open, close)| Some((open.len(), text.strip_prefix(open)?.strip_suffix(close)?)))
        .unwrap_or((0, text));
    let start = outer.0 + prefix;
    let leading = content.len() - content.trim_start().len();
    SyntaxObject {
        kind: SyntaxObjectKind::Comment,
        outer,
        inner: (start + leading, start + leading + content.trim().len()),
        parent: None,
    }
}

/// The outer range is the whole node; the inner range is its body without the braces, the
/// newline after `{` and the indentation before `}`, so `dif` leaves an empty block.
fn block(node: tree_sitter::Node<'_>, source: &str, kind: SyntaxObjectKind) -> SyntaxObject {
    let mut outer = (node.start_byte(), node.end_byte());
    let mut body = node.child_by_field_name("body").or_else(|| {
        let mut walk = node.walk();
        node.named_children(&mut walk)
            .find(|child| KINDS.bodies.iter().any(|kind| kind == child.kind()))
    });
    if node.kind() == "type_spec" {
        if let Some(parent) = node.parent().filter(|parent| {
            let mut walk = parent.walk();
            parent.kind() == "type_declaration"
                && !parent.children(&mut walk).any(|child| child.kind() == "(")
        }) {
            outer = (parent.start_byte(), parent.end_byte());
        }
        if let Some(ty) = node.child_by_field_name("type") {
            let mut walk = ty.walk();
            body = ty
                .named_children(&mut walk)
                .find(|child| child.kind() == "field_declaration_list");
        }
    } else if node.kind() == "function_def" {
        let mut walk = node.walk();
        body = node
            .named_children(&mut walk)
            .find(|child| child.kind() == "body");
    }
    let mut body = body.map_or((outer.1, outer.1), |body| {
        (body.start_byte(), body.end_byte())
    });
    // Julia stores statements directly between the signature and the `end` keyword.
    if node.kind() == "function_definition"
        && let Some(signature) = node
            .named_child(0)
            .filter(|child| child.kind() == "signature")
        && let Some(end) = node
            .child(node.child_count() - 1)
            .filter(|child| child.kind() == "end")
    {
        body = (signature.end_byte(), end.start_byte());
        if source[body.0..body.1].starts_with('\n') {
            body.0 += 1;
            let trimmed = source[body.0..body.1].trim_end_matches([' ', '\t']);
            if trimmed.ends_with('\n') {
                body.1 = body.0 + trimmed.len();
            }
        }
    }
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
    SyntaxObject {
        kind,
        outer,
        inner,
        parent: None,
    }
}

/// Each named child of an argument list. The outer range takes the separating comma and
/// spaces after it, or before it for the last argument.
fn arguments(list: tree_sitter::Node<'_>, objects: &mut Vec<SyntaxObject>, remaining: usize) {
    let mut walker = list.walk();
    let mut items = list
        .named_children(&mut walker)
        .filter(|child| !child.kind().contains("comment"))
        .peekable();
    let mut previous = None;
    for _ in 0..remaining {
        let Some(item) = items.next() else { break };
        let inner = (item.start_byte(), item.end_byte());
        let outer = if let Some(next) = items.peek() {
            (inner.0, next.start_byte())
        } else {
            let mut walker = list.walk();
            let mut commas = list
                .children(&mut walker)
                .filter(|child| child.kind() == ",");
            if let Some(previous) = previous {
                (
                    commas
                        .find(|comma| comma.start_byte() >= previous && comma.end_byte() <= inner.0)
                        .map_or(inner.0, |comma| comma.start_byte()),
                    inner.1,
                )
            } else {
                (
                    inner.0,
                    commas
                        .find(|comma| comma.start_byte() >= inner.1)
                        .map_or(inner.1, |comma| comma.end_byte()),
                )
            }
        };
        previous = Some(inner.1);
        objects.push(SyntaxObject {
            kind: SyntaxObjectKind::Argument,
            outer,
            inner,
            parent: Some((list.start_byte(), list.end_byte())),
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::language::syntax::objects::*;

    fn objects(source: &str) -> Vec<SyntaxObject> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        collect(&tree, source, 100)
    }

    #[test]
    fn comments_trim_markers_and_space_without_splitting_unicode() {
        for (source, language, expected) in [
            ("// café\n", tree_sitter_rust::LANGUAGE.into(), "café"),
            ("/* éclair */", tree_sitter_rust::LANGUAGE.into(), "éclair"),
            ("/// docs\n", tree_sitter_rust::LANGUAGE.into(), "docs"),
            ("/**/", tree_sitter_rust::LANGUAGE.into(), ""),
            ("# hello\n", tree_sitter_python::LANGUAGE.into(), "hello"),
            ("<!-- hello -->", tree_sitter_html::LANGUAGE.into(), "hello"),
            ("-- hello\n", tree_sitter_lua::LANGUAGE.into(), "hello"),
        ] {
            let found = parsed(source, language);
            let object = found
                .iter()
                .find(|o| o.kind == SyntaxObjectKind::Comment)
                .unwrap();
            assert_eq!(
                &source[object.inner.0..object.inner.1],
                expected,
                "{source}"
            );
            assert!(
                source.is_char_boundary(object.inner.0) && source.is_char_boundary(object.inner.1)
            );
        }
    }

    #[test]
    fn definition_bodies_without_named_fields_remain_selectable() {
        for (source, language, kind, expected) in [
            (
                "class A {\n fun f(x: Int) { println(x) }\n}\n",
                tree_sitter_kotlin_sg::LANGUAGE.into(),
                SyntaxObjectKind::Class,
                "fun f(x: Int) { println(x) }",
            ),
            (
                "class A { func f() { print(1) } }",
                tree_sitter_swift::LANGUAGE.into(),
                SyntaxObjectKind::Class,
                "func f() { print(1) }",
            ),
            (
                "fn f(x: u8) u8 { return x; }",
                tree_sitter_zig::LANGUAGE.into(),
                SyntaxObjectKind::Function,
                "return x;",
            ),
            (
                "function f(x) return x end",
                tree_sitter_lua::LANGUAGE.into(),
                SyntaxObjectKind::Function,
                "return x",
            ),
        ] {
            let found = parsed(source, language);
            let object = found.iter().find(|o| o.kind == kind).unwrap();
            assert_eq!(
                source[object.inner.0..object.inner.1].trim(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn nodes_and_argument_parents_preserve_the_parse_tree_structure() {
        let source = "fn main() { call(inner(a, b), c); }";
        let found = objects(source);
        let argument = |name| {
            found
                .iter()
                .find(|o| {
                    o.kind == SyntaxObjectKind::Argument && &source[o.inner.0..o.inner.1] == name
                })
                .unwrap()
        };
        assert_eq!(argument("a").parent, argument("b").parent);
        assert_ne!(argument("a").parent, argument("c").parent);
        assert_eq!(argument("inner(a, b)").parent, argument("c").parent);
        for expected in ["a", "(a, b)", "inner(a, b)", "(inner(a, b), c)", source] {
            assert!(
                found.iter().any(|o| o.kind == SyntaxObjectKind::Node
                    && &source[o.outer.0..o.outer.1] == expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn bundled_grammar_definitions_and_parameters_are_available_without_lsp() {
        for (source, language) in [
            (
                "class A { method(x, y) { return x; } }",
                tree_sitter_javascript::LANGUAGE.into(),
            ),
            (
                "class A { method(x: number, y: number) { return x; } }",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ),
            (
                "class A:\n    def method(self, x):\n        return x\n",
                tree_sitter_python::LANGUAGE.into(),
            ),
            (
                "class A { int method(int x, int y) { return x; } }",
                tree_sitter_java::LANGUAGE.into(),
            ),
            (
                "class A { int Method(int x, int y) { return x; } }",
                tree_sitter_c_sharp::LANGUAGE.into(),
            ),
            (
                "class A\n  def method(x, y)\n    x\n  end\nend",
                tree_sitter_ruby::LANGUAGE.into(),
            ),
        ] {
            let found = parsed(source, language);
            for kind in [
                SyntaxObjectKind::Class,
                SyntaxObjectKind::Function,
                SyntaxObjectKind::Argument,
                SyntaxObjectKind::Node,
            ] {
                assert!(found.iter().any(|o| o.kind == kind), "{kind:?}: {source}");
            }
        }
    }

    #[test]
    fn functions_and_arguments_have_inner_and_outer_ranges() {
        let source = "fn add(x: u8, y: u8) {\n    x + y\n}\n";
        let found = objects(source);
        let function = found
            .iter()
            .find(|object| object.kind == SyntaxObjectKind::Function)
            .unwrap();
        assert_eq!(
            &source[function.outer.0..function.outer.1],
            source.trim_end()
        );
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
    fn parsed(source: &str, language: tree_sitter::Language) -> Vec<SyntaxObject> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        collect(&tree, source, 100)
    }
    #[test]
    fn ruby_methods_and_parameters_have_objects() {
        for source in [
            "def f(x, y)\n  x + y\nend",
            "def self.f(x, y)\n  x + y\nend",
        ] {
            let found = parsed(source, tree_sitter_ruby::LANGUAGE.into());
            let function = found
                .iter()
                .find(|o| o.kind == SyntaxObjectKind::Function)
                .unwrap();
            assert_eq!(&source[function.outer.0..function.outer.1], source);
            assert_eq!(source[function.inner.0..function.inner.1].trim(), "x + y");
            let parameters = found
                .iter()
                .filter(|o| o.kind == SyntaxObjectKind::Argument)
                .map(|o| (&source[o.inner.0..o.inner.1], &source[o.outer.0..o.outer.1]))
                .collect::<Vec<_>>();
            assert_eq!(parameters, [("x", "x, "), ("y", ", y")]);
        }
    }
    #[test]
    fn cmake_function_objects_cover_definitions_not_keywords() {
        let source = "function(foo)\n  message(hi)\nendfunction()";
        let found = parsed(source, tree_sitter_cmake::LANGUAGE.into());
        let functions = found
            .iter()
            .filter(|o| o.kind == SyntaxObjectKind::Function)
            .collect::<Vec<_>>();
        assert_eq!(functions.len(), 1);
        let function = functions[0];
        assert_eq!(&source[function.outer.0..function.outer.1], source);
        assert_eq!(
            source[function.inner.0..function.inner.1].trim(),
            "message(hi)"
        );
    }
    #[test]
    fn julia_function_inner_stays_between_signature_and_end() {
        for (source, expected) in [
            ("function f()\n  x = 1\nend", "  x = 1\n"),
            (
                "function f()\n  if true\n    x = 1\n  end\nend",
                "  if true\n    x = 1\n  end\n",
            ),
            ("function f()\nend", ""),
            ("function f(); x = 1; end", "; x = 1; "),
        ] {
            let found = parsed(source, tree_sitter_julia::LANGUAGE.into());
            let function = found
                .iter()
                .find(|o| o.kind == SyntaxObjectKind::Function)
                .unwrap();
            assert_eq!(&source[function.outer.0..function.outer.1], source);
            assert_eq!(&source[function.inner.0..function.inner.1], expected);
            assert!(function.inner.1 < function.outer.1);
        }
    }
    #[test]
    fn anonymous_keywords_are_not_objects() {
        let source = "function f() { return 1; } class C { method() {} }";
        let found = parsed(source, tree_sitter_javascript::LANGUAGE.into());
        assert!(
            found
                .iter()
                .all(|o| !matches!(&source[o.outer.0..o.outer.1], "function" | "class"))
        );
        let function = found.iter().find(|o| o.outer.0 == 0).unwrap();
        assert_eq!(&source[function.inner.0..function.inner.1], " return 1; ");
    }
    #[test]
    fn go_struct_inner_is_its_field_list() {
        for declaration in [
            "type Point struct { X int }",
            "type /* comment */ Point struct { X int }",
        ] {
            let source = format!("package main\n{declaration}\n");
            let found = parsed(&source, tree_sitter_go::LANGUAGE.into());
            let classes = found
                .iter()
                .filter(|o| o.kind == SyntaxObjectKind::Class)
                .collect::<Vec<_>>();
            assert_eq!(classes.len(), 1);
            let class = classes[0];
            assert_eq!(&source[class.inner.0..class.inner.1], " X int ");
            assert_eq!(&source[class.outer.0..class.outer.1], declaration);
        }
    }
    #[test]
    fn grouped_go_types_have_separate_class_bodies() {
        for prefix in ["type (", "type /* comment */ (", "type (\n // comment"] {
            let source = format!(
                "package main\n{prefix}\n A struct {{ X int }}\n B struct {{ Y int }}\n)\n"
            );
            let found = parsed(&source, tree_sitter_go::LANGUAGE.into());
            let classes = found
                .iter()
                .filter(|o| o.kind == SyntaxObjectKind::Class)
                .map(|o| (&source[o.outer.0..o.outer.1], &source[o.inner.0..o.inner.1]))
                .collect::<Vec<_>>();
            assert_eq!(
                classes,
                [
                    ("A struct { X int }", " X int "),
                    ("B struct { Y int }", " Y int "),
                ],
                "{source}"
            );
        }
    }
    #[test]
    fn argument_collection_respects_remaining_object_capacity() {
        let source = format!("fn f() {{ g({}); }}", vec!["x"; 1000].join(", "));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        for limit in [0, 1, 2, 10, 1001, 2000] {
            let found = collect(&tree, &source, limit);
            assert!(found.len() <= limit);
            assert_eq!(
                found
                    .iter()
                    .filter(|o| o.kind != SyntaxObjectKind::Node)
                    .count(),
                limit.min(1001)
            );
            if limit > 1 {
                let argument = found[1];
                assert_eq!(&source[argument.inner.0..argument.inner.1], "x");
                assert_eq!(&source[argument.outer.0..argument.outer.1], "x, ");
            }
        }
    }
    #[test]
    fn argument_separator_ignores_comment_commas() {
        let source = "f(a /* note, detail */, b)";
        let found = parsed(source, tree_sitter_javascript::LANGUAGE.into());
        let b = found
            .iter()
            .find(|o| o.kind == SyntaxObjectKind::Argument && &source[o.inner.0..o.inner.1] == "b")
            .unwrap();
        assert_eq!(&source[b.outer.0..b.outer.1], ", b");
    }
    #[test]
    fn sole_argument_includes_trailing_comma() {
        let source = "f(x,);";
        let found = parsed(source, tree_sitter_javascript::LANGUAGE.into());
        let x = found
            .iter()
            .find(|o| o.kind == SyntaxObjectKind::Argument)
            .unwrap();
        assert_eq!(&source[x.outer.0..x.outer.1], "x,");
    }
    #[test]
    fn python_and_c_definitions_have_function_objects() {
        for (source, language) in [
            (
                "def f():\n    return 1\n",
                tree_sitter_python::LANGUAGE.into(),
            ),
            ("int f() { return 1; }", tree_sitter_c::LANGUAGE.into()),
            ("int f() { return 1; }", tree_sitter_cpp::LANGUAGE.into()),
        ] {
            let found = parsed(source, language);
            assert!(
                found
                    .iter()
                    .any(|o| o.kind == SyntaxObjectKind::Function && o.inner != o.outer)
            );
        }
    }
}
