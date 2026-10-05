# `mgsloan-bugfixes` soft fork

This is a soft-fork of [Tree-sitter](https://github.com/tree-sitter/tree-sitter) which includes a variety of bugfixes discovered while doing bisimulation property testing vs a new implementation of Tree-sitter's representation of trees.

The fixes vary in quality, due to negative and/or silent response to opening these issues.

The aggregate is based on Tree-sitter `v0.27.0`. The [manifest](tools/bugfixes/manifest.json)
records the source branches and regression tests; [maintenance instructions](tools/bugfixes/README.md)
describe how to rebuild it. Maintain this README on `mgsloan-bugfix-maintenance`.

## Clean fixes for reported issues

- [#5932](https://github.com/tree-sitter/tree-sitter/issues/5932): Update the wildcard-pattern count when disabling query patterns (`fix/disable-wildcard-pattern`).
- [#5934](https://github.com/tree-sitter/tree-sitter/issues/5934): Skip hidden zero-width subtrees during descendant lookup (`fix/hidden-zero-width-descendant`). This fix has reported performance tradeoffs.
- [#5950](https://github.com/tree-sitter/tree-sitter/issues/5950): Avoid child-index truncation during backward sibling traversal (`fix-previous-sibling-index-underflow`).
- [#5987](https://github.com/tree-sitter/tree-sitter/issues/5987): Retain siblings during byte-based named-child lookup (`fix/first-named-child-for-byte-upstream`).

## Vibecoded fixes for reported issues

- [#5948](https://github.com/tree-sitter/tree-sitter/issues/5948): Keep inherited field lookup from crossing visible aliases (`fix/visible-alias-field-lookup`).
- [#5949](https://github.com/tree-sitter/tree-sitter/issues/5949): Verify ancestry when looking up a child containing a descendant with the same range (`fix/child-with-descendant-self`).

## Vibecoded fixes for unreported issues

- Preserve aliases when traversing backward past extras (`fix/previous-sibling-alias-after-extra`).
- Reject non-rooted query starts outside the cursor root's range while retaining matches spanning a range (`fix/non-rooted-query-range`).
- Preserve non-rooted wildcard matches when ranges prune hidden repetitions (`fix/non-rooted-wildcard-range-pruning`).
- Prevent phantom captures from unjustified wildcard-child guarantees (`fix/wildcard-child-guarantees`).
- Preserve quoted missing and unexpected tokens during Rust S-expression formatting (`fix/quoted-missing-token-formatting`).
- Preserve nullable-root query matches through hidden repetitions when traversal is restricted by ranges (`fix/nullable-root-hidden-structure`).
- Complete deferred query matches after ascending from a skipped hidden subtree (`fix/containing-range-hidden-ascent`).
- Preserve descendant indices during backward sibling traversal (`fix/previous-sibling-descendant-index`).
- Decode UTF-8 characters split across short input callback chunks (`fix/short-utf8-input-chunks`).
- Honor query progress-callback cancellation with queued captures and preserve resumable execution (`fix/query-capture-cancellation`).
- Prevent orphan captures from unjustified ERROR-child guarantees (`fix/error-child-guarantees`).
- Prevent extra ERROR parents from inheriting structural-child aliases during query execution (`fix/query-extra-parent-alias`).
- Preserve later siblings when byte-based child lookup exhausts nested hidden nodes (`fix/first-child-for-byte-hidden-siblings`).
- Include missing nodes at containing-range boundaries (`fix/query-containing-range-missing-boundary`).
- Preserve wildcard-parent matches at the maximum start depth, including through hidden wrappers and query cloning (`fix/query-wildcard-parent-depth`).
- Correct cursor and query API documentation and Rust documentation links (`fix/api-documentation`; documentation only).

The optional [#5935](https://github.com/tree-sitter/tree-sitter/issues/5935)
descendant-range optimization is disabled and is excluded from these lists.

# tree-sitter

[![DOI](https://zenodo.org/badge/14164618.svg)](https://zenodo.org/badge/latestdoi/14164618)
[![discord][discord]](https://discord.gg/w7nTvsVJhm)
[![matrix][matrix]](https://matrix.to/#/#tree-sitter-chat:matrix.org)

Tree-sitter is a parser generator tool and an incremental parsing library. It can build a concrete syntax tree for a source file and efficiently update the syntax tree as the source file is edited. Tree-sitter aims to be:

- **General** enough to parse any programming language
- **Fast** enough to parse on every keystroke in a text editor
- **Robust** enough to provide useful results even in the presence of syntax errors
- **Dependency-free** so that the runtime library (which is written in pure C) can be embedded in any application

## Links
- [Documentation](https://tree-sitter.github.io)
- [Rust binding](lib/binding_rust/README.md)
- [Wasm binding](lib/binding_web/README.md)
- [Command-line interface](crates/cli/README.md)

[discord]: https://img.shields.io/discord/1063097320771698699?logo=discord&label=discord
[matrix]: https://img.shields.io/matrix/tree-sitter-chat%3Amatrix.org?logo=matrix&label=matrix
