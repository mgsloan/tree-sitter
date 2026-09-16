# Queue

- [ ] Review and polish design.md.

- [ ] Dig into how grammars are identified in persistence.  And what can be done to increase forward/back compat

- [ ] Revisit persistence and slab version tags - should be reset

- [ ] key-design.md

# Backburner

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

- [ ] Support conversion to and from little-endian representation on big-endian? (for inter-architecture communication)

- [ ] Update persistence cache properly for renames

- [ ] Consider allowing persistent cache waiters

- [ ] Revisit grammar_symbol defaults table and persiting it in LMDB or including in generate grammars
