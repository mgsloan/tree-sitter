# Tighter grammar mask analysis — 2026-09-14

Comparison against the previous version-8 grammar-wide dictionary implementation, not the original tree-local interner. The candidate is version 9. Raw measurements, input hashes, source hashes, compiler details, and all paired samples are in [the JSON results](supertype-tight-results-2026-09-14.json).

The main precision fix identifies nonterminal extras using EOF reductions in null-lookahead lex states. An ordinary recursive self-loop goto does not imply an extra. Analysis visits shared reduction action lists once per state, explores only hidden definitions reachable from supertypes or hidden extras, and handles unary productions using deduplicated hidden incoming transitions. Full predecessor graphs are built only for reachable multi-child productions. The direct-mask allocation path also avoids a second symbol-metadata scan.

## Dictionary size and initialization

Cold context creation/deletion includes building and releasing the dictionary; medians below use the large input cases (initialization itself is source-independent).

| Grammar | Masks before → after | Index bits | Cold initialization | Speedup | Shared retained bytes |
|---|---:|---:|---:|---:|---:|
| csharp | 416 → 14 | 16 → 8 | 80.21 → 8.67 ms | 9.3× | 7,480 → 296 |
| sml | 1,408 → 16 | 16 → 8 | 9.01 → 0.56 ms | 16.2× | 27,704 → 312 |

## Warm operations

Change in process CPU time; negative means faster. Ratios compare the median of three process medians. Small differences are not statistically established. Large membership walks improve about 10%; warm packing does not uniformly improve. Tiny C#/SML warm one-shot operations regress by 8–16% in this run, despite unchanged high-level warm-path logic for dictionary grammars. The experiment does not isolate their cause.

| Input | Warm pack | Reused-context pack | Warm copied load | Membership walk |
|---|---:|---:|---:|---:|
| json-tiny | -3.2% | +0.4% | +1.2% | -0.6% |
| json-large | +2.6% | +1.3% | -0.3% | +3.2% |
| python-tiny | -4.3% | +0.1% | -8.2% | -3.0% |
| python-large | -0.9% | -2.2% | -0.9% | -3.3% |
| cpp-tiny | -7.3% | -0.1% | -10.8% | +0.7% |
| cpp-large | +0.4% | +2.1% | +2.6% | -0.8% |
| tsx-tiny | -7.9% | -3.1% | -5.2% | -2.4% |
| tsx-large | -0.8% | -1.7% | -1.9% | +0.9% |
| csharp-tiny | +7.7% | +4.0% | +10.5% | -8.5% |
| csharp-large | +3.1% | +1.9% | -1.0% | -11.0% |
| sml-tiny | +8.3% | +1.1% | +15.5% | -10.3% |
| sml-large | -1.2% | -4.3% | -2.1% | -9.5% |

Tiny Python/C++ cold one-shot packing improves by 3.7%/9.0%; eliminating the second metadata scan is a plausible contributor. Grammars with at most eight supertypes retain direct masks and unchanged storage.

## Memory

Requested allocator bytes, excluding the parsed tree and grammar library. One-tree retention includes the shared dictionary. First-pack peak starts with an empty cache.

| Input | Slab bytes before → after | Slab change | One tree retained | First-pack peak |
|---|---:|---:|---:|---:|
| csharp-tiny | 728 → 680 | -6.6% | 9,400 → 2,168 | 10,919,095 → 433,678 |
| csharp-large | 167,656 → 158,296 | -5.6% | 176,328 → 159,784 | 10,919,095 → 433,678 |
| sml-tiny | 496 → 464 | -6.5% | 28,888 → 1,464 | 969,879 → 308,066 |
| sml-large | 759,528 → 711,528 | -6.3% | 787,920 → 712,528 | 969,879 → 734,624 |

Compact (`repack=true`) large slabs:
- csharp-large: 150,944 → 142,752 bytes (-5.4%).
- sml-large: 623,608 → 585,256 bytes (-6.2%).

## Method and reproduction

GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, no LTO, points enabled, 16-slot groups, eight-byte alignment. Twelve tiny/large inputs across six compiled grammars; three alternating paired rounds; five calibrated process-CPU samples per operation, at least 15 ms per batch. Parsing is excluded. Timing binaries do not wrap allocations; separate memory binaries do. All paired node counts, group counts, and membership checksums match.

Concurrent builds and another benchmark were active on the shared host. This suite uses process CPU time pinned to CPU 0; small timing differences remain noisy. Do not compare absolute times with the earlier report.

```sh
python3 tools/squatter/benchmark-supertype-cache.py --prepare \
  --baseline-snapshot build/supertype-cache-bench/current \
  --output build/supertype-tight-bench \
  --inputs build/supertype-cache-bench/inputs.json
```

The baseline snapshot is the frozen candidate from the previous experiment; preserve it before replacing live sources. Without `--baseline-snapshot`, the runner still reconstructs the original tree-local baseline. The committed JSON identifies the exact benchmark sources and binaries.

## Correctness and compatibility

Default and points-disabled C checks pass, as do AddressSanitizer/UndefinedBehaviorSanitizer checks with 32-slot groups and 64-byte alignment.

Regression cases cover ordinary recursion versus nonterminal extras, multi-child predecessor walks and alias boundaries, cyclic masks, 16-bit indices, 65,536-entry capacity, multiword masks, deterministic IDs, persistence, and cache lifetime/concurrency. Allocation-failure injection covers both unary and multi-child analysis. C#/SML comparisons include fresh parses of destructive source edits and recovery edge cases; C++/TSX additionally use synthetic nine-supertype metadata. TSX has 80 seek mismatches in both baseline and candidate; other comparison attributes pass.

Version 9 rejects older slabs because tighter mask sets can change dictionary IDs even when the entry count happens to stay the same. The analysis remains conservative over merged LR states; the resulting dictionary is not claimed to be minimal.
