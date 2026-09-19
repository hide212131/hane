import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HANE_PACKAGES = (
    "hane",
    "hane-benchmark",
    "hane-document",
    "hane-editor",
    "hane-markdown",
    "hane-metrics",
    "hane-presentation",
    "hane-session",
    "hane-ui",
)


class ReleaseVersionTests(unittest.TestCase):
    def test_workspace_and_lock_versions_match(self):
        with (ROOT / "Cargo.toml").open("rb") as manifest:
            version = tomllib.load(manifest)["workspace"]["package"]["version"]

        lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
        for package in HANE_PACKAGES:
            match = re.search(
                rf'\[\[package\]\]\nname = "{re.escape(package)}"\nversion = "([^"]+)"',
                lock,
            )
            self.assertIsNotNone(match, package)
            self.assertEqual(match.group(1), version, package)

    def test_release_workflow_publishes_a_new_manifest_version(self):
        workflow = (
            ROOT / ".github" / "workflows" / "windows-release.yml"
        ).read_text(encoding="utf-8")

        for expected in (
            "branches: [main]",
            "- Cargo.toml",
            "cargo metadata --locked",
            'release_tag="$manifest_tag"',
            "must be newer than latest tag",
            "should_release",
            "group: release-builds",
            "cancel-in-progress: false",
            "if ! gh api --paginate",
            "Failed to read repository tags; refusing to release.",
        ):
            self.assertIn(expected, workflow)

        self.assertNotIn(
            'gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$release_tag"',
            workflow,
        )


if __name__ == "__main__":
    unittest.main()
