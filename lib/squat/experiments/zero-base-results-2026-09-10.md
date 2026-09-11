# Zero bases without changing the encoding — 2026-09-10

Choosing zero bases where existing u8 deltas can hold absolute values looks
promising for **AVX2 start-column decoding**, which takes 6.5–7.4% less time in
this probe. Per-node scalar reads show no gain. Subtree spans gain almost nothing
from choosing additional zero bases: most eligible groups already have zero
bases. The experiment left production packing, decoding, and the serialized
format unchanged.

This is a fixed-16-slot **column-decoding microbenchmark**, not an end-to-end
iterator benchmark. It does not establish a traversal speedup or justify changing
the default implementation. In particular, the current iterator does not cache
subtree spans; SIMD span decoding here explores a possible bulk consumer.

The experiment preserves the existing encoding exactly:

- Span and start column retain `value = base + u8_delta`. If every live value in
  a group is at most 255, a private copy uses base zero and recomputed u8 deltas.
  Otherwise it retains the original base and deltas. Group boundaries and lane
  widths stay fixed, and no extra flags or sentinel meanings are introduced.
- End column retains `value = base - u8_delta` and its original base/deltas.
  It is only a control for checking already-zero bases. There is no special
  zero-base interpretation and no alternate end-column encoding.
- The base load remains necessary to test whether it is zero. The experiment
  skips arithmetic, not base loads or storage.

Four variants separate the effects: **baseline** uses original bases and always
performs arithmetic; **existing-skip** skips arithmetic for already-zero bases;
**zero-add** chooses additional zero bases but retains arithmetic; **zero-skip**
combines choosing zero bases with skipping arithmetic. Every variant returns
the same live values.

The tables report median per-file elapsed-time ratios, lower is faster.
Each cell gives **forward-file-order pass / reverse-file-order pass**.
The bounded sample has eleven grammar representatives; the large sample has
nine original inputs of at least 1 MiB. All inputs are originals, not mutations.

**Zero-skip / baseline:**

| Sample | Column | Scalar per node | Scalar per group | SSE2 per group | AVX2 per group |
|---|---|---:|---:|---:|---:|
| Bounded | Span | 1.002 / 1.005 | 0.914 / 0.909 | 0.994 / 1.006 | 0.964 / 0.963 |
| Large | Span | 1.003 / 1.004 | 0.901 / 0.888 | 0.967 / 0.971 | 0.957 / 0.958 |
| Bounded | Start column | 1.000 / 1.000 | 0.866 / 0.874 | 0.957 / 0.954 | 0.926 / 0.934 |
| Large | Start column | 1.003 / 1.003 | 0.883 / 0.889 | 0.953 / 0.958 | 0.927 / 0.935 |

Scalar group decoding takes about 11–13% less time for start columns. GCC
15.3.0 folds the additive per-node zero check back into an unconditional add:
`read_add` and `read_add_skip` both load the byte and add the group base.
The group scalar and SIMD functions retain zero paths that omit the arithmetic;
AVX2 also avoids the broadcast on that path. Scalar functions explicitly disable
autovectorization, so the scalar group result is not hidden SIMD.

**Zero-skip / existing-skip**, isolating the additional base choices while
using the same skip kernel:

| Sample | Column | Scalar per group | SSE2 per group | AVX2 per group |
|---|---|---:|---:|---:|
| Bounded | Span | 1.006 / 1.000 | 1.001 / 1.003 | 1.001 / 0.996 |
| Large | Span | 0.996 / 0.994 | 0.995 / 0.996 | 0.998 / 1.000 |
| Bounded | Start column | 0.850 / 0.846 | 1.090 / 1.026 | 0.870 / 0.882 |
| Large | Start column | 0.877 / 0.873 | 1.010 / 1.001 | 0.863 / 0.832 |

