# Queue

- [ ] Review and polish design.md

- [ ] Put grammar-derived tables in LMDB

- [ ] Big-endian specific path for the persistent cache (LMDB repr is not portable)

- [ ] Make sure that the worker contexts also reuse a TreeSitter parser

# Backburner

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
