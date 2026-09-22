# MVP

## Polish

- [ ] Review and polish design.md

- [x] Library that uses tree-sitter instead of a fork

- [ ] Document Send+Sync for scans

- [ ] Separate viz branch for visualizer

- [x] Can query compilation code be similar to upstream with a minimized diff?

- [x] Port core to Rust.  Use the scan iterators to reduce code complexity

- [x] Decide how much to use newtypes.

- [ ] Should presence and points have magic? hmmmm.. Should tree even?

- [ ] A deferred first writer can resurrect retired records after missing-file cleanup.
  crates/persistence/src/store.rs:618 treats CurrentGuard::Missing as valid when the current record
  is absent again. I reproduced: capture a deferred first load → publish another load → delete the
  file and finish cleanup → publish the deferred load. Publication succeeds and restores the retired
  records. The guard needs to distinguish “never published” from “published and subsequently
  retired.”

- [ ] Look into the unexpected cargo features

## Performance

- [x] https://github.com/Dekker1/tree-feller

      >   codex resume 01a0b257-8b8e-7123-bdd6-c5deddc08fb7

- [ ] Try adding sibling / parent jump columns

- [ ] slot count and field widths. Range scan benchmarks suggest 32 slots might
  be better for both space and time.

- [ ] Make groups cache-line aligned via offsets and choice of initial group
  count capacity?

## Query

- [x] Range containment

## Injections

- [ ] Forests

## Use in Zed

- [ ] Change persistence path to .tree-sitter/squat.*?

- [ ] Review and polish the traits

- [ ] Make hot loops generic over representation

- [ ] Test that Zed file decode/transform code works.  How to make sure it doesn't diverge? Divergence does not cause incorrectness, but does cause hash mismatches

## Misc




# Post MVP

## Correctness

- [ ] Test for back compat. Store a bunch of persisted trees and check that they
  decode.

- [ ] Consider how to also check forwards compat efficiently.

## Persistence

- [ ] Put on a separate branch

- [ ] Dedicated heed thread(s)?

- [ ] Dig into how grammars are identified in persistence.  And what can be done to increase forward/back compat - key-design.md

     * Do sources specify their grammar?

- [ ] Pull PackOptions out of CacheMiss and LoadOptions

- [ ] Persistence API for re-checking a CacheMiss

- [ ] Persistence API for waiting on other writer for some amount of time?

- [ ] Skip persisting when it's better to just reparse

      * Consider similar logic for the side caches

      * Consider also tracking loading speed - may be better to parse
        instead when I/O is slow.

- [ ] Skip caching when frequently edited? (reduce churn)

- [ ] Make sure that the DB isn't trusted - no exploits via DB contents.

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


# Backburner

- [ ] Dedupe query compiler with upstream TS?

## Correctness

- [ ] Shared comparison / property testing repo for tree-sitter, squatter, and feller.

## Persistene

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
