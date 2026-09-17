# Queue

- [ ] Persistence API for using cache even though source file is not yet loaded

- [ ] Persistence API for using cache with source file that's loaded in a rope or similar

     * Have some transformation flags enough to support Zed's transforms (BOM removal, newline canonicalization).  Support loading files with these transforms applied.

- [ ] Dig into how grammars are identified in persistence.  And what can be done to increase forward/back compat - key-design.md

- [ ] Pull PackOptions out of CacheMiss and LoadOptions

- [ ] Persistence API for re-checking a CacheMiss

- [ ] Persistence API for waiting on other writer for some amount of time?

# Backburner

- [ ] Review and polish design.md.

- [ ] Once there's a release have a test for back compat. Store a bunch of
  persisted trees and check that they decode. Consider how to also check forwards compat efficiently

- [ ] Skip persisting when it's better to just reparse

  - Also similar logic for the caches

- [ ] Skip caching when frequently edited? (reduce churn)

- [ ] Consider what APIs could make it faster

- [ ] Tuning:

      * threshold between scan and parent walk

      * symbol presence cache threshold

      * scan window

      * bit packing thresholds

- [ ] Rust impl for better LTO - or just have Rust-side implementations for small ops

- [ ] Update persistence cache properly for renames

- [ ] Consider allowing persistent cache waiters

- [ ] Consider other layer(s) above group bases. Like if the group bases made a very shallow b-tree. Storing such a layer could be more cache friendly for the first few steps of search.

      * Especially could be nice to have hierarchical symbol presence.  Maybe should just change symbol presence stride?  Tricky interaction with forests.

- [ ] Could postorder be better for direct parse to packed?

- [ ] Compression support for persistence?  Compress while compacting.  Use bitpacker?

      * Could also decompress group bases on the fly.  Symbol / field / supertype search doesn't need em

- [ ] Consider using fastpfor style exceptions?  Probably not, avoid branches
