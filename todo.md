
Viz
Fuzz
Docs
Lean

# Todos

- [ ] Sweep of primitives for spots they should be newtypes

- [ ] newtype for SlotSpan?  slot.is_multiple_of(GROUP_SIZE) helper method - other such methods?

- [ ] Do not namespace qualify types that are unique to tree-squatter

- [ ] Make SlotIx private

- [ ] Update agent rules about documentation and refine documentation.  Should copy text from tree-sitter docs where sensible.  Should describe what's important / guaranteed to the user, not how it's implemented

- [ ] Fuzz tests to help ensure that untrusted bytes don't cause wrong memory access etc.  To make this efficient the solution may be validation

- [ ] set_point_data is called in some spots where validation shouldn't be needed

## Organization

`experimental` branch which has tree-sitter / persistence / viz / lean. My development happens here.

`main` branch which only has squatter and tree-feller and nothing else

Script lives on `experimental` which helps out with merging back to `main` - handles any new deletions and new moves.

## Forests

- [ ] Figure out forest construction and supplying all grammars.  How many bytes are used for symbols / grammar id needs to be known upfront

- [*] API for query scanning forest region

- [*] API for query scanning subset of forest region

## Use in Zed

- [ ] Review and polish the traits

- [ ] Test that Zed file decode/transform code works.  How to make sure it doesn't diverge? Divergence does not cause incorrectness, but does cause hash mismatches

- [ ] Prototype without injections support




# Post initial release

## Injections

* Consider having one forest per injection-depth.  Otherwise we need to run injection queries and then grow the forest.

## Cleanup

- [ ] Review and polish design.md

- [ ] Open Tree-sitter bug about documentation saying captures occur in source order.

- [ ] Try having a Vec per column - may allow growing to use realloc - to skip copies when it can grow inplace.

      * Can still read columns from a single allocation, and pack them together on serialization

      * Could make sidecars a more homogenous thing - they are just columns stored in a separate allocation

- [ ] Reduce unsafe via bytemuck?

## Parity

- [ ] Implement descendant_index() and goto_descendant() with DescendantIx.  Optimize with index?

- [ ] Implement to_sexp ? Issue is it shows some info tree-squatter doesn't have

- [ ] containing ranges for QueryCursor

## Tree-feller

- [ ] Try implementing the callback in Rust not C

- [ ] progress and cancellation? Currently just never calls the callback

- [ ] operation on

- [ ] ABI 13 and 14?

- [ ] external scanners?

## Correctness

- [ ] Test for back compat. Store a bunch of persisted trees and check that they
  decode.

- [ ] Consider how to also check forwards compat efficiently.

## Persistence

- [ ] Clean up tests / benchmarks

- [ ] Ability to use tree-feller as a fallback

- [ ] Define flat format that includes sidecars

- [ ] Store injections in a forest separate from the main tree

- [ ] Dedicated heed thread(s)?

- [ ] Pull PackOptions out of CacheMiss and LoadOptions

- [ ] API for re-checking a CacheMiss

- [ ] API for waiting on other writer for some amount of time?

- [ ] Skip persisting when it's better to just reparse

      * Consider similar logic for the side caches

      * Consider also tracking loading speed - may be better to parse
        instead when I/O is slow.

- [ ] Skip caching when frequently edited? (reduce churn)

- [ ] Make sure that the DB isn't trusted - no exploits via DB contents.

- [ ] Store last access info for GC

- [ ] Make PendingWrite cancellation clean up if it did get written

## Use in ast-grep / similar tools

Parse only the needed info.

## Performance tuning

- [ ] threshold between scan and parent walk

- [ ] symbol presence cache threshold

- [ ] scan window

- [ ] Threshold for eager subtree packing

- [ ] Try pure Rust impl for better LTO - or just have Rust-side implementations for small ops

- [ ] tune iterator code - could shorter impls inline more for more complex pipelines?

- [ ] symbol presence optimizations - bitmap per group and then do a SIMD transpose?

- [ ] PGO of the C parts? Switch back to C to allow use of PGO data within the crate?

- [ ] WASM / neon / etc simd

- [ ] How does growing during parsing estimate how much more space?

# Backburner

- [ ] Does a `grow` method make sense?  Forest::grow() is just plain appealing

- [ ] Dedupe query compiler with upstream TS?

- [ ] Parent / previous sibling links (slot counts)

- [ ] Remap symbol IDs to make some queries able to check an interval?  Remapping is already happening, but it is deterministic based on grammar definitions.  For example, if the symbols matched at the top of highlighting queries were all a contiguous range, it could benefit from SIMD comparisons

- [ ] `PointsData` could be treated as a cache where the inputs are the tree + source code (or
  newline index). In other words, it's possible to build it after the fact. However, this can cause more slots to get wasted in groups due to points not fitting, though this should be rare in practice. So it would require being able to shift the data in the main slab and all other caches, and this would make anything that holds SlotIx potentially be invalid.  Doesn't seem worth the complexity.

- [ ] Could use the scan API to allow for a pretty way to restrict queries. So, it would build up a
  scan struct that implements a trait which outputs a struct with the restrictions. For now this
  idea is discarded - complexity seems more than it's worth. Tree-sitter's semantics for query
  restriction also make the story more complex than what such an API would imply.  For example, captures() filters by the restriction whereas matches may return captures outside the restriction.

- [ ] Containment index for regions with nested ordering? This can potentially make range restricted
  queries on injections a little faster. Deferring for now, as it may just use new regions at each nesting depth.

- [ ] Scan APIs that scan over regions?

## Correctness

- [ ] Shared comparison / property testing repo for tree-sitter, squatter, and feller.

## Persistence

- [ ] Update persistence cache properly for renames

- [ ] Consider allowing persistent cache waiters

- [ ] Consider other layer(s) above group bases. Like if the group bases made a very shallow b-tree. Storing such a layer could be more cache friendly for the first few steps of search.

      * Especially could be nice to have hierarchical symbol presence.  Maybe should just change symbol presence stride?  Tricky interaction with forests.

- [ ] Compression support for persistence?  Compress while compacting.  Use bitpacker crate? `fastlanes` crate (has cmp support)?

      * Could also decompress group bases on the fly.  Symbol / field / supertype search doesn't need em

- [ ] Document choice to not cache for symlinks that point outside the root

- [ ] Capacity estimates based on stale cache size

## Performance

- [ ] Radix table on monotonic bases

- [ ] With feller parse, estimate capacity based on per-grammar stats

- [ ] Reaally not worth it, but could use weak symbols to access "grow_in_place" functions for specific allocators. Interesting that there doesn't seem to be a crate for this.

- [ ] Try OR-ing together presence bitmaps when appropriate

- [ ] Consider progress calback stride

- [ ] Not a big win, but could use perfect hashing for the supertype masks table

- [ ] Benchmark supertype small list-like set vs supertype bitmap for keys

- [ ] Wild and probably impractical / unuseful idea: parallel parsing. The thought is to find chunk
  boundaries that have good heuristics for determining parse state - such as indentation level.
  Intuition is that "I'm about to read a declaration" should be predictable for a lot of normal
  code.

- [ ] Explicit memory prefetching?

# Misc

- [ ] Some applications like ast-grep only care about a file if it contains particular node types.
  The client being able to have some custom handling of the tree-feller stream could allow for early
  exit before packing.
