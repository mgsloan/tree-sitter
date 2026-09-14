# Tiny-file slab order: cloud rerun, 2026-09-13

The cloud rerun does not demonstrate a substantial benefit for very small files.
In the 1–64 byte bucket, all paired aggregate changes are within 1.1%.
For larger buckets, walks tend to improve slightly without points and regress
slightly with points. Packing and seeks are mixed. The shared VM still shows
run-to-run timing variability; small changes are not definitive causal estimates.

This supersedes the performance interpretation of the
[local tiny-file run](slab-order-tiny-results-2026-09-13.md), which was affected
by a laptop power-state change. It reruns the tiny-file suite, not the earlier
large-file or 200-file query suite.

Positive percentages mean slower. Each value is the median of seven paired
aggregate changes, using the same aggregation as the local tiny-file report.

| Points | Source bytes | Files | Ordinary pack | Context pack | Cursor walk | Byte seeks |
|---|---|---:|---:|---:|---:|---:|
| Disabled | 1–64 | 126 | +0.51% | -0.46% | -0.22% | -0.72% |
| Disabled | 65–256 | 328 | +0.09% | -1.54% | -1.48% | -0.65% |
| Disabled | 257–1024 | 400 | -2.44% | -0.97% | -1.11% | +0.94% |
| Enabled | 1–64 | 126 | -0.25% | +0.33% | +1.08% | +0.03% |
| Enabled | 65–256 | 328 | +1.80% | +0.40% | +1.47% | +0.49% |
| Enabled | 257–1024 | 400 | +1.62% | -0.59% | +1.60% | +1.83% |

The byte-only 65–256 byte walk improvement appears in all seven paired aggregate
runs (-2.1% to -0.5%). By contrast, byte-only 257–1024 byte ordinary packing
ranges from -13.4% to +4.9%, despite a median of -2.44%; that packing number
should not be interpreted as a reliable speedup. The full raw data retain these
outliers, both aggregation methods, and all individual timed samples.

## Machine and method

GCP `squatter-benchmark`, `us-central1-a`, two-vCPU Xeon Broadwell machine
(model 79, 2.20 GHz), Linux x86-64. Runs pinned to CPU 0 and executed serially.
The VM started from a stopped state for this task. The benchmark was the only
CPU-intensive process observed during the run. The driver held the existing
`squatter-idle` lock for the full benchmark.

Exactly the same four before/after, points-disabled/enabled binaries as the
local tiny-file run: GCC 15.3.0, `-O3 -g -fno-omit-frame-pointer`, default
16-slot groups, eight-byte alignment, default noncompact packing. Baseline is
`7533e47d9c2d95398431edb325b4c03713c4f7a4`; candidate contains the pending slab,
SQLayout, resize-copy ordering and version-6 changes. The system ELF loader is
invoked explicitly so the uploaded binaries do not need their local Nix loader
path. Executable and grammar hashes are recorded and verified before timing.

Same 854 distinct nonempty corpus files up to 1 KiB, four grammars, three size
buckets. Of 126 files up to 64 bytes, 100 occupy one live slab group. Every input
content hash was verified on the VM. Each of 22 grammar/bucket/point-mode rows
uses seven alternating process pairs, with seven calibrated samples per
operation per process: 154 pairs total. Each sample targets at least about
10 ms of process CPU time by repeating whole batches.

Ordinary packing, reusable-context packing, full-attribute cursor walking,
and eight byte-range seeks per tree use [tiny-layout.c](tiny-layout.c).
Parsing is outside timing; output allocation/deletion is inside packing timing.
Read workloads repeatedly revisit prepared trees without explicit cache eviction.
This is a warm-batch experiment; it does not measure cold first access,
compact slabs, queries, or iterator-specific traversal.

Every input walk checksum matches mainline. Every before/after process pair
matches file/node/group/one-group/source-byte/slab-byte counts and all four
operation checksums. No validation failures occurred.

## Artifacts

[Complete cloud results, machine details, hashes, and paired summaries](slab-order-tiny-cloud-results-2026-09-13.json).
Local copies of raw output, machine metadata, and aggregation scripts are in
`build/slab-order-tiny/cloud-results`. The uploaded bundle and runner are in
`build/slab-order-tiny/cloud-upload`; the same bundle remains at
`/home/mgsloan/slab-order-tiny-cloud-20260913` on the VM.

Run the driver with `python3 -I run.py`: isolated mode prevents the uploaded
`json.so` grammar from shadowing Python's standard-library `json` module.
The runner validates hashes, pins CPU affinity, alternates the two versions,
and checkpoints completed rows. Use a fresh output path for a new measurement.
