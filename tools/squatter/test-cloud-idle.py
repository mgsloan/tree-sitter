#!/usr/bin/env python3
"""Exercise the idle cutoff without issuing an actual shutdown."""

import contextlib
import fcntl
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "cloud_idle", Path(__file__).with_name("cloud-idle.py"))
idle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(idle)


class IdleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.state = Path(self.directory.name)
        self.stamp = self.state / "last-busy"

    def invoke(self, action, now, *command):
        argv = ["cloud-idle", "--state", str(self.state), action, *command]
        with patch("sys.argv", argv), patch.object(idle.time, "monotonic", return_value=now):
            with contextlib.redirect_stdout(io.StringIO()):
                return idle.main()

    def test_first_check_starts_idle_period(self):
        with patch.object(idle.subprocess, "run") as poweroff:
            self.invoke("check", 10000)
            poweroff.assert_not_called()
        self.assertEqual(float(self.stamp.read_text()), 10000)

    def test_exact_cutoff_and_read_only_status(self):
        self.stamp.write_text("100")
        with patch.object(idle.subprocess, "run") as poweroff:
            self.invoke("check", 1899)
            self.invoke("status", 2000)
            poweroff.assert_not_called()
            with patch.object(idle.os, "geteuid", return_value=0):
                self.invoke("check", 1900)
            poweroff.assert_called_once_with(["systemctl", "poweroff"], check=True)

    def test_active_lock_defers_shutdown(self):
        self.stamp.write_text("0")
        with (self.state / "activity.lock").open("a+") as active:
            fcntl.flock(active, fcntl.LOCK_SH)
            with patch.object(idle.subprocess, "run") as poweroff:
                self.invoke("check", 5000)
                poweroff.assert_not_called()
        self.assertEqual(float(self.stamp.read_text()), 5000)

    def test_driver_lock_and_exit_status(self):
        def child(command, pass_fds):
            self.assertEqual(command, ["example", "argument"])
            self.assertEqual(len(pass_fds), 1)
            with (self.state / "activity.lock").open("a+") as other:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return 17

        with patch.object(idle.subprocess, "call", side_effect=child):
            self.assertEqual(self.invoke("run", 500, "--", "example", "argument"), 17)
        self.assertEqual(float(self.stamp.read_text()), 500)


if __name__ == "__main__":
    unittest.main()
