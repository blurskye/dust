# Function surround and text objects

Shows tree-sitter captures, anchored ranges, and an `edit.operator` wrapper.
[Build and install](../README.md), open a Rust file, wait for syntax parsing, and run
`:plugin.surround.enable`. `dif`, `yaf`, and `cif` use the guest's function ranges;
`gsaif(` surrounds a function body. Counts select enclosing functions, just like
native tree-sitter objects; `"ayif`, undo, dot and macros remain host-owned.
Other motions pass through unchanged. The guest implements the surround edit;
ordinary operators consume the guest's checked range in the native implementation.

The query is Rust-specific. Captures refresh on document events, and marks move
with intervening edits and undo. A parse error is reported rather than guessed.
