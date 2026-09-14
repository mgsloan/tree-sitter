# Performance follow-up

Keep active experiments here; measurements and rejected alternatives belong in
`lib/squat/experiments/`.

- Optimize the cached iterator's snapshot construction. Typical C/Python
  profiles put about 32% of byte-only walk samples in
  `sq_node_iterator_attributes`, plus about 12% in language name/metadata
  accessors. Evaluate sharing grammar metadata across nodes or preparing it
  per grammar; measure the memory and tiny-tree setup costs.
- Investigate block ID unpacking, about 20% of the same profiles. Check which
  columns the snapshot needs and compare implementations on the cloud CPU
  before changing dispatch. Preserve individual getter and uncached controls.
- Inspect the existing parser-hint and exact-capacity work on
  `parser-hints-exact-capacity` (`44075e48b`, cloud results `92d1f2ef0`)
  before starting another packing-capacity experiment. The earlier percentage
  sweep retained the conservative default.

Completed: O(1) bulk snapshots, direct-field context caching, byte-only reverse
positions, batch-consumer context reuse, and scalar decoder-parameter caching.
Points scratch reductions are not active work: both measured alternatives lost
on aggregate large-file packing. The old `conversion-optimizations.md` roadmap
was removed because it mixed completed work and rejected experiments.

Evidence:

- [Bulk walks, decoder caching, and profiles](lib/squat/experiments/bulk-walk-results-2026-09-13.md)
- [Field caching and points experiments](lib/squat/experiments/field-cache-points-results-2026-09-13.md)
- [Byte-only reverse positions and individual-getter walks](lib/squat/experiments/byte-reverse-results-2026-09-13.md)
- [Earlier conversion results](lib/squat/experiments/conversion-results-2026-09-13.md)
