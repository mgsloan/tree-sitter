# Tree-squatter

Tree-squatter stores syntax trees in a compact representation with traversal,
query execution, and direct parsing through tree-feller.

This branch contains the Rust crate in `crates/tree-squatter` and the adapted
tree-feller C sources in `crates/tree-squatter/native/tree_feller`. Tree-sitter
is fetched as a pinned dependency; its private headers are required by the native
build.

Build requirements: Rust supporting edition 2024, a C11 compiler, and libclang
for bindgen.

```sh
cargo test --locked --workspace
cargo doc --locked --workspace --no-deps
```

This is a prototype. Backward compatibility for its APIs and data formats is
not guaranteed.

Development happens on `main`, including persistence, visualization, and other
experiments. `pristine` contains publications made by `tools/publish.py` on
`main`. Each publication retains the development commits as merge ancestry.
