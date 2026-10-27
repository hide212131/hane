#!/usr/bin/env python3
"""Tests for scripts/windows_gui_lock_launcher.py.

Lock path injection happens here, outside the production CLI: helper processes
call ``launch_with_lock`` with a temporary path so the real canonical lock is
never taken. OS-lock lifecycle tests run on Windows only.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import windows_gui_lock_launcher as wl

SCRIPTS_DIR = str(Path(__file__).resolve().parent.parent)

RUNNER = (
    "import sys; sys.path.insert(0, sys.argv[1]);"
    "import windows_gui_lock_launcher as wl;"
    "from pathlib import Path;"
    "sys.exit(wl.launch_with_lock(Path(sys.argv[2]), sys.argv[3:]))"
)

WIN = sys.platform == "win32"


def start_launcher(lock: Path, command: list[str]) -> subprocess.Popen:
    return subprocess.Popen(
        [sys.executable, "-c", RUNNER, SCRIPTS_DIR, str(lock), *command],
        stderr=subprocess.PIPE, text=True,
    )


def wait_for(predicate, timeout=20.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return False


class CliContractTests(unittest.TestCase):
    def test_requires_separator_and_command(self):
        for argv in ([], ["--"], ["python"]):
            self.assertEqual(wl.main(argv), wl.EXIT_USAGE)

    def test_profile_failure_does_not_launch(self):
        with mock.patch.object(wl, "canonical_lock_path", side_effect=wl.LockError("no profile")), \
                mock.patch.object(wl.subprocess, "Popen") as popen:
            self.assertEqual(wl.main(["--", "candidate.exe"]), wl.EXIT_BLOCKED)
        popen.assert_not_called()

    def test_lock_os_error_does_not_launch(self):
        with mock.patch.object(wl, "acquire_exclusive", side_effect=wl.LockError("os error")), \
                mock.patch.object(wl.subprocess, "Popen") as popen:
            self.assertEqual(wl.launch_with_lock(Path("x.lock"), ["candidate.exe"]), wl.EXIT_BLOCKED)
        popen.assert_not_called()

    def test_busy_lock_does_not_launch(self):
        with mock.patch.object(wl, "acquire_exclusive", side_effect=wl.LockBusy("busy")), \
                mock.patch.object(wl.subprocess, "Popen") as popen:
            self.assertEqual(wl.launch_with_lock(Path("x.lock"), ["candidate.exe"]), wl.EXIT_BLOCKED)
        popen.assert_not_called()

    def test_production_cli_has_no_lock_override(self):
        # Anything before "--" is a usage error; there is no path option.
        self.assertEqual(wl.main(["--lock", "x.lock", "--", "candidate.exe"]), wl.EXIT_USAGE)


@unittest.skipUnless(WIN, "real OS lock lifecycle requires Windows")
class WindowsLockLifecycleTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.lock = self.dir / "gui-validation.lock"
        self.procs: list[subprocess.Popen] = []
        self.addCleanup(self._kill_all)

    def _kill_all(self):
        for p in self.procs:
            if p.poll() is None:
                p.kill()
                p.wait()
            if p.stderr:
                p.stderr.close()

    def spawn(self, command):
        p = start_launcher(self.lock, command)
        self.procs.append(p)
        return p

    def hold_command(self, ready: Path, release: Path) -> list[str]:
        code = (
            "import pathlib,time,sys;"
            "pathlib.Path(sys.argv[1]).write_text('x');"
            "r=pathlib.Path(sys.argv[2]);"
            "t=time.time()\n"
            "while not r.exists() and time.time()-t<60: time.sleep(0.05)"
        )
        return [sys.executable, "-c", code, str(ready), str(release)]

    def test_lock_lifecycle_across_processes(self):
        ready = self.dir / "ready"
        release = self.dir / "release"
        first = self.spawn(self.hold_command(ready, release))
        self.assertTrue(wait_for(ready.exists), "child did not start")

        marker = self.dir / "second-ran"
        second = self.spawn([sys.executable, "-c", "import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('x')", str(marker)])
        self.assertEqual(second.wait(timeout=20), wl.EXIT_BLOCKED)
        self.assertFalse(marker.exists(), "blocked launcher must not start its child")

        # Kill the launcher; the child keeps the inherited handle, so the lock stays held.
        first.kill()
        first.wait()
        third = self.spawn([sys.executable, "-c", "pass"])
        self.assertEqual(third.wait(timeout=20), wl.EXIT_BLOCKED)

        release.write_text("x")
        # Child exit releases the lock; poll until the OS has reaped it.
        def reacquire() -> bool:
            p = self.spawn([sys.executable, "-c", "pass"])
            return p.wait(timeout=20) == 0
        self.assertTrue(wait_for(reacquire, 30), "lock was not released after child exit")

    def test_child_exit_status_and_argv_with_spaces(self):
        out = self.dir / "argv.txt"
        code = "import sys,pathlib;pathlib.Path(sys.argv[1]).write_text(repr(sys.argv[2:]));sys.exit(7)"
        p = self.spawn([sys.executable, "-c", code, str(out), "a b", "c  d", 'e"f'])
        self.assertEqual(p.wait(timeout=30), 7)
        self.assertEqual(out.read_text(), repr(["a b", "c  d", 'e"f']))

    def test_missing_parent_fails_closed(self):
        marker = self.dir / "ran"
        p = start_launcher(
            self.dir / "missing" / "gui-validation.lock",
            [sys.executable, "-c", "import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('x')", str(marker)],
        )
        self.procs.append(p)
        self.assertEqual(p.wait(timeout=20), wl.EXIT_BLOCKED)
        self.assertFalse(marker.exists())

    def test_canonical_path_uses_profile_relative_location(self):
        path = wl.canonical_lock_path()
        self.assertEqual(path.parts[-3:], (".cache", "hane", "gui-validation.lock"))


if __name__ == "__main__":
    unittest.main()
