- [ ] Review and polish design.md

# Queue

- [ ] Make benchmarking more realistic in terms of other CPU loads / cache pressure

- [ ] Revisit choices of bit packing thresholds

# Backburner

- [ ] Consider what APIs could make it faster

- [ ] Tuning:

  * threshold between scan and parent walk

  * symbol presence cache

  * scan window

- [ ] Rust impl for better LTO - or just have Rust-side implementations for small ops

- [ ] Support conversion to and from little-endian representation on big-endian? (for inter-architecture communication)

- [ ] Big-endian specific path for the persistent cache (LMDB repr is not portable)

- [ ] Update persistence cache properly for renames

- [ ] Put grammar-derived tables in LMDB?

- [ ] Consider allowing persistent cache waiters

- [ ] Make sure that the worker contexts also reuse a TreeSitter parser
