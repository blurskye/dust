//! Language identification and analysis.

#![forbid(unsafe_code)]

pub mod lsp;
/// Highlighting and filetype detection, from the `dusty-ui` crate.
pub use dusty_ui::syntax;
