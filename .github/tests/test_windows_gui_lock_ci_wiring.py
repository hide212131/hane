"""Guard the focused Windows and macOS GUI lock CI job wiring."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")


def job_block(name: str) -> str:
    marker = f"  {name}:\n"
    start = WORKFLOW.index(marker) + len(marker)
    next_job = re.search(r"(?m)^  [a-zA-Z0-9_-]+:\s*$", WORKFLOW[start:])
    end = start + next_job.start() if next_job else len(WORKFLOW)
    return WORKFLOW[start:end]


class WindowsGuiLockCiWiringTests(unittest.TestCase):
    def test_path_filter_output_is_connected_to_ci(self):
        self.assertIn(
            "windows_gui_lock_required: ${{ steps.filter.outputs.windows_gui_lock_required }}",
            WORKFLOW,
        )
        self.assertIn("rust_ci_path_filter.py --null --windows-gui-lock", WORKFLOW)
        self.assertIn(
            'echo "windows_gui_lock_required=$windows_gui_lock_required" >> "$GITHUB_OUTPUT"',
            WORKFLOW,
        )

    def test_windows_job_runs_the_windows_process_lifecycle_tests(self):
        job = job_block("windows_gui_lock_tests")
        self.assertIn("runs-on: windows-latest", job)
        self.assertIn("needs.rust_ci_changes.outputs.windows_gui_lock_required == 'true'", job)
        self.assertIn("needs.release_only_detector.outputs.release_only != 'true'", job)
        self.assertIn('python-version: "3.12"', job)
        self.assertIn("python scripts/tests/test_windows_gui_lock_launcher.py", job)

    def test_macos_job_runs_the_existing_gui_validator_lock_tests(self):
        job = job_block("macos_gui_validator_lock_tests")
        self.assertIn("runs-on: macos-latest", job)
        self.assertIn("needs.rust_ci_changes.outputs.windows_gui_lock_required == 'true'", job)
        self.assertIn("needs.release_only_detector.outputs.release_only != 'true'", job)
        self.assertIn('python-version: "3.12"', job)
        self.assertIn("python scripts/tests/test_gui_validate.py", job)


if __name__ == "__main__":
    unittest.main()
