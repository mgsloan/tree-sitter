#!/usr/bin/env python3
"""Opt-in LE/BE slab compatibility test using Zig and QEMU user emulation."""
import argparse
import pathlib
import shlex
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]


def run(command):
    print(shlex.join(map(str, command)), flush=True)
    subprocess.run(command, check=True, timeout=600)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--grammar", type=pathlib.Path, required=True,
                        help="grammar directory containing src/parser.c (optional scanner.c)")
    parser.add_argument("--symbol", required=True, help="grammar export, e.g. tree_sitter_json")
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True,
                        help="fresh directory for binaries and exchanged slabs")
    parser.add_argument("--cc", default="cc")
    parser.add_argument("--zig", default="zig")
    parser.add_argument("--bits", type=int, choices=[32, 64], default=64,
                        help="big-endian guest pointer width (default: 64)")
    parser.add_argument("--qemu", help="defaults to qemu-ppc or qemu-ppc64")
    args = parser.parse_args()
    if sys.byteorder != "little":
        parser.error("run on a little-endian host; QEMU supplies the big-endian peer")
    grammar = args.grammar.resolve() / "src"
    source = args.source.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    squat = ROOT / "lib/squat"
    # Use the Makefile's source list so new library components are included.
    sources = next(line.split(":=", 1)[1].split()
                   for line in (squat / "Makefile").read_text().splitlines()
                   if line.startswith("SOURCES :="))
    common = ["-O2", "-UNDEBUG", "-std=c11", "-D_DEFAULT_SOURCE",
              f"-DSQ_TEST_LANGUAGE={args.symbol}",
              "-I" + str(ROOT / "lib/include"), "-I" + str(ROOT / "lib/src"),
              "-I" + str(squat / "include"), "-I" + str(grammar),
              str(squat / "tests/slab-compatibility.c"),
              *[str(squat / name) for name in sources],
              str(ROOT / "lib/src/lib.c"), str(grammar / "parser.c")]
    if (grammar / "scanner.c").exists():
        common.append(str(grammar / "scanner.c"))
    run([args.cc, *common, "-DSQ_EXPECT_BIG_ENDIAN=0", "-o", output / "little"])
    target = "powerpc64-linux-musl" if args.bits == 64 else "powerpc-linux-musleabihf"
    emulator = args.qemu or ("qemu-ppc64" if args.bits == 64 else "qemu-ppc")
    run([args.zig, "cc", "-target", target, "-static", *common,
         "-DSQ_EXPECT_BIG_ENDIAN=1", "-o", output / "big"])
    peers = {"little": [output / "little"], "big": [emulator, output / "big"]}
    for name, command in peers.items():
        run([*command, "static", args.symbol, source, output / name])
    failures = []
    for reader, writer in [("little", "little"), ("big", "big"),
                           ("big", "little"), ("little", "big")]:
        try:
            run([*peers[reader], "static", args.symbol, source,
                 output / f"{reader}-from-{writer}", output / writer])
        except subprocess.CalledProcessError:
            failures.append(f"{writer} -> {reader}")
    if failures:
        raise SystemExit("FAILED: " + ", ".join(failures))
    print("ok: identical LE/BE bytes and copied/borrowed node reads in both directions")


if __name__ == "__main__":
    main()
