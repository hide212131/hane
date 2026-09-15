import subprocess
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import rust_ci_path_filter as path_filter


class Tests(unittest.TestCase):
    def test_rust_sources_dependencies_toolchain_vendor_assets_and_ci_run(self):
        paths = [
            "crates/app/src/main.rs",
            "crates/presentation/README.md",
            "vendor/gpui/src/lib.rs",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "assets/app-icon.ico",
            "assets/icons/work-folder/file.svg",
            ".github/workflows/ci.yml",
            ".github/scripts/rust_ci_path_filter.py",
            ".github/tests/test_rust_ci_path_filter.py",
        ]
        for path in paths:
            with self.subTest(path=path):
                self.assertTrue(path_filter.requires_rust_ci(path))

    def test_docs_agents_and_unrelated_automation_skip(self):
        paths = [
            "README.md",
            "docs/architecture.md",
            ".agents/skills/reviewer/SKILL.md",
            ".claude/settings.json",
            ".github/scripts/aadw_observer.py",
            ".github/tests/test_aadw_observer.py",
            ".github/workflows/aadw-notifications.yml",
            "assets/app-icon.svg",
            "assets/phase4-feather.svg",
        ]
        for path in paths:
            with self.subTest(path=path):
                self.assertFalse(path_filter.requires_rust_ci(path))

    def test_similarly_named_paths_do_not_match_prefixes(self):
        self.assertFalse(path_filter.requires_rust_ci("crates-old/example.rs"))
        self.assertFalse(path_filter.requires_rust_ci("vendor-notes/readme.txt"))
        self.assertFalse(path_filter.requires_rust_ci("assets.md"))

    def test_any_rust_input_in_mixed_changes_runs_ci(self):
        self.assertTrue(
            path_filter.changed_paths_require_rust_ci(
                ["docs/design.md", ".agents/config.md", "crates/ui/src/icons.rs"]
            )
        )

    def test_only_skippable_changes_skip_ci(self):
        self.assertFalse(
            path_filter.changed_paths_require_rust_ci(
                ["README.md", "docs/design.md", ".github/scripts/aadw_notify.py"]
            )
        )

    def test_cli_accepts_git_null_delimited_output(self):
        command = [sys.executable, str(SCRIPTS / "rust_ci_path_filter.py"), "--null"]

        run = subprocess.run(
            command,
            input=b"README.md\0docs/design.md\0",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        self.assertEqual(run.stdout, b"false\n")

        run = subprocess.run(
            command,
            input=b"README.md\0crates/app/src/main.rs\0",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        self.assertEqual(run.stdout, b"true\n")


if __name__ == "__main__":
    unittest.main()
