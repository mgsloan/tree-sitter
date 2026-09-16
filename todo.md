# Queue

- [ ] Review and polish design.md.

- [ ] Dig into how grammars are identified in persistence.  And what can be done to increase forward/back compat - key-design.md

- [ ] Better representation of grammar_id

# Backburner

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
