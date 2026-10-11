import json
import re
import subprocess
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def workspace_packages():
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    member_ids = set(metadata["workspace_members"])
    packages = [
        package for package in metadata["packages"] if package["id"] in member_ids
    ]
    if not packages or len({package["name"] for package in packages}) != len(packages):
        raise AssertionError("workspace package 名を一意に取得できません")
    return packages


class ReleaseVersionTests(unittest.TestCase):
    def test_workspace_and_lock_versions_match_for_every_member(self):
        with (ROOT / "Cargo.toml").open("rb") as manifest:
            version = tomllib.load(manifest)["workspace"]["package"]["version"]

        packages = workspace_packages()
        self.assertEqual({package["version"] for package in packages}, {version})
        package_names = {package["name"] for package in packages}

        with (ROOT / "Cargo.lock").open("rb") as lock_file:
            lock = tomllib.load(lock_file)
        local_packages = [
            package
            for package in lock["package"]
            if package.get("source") is None and package["name"] in package_names
        ]
        self.assertEqual(
            {package["name"] for package in local_packages},
            package_names,
        )
        self.assertEqual(len(local_packages), len(package_names))
        for package in local_packages:
            self.assertEqual(package["version"], version, package["name"])

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
            "if ! gh api --paginate",
            "Failed to read repository tags; refusing to release.",
            "expected_sha:",
            'EXPECTED_SHA: ${{ inputs.expected_sha }}',
            'if [ "$GITHUB_SHA" != "$EXPECTED_SHA" ]; then',
        ):
            self.assertIn(expected, workflow)

        self.assertNotIn(
            'gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$release_tag"',
            workflow,
        )

    def test_automated_release_dispatch_is_bound_to_the_merge_commit(self):
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("      - name: Merge with the tested head SHA\n        id: merge", workflow)
        self.assertIn('merge_sha="$(jq -er \'.sha\' <<< "$result")"', workflow)
        self.assertIn('RELEASE_SOURCE_SHA: ${{ steps.merge.outputs.merge_sha }}', workflow)
        self.assertIn('--field "expected_sha=$RELEASE_SOURCE_SHA"', workflow)

    def test_release_request_uses_a_token_that_triggers_pull_request_ci(self):
        workflow = (
            ROOT / ".github" / "workflows" / "release-request.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("RELEASE_AUTOMATION_TOKEN", workflow)
        self.assertIn("gh pr create", workflow)
        self.assertIn("gh run watch", workflow)
        self.assertIn("branch: ${{ steps.validate.outputs.branch }}", workflow)
        self.assertNotIn("branch: ${{ steps.create.outputs.branch }}", workflow)

    def test_release_ci_does_not_prefix_the_release_tag_twice(self):
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            "RELEASE_TAG: ${{ needs.release_checks.outputs.release_version }}",
            workflow,
        )
        self.assertNotIn(
            "RELEASE_TAG: v${{ needs.release_checks.outputs.release_version }}",
            workflow,
        )

    def test_release_merge_job_can_read_required_checks_and_statuses(self):
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        merge_job = workflow.split("  merge_release_pr:", 1)[1]

        self.assertIn("checks: read", merge_job)
        self.assertIn("statuses: read", merge_job)

    def test_release_automation_documents_main_ruleset_requirements(self):
        guide = (ROOT / ".github" / "RELEASE_AUTOMATION.md").read_text(
            encoding="utf-8"
        )

        self.assertIn("cargo test / clippy (macos-latest)", guide)
        self.assertIn("cargo test / clippy (windows-latest)", guide)
        self.assertIn("required approval review", guide)

    def test_release_notes_exclude_release_only_pulls(self):
        config = (ROOT / ".github" / "release.yml").read_text(encoding="utf-8")
        self.assertRegex(
            config,
            re.compile(r"changelog:\s+exclude:\s+labels:\s+- release"),
        )


if __name__ == "__main__":
    unittest.main()

