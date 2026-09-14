# Direct-field caching and points experiments, 2026-09-13

The retained conversion change caches direct fields by production in
`SQPackContext`. Ordinary `sq_tree_pack` keeps per-frame scratch so a tiny tree
need not prepare all of its grammar's productions. Cached tables are immutable,
survive trimming, and are freed with the context. Frames and hidden singleton
wrappers use the same lookup. Only non-inherited entries are stored, the first
matching field wins, extras do not consume structural indexes, and indexes past
the cached production's length have no direct field.

The cache occupies 22 B–5.5 KB for the measured grammars (slices plus field IDs),
and the context descriptor grows from 120 to 128 bytes on x86-64. Frame sizes
remain 104 bytes without points and 128 bytes with points. Context creation adds
two allocations for grammars with direct fields; one-shot packing does not pay
this cost. The allocation-failure test now exercises every context-creation
allocation, in addition to every packing allocation.

## Cloud measurements

Existing `squatter-benchmark`, GCP e2-standard-2, Intel Xeon Broadwell 2.20 GHz,
pinned to CPU 0. GCC 15.3.0, `-O3 -g`, no LTO, default 16-slot groups, presence
on, default noncompact packing. Baseline includes the earlier byte-only reverse
position change; it is not bare `7734a5741`. Each comparison uses identical
runtime objects with only `pack.c` replaced. Input and artifact hashes accompany
the raw results.

Nine large inputs: five alternating process pairs, median of seven timed batches
per process; totals sum per-input process medians. Parsing and context creation
are outside timing; packing and output deletion are inside. Context rows reuse
one context. Tiny inputs: 854 files in four grammars, calibrated whole-batch
samples of at least about 10 ms, seven samples and five alternating process
pairs. Bucket changes are medians of paired aggregate changes. These timings are
not directly comparable to older conversion probes that exclude output deletion.

| Points | Packing | Before | Field cache | Change |
|---|---|---:|---:|---:|
| Off | One-shot | 552.73 ms | 552.94 ms | +0.04% |
| Off | Reused context | 549.22 ms | 525.73 ms | -4.28% |
| On | One-shot | 687.62 ms | 685.29 ms | -0.34% |
| On | Reused context | 731.73 ms | 691.95 ms | -5.44% |

Context packing improves on all nine byte-only inputs and eight of nine points
inputs. Points-enabled Python is +3.01%, with substantial process variation
(baseline medians 86–136 ms; candidate 100–169 ms). One-shot controls are flat
in aggregate but noisy by input: points-enabled YAML is +9.90%. These results
support a reused-context improvement, not a universal per-input speedup or
sub-percent conclusions.

| Source bytes | Files | Context, no points | Context, points |
|---|---:|---:|---:|
| 1-64 | 126 | -3.36% | -1.84% |
| 65-256 | 328 | -2.91% | -2.09% |
| 257-1024 | 400 | -5.23% | -3.12% |

One-shot tiny controls range from −0.02% to +3.11%.

| Points experiment (versus field cache) | Large one-shot | Large context |
|---|---:|---:|
| Single-line reverse | +5.53% | +8.25% |
| Point-only scratch | +2.11% | +1.58% |

The second experiment has its own paired field-cache control; ratios compare
within that run, not across the two run intervals.

| Source bytes | Single-line, one-shot | Single-line, context | Point-only, one-shot | Point-only, context |
|---|---:|---:|---:|---:|
| 1-64 | +6.84% | +5.33% | -2.47% | -2.44% |
| 65-256 | +0.99% | +3.54% | +0.18% | +0.05% |
| 257-1024 | +6.08% | +4.49% | +11.07% | +3.86% |

## Points experiments

Two alternatives preserve exact serialized output, but saving position scratch
alone does not establish a conversion speedup.

1. [Single-line reverse positions](single-line-points-2026-09-13.patch): omit
   position arrays for single-line frames and subtract byte and column extents
   independently. Across the nine large inputs, 1,886,111 of 2,473,267 frames
   (76.26%) qualify; this avoids 4,110,357 of 5,537,767 position-array entries
   (74.22%, about 49.3 MB of baseline position-array writes, not peak allocation).
   Multiline and edited frames retain forward calculation. Edited ancestor
   extents can disagree with their children's extents; the added regression
   check covers this case. Changes in byte and column widths are independent.
2. [Point-only scratch](point-only-scratch-2026-09-13.patch): retain forward row
   and column calculation, store an 8-byte `TSPoint` instead of a 12-byte
   `Length`, and subtract bytes while walking backward in all builds. This
   reduces position-array storage by one third without a per-child single-line
   branch.

Neither points experiment is retained: both increase aggregate large-file time.
The working implementation retains forward point positions and the earlier
byte-only reverse calculation. The patches preserve the tested alternatives.
The smaller-scratch candidate also passes all nine exact-output/context checks
and 11-grammar sanitized edge cases, including the edited-tree regression.

## Constant-time bulk attributes

`SQCursorAttributes` and Rust `Attributes` no longer contain child, named-child,
or descendant counts. Their explicit node APIs remain available, and sampled
untimed relationship checks still compare all three counts. C callers and Rust
callers must rebuild and request counts explicitly.

New `sq_node_attributes` / Rust `Node::attributes()` reuse the same shared ID and
metadata decoder as cursor/iterator snapshots. Rust attribute walks use these
constant-time bulk snapshots again; the cached iterator now exercises its block
unpack cache. Navigation-only iterator workloads still leave the cache idle.
No reader speedup is claimed from the packing timings above.

## Validation

- Nine large inputs, points enabled/disabled: exact serialized equality across
  default/forced-growth capacity and compact/noncompact output. Context and
  ordinary output agree across all eight capacity/compaction/presence settings,
  including reuse, trimming, error recovery, and output lifetime.
- Eleven grammars: mainline differential traversal, bulk snapshots versus
  individual accessors, cached/uncached iterator snapshots, persistence, and edge
  cases under ASan/UBSan with leak detection, in both point configurations.
  Count APIs retain their separate comparisons. Existing ignored seek
  differences retain the harness policy; unexpected field differences are zero.
- Four corpus grammars: allocation-failure recovery for packing and every context
  creation allocation under sanitizers.
- Edited-tree byte/point regression covers insertion with unequal byte and column
  widths; context and ordinary output agree.
- Rust tests and 32 files in four grammars, original/mutated inputs, points on/off,
  three attribute-walk selectors: 384 file/workload comparisons, zero failures.

[Raw cloud results, hashes, inputs, diagnostics, and drivers](field-cache-points-cloud-results-2026-09-13.json).
