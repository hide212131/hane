import sys
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / ".github" / "scripts"))

import release_pr  # noqa: E402


BASE_MANIFEST = "[workspace]\nmembers = [\"crates/ai\"]\n\n[workspace.package]\nversion = \"0.23.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nserde = \"1\"\n"
HEAD_MANIFEST = BASE_MANIFEST.replace('version = "0.23.0"', 'version = "0.24.0"')

BASE_LOCK = "version = 4\n\n[[package]]\nname = \"hane\"\nversion = \"0.23.0\"\ndependencies = [\"hane-ai\"]\n\n[[package]]\nname = \"hane-ai\"\nversion = \"0.23.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"unchanged\"\n"
HEAD_LOCK = BASE_LOCK.replace('version = "0.23.0"', 'version = "0.24.0"')


class ReleasePrepareTests(unittest.TestCase):
    def test_manifest_update_changes_workspace_package_version(self):
        updated = release_pr.replace_workspace_version(BASE_MANIFEST, "0.23.0", "0.24.0")
        self.assertEqual(release_pr.manifest_version(updated), "0.24.0")
        self.assertIn('serde = "1"', updated)

    def test_lock_update_changes_every_workspace_package_only(self):
        updated = release_pr.update_lock_versions(
            BASE_LOCK, {"hane", "hane-ai"}, "0.23.0", "0.24.0"
        )
        document = release_pr.tomllib.loads(updated)
        versions = {
            package["name"]: package["version"]
            for package in document["package"]
            if package["name"] in {"hane", "hane-ai"}
        }
        self.assertEqual(versions, {"hane": "0.24.0", "hane-ai": "0.24.0"})
        serde = next(package for package in document["package"] if package["name"] == "serde")
        self.assertEqual(serde["checksum"], "unchanged")

    def test_lock_update_rejects_a_missing_workspace_package(self):
        with self.assertRaises(ValueError):
            release_pr.update_lock_versions(
                BASE_LOCK, {"hane", "hane-ai", "hane-ui"}, "0.23.0", "0.24.0"
            )


