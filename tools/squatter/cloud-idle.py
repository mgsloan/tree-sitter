#!/usr/bin/env python3
"""Stop the benchmark VM after 30 idle minutes; jobs hold a shared file lock.

Install as /usr/local/bin/squatter-idle and run `check` from a root systemd
timer every minute. Wrap the entire benchmark driver with `run -- COMMAND`.
The state directory must be writable by the benchmark user. /run clears stale
timestamps on boot; monotonic time avoids wall-clock adjustments.
"""

import argparse
import fcntl
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, default=Path("/run/squatter-benchmark"))
    parser.add_argument("--idle-seconds", type=int, default=1800)
    parser.add_argument("action", choices=("run", "check", "status"))
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    stamp = args.state / "last-busy"

    with (args.state / "activity.lock").open("a+") as lock:
        # The root timer may be the first caller after boot. Keep both files
        # writable by the job user, whose ownership is set on the directory.
        owner = args.state.stat()
        if os.geteuid() == 0:
            os.fchown(lock.fileno(), owner.st_uid, owner.st_gid)

        def record_activity():
            # A killed driver must not leave an empty/truncated timestamp that
            # would prevent future timer checks from reaching the idle cutoff.
            temporary = stamp.with_suffix(f".{os.getpid()}.tmp")
            temporary.write_text(str(time.monotonic()))
            if os.geteuid() == 0:
                os.chown(temporary, owner.st_uid, owner.st_gid)
            temporary.replace(stamp)

        if args.action == "run":
            command = args.command[1:] if args.command[:1] == ["--"] else args.command
            if not command:
                parser.error("run needs a command")
            fcntl.flock(lock, fcntl.LOCK_SH)
            record_activity()
            try:
                # Inherit the lock so an orphaned job still prevents shutdown.
                return subprocess.call(command, pass_fds=(lock.fileno(),))
            finally:
                record_activity()

        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("benchmark work active", flush=True)
            if args.action == "check":
                record_activity()
            return 0

        now = time.monotonic()
        if not stamp.exists():
            record_activity()
        idle = max(0, now - float(stamp.read_text()))
        print(f"idle for {idle:.0f}s; shutdown threshold {args.idle_seconds}s", flush=True)
        if args.action == "check" and idle >= args.idle_seconds:
            if os.geteuid() != 0:
                raise SystemExit("shutdown check must run as root")
            # Keep the exclusive lock until poweroff is requested, closing the
            # race with a new driver starting between the check and shutdown.
            subprocess.run(["systemctl", "poweroff"], check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
