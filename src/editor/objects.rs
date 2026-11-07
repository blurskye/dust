//! Syntax object kinds and byte ranges.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxObjectKind {
    Function,
    Class,
    Argument,
    Comment,
    Node,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxObject {
    pub kind: SyntaxObjectKind,
    /// Byte range of the `a` object.
    pub outer: (usize, usize),
    /// Byte range of the `i` object.
    pub inner: (usize, usize),
    /// The containing argument list, for swapping only sibling arguments.
    pub parent: Option<(usize, usize)>,
}