Each ratio is calculated per file before taking the median; dividing medians
from separate tables does not reproduce these values. SSE2 is mixed: choosing
additional zero start-column bases does not improve its median over simply
checking existing bases. The generated zero/nonzero loops have different
instruction and branch layouts, so these are implementation measurements,
not isolated instruction-latency measurements.

The unchanged end-column control makes that limitation visible. Adding a
zero check costs roughly 9–11% for per-node scalar reads and 7–8% for AVX2,
while the SSE2 version takes about 11% less time even though almost no end
bases are zero. Thus a blanket check on all columns is not supported, and
not every apparent improvement can be attributed to avoiding arithmetic.
The zero-add controls stay near baseline; all control ratios and raw samples
are retained in the data artifact.

Median fraction of groups across files:

| Sample | Column | Already zero | Eligible for zero |
|---|---|---:|---:|
| Bounded | Span | 90.2% | 93.5% |
| Large | Span | 96.3% | 97.4% |
| Bounded | Start column | 9.0% | 100.0% |
| Large | Start column | 0.8% | 99.9% |

These are equal-file medians, not pooled fractions. Long lines can sharply
reduce eligibility: the large minified TypeScript-parser input has only about
1.1% eligible start-column groups. Span means the physical subtree span,
including any intervening group waste, rather than simply a node count.

Runs used the existing two-vCPU benchmark VM, Xeon 2.20 GHz (family 6, model 79),
pinned to CPU 0, with GCC 15.3.0 `-O2 -g` and no LTO. Both passes used the same
binary. Each file/kernel calibrated batch counts to at least 4 ms of baseline
work, then ran sixteen repeats of all four variants, rotating and reversing
variant order. The second pass reversed the input order. Parsing, packing,
reconstruction of private columns, and correctness checks are outside timing.
Group decoders write all sixteen physical lanes and consume a changing live
lane for a checksum; per-node reads consume every live value. They are separate
workloads and their absolute timings should not be compared directly.

All forty file/pass executions checked every live decoded value for every
variant/kernel, and all cross-variant checksums matched. Synthetic checks cover
u8 eligibility boundaries, partial groups, high-bit bases, and large unsigned
values. ASan/UBSan passed those checks and all variants on the C representative.
No sanitizer timings are used. The Makefile target and Python runner also passed
their build/syntax checks.

Artifacts:

- [Probe source](zero-base.c) and [runner](../../../tools/squatter/benchmark-zero-base.py).
- [Results](zero-base-results-2026-09-10.json), including both passes, per-file
  samples, checksums, source/input/grammar/binary hashes, exact commands,
  CPU metadata, and sanitizer validation metadata.
- [Raw CSVs, logs, build metadata, and assembly](../../../build/squat-zero-base/).

Build the GCC/x86-64 probe with:

```sh
make -C lib/squat ../../build/squat/zero-base-bench
build/squat/zero-base-bench --check
```

On the existing benchmark VM, upload that binary and the runner, then use the
pinned corpus bundle and this report's JSON as the input manifest. The runner
requires a fresh output directory and supports `--reverse` for the second pass:

```sh
squatter-idle run -- python3 benchmark-zero-base.py \
  --bundle /home/mgsloan/squatter-benchmark/named \
  --inputs zero-base-results-2026-09-10.json \
  --binary ./zero-base-bench --output ./fresh-results --repeats 16
```

The data artifact records the exact compile command used for these measurements;
build paths can change binary hashes through assertion strings and debug data.

Following these measurements, zero-base packing for spans/start columns and
zero-addition branches in AVX2/scalar group decoding were adopted at the user's
request. SSE2, per-node arithmetic, and end-column encoding retain their previous
behavior. The tables above describe the original experiment, not an end-to-end
measurement of that adoption. To reproduce the original baseline, use the saved
probe binary or the packer from `bbb0966676d9e6ae2e79f320fb6a5408ff7bcf42`; rebuilding
the probe against the new packer would already start with the chosen zero bases.
