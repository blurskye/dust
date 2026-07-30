# Bundled syntax assets

These queries are compiled only with the pinned grammar crates in `Cargo.lock`.
Highlight queries originate from the corresponding upstream grammar release; TypeScript composes the JavaScript base (excluding JSX nodes) with the TypeScript extension, while TSX composes the JavaScript/JSX base with that extension. Fold/indent queries are Dust-maintained. Upstream grammar/query assets are MIT
licensed:

- tree-sitter-rust 0.24.0
- tree-sitter-typescript 0.23.2
- tree-sitter-javascript 0.25.0
- tree-sitter-go 0.25.0
- tree-sitter-json 0.24.8
- tree-sitter-toml-ng 0.7.0
- tree-sitter-md-025 0.5.6

See each crate's packaged license and repository metadata in Cargo.lock/SBOM.
Dust never loads query or parser code from a project or plugin.
