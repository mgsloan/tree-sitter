# Conversion follow-up, 2026-09-13

## Reusable context

`SQPackContext` retains one language, its ordered supertype list and index table,
the public-symbol mapping, and traversal/presence scratch. Per-pack counters and
dictionary IDs reset each call, including after failures. Output trees own their
slabs and retain their own language references. `trim` releases high-water
scratch, retaining grammar tables. Separate contexts support concurrent callers;
one context requires exclusive access. Rust exposes the same ownership through
`PackContext`, whose pack and trim methods require `&mut self`.

[Measurements](conversion-context-2026-09-13.json) were taken on the small GCP
`squatter-benchmark` VM, CPU 0, GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`,
points enabled, 16-slot groups. Five alternating process pairs, with nine timed
samples per small-file process, each doing 16 complete batches; reported times
are divided by 16. Context creation is outside timing, while per-tree output
allocation and deletion are inside. All eight grammars have 300 files below
2 KiB. Lists use newline separation because some YAML paths contain spaces.
An initial sub-millisecond-batch pilot was discarded in favor of these longer
samples.

| Grammar | Ordinary ms/batch | Context ms/batch | Change |
|---|---:|---:|---:|
| C++ | 1.842 | 1.618 | −12.2% |
| CSS | 1.071 | 0.995 | −7.1% |
| Go | 6.817 | 6.597 | −3.2% |
| JSON | 0.945 | 0.890 | −5.8% |
| Python | 0.894 | 0.782 | −12.6% |
| TSX | 1.208 | 1.069 | −11.5% |
| TypeScript | 0.456 | 0.330 | −27.6% |
| YAML | 0.559 | 0.476 | −14.8% |
| Total | 13.792 | 12.757 | −7.5% |

The nine large files are a control, not the target. Against the previous binary,
the new ordinary path measured +0.7% default / +1.2% compact; context measured
−0.8% / +0.8%. These small movements do not establish a large-file benefit.
Default totals were 377.59 / 380.16 / 374.62 ms (old / ordinary / context), compact
385.44 / 390.23 / 388.37 ms. Each uses five alternating pairs of seven-conversion
medians, with equal serialized hashes, bytes, groups, capacities, and node counts.
These are conversion measurements, not parse-plus-convert improvements.

Validation: exact ordinary/context byte equality across eight combinations of
capacity, compaction, and presence on all nine large inputs; context trimming,
wrong-language rejection, invalid arguments, overflow, output lifetime, and
allocation-failure recovery. A large TypeScript input also passed every pack
allocation failure under ASan/UBSan with leak detection. Rust `cargo check`
passed. Alternate-layout and broader sanitizer checks are recorded below when
complete.
