#[derive(Clone, Debug)]
struct UndoNode {
    parent: Option<usize>,
    children: Vec<usize>,
    text: String,
}

/// Branch-preserving snapshot undo tree. Text storage can later be replaced by
/// compact inverse deltas without changing its traversal contract.
#[derive(Clone, Debug)]
pub struct UndoTree {
    nodes: Vec<UndoNode>,
    current: usize,
    merge_active: bool,
}

impl UndoTree {
    #[must_use]
    pub fn new(initial_text: String) -> Self {
        Self {
            nodes: vec![UndoNode {
                parent: None,
                children: Vec::new(),
                text: initial_text,
            }],
            current: 0,
            merge_active: false,
        }
    }

    pub fn record(&mut self, text: String, merge: bool) {
        if self.nodes[self.current].text == text {
            return;
        }
        if merge && self.merge_active && self.current != 0 {
            self.nodes[self.current].text = text;
            return;
        }
        let next = self.nodes.len();
        self.nodes.push(UndoNode {
            parent: Some(self.current),
            children: Vec::new(),
            text,
        });
        self.nodes[self.current].children.push(next);
        self.current = next;
        self.merge_active = merge;
    }

    pub fn boundary(&mut self) {
        self.merge_active = false;
    }

    pub fn undo(&mut self) -> Option<&str> {
        let parent = self.nodes[self.current].parent?;
        self.current = parent;
        self.merge_active = false;
        Some(&self.nodes[self.current].text)
    }

    pub fn redo(&mut self) -> Option<&str> {
        let child = *self.nodes[self.current].children.last()?;
        self.current = child;
        self.merge_active = false;
        Some(&self.nodes[self.current].text)
    }

    #[must_use]
    pub fn redo_branch_count(&self) -> usize {
        self.nodes[self.current].children.len()
    }

    #[must_use]
    pub const fn current_node(&self) -> usize {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::UndoTree;

    #[test]
    fn preserves_redo_branch_after_new_edit() {
        let mut tree = UndoTree::new("a".into());
        tree.record("ab".into(), false);
        tree.record("abc".into(), false);
        assert_eq!(tree.undo(), Some("ab"));
        tree.record("abX".into(), false);
        assert_eq!(tree.undo(), Some("ab"));
        assert_eq!(tree.redo_branch_count(), 2);
        assert_eq!(tree.redo(), Some("abX"));
    }
}
