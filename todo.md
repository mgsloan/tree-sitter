# MVP

## Polish

- [ ] Review and polish design.md

- [ ] Library that uses tree-sitter instead of a fork

- [ ] Document Send+Sync for scans

- [ ] Write blog post

- [ ] Separate viz branch for visualizer

- [ ] Can query compilation code be similar to upstream with a minimized diff?

- [ ] Port core to Rust.  Use the scan iterators to reduce code complexity

## Performance

- [ ] https://github.com/Dekker1/tree-feller

      >   codex resume 01a0b257-8b8e-7123-bdd6-c5deddc08fb7

      * [ ] Relatedly, also have a postorder representation?!

- [ ] Try adding sibling / parent jump columns

## Query

- [ ] Range containment

## Injections

- [ ] Forests

## Use in Zed

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

## Use in ast-grep / similar tools

Parse only the needed info.

## Performance tuning

- [ ] try eliminating / reducing the conversion arena by storing post-order? Or modifying tree-feller?

- [ ] slot count and field widths

- [ ] threshold between scan and parent walk

- [ ] symbol presence cache threshold

- [ ] scan window

- [ ] Try pure Rust impl for better LTO - or just have Rust-side implementations for small ops


# Backburner

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