class ReleaseClassificationTests(unittest.TestCase):
    def validate(self, **overrides):
        values = {
            "changed_paths": {"Cargo.toml", "Cargo.lock"},
            "base_manifest": BASE_MANIFEST,
            "head_manifest": HEAD_MANIFEST,
            "base_lock": BASE_LOCK,
            "head_lock": HEAD_LOCK,
            "package_names": {"hane", "hane-ai"},
        }
        values.update(overrides)
        return release_pr.validate_release_diff(**values)

    def test_accepts_a_workspace_only_version_bump(self):
        valid, _, version = self.validate()
        self.assertTrue(valid)
        self.assertEqual(version, "0.24.0")

    def test_rejects_any_other_changed_file(self):
        valid, _, _ = self.validate(changed_paths={"Cargo.toml", "Cargo.lock", "README.md"})
        self.assertFalse(valid)

    def test_rejects_rust_source_contamination(self):
        valid, _, _ = self.validate(
            changed_paths={"Cargo.toml", "Cargo.lock", "crates/ai/src/lib.rs"}
        )
        self.assertFalse(valid)

    def test_rejects_manifest_changes_other_than_workspace_version(self):
        changed = HEAD_MANIFEST.replace('serde = "1"', 'serde = "2"')
        valid, _, _ = self.validate(head_manifest=changed)
        self.assertFalse(valid)

    def test_rejects_a_workspace_package_added_to_manifest(self):
        changed = HEAD_MANIFEST.replace(
            'members = ["crates/ai"]',
            'members = ["crates/ai", "crates/new"]',
        )
        valid, _, _ = self.validate(head_manifest=changed)
        self.assertFalse(valid)

    def test_rejects_a_workspace_package_removed_from_manifest(self):
        changed = HEAD_MANIFEST.replace('members = ["crates/ai"]', 'members = []')
        valid, _, _ = self.validate(head_manifest=changed)
        self.assertFalse(valid)

    def test_rejects_manifest_comment_changes(self):
        changed = HEAD_MANIFEST.replace(
            "[workspace.package]", "# extra comment\n[workspace.package]"
        )
        valid, _, _ = self.validate(head_manifest=changed)
        self.assertFalse(valid)

    def test_rejects_external_checksum_or_dependency_changes(self):
        changed = HEAD_LOCK.replace('checksum = "unchanged"', 'checksum = "changed"')
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_external_dependency_version_changes(self):
        changed = HEAD_LOCK.replace('version = "1.0.0"', 'version = "1.1.0"')
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_external_dependency_source_changes(self):
        changed = HEAD_LOCK.replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            "registry+https://example.invalid/index",
        )
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_external_dependency_list_changes(self):
        changed = HEAD_LOCK.replace(
            'name = "serde"\nversion = "1.0.0"',
            'name = "serde"\nversion = "1.0.0"\ndependencies = ["other 1.0.0"]',
        )
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_workspace_package_added_to_head_lock(self):
        changed = HEAD_LOCK + '\n[[package]]\nname = "hane-new"\nversion = "0.24.0"\n'
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_workspace_package_removed_from_head_lock(self):
        changed = HEAD_LOCK.replace(
            '\n[[package]]\nname = "hane-ai"\nversion = "0.24.0"\n', "\n"
        )
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_lock_comment_changes(self):
        changed = HEAD_LOCK.replace("version = 4", "version = 4\n# extra comment")
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_a_workspace_package_left_at_the_old_lock_version(self):
        changed = HEAD_LOCK.replace(
            'name = "hane-ai"\nversion = "0.24.0"',
            'name = "hane-ai"\nversion = "0.23.0"',
        )
        valid, _, _ = self.validate(head_lock=changed)
        self.assertFalse(valid)

    def test_rejects_a_non_increasing_version(self):
        manifest = HEAD_MANIFEST.replace('0.24.0', '0.23.0')
        lock = HEAD_LOCK.replace('0.24.0', '0.23.0')
        valid, _, _ = self.validate(head_manifest=manifest, head_lock=lock)
        self.assertFalse(valid)

    def test_rejects_an_older_version(self):
        manifest = HEAD_MANIFEST.replace('0.24.0', '0.22.0')
        lock = HEAD_LOCK.replace('0.24.0', '0.22.0')
        valid, _, _ = self.validate(head_manifest=manifest, head_lock=lock)
        self.assertFalse(valid)

    def test_release_branch_must_match_detected_version(self):
        release_pr.validate_release_branch("release/v0.24.0", "0.24.0")
        with self.assertRaises(ValueError):
            release_pr.validate_release_branch("release/v0.25.0", "0.24.0")

    def test_classifies_candidate_against_base_workspace_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.run(
                    ["git", *args], cwd=root, check=True, capture_output=True, text=True
                ).stdout.strip()

            git("init", "--quiet")
            git("config", "user.name", "Release test")
            git("config", "user.email", "release-test@example.invalid")
            (root / "Cargo.toml").write_text(BASE_MANIFEST, encoding="utf-8")
            (root / "Cargo.lock").write_text(BASE_LOCK, encoding="utf-8")
            git("add", "Cargo.toml", "Cargo.lock")
            git("commit", "--quiet", "-m", "base")
            base = git("rev-parse", "HEAD")

            (root / "Cargo.toml").write_text(HEAD_MANIFEST, encoding="utf-8")
            (root / "Cargo.lock").write_text(HEAD_LOCK, encoding="utf-8")
            git("commit", "--quiet", "-am", "release")
            head = git("rev-parse", "HEAD")

            base_packages = {
                "hane": {"version": "0.23.0"},
                "hane-ai": {"version": "0.23.0"},
            }
            with mock.patch.object(release_pr, "workspace_packages", return_value=base_packages):
                valid, _, version = release_pr.classify(root, base, head)

        self.assertTrue(valid)
        self.assertEqual(version, "0.24.0")


if __name__ == "__main__":
    unittest.main()
