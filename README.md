# `mgsloan-bugfixes` soft fork

This is a soft-fork of [Tree-sitter](https://github.com/tree-sitter/tree-sitter), based on `v0.27.0`, which includes a variety of bugfixes discovered while doing bisimulation property testing vs a new implementation of Tree-sitter's representation of trees.

The fixes vary in quality, due to negative and/or silent response to opening these issues causing me to no longer put in the effort.  For me the purpose of these fixes is for the bisimulation property test to not need to work around upstream bugs.

This branch is automatically generated based on the [`mgsloan-bugfix-maintenance` branch](https://github.com/mgsloan/tree-sitter/tree/mgsloan-bugfix-maintenance).

Unresolved observations are tracked in [potential-bugs.md](potential-bugs.md).

## Clean fixes for reported issues

- [#5932](https://github.com/tree-sitter/tree-sitter/issues/5932): Update the wildcard-pattern count when disabling query patterns ([`fix/disable-wildcard-pattern`](https://github.com/mgsloan/tree-sitter/compare/de98c6c970f4c5d3a725ee48199c478090d614af...fix/disable-wildcard-pattern?expand=1)).
- [#5934](https://github.com/tree-sitter/tree-sitter/issues/5934): Skip hidden zero-width subtrees during descendant lookup ([`fix/hidden-zero-width-descendant`](https://github.com/mgsloan/tree-sitter/compare/de98c6c970f4c5d3a725ee48199c478090d614af...fix/hidden-zero-width-descendant?expand=1)). This fix has reported performance tradeoffs.
- [#5950](https://github.com/tree-sitter/tree-sitter/issues/5950): Avoid child-index truncation during backward sibling traversal ([`fix-previous-sibling-index-underflow`](https://github.com/mgsloan/tree-sitter/compare/1b8407d1e718f2a26e2886c03cc55622d8d1d7bd...fix-previous-sibling-index-underflow?expand=1)).
- [#5987](https://github.com/tree-sitter/tree-sitter/issues/5987): Retain siblings during byte-based named-child lookup ([`fix/first-named-child-for-byte-upstream`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/first-named-child-for-byte-upstream?expand=1)).

## Vibecoded fixes for reported issues

- [#5948](https://github.com/tree-sitter/tree-sitter/issues/5948): Keep inherited field lookup from crossing visible aliases ([`fix/visible-alias-field-lookup`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/visible-alias-field-lookup?expand=1)).
- [#5949](https://github.com/tree-sitter/tree-sitter/issues/5949): Verify ancestry when looking up a child containing a descendant with the same range ([`fix/child-with-descendant-self`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/child-with-descendant-self?expand=1)).
- [#6005](https://github.com/tree-sitter/tree-sitter/issues/6005): Preserve descendant indices during backward sibling traversal ([`fix/previous-sibling-descendant-index`](https://github.com/mgsloan/tree-sitter/compare/5091247754e69084613e48613fe2215c7df8ec68...fix/previous-sibling-descendant-index?expand=1)).

## Vibecoded fixes for unreported issues

- Preserve aliases when traversing backward past extras ([`fix/previous-sibling-alias-after-extra`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/previous-sibling-alias-after-extra?expand=1)).
- Reject non-rooted query starts outside the cursor root's range while retaining matches spanning a range ([`fix/non-rooted-query-range`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/non-rooted-query-range?expand=1)).
- Preserve non-rooted wildcard matches when ranges prune hidden repetitions ([`fix/non-rooted-wildcard-range-pruning`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/non-rooted-wildcard-range-pruning?expand=1)).
- Prevent phantom captures from unjustified wildcard-child guarantees ([`fix/wildcard-child-guarantees`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/wildcard-child-guarantees?expand=1)).
- Preserve quoted missing and unexpected tokens during Rust S-expression formatting ([`fix/quoted-missing-token-formatting`](https://github.com/mgsloan/tree-sitter/compare/9b9de365078b0c82f8f8d7af175b1a4207c9b4d5...fix/quoted-missing-token-formatting?expand=1)).
- Preserve nullable-root query matches through hidden repetitions when traversal is restricted by ranges ([`fix/nullable-root-hidden-structure`](https://github.com/mgsloan/tree-sitter/compare/5091247754e69084613e48613fe2215c7df8ec68...fix/nullable-root-hidden-structure?expand=1)).
- Complete deferred query matches after ascending from a skipped hidden subtree ([`fix/containing-range-hidden-ascent`](https://github.com/mgsloan/tree-sitter/compare/5091247754e69084613e48613fe2215c7df8ec68...fix/containing-range-hidden-ascent?expand=1)).
- Decode UTF-8 characters split across short input callback chunks ([`fix/short-utf8-input-chunks`](https://github.com/mgsloan/tree-sitter/compare/43e82a2767cf75df4f668752cd884368e21a5ccd...fix/short-utf8-input-chunks?expand=1)).
- Honor query progress-callback cancellation with queued captures and preserve resumable execution ([`fix/query-capture-cancellation`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/query-capture-cancellation?expand=1)).
- Prevent orphan captures from unjustified ERROR-child guarantees ([`fix/error-child-guarantees`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/error-child-guarantees?expand=1)).
- Prevent extra ERROR parents from inheriting structural-child aliases during query execution ([`fix/query-extra-parent-alias`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/query-extra-parent-alias?expand=1)).
- Preserve later siblings when byte-based child lookup exhausts nested hidden nodes ([`fix/first-child-for-byte-hidden-siblings`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/first-child-for-byte-hidden-siblings?expand=1)).
- Include missing nodes at containing-range boundaries ([`fix/query-containing-range-missing-boundary`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/query-containing-range-missing-boundary?expand=1)).
- Preserve wildcard-parent matches at the maximum start depth, including through hidden wrappers and query cloning ([`fix/query-wildcard-parent-depth`](https://github.com/mgsloan/tree-sitter/compare/2ef426e10b4fa8189a761025f17548073249c2fc...fix/query-wildcard-parent-depth?expand=1)).
- Preserve source-order capture iteration for captured wildcard parents ([`fix/query-captured-wildcard-order`](https://github.com/mgsloan/tree-sitter/compare/6070dbfefd326bd735e5683eb128cc1b57dad0c0...fix/query-captured-wildcard-order?expand=1)).
- Find empty visible descendants at the end of hidden subtrees while retaining later siblings ([`fix/descendant-range-hidden-end`](https://github.com/mgsloan/tree-sitter/compare/6070dbfefd326bd735e5683eb128cc1b57dad0c0...fix/descendant-range-hidden-end?expand=1)).
- Correct cursor and query API documentation and Rust documentation links ([`fix/api-documentation`](https://github.com/mgsloan/tree-sitter/compare/20cf25c10f13ed9b8c499dd966b4e7a57bedbf81...fix/api-documentation?expand=1); documentation only).

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
