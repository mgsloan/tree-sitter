#!/usr/bin/env python3
"""Exchange Squatter slabs between little- and big-endian builds."""
import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
BIG_TARGET = 'powerpc64-unknown-linux-musl'


def run(command, **kwargs):
    print(shlex.join(map(str, command)), flush=True)
    subprocess.run(command, cwd=ROOT, check=True, timeout=600, **kwargs)


def compiler_environment(zig, target, zig_target, compiler):
    compiler.write_text(
        '#!/usr/bin/env python3\nimport os, sys\n'
        + 'command = ' + repr([zig, 'cc', '-target', zig_target]) + '\n'
        + 'os.execvp(command[0], command + [argument for argument in sys.argv[1:] '
        'if argument != "-Wl,-melf_i386"])\n'
    )
    compiler.chmod(0o755)
    headers = subprocess.run(
        [zig, 'cc', '-target', zig_target, '-E', '-v', '-x', 'c', '/dev/null'],
        capture_output=True, text=True, check=True,
    ).stderr
    headers = headers.split('#include <...> search starts here:', 1)[1]
    headers = headers.split('End of search list.', 1)[0]
    clang_arguments = ['-nostdinc']
    for path in headers.splitlines():
        if path.strip():
            clang_arguments.extend(['-isystem', path.strip()])
    environment = os.environ.copy()
    # Zig supplies the target spelling, headers, and startup objects.
    environment['CRATE_CC_NO_DEFAULTS'] = '1'
    target = target.replace('-', '_')
    environment[f'BINDGEN_EXTRA_CLANG_ARGS_{target}'] = shlex.join(clang_arguments)
    environment[f'CC_{target}'] = str(compiler)
    environment[f'CARGO_TARGET_{target.upper()}_LINKER'] = str(compiler)
    environment[f'CARGO_TARGET_{target.upper()}_RUSTFLAGS'] = (
        '-C target-feature=+crt-static -C link-self-contained=no'
    )
    return environment


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='fresh output directory')
    parser.add_argument('--target-dir', type=Path, help='reuse Cargo artifacts')
    parser.add_argument('--bits', type=int, choices=[32, 64], default=64,
                        help='little-endian peer pointer width')
    parser.add_argument('--zig', default='zig')
    parser.add_argument('--qemu', default='qemu-ppc64')
    args = parser.parse_args()
    if sys.byteorder != 'little' or sys.maxsize <= 2**32:
        parser.error('run on a 64-bit little-endian host')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    target_dir = args.target_dir.resolve() if args.target_dir else output / 'target'
    command = [
        'cargo', 'build', '--locked', '-p', 'tree-squatter',
        '--example', 'slab-compatibility', '--target-dir', str(target_dir),
    ]
    little_target = 'i686-unknown-linux-musl' if args.bits == 32 else None
    if little_target:
        environment = compiler_environment(args.zig, little_target, 'x86-linux-musl', output / 'cc-little')
        run(command + ['--target', little_target], env=environment)
        little_directory = target_dir / little_target
    else:
        run(command)
        little_directory = target_dir
    environment = compiler_environment(args.zig, BIG_TARGET, 'powerpc64-linux-musl', output / 'cc-big')
    run(command + ['--target', BIG_TARGET], env=environment)
    peers = {
        'little': [little_directory / 'debug/examples/slab-compatibility'],
        'big': [args.qemu, target_dir / BIG_TARGET / 'debug/examples/slab-compatibility'],
    }
    for name, command in peers.items():
        run([*command, name, output / name])
    for reader, writer in [('little', 'little'), ('big', 'big'), ('big', 'little'), ('little', 'big')]:
        run([*peers[reader], reader, output / f'{reader}-from-{writer}', output / writer])
    print('ok: identical LE/BE slabs and copied/borrowed reads in both directions')


if __name__ == '__main__':
    main()
