"""Guard the focused Windows and macOS GUI lock CI job wiring."""

from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_workflow() -> dict:
    result = subprocess.run(
        [
            "ruby",
            "-rjson",
            "-ryaml",
            "-e",
            "puts JSON.generate(YAML.safe_load(File.read(ARGV.fetch(0))))",
            str(ROOT / ".github/workflows/ci.yml"),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


WORKFLOW = load_workflow()


def job(name: str) -> dict:
    return WORKFLOW["jobs"][name]


def step(job_config: dict, name: str) -> dict:
    return next(item for item in job_config["steps"] if item.get("name") == name)


class WindowsGuiLockCiWiringTests(unittest.TestCase):
    def test_path_filter_output_is_connected_to_ci(self):
        rust_ci = job("rust_ci_changes")
        self.assertEqual(
            rust_ci["outputs"]["windows_gui_lock_required"],
            "${{ steps.filter.outputs.windows_gui_lock_required }}",
        )
        filter_step = step(rust_ci, "Detect Rust CI inputs")
        self.assertEqual(filter_step["id"], "filter")
        self.assertIn("rust_ci_path_filter.py --null --windows-gui-lock", filter_step["run"])
        self.assertIn(
            'echo "windows_gui_lock_required=$windows_gui_lock_required" >> "$GITHUB_OUTPUT"',
            filter_step["run"],
        )

    def test_windows_job_runs_the_windows_process_lifecycle_tests(self):
        windows_job = job("windows_gui_lock_tests")
        self.assertEqual(windows_job["runs-on"], "windows-latest")
        self.assertEqual(windows_job["needs"], ["release_only_detector", "rust_ci_changes"])
        self.assertIn("needs.rust_ci_changes.outputs.windows_gui_lock_required == 'true'", windows_job["if"])
        self.assertIn("needs.release_only_detector.outputs.release_only != 'true'", windows_job["if"])
        python_step = step(windows_job, "Install Python")
        self.assertEqual(python_step["uses"], "actions/setup-python@v6")
        self.assertEqual(python_step["with"]["python-version"], "3.12")
        self.assertEqual(
            step(windows_job, "Run Windows GUI lock lifecycle tests")["run"],
            "python scripts/tests/test_windows_gui_lock_launcher.py",
        )

    def test_macos_job_runs_the_existing_gui_validator_lock_tests(self):
        macos_job = job("macos_gui_validator_lock_tests")
        self.assertEqual(macos_job["runs-on"], "macos-latest")
        self.assertEqual(macos_job["needs"], ["release_only_detector", "rust_ci_changes"])
        self.assertIn("needs.rust_ci_changes.outputs.windows_gui_lock_required == 'true'", macos_job["if"])
        self.assertIn("needs.release_only_detector.outputs.release_only != 'true'", macos_job["if"])
        python_step = step(macos_job, "Install Python")
        self.assertEqual(python_step["uses"], "actions/setup-python@v6")
        self.assertEqual(python_step["with"]["python-version"], "3.12")
        self.assertEqual(
            step(macos_job, "Run existing macOS GUI validator lock tests")["run"],
            "python scripts/tests/test_gui_validate.py",
        )


if __name__ == "__main__":
    unittest.main()
