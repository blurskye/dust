//! Read workflow-dispatch fields with Dusty's YAML grammar.
use tree_sitter::Node;

pub(super) struct Input {
    pub name: String,
    pub default: String,
    pub required: bool,
    pub choices: Vec<String>,
}
pub(super) struct Definition {
    pub dispatch: bool,
    pub inputs: Vec<Input>,
}
impl Definition {
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.len() > 512 * 1024 {
            return Err("Workflow definition exceeds 512 KiB".into());
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .map_err(|error| error.to_string())?;
        let tree = parser
            .parse(source, None)
            .ok_or("Could not parse workflow YAML")?;
        if tree.root_node().has_error() {
            return Err("Workflow YAML contains syntax errors".into());
        }
        let root = unwrap(tree.root_node());
        let on = entry(root, "on", source);
        let trigger = on.and_then(|node| entry(node, "workflow_dispatch", source));
        let dispatch = trigger.is_some()
            || on.is_some_and(|node| {
                text(node, source) == "workflow_dispatch"
                    || sequence(node)
                        .iter()
                        .any(|node| text(*node, source) == "workflow_dispatch")
            });
        let mut inputs = Vec::new();
        if let Some(node) = trigger.and_then(|node| entry(node, "inputs", source)) {
            for (name, value) in mapping(node, source) {
                let get = |key| {
                    entry(value, key, source)
                        .map(|node| text(node, source))
                        .unwrap_or_default()
                };
                let kind = get("type");
                let choices = if kind == "boolean" {
                    vec!["true".into(), "false".into()]
                } else {
                    entry(value, "options", source)
                        .map(|node| {
                            sequence(node)
                                .iter()
                                .map(|node| text(*node, source))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let default = get("default");
                let default = if default.is_empty() && kind == "boolean" {
                    "false".into()
                } else if default.is_empty() && kind == "choice" {
                    choices.first().cloned().unwrap_or_default()
                } else {
                    default
                };
                inputs.push(Input {
                    name,
                    default,
                    required: get("required") == "true",
                    choices,
                });
            }
        }
        Ok(Self { dispatch, inputs })
    }
}
fn unwrap(mut node: Node<'_>) -> Node<'_> {
    loop {
        if !matches!(
            node.kind(),
            "stream"
                | "document"
                | "block_node"
                | "flow_node"
                | "block_sequence_item"
                | "plain_scalar"
        ) {
            return node;
        }
        let mut cursor = node.walk();
        let next = node
            .named_children(&mut cursor)
            .find(|child| child.kind() != "comment");
        match next {
            Some(next) => node = next,
            None => return node,
        }
    }
}
fn mapping<'a>(node: Node<'a>, source: &str) -> Vec<(String, Node<'a>)> {
    let node = unwrap(node);
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|pair| {
            let key = pair.child_by_field_name("key")?;
            let value = pair.child_by_field_name("value").unwrap_or(pair);
            Some((text(key, source), unwrap(value)))
        })
        .collect()
}
fn entry<'a>(node: Node<'a>, key: &str, source: &str) -> Option<Node<'a>> {
    mapping(node, source)
        .into_iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}
fn sequence(node: Node<'_>) -> Vec<Node<'_>> {
    let node = unwrap(node);
    if !matches!(node.kind(), "block_sequence" | "flow_sequence") {
        return Vec::new();
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .map(unwrap)
        .collect()
}
fn text(node: Node<'_>, source: &str) -> String {
    let text = unwrap(node)
        .utf8_text(source.as_bytes())
        .unwrap_or_default()
        .trim();
    if text.starts_with('"') {
        serde_json::from_str::<String>(text).unwrap_or_else(|_| text.trim_matches('"').into())
    } else if text.starts_with('\'') {
        text.trim_matches('\'').replace("''", "'")
    } else {
        text.into()
    }
}
