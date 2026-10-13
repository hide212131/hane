"""Local fixture tests for prepare-codex-runtime.py.

These never touch the network or any real upstream archive: every test
builds a small synthetic `tar.gz` in a temporary directory and a matching
synthetic lock/pinned-inventory, then exercises the pipeline against those
fixtures via `archive_override`. No secrets or user environment are used.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import sys
import tarfile
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

MODULE_PATH = Path(__file__).resolve().parent.parent / "prepare-codex-runtime.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("prepare_codex_runtime", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


pcr = _load_module()


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_tar_gz(path: Path, entries: list) -> None:
    """`entries`: list of dicts with `name`, optional `kind` ("file" default,
    "dir", or "symlink"), `content` (bytes, for files), `mode`, `linkname`."""
    with tarfile.open(path, mode="w:gz") as archive:
        for spec in entries:
            name = spec["name"]
            kind = spec.get("kind", "file")
            if kind == "dir":
                info = tarfile.TarInfo(name=name)
                info.type = tarfile.DIRTYPE
                info.mode = spec.get("mode", 0o755)
                archive.addfile(info)
            elif kind == "symlink":
                info = tarfile.TarInfo(name=name)
                info.type = tarfile.SYMTYPE
                info.linkname = spec.get("linkname", "target")
                archive.addfile(info)
            else:
                content = spec.get("content", b"")
                info = tarfile.TarInfo(name=name)
                info.size = len(content)
                info.mode = spec.get("mode", 0o644)
                archive.addfile(info, io.BytesIO(content))


ENTRYPOINT_CONTENT = b"fake-codex-app-server-binary"
LIBRARY_CONTENT = b"fake-shared-library"
LICENSE_CONTENT = b"fake license text"


def standard_entries():
    return [
        {"name": "codex-app-server", "content": ENTRYPOINT_CONTENT, "mode": 0o755},
        {"name": "lib/helper.so", "content": LIBRARY_CONTENT, "mode": 0o644},
        {"name": "LICENSE.txt", "content": LICENSE_CONTENT, "mode": 0o644},
    ]


def inventory_for(entries_with_content: list) -> list:
    inventory = []
    for name, content, executable in entries_with_content:
        inventory.append(
            {
                "path": name,
                "type": "file",
                "size": len(content),
                "sha256": sha256_hex(content),
                "executable": executable,
            }
        )
    return sorted(inventory, key=lambda e: e["path"])


def standard_inventory() -> list:
    return inventory_for(
        [
            ("codex-app-server", ENTRYPOINT_CONTENT, True),
            ("lib/helper.so", LIBRARY_CONTENT, False),
            ("LICENSE.txt", LICENSE_CONTENT, False),
        ]
    )


class PipelineFixture:
    """Builds a self-contained lock + pinned inventory + archive under a
    temporary directory, mirroring the real `assets/ai/` layout closely
    enough for `build_bundle` to run unmodified against it."""

    def __init__(self, tmp_root: Path, *, archive_entries=None, pinned_inventory=None, licenses=None):
        self.tmp_root = tmp_root
        self.lock_dir = tmp_root / "lock-dir"
        self.lock_dir.mkdir()
        (self.lock_dir / "inventory").mkdir()

        self.archive_path = tmp_root / "archive.tar.gz"
        make_tar_gz(self.archive_path, archive_entries if archive_entries is not None else standard_entries())
        archive_bytes = self.archive_path.read_bytes()

        pinned = pinned_inventory if pinned_inventory is not None else standard_inventory()
        (self.lock_dir / "inventory" / "test-target-1.0.0.json").write_text(json.dumps(pinned), encoding="utf-8")

        self.lock_path = self.lock_dir / "codex-runtime-lock.json"
        self.lock_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "upstream_tag": "test-tag",
                    "codex_version": "1.0.0",
                    "source_commit": "deadbeef",
                    "targets": {
                        "test-target": {
                            "url": "https://github.com/example/example/releases/download/test/archive.tar.gz",
                            "archive_size": len(archive_bytes),
                            "archive_sha256": sha256_hex(archive_bytes),
                            "entrypoint_name": "codex-app-server",
                            "inventory": "inventory/test-target-1.0.0.json",
                            "licenses": licenses if licenses is not None else ["LICENSE.txt"],
                            "runtime_profile": {"name": "default"},
                            "config_profile": {"name": "default"},
                            "state_profile": {"name": "default"},
                        }
                    },
                }
            ),
            encoding="utf-8",
        )

    def build(self, output_dir: Path) -> Path:
        return pcr.build_bundle(
            target="test-target",
            lock_path=self.lock_path,
            output_dir=output_dir,
            archive_override=self.archive_path,
        )


class BuildBundleTests(unittest.TestCase):
    def test_valid_archive_produces_expected_bundle_layout(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root)
            output_dir = tmp_root / "out" / "bundle"

            result = fixture.build(output_dir)

            self.assertEqual(result, output_dir)
            manifest = json.loads((output_dir / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["schema_version"], 1)
            self.assertEqual(manifest["target"], "test-target")
            self.assertEqual(manifest["codex_version"], "1.0.0")
            self.assertEqual(manifest["source_commit"], "deadbeef")
            self.assertEqual(manifest["entrypoint"], "codex-app-server")
            self.assertEqual(manifest["licenses"], ["LICENSE.txt"])
            self.assertEqual(len(manifest["files"]), 3)

            payload_entrypoint = output_dir / "payload" / "codex-app-server"
            self.assertEqual(payload_entrypoint.read_bytes(), ENTRYPOINT_CONTENT)
            self.assertTrue(payload_entrypoint.stat().st_mode & 0o111)

            license_copy = output_dir / "licenses" / "LICENSE.txt"
            self.assertEqual(license_copy.read_bytes(), LICENSE_CONTENT)

    def test_archive_size_mismatch_is_rejected_and_output_untouched(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root)
            lock_data = json.loads(fixture.lock_path.read_text(encoding="utf-8"))
            lock_data["targets"]["test-target"]["archive_size"] += 1
            fixture.lock_path.write_text(json.dumps(lock_data), encoding="utf-8")
            output_dir = tmp_root / "out" / "bundle"

            with self.assertRaises(pcr.PipelineError):
                fixture.build(output_dir)
            self.assertFalse(output_dir.exists())

    def test_archive_hash_mismatch_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root)
            lock_data = json.loads(fixture.lock_path.read_text(encoding="utf-8"))
            lock_data["targets"]["test-target"]["archive_sha256"] = "0" * 64
            fixture.lock_path.write_text(json.dumps(lock_data), encoding="utf-8")
            output_dir = tmp_root / "out" / "bundle"

            with self.assertRaises(pcr.PipelineError):
                fixture.build(output_dir)
            self.assertFalse(output_dir.exists())

    def test_failed_rebuild_does_not_clobber_an_existing_good_bundle(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root)
            output_dir = tmp_root / "out" / "bundle"
            fixture.build(output_dir)
            original_manifest = (output_dir / "manifest.json").read_text(encoding="utf-8")

            broken_pinned = tmp_root / "lock-dir" / "inventory" / "test-target-1.0.0.json"
            broken_pinned.write_text(json.dumps(standard_inventory()[:1]), encoding="utf-8")

            with self.assertRaises(pcr.PipelineError):
                fixture.build(output_dir)

            self.assertEqual((output_dir / "manifest.json").read_text(encoding="utf-8"), original_manifest)

    def test_path_traversal_entry_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "../escaped", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("../escaped", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_absolute_path_entry_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "/etc/passwd", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("/etc/passwd", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_drive_prefixed_path_entry_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "C:/evil", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("C:/evil", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_alternate_data_stream_style_path_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "codex-app-server:hidden", "content": b"x"}]
            fixture = PipelineFixture(
                tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("codex-app-server:hidden", b"x", False)])
            )
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_windows_reserved_name_component_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "CON.txt", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("CON.txt", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_trailing_dot_component_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "trailing.", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("trailing.", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_trailing_space_component_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "trailing ", "content": b"x"}]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("trailing ", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_duplicate_entry_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [
                {"name": "dup.txt", "content": b"x"},
                {"name": "dup.txt", "content": b"y"},
            ]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("dup.txt", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_case_fold_collision_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [
                {"name": "Name.txt", "content": b"x"},
                {"name": "name.txt", "content": b"y"},
            ]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("Name.txt", b"x", False)]))
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_symlink_entry_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "link", "kind": "symlink", "linkname": "/etc/passwd"}]
            placeholder_pinned = [
                {"path": "link", "type": "file", "size": 0, "sha256": "0" * 64, "executable": False}
            ]
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=placeholder_pinned)
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_oversized_entry_is_rejected(self):
        original_max = pcr.MAX_ENTRY_SIZE
        pcr.MAX_ENTRY_SIZE = 4
        try:
            with TemporaryDirectory() as tmp:
                tmp_root = Path(tmp)
                entries = [{"name": "big.bin", "content": b"too-big"}]
                fixture = PipelineFixture(
                    tmp_root, archive_entries=entries, pinned_inventory=inventory_for([("big.bin", b"too-big", False)])
                )
                with self.assertRaises(pcr.PipelineError):
                    fixture.build(tmp_root / "out" / "bundle")
        finally:
            pcr.MAX_ENTRY_SIZE = original_max

    def test_empty_license_list_is_fail_closed(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root, licenses=[])
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_missing_license_file_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root, licenses=["MISSING-LICENSE.txt"])
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_license_name_collision_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = standard_entries() + [{"name": "nested/LICENSE.txt", "content": b"other license", "mode": 0o644}]
            pinned = standard_inventory() + inventory_for([("nested/LICENSE.txt", b"other license", False)])
            fixture = PipelineFixture(
                tmp_root,
                archive_entries=entries,
                pinned_inventory=pinned,
                licenses=["LICENSE.txt", "nested/LICENSE.txt"],
            )
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_ambiguous_entrypoint_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [
                {"name": "codex-app-server", "content": ENTRYPOINT_CONTENT, "mode": 0o755},
                {"name": "nested/codex-app-server", "content": ENTRYPOINT_CONTENT, "mode": 0o755},
                {"name": "LICENSE.txt", "content": LICENSE_CONTENT, "mode": 0o644},
            ]
            pinned = inventory_for(
                [
                    ("codex-app-server", ENTRYPOINT_CONTENT, True),
                    ("nested/codex-app-server", ENTRYPOINT_CONTENT, True),
                    ("LICENSE.txt", LICENSE_CONTENT, False),
                ]
            )
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=pinned)
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_missing_entrypoint_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            entries = [{"name": "LICENSE.txt", "content": LICENSE_CONTENT, "mode": 0o644}]
            pinned = inventory_for([("LICENSE.txt", LICENSE_CONTENT, False)])
            fixture = PipelineFixture(tmp_root, archive_entries=entries, pinned_inventory=pinned, licenses=["LICENSE.txt"])
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_missing_pinned_inventory_is_fail_closed(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            fixture = PipelineFixture(tmp_root)
            (tmp_root / "lock-dir" / "inventory" / "test-target-1.0.0.json").unlink()
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")

    def test_pinned_inventory_hash_mismatch_is_rejected(self):
        with TemporaryDirectory() as tmp:
            tmp_root = Path(tmp)
            tampered = standard_inventory()
            tampered[0]["sha256"] = "1" * 64
            fixture = PipelineFixture(tmp_root, pinned_inventory=tampered)
            with self.assertRaises(pcr.PipelineError):
                fixture.build(tmp_root / "out" / "bundle")


class UrlSafetyTests(unittest.TestCase):
    def test_redact_url_strips_query_and_fragment(self):
        signed = "https://objects.githubusercontent.com/path/to/archive?X-Amz-Signature=secret&X-Amz-Expires=300"
        self.assertEqual(
            pcr.redact_url(signed),
            "https://objects.githubusercontent.com/path/to/archive",
        )

    def test_download_rejects_disallowed_host_without_any_network_access(self):
        with TemporaryDirectory() as tmp:
            dest = Path(tmp) / "out.bin"
            with self.assertRaises(pcr.PipelineError):
                pcr.download_archive(
                    "https://evil.example/archive.tar.gz",
                    10,
                    "0" * 64,
                    dest,
                )
            self.assertFalse(dest.exists())

    def test_download_rejects_non_https_scheme(self):
        with TemporaryDirectory() as tmp:
            dest = Path(tmp) / "out.bin"
            with self.assertRaises(pcr.PipelineError):
                pcr.download_archive(
                    "http://github.com/archive.tar.gz",
                    10,
                    "0" * 64,
                    dest,
                )
            self.assertFalse(dest.exists())


if __name__ == "__main__":
    unittest.main()
