# Bundled syntax assets

These queries are compiled only with the pinned grammar crates in `Cargo.lock`.
Highlight queries originate from the corresponding upstream grammar release; TypeScript composes the JavaScript base (excluding JSX nodes) with the TypeScript extension, while TSX composes the JavaScript/JSX base with that extension. Fold/indent queries are Dust-maintained. Upstream grammar/query assets are MIT
licensed unless noted:

- tree-sitter-rust 0.24.0
- tree-sitter-typescript 0.23.2
- tree-sitter-javascript 0.25.0
- tree-sitter-go 0.25.0
- tree-sitter-json 0.24.8
- tree-sitter-toml-ng 0.7.0
- tree-sitter-md-025 0.5.6 (block and inline grammars)
- tree-sitter-bash 0.25.1
- tree-sitter-c 0.24.2
- tree-sitter-cpp 0.23.4
- tree-sitter-c-sharp 0.23.5
- tree-sitter-css 0.25.0
- tree-sitter-html 0.23.2
- tree-sitter-python 0.25.0
- tree-sitter-yaml 0.7.2
- tree-sitter-lua 0.5.0
- tree-sitter-nix 0.3.0
- tree-sitter-java 0.23.5
- tree-sitter-ruby 0.23.1
- tree-sitter-php 0.24.2
- tree-sitter-zig 1.1.2
- tree-sitter-haskell 0.23.1
- tree-sitter-elixir 0.3.5
- tree-sitter-ocaml 0.24.2
- tree-sitter-scala 0.26.2
- tree-sitter-swift 0.7.3
- tree-sitter-kotlin-sg 0.4.1
- tree-sitter-dart 0.2.0
- tree-sitter-make 1.1.1
- tree-sitter-cmake 0.7.5
- tree-sitter-xml 0.7.0
- tree-sitter-diff 0.1.0
- tree-sitter-ini 1.4.0
- tree-sitter-proto 0.4.0
- tree-sitter-sequel 0.3.11
- tree-sitter-erlang 0.16.0
- tree-sitter-r 1.3.0
- tree-sitter-julia 0.23.1
- tree-sitter-vue-next 0.1.0

The other grammars use their upstream highlight query as is; C++ and Vue put the C and HTML
queries they build on first. tree-sitter-elixir and tree-sitter-ini are Apache-2.0 licensed.

See each crate's packaged license and repository metadata in Cargo.lock/SBOM.
Dust never loads query or parser code from a project or plugin.
