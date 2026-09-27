- [ ] Proper parser APIs

- [ ] Move away from AtomicBool cancellation and to cancellation / progress callback

- [ ] Fuzz tests to help ensure that untrusted bytes don't cause wrong memory access etc.  To make this efficient the solution may be validation

- [ ] What to do about Forest::clone() / Tree::clone()

- [ ] Benchmark whether presence cache rare list actually helps

- [ ] How does growing during parsing estimate how much more space?

## Organization

- [ ] Review and polish design.md

- [ ] Separate viz branch for visualizer

- [ ] Put persistence and cache on a separate branch

- [ ] Split traits into a separate compat crate

## Injections

- [ ] Forests

- [ ] API for query scanning forest region

- [ ] API for query scanning subset of forest region

## Use in Zed

- [ ] Review and polish the traits

- [ ] Make hot loops generic over representation

- [ ] Test that Zed file decode/transform code works.  How to make sure it doesn't diverge? Divergence does not cause incorrectness, but does cause hash mismatches




# Post initial release

## Cleanup

- [ ] fearless_simd is mostly used for feature tokens and the kernel! macro, which could be provied
  by far less code than that crate.

- [ ] API for construction of PointData

- [ ] Open Tree-sitter bug about documentation saying captures occur in source order.

- [ ] Try having a Vec per column - may allow growing to use realloc - to skip copies when it can grow inplace.

      * Can still read columns from a single allocation, and pack them together on serialization

      * Could make sidecars a more homogenous thing - they are just columns stored in a separate allocation

## Parity

- [ ] Implement descendant_index() and goto_descendant() with DescendantIx.  Optimize with index?

- [ ] Implement to_sexp ? Issue is it shows some info tree-squatter doesn't have

- [ ] containing ranges for QueryCursor

## Tree-feller

- [ ] progress and cancellation?

- [ ] operation on

- [ ] ABI 13 and 14?

- [ ] external scanners?

## Correctness

- [ ] Test for back compat. Store a bunch of persisted trees and check that they
  decode.

- [ ] Consider how to also check forwards compat efficiently.

## Persistence

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

- [ ] Optimize use of LineIndex (often prior access is nearby)

- [ ] LineIndex should handle newlines the same as tree-sitter parsing.  This also informs wheher SIMD could be used to populate it.

## Use in ast-grep / similar tools

Parse only the needed info.

## Performance tuning

- [ ] try two columns for symbol_id. If symbol count is < 512 (almost all), can get away with one
  read for the lowest symbols. Motivation is to be able to use memchr. Can also only have one column
  if need be

- [ ] try eliminating / reducing the conversion arena by storing post-order? Or modifying tree-feller?

- [ ] threshold between scan and parent walk

- [ ] symbol presence cache threshold

- [ ] scan window

- [ ] Try pure Rust impl for better LTO - or just have Rust-side implementations for small ops

- [ ] tune iterator code - could shorter impls inline more for more complex pipelines?

- [ ] Update C version with all lessons learned from Rust version - enssure language choice isn't causing performance losses

- [ ] symbol presence optimizations - bitmap per group and then do a SIMD transpose?

- [ ] PGO of the C parts? Switch back to C to allow use of PGO data within the crate?

- [ ] WASM / neon / etc simd

# Backburner

- [ ] Return to query-simd-experiment?

- [ ] Dedupe query compiler with upstream TS?

- [ ] Parent / previous sibling links (slot counts)

## Correctness

- [ ] Shared comparison / property testing repo for tree-sitter, squatter, and feller.

- [ ] Reduce unsafe via bytemuck

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

- [ ] Consider using fastpfor style exceptions?  Probably not, avoid branches

- [ ] Potentially relevant technique: selection pushdown

- [ ] Potentially relevant: vortex-array

- [ ] Use blocking file reads during TS chunk reads to avoid full materialize? Skipping this for now, gnarly to block threads.

- [ ] With feller parse, estimate capacity based on per-grammar stats

- [ ] Reaally not worth it, but could use weak symbols to access "grow_in_place" functions for specific allocators. Interesting that there doesn't seem to be a crate for this.

- [ ] Try OR-ing together presence bitmaps when appropriate

- [ ] Consider progress calback stride
