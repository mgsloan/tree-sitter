# MVP

## Polish

- [ ] Review and polish design.md

- [ ] Separate viz branch for visualizer

- [x] Rename the prepared language wrapper to Language.

- [ ] Put persistence and cache on a separate branch

## Injections

- [ ] Forests

## Use in Zed

- [ ] Review and polish the traits

- [ ] Make hot loops generic over representation

- [ ] Test that Zed file decode/transform code works.  How to make sure it doesn't diverge? Divergence does not cause incorrectness, but does cause hash mismatches




# Post MVP

## Cleanup

- [ ] fearless_simd is mostly used for feature tokens and the kernel! macro, which could be provied
  by far less code than that crate.

- [ ] API for construction of PointData

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

## Use in ast-grep / similar tools

Parse only the needed info.

## Performance tuning

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
