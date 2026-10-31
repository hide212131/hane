"""Fixture tests for scripts/release_catalog.py (Issue #510, stage U2).

Everything runs with throwaway keys generated here. No production key, Apple
identity or certificate is used, read or needed; the fixtures are never published.
"""
from __future__ import annotations

import base64
import datetime as dt
import io
import json
import os
import plistlib
import re
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import release_catalog as rc  # noqa: E402
from release_catalog import ReleaseError  # noqa: E402

SHA = "0123456789abcdef0123456789abcdef01234567"
OTHER_SHA = "89abcdef0123456789abcdef0123456789abcdef"
TAG = "v0.27.0"
VERSION = "0.27.0"
TEAM = "ABCDE12345"  # only used to show a catalog carrying a Team ID is refused
KEY_ID = "hane-update-test"
REPO = rc.REPOSITORY
NOW = dt.datetime(2026, 10, 19, tzinfo=dt.timezone.utc)
LATER = NOW + dt.timedelta(days=1)


def identity(version=VERSION, target=rc.WINDOWS_TARGET, source=SHA, protocol=1) -> bytes:
    return (f"hane-updater-identity-v1;version={version};target={target};"
            f"source={source};protocol={protocol};end").encode()


def pe(version=VERSION, source=SHA, target=rc.WINDOWS_TARGET) -> bytes:
    return b"MZ\x90\x00" + b"\0" * 32 + identity(version, target, source) + b"\0" * 32


def macho(version=VERSION, source=SHA, signed=True) -> bytes:
    """A 64-bit Mach-O header with (by default) an LC_CODE_SIGNATURE load command."""
    command = (0x1D).to_bytes(4, "little") + (16).to_bytes(4, "little") + b"\0" * 8 if signed else b""
    header = (b"\xcf\xfa\xed\xfe" + b"\0" * 12 + (1 if signed else 0).to_bytes(4, "little")
              + len(command).to_bytes(4, "little") + b"\0" * 8)
    return header + command + identity(version, rc.MACOS_TARGET, source) + b"\0" * 32


def manifest(version=VERSION, dll_version=None, executable="hane.exe") -> str:
    path = f"shell-integration\\{dll_version or version}\\hane_shell_extension.dll"
    return f"""<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
    xmlns:com="http://schemas.microsoft.com/appx/manifest/com/windows10">
  <Identity Name="Hane.ShellIntegration" Publisher="CN=Test" Version="{version}.0" ProcessorArchitecture="neutral" />
  <Applications><Application Id="Hane" Executable="{executable}">
    <Extensions><com:Extension Category="windows.comServer"><com:ComServer><com:SurrogateServer>
      <com:Class Id="92b8fc05-dc63-44ed-9afc-fb10a36bee8a" Path="{path}" ThreadingModel="STA" />
      <com:Class Id="a6aab9f1-298b-4a9b-9419-81938c993571" Path="{path}" ThreadingModel="STA" />
    </com:SurrogateServer></com:ComServer></com:Extension></Extensions>
  </Application></Applications>
</Package>
"""


def msix(version=VERSION, dll_version=None, signed=True, executable="hane.exe") -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as package:
        package.writestr("AppxManifest.xml", manifest(version, dll_version, executable))
        if signed:
            package.writestr("AppxSignature.p7x", b"PKCX")
    return buffer.getvalue()


def write_zip(path: Path, entries) -> None:
    """entries: iterable of (name, data, unix_mode)."""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data, mode in entries:
            info = zipfile.ZipInfo(name, date_time=rc.ZIP_TIMESTAMP)
            info.create_system = 3
            info.external_attr = mode << 16
            # A ZipInfo passed to writestr ignores the archive default and would be stored uncompressed.
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)


class Fixture:
    """Throwaway key plus builders for both target ZIPs."""

    def __init__(self, directory: Path):
        self.dir = Path(directory)
        self.seed = os.urandom(32)
        self.public = rc.public_key_of(self.seed)
        self.trust = rc.Trust({KEY_ID: self.public})

    def sign(self, domain: bytes, raw: bytes, key_id: str = KEY_ID) -> bytes:
        return rc.make_signature_file(domain, raw, key_id, self.seed)

    def windows_files(self, *, version=VERSION, source=SHA, dll_version=None, manifest_version=None,
                      signed_msix=True, helper=None) -> dict:
        dll_version = dll_version or version
        return {
            "hane.exe": pe(version, source),
            "hane-updater.exe": helper if helper is not None else pe(version, source),
            "Hane.ShellIntegration.msix": msix(manifest_version or version, dll_version, signed_msix),
            "Hane.ShellIntegration.cer": b"-----CERT-----",
            "Trust-HaneShellCertificate.ps1": b"# trust",
            f"shell-integration/{dll_version}/hane_shell_extension.dll": b"MZ" + b"dll" * 10,
            "README.md": b"readme",
            "LICENSE": b"license",
        }

    def windows_zip(self, path: Path, *, tweak=None, drop=(), extra=None, **kwargs) -> Path:
        """Uses the real generator (build_inventory + make_zip) for the happy path."""
        files = self.windows_files(**kwargs)
        for name in drop:
            files.pop(name, None)
        files.update(extra or {})
        staging = Path(tempfile.mkdtemp(dir=self.dir))
        for name, data in files.items():
            target = staging / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        version = kwargs.get("version", VERSION)
        source = kwargs.get("source", SHA)
        raw = rc.build_inventory(staging, version, source)
        if tweak:
            document = json.loads(raw)
            tweak(document)
            raw = rc.canonical_json(document)
        (staging / rc.INVENTORY_NAME).write_bytes(raw)
        (staging / rc.INVENTORY_SIGNATURE_NAME).write_bytes(self.sign(rc.INVENTORY_DOMAIN, raw))
        rc.make_zip(staging, path)
        return path

    def macos_zip(self, path: Path, *, version=VERSION, source=SHA, plist_updates=None, drop=(), extra=(),
                  helper=None, bundle_id=rc.MACOS_BUNDLE_ID, code_resources=True, tweak=None, tamper=None,
                  inventory=True, signer=None, app_binary=None, legacy_inventory=False,
                  sidecar_version=None, sidecar_source=None) -> Path:
        """Layout 2 fixture. The app is sealed (CodeResources present, Mach-Os signed); this is
        structural only and says nothing about a real Apple signature. The Ed25519 inventory is
        the sidecar `Hane.app.update/<version>-<source>/`, never inside the app."""
        info = {
            "CFBundleIdentifier": bundle_id,
            "CFBundleExecutable": "hane",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version,
            "CFBundleVersion": version,
            "HaneSourceCommit": source,
            "HaneUpdaterProtocol": 1,
        }
        info.update(plist_updates or {})
        for key in [k for k, v in info.items() if v is None]:
            del info[key]
        app = rc.MACOS_APP
        entries = {
            f"{app}/Contents/Info.plist": (plistlib.dumps(info), 0o644),
            f"{app}/Contents/MacOS/hane": (app_binary if app_binary is not None else macho(version, source), 0o755),
            f"{app}/Contents/Helpers/hane-updater": (helper if helper is not None else macho(version, source), 0o755),
            f"{app}/Contents/Resources/Hane.icns": (b"icns", 0o644),
            f"{rc.MACOS_ROOT}/README.md": (b"readme", 0o644),
        }
        if code_resources:
            entries[f"{app}/Contents/_CodeSignature/CodeResources"] = (b"<plist/>", 0o644)
        for name in drop:
            entries.pop(f"{app}/{name}", None)
        if inventory:
            staging = Path(tempfile.mkdtemp(dir=self.dir))
            for name, (data, _) in entries.items():
                if name.startswith(f"{app}/"):
                    target = staging / name[len(app) + 1:]
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(data)
            files = rc._inventory_files(staging, ())
            raw = rc._inventory_document(files, version=version, target=rc.MACOS_TARGET, source_sha=source,
                                         layout=rc.MACOS_LAYOUT)
            if tweak:
                document = json.loads(raw)
                tweak(document)
                raw = rc.canonical_json(document)
            sign = (signer or self).sign
            inventory_path, signature_path = rc.macos_sidecar_paths(sidecar_version or version,
                                                                    sidecar_source or source)
            if legacy_inventory:  # the v0.27.0 place, which layout 2 refuses
                inventory_path = f"{app}/{rc.MACOS_LEGACY_INVENTORY_REL}"
                signature_path = f"{app}/{rc.MACOS_LEGACY_INVENTORY_SIGNATURE_REL}"
            entries[inventory_path] = (raw, 0o644)
            entries[signature_path] = (sign(rc.INVENTORY_DOMAIN, raw), 0o644)
        for name, data in (tamper or {}).items():  # changed after the inventory was made
            entries[f"{app}/{name}"] = (data, entries.get(f"{app}/{name}", (b"", 0o644))[1])
        write_zip(path, [(n, d, m) for n, (d, m) in entries.items()] + list(extra))
        return path

    def release(self, name: str = "release", *, sequence: int = 1, windows=None, macos=None) -> Path:
        directory = self.dir / name
        directory.mkdir()
        windows = windows or self.windows_zip(self.dir / f"{name}-w.zip")
        macos = macos or self.macos_zip(self.dir / f"{name}-m.zip")
        (directory / rc.WINDOWS_ASSET).write_bytes(Path(windows).read_bytes())
        (directory / rc.MACOS_ASSET).write_bytes(Path(macos).read_bytes())
        raw = rc.build_catalog(directory, tag=TAG, source_sha=SHA, sequence=sequence,
                               trust=self.trust, published=NOW)
        (directory / rc.CATALOG_NAME).write_bytes(raw)
        (directory / rc.SIGNATURE_NAME).write_bytes(self.sign(rc.CATALOG_DOMAIN, raw))
        return directory

    def resign(self, directory: Path, mutate) -> None:
        catalog = json.loads((directory / rc.CATALOG_NAME).read_bytes())
        mutate(catalog)
        raw = rc.canonical_json(catalog)
        (directory / rc.CATALOG_NAME).write_bytes(raw)
        (directory / rc.SIGNATURE_NAME).write_bytes(self.sign(rc.CATALOG_DOMAIN, raw))

    def verify(self, directory: Path, **kwargs):
        defaults = dict(tag=TAG, source_sha=SHA, trust=self.trust, now=LATER)
        defaults.update(kwargs)
        return rc.verify_release_dir(directory, **defaults)


class TempCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.fx = Fixture(Path(self._tmp.name))
        self.tmp = Path(self._tmp.name)

    def other_fixture(self) -> Fixture:
        """A second, unrelated throwaway key (never trusted by self.fx)."""
        directory = Path(tempfile.mkdtemp(dir=self.tmp))
        return Fixture(directory)


class CatalogAndSignatureTests(TempCase):
    def test_complete_release_verifies_and_covers_both_targets(self):
        directory = self.fx.release()
        catalog = self.fx.verify(directory, sequence=1)
        self.assertEqual(set(catalog["targets"]), {rc.WINDOWS_TARGET, rc.MACOS_TARGET})
        windows = catalog["targets"][rc.WINDOWS_TARGET]
        self.assertEqual(windows["archive_sha256"], rc.file_sha256(directory / rc.WINDOWS_ASSET)[1])
        self.assertEqual(windows["archive_size"], (directory / rc.WINDOWS_ASSET).stat().st_size)
        self.assertEqual(windows["layout_revision"], 2)
        mac = catalog["targets"][rc.MACOS_TARGET]
        self.assertEqual((mac["bundle_id"], mac["bundle_version"]), (rc.MACOS_BUNDLE_ID, VERSION))
        self.assertNotIn("team_id", mac)
        self.assertEqual(mac["layout_revision"], 2)
        with zipfile.ZipFile(directory / rc.MACOS_ASSET) as archive:
            inventory = archive.read(rc.macos_sidecar_paths(VERSION, SHA)[0])
        self.assertEqual(mac["inventory_sha256"], rc.sha256_hex(inventory))

    def test_catalog_bytes_are_signed_raw_and_tampering_is_rejected(self):
        directory = self.fx.release(sequence=5)
        path = directory / rc.CATALOG_NAME
        raw = path.read_bytes()
        self.assertIn(b'"catalog_sequence": 5', raw)
        path.write_bytes(raw.replace(b'"catalog_sequence": 5', b'"catalog_sequence": 6'))
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            self.fx.verify(directory)

    def test_signature_is_domain_separated(self):
        directory = self.fx.release()
        raw = (directory / rc.CATALOG_NAME).read_bytes()
        (directory / rc.SIGNATURE_NAME).write_bytes(self.fx.sign(rc.INVENTORY_DOMAIN, raw))
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            self.fx.verify(directory)
        (directory / rc.SIGNATURE_NAME).write_bytes(self.fx.sign(rc.CATALOG_DOMAIN, raw))
        self.fx.verify(directory)
        self.assertEqual(rc.CATALOG_DOMAIN, b"hane-update-catalog-v1\0")
        self.assertEqual(rc.INVENTORY_DOMAIN, b"hane-install-v1\0")

    def test_unknown_or_wrong_key_is_rejected_and_never_auto_registered(self):
        directory = self.fx.release()
        other = self.other_fixture()
        raw = (directory / rc.CATALOG_NAME).read_bytes()
        (directory / rc.SIGNATURE_NAME).write_bytes(other.sign(rc.CATALOG_DOMAIN, raw))
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            self.fx.verify(directory)
        (directory / rc.SIGNATURE_NAME).write_bytes(other.sign(rc.CATALOG_DOMAIN, raw, key_id="attacker"))
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            self.fx.verify(directory)

    def test_malformed_signature_files_are_rejected(self):
        directory = self.fx.release()
        raw = (directory / rc.CATALOG_NAME).read_bytes()
        for bad in (b"{}", b"not json", b'{"schema":1,"signatures":[]}',
                    b'{"schema":2,"signatures":[{"key_id":"hane-update-test","signature_base64":"AA=="}]}',
                    b'{"schema":1,"schema":1,"signatures":[]}'):
            with self.assertRaises(ReleaseError):
                rc.verify_signature(rc.CATALOG_DOMAIN, raw, bad, self.fx.trust)

    def test_duplicate_json_keys_are_rejected(self):
        with self.assertRaisesRegex(ReleaseError, "duplicate key"):
            rc.strict_json(b'{"a":1,"a":2}')

    def test_hash_mismatch_with_the_zip_is_rejected(self):
        directory = self.fx.release()
        self.fx.resign(directory, lambda c: c["targets"][rc.MACOS_TARGET].update(archive_sha256="0" * 64))
        with self.assertRaisesRegex(ReleaseError, "archive_sha256"):
            self.fx.verify(directory)

    def test_changed_zip_after_cataloging_is_rejected(self):
        directory = self.fx.release()
        zip_path = directory / rc.WINDOWS_ASSET
        zip_path.write_bytes(zip_path.read_bytes() + b"\0")
        with self.assertRaises(ReleaseError):
            self.fx.verify(directory)

    def test_size_and_unpacked_size_and_layout_must_match(self):
        for field, value in (("archive_size", 1), ("unpacked_size", 1), ("layout_revision", 9)):
            directory = self.fx.release(name=f"r-{field}")
            self.fx.resign(directory, lambda c, f=field, v=value: c["targets"][rc.WINDOWS_TARGET].update({f: v}))
            with self.assertRaisesRegex(ReleaseError, field):
                self.fx.verify(directory)

    def test_missing_target_is_rejected(self):
        directory = self.fx.release()
        self.fx.resign(directory, lambda c: c["targets"].pop(rc.MACOS_TARGET))
        with self.assertRaisesRegex(ReleaseError, "exactly the Windows x64 and macOS arm64"):
            self.fx.verify(directory)

    def test_missing_asset_or_extra_asset_is_rejected(self):
        directory = self.fx.release()
        (directory / "extra.txt").write_text("x")
        with self.assertRaisesRegex(ReleaseError, "expected exactly"):
            self.fx.verify(directory)
        (directory / "extra.txt").unlink()
        (directory / rc.SIGNATURE_NAME).unlink()
        with self.assertRaisesRegex(ReleaseError, "expected exactly"):
            self.fx.verify(directory)

    def test_version_source_tag_and_sequence_binding(self):
        directory = self.fx.release(sequence=3)
        with self.assertRaisesRegex(ReleaseError, "differ from the release"):
            self.fx.verify(directory, source_sha=OTHER_SHA)
        with self.assertRaisesRegex(ReleaseError, "differ from the release"):
            self.fx.verify(directory, tag="v0.27.1")
        with self.assertRaisesRegex(ReleaseError, "planned sequence"):
            self.fx.verify(directory, sequence=4)
        self.fx.verify(directory, sequence=3)

    def test_wrong_repository_channel_schema_and_extra_fields_are_rejected(self):
        for name, mutate in (
            ("repo", lambda c: c.update(repository="someone/else")),
            ("channel", lambda c: c.update(channel="beta")),
            ("schema", lambda c: c.update(schema=2)),
            ("extra", lambda c: c.update(unexpected=1)),
            ("protocol", lambda c: c.update(min_updater_protocol=2)),
            ("state", lambda c: c.update(state_compatibility="breaking")),
            ("seq0", lambda c: c.update(catalog_sequence=0)),
            ("seqbool", lambda c: c.update(catalog_sequence=True)),
        ):
            directory = self.fx.release(name=f"r-{name}")
            self.fx.resign(directory, mutate)
            with self.assertRaises(ReleaseError, msg=name):
                self.fx.verify(directory)

    def test_validity_window(self):
        directory = self.fx.release()
        with self.assertRaisesRegex(ReleaseError, "not valid at the current time"):
            self.fx.verify(directory, now=NOW + dt.timedelta(days=151))
        with self.assertRaisesRegex(ReleaseError, "not valid at the current time"):
            self.fx.verify(directory, now=NOW - dt.timedelta(days=2))
        self.fx.resign(directory, lambda c: c.update(expires_at="2027-12-01T00:00:00Z"))
        with self.assertRaisesRegex(ReleaseError, "validity window"):
            self.fx.verify(directory)

    def test_bundle_metadata_and_inventory_hash_must_match(self):
        directory = self.fx.release()
        self.fx.resign(directory, lambda c: c["targets"][rc.MACOS_TARGET].update(bundle_version="0.0.1"))
        with self.assertRaisesRegex(ReleaseError, "bundle_version"):
            self.fx.verify(directory)
        directory = self.fx.release("r-inv")
        self.fx.resign(directory, lambda c: c["targets"][rc.MACOS_TARGET].update(inventory_sha256="0" * 64))
        with self.assertRaisesRegex(ReleaseError, "inventory_sha256"):
            self.fx.verify(directory)

    def test_a_catalog_with_a_team_id_is_not_accepted(self):
        directory = self.fx.release()
        self.fx.resign(directory, lambda c: c["targets"][rc.MACOS_TARGET].update(team_id=TEAM))
        with self.assertRaisesRegex(ReleaseError, "unexpected fields"):
            self.fx.verify(directory)

    def test_catalog_generation_requires_a_valid_sequence(self):
        directory = self.fx.release()
        for sequence in (0, -1, 2**64, True):
            with self.assertRaises(ReleaseError):
                rc.build_catalog(directory, tag=TAG, source_sha=SHA, sequence=sequence, trust=self.fx.trust)
        rc.build_catalog(directory, tag=TAG, source_sha=SHA, sequence=2**64 - 1, trust=self.fx.trust)

    def test_catalog_is_not_built_for_an_unsigned_or_foreign_signed_macos_bundle(self):
        other = self.other_fixture()
        for name, kwargs in (("foreign", dict(signer=other)), ("noinv", dict(inventory=False))):
            macos = self.fx.macos_zip(self.tmp / f"{name}.zip", **kwargs)
            with self.assertRaises(ReleaseError, msg=name):
                self.fx.release(name=f"r-{name}", macos=macos)


class WindowsZipTests(TempCase):
    def verify(self, path, **kwargs):
        args = dict(version=VERSION, source_sha=SHA, trust=self.fx.trust)
        args.update(kwargs)
        return rc.verify_windows_zip(path, **args)

    def test_layout_keeps_versioned_dll_msix_cert_and_register_script(self):
        path = self.fx.windows_zip(self.tmp / "w.zip")
        with zipfile.ZipFile(path) as archive:
            names = set(archive.namelist())
        self.assertTrue({"hane.exe", "hane-updater.exe", "hane-install.json", "hane-install.json.sig",
                         "Hane.ShellIntegration.msix", "Hane.ShellIntegration.cer",
                         "Trust-HaneShellCertificate.ps1",
                         f"shell-integration/{VERSION}/hane_shell_extension.dll"} <= names)
        self.assertNotIn("hane_shell_extension.dll", names)
        self.verify(path)

    def test_inventory_covers_every_owned_file_with_size_and_hash(self):
        path = self.fx.windows_zip(self.tmp / "w.zip")
        with zipfile.ZipFile(path) as archive:
            inventory = json.loads(archive.read("hane-install.json"))
            owned = set(archive.namelist()) - {"hane-install.json", "hane-install.json.sig"}
            self.assertEqual({f["path"] for f in inventory["files"]}, owned)
            for entry in inventory["files"]:
                data = archive.read(entry["path"])
                self.assertEqual((entry["size"], entry["sha256"]), (len(data), rc.sha256_hex(data)))
        self.assertEqual((inventory["version"], inventory["target"], inventory["source_sha"],
                          inventory["updater_protocol"], inventory["layout_revision"]),
                         (VERSION, rc.WINDOWS_TARGET, SHA, 1, 2))

    def test_inventory_that_omits_or_alters_a_file_is_rejected(self):
        def drop_readme(doc):
            doc["files"] = [f for f in doc["files"] if f["path"] != "README.md"]

        def alter(doc):
            doc["files"][0]["sha256"] = "0" * 64

        def phantom(doc):
            doc["files"].append({"path": "ghost.exe", "size": 1, "sha256": "0" * 64})

        for tweak in (drop_readme, alter, phantom):
            path = self.fx.windows_zip(self.tmp / f"{tweak.__name__}.zip", tweak=tweak)
            with self.assertRaises(ReleaseError, msg=tweak.__name__):
                self.verify(path)

    def test_inventory_identity_fields_are_checked(self):
        for field, value in (("version", "0.27.1"), ("target", rc.MACOS_TARGET), ("source_sha", OTHER_SHA),
                             ("updater_protocol", 2), ("layout_revision", 1)):
            path = self.fx.windows_zip(self.tmp / f"{field}.zip", tweak=lambda d, f=field, v=value: d.update({f: v}))
            with self.assertRaisesRegex(ReleaseError, field, msg=field):
                self.verify(path)

    def test_inventory_signature_must_be_valid(self):
        path = self.fx.windows_zip(self.tmp / "w.zip")
        other = self.other_fixture()
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            rc.verify_windows_zip(path, version=VERSION, source_sha=SHA, trust=other.trust)

    def test_dll_directory_must_match_the_version(self):
        path = self.fx.windows_zip(self.tmp / "w.zip", dll_version="0.26.0", manifest_version=VERSION)
        with self.assertRaises(ReleaseError):
            self.verify(path)

    def test_msix_manifest_version_dll_path_and_signature_are_checked(self):
        for name, kwargs in (
            ("version", dict(manifest_version="0.26.0")),
            ("unsigned", dict(signed_msix=False)),
        ):
            path = self.fx.windows_zip(self.tmp / f"{name}.zip", **kwargs)
            with self.assertRaises(ReleaseError, msg=name):
                self.verify(path)
        with self.assertRaisesRegex(ReleaseError, "hane.exe"):
            rc.msix_check(msix(executable="other.exe"), VERSION)

    def test_flat_root_dll_and_extra_or_missing_files_are_rejected(self):
        flat = self.fx.windows_zip(self.tmp / "flat.zip", extra={"hane_shell_extension.dll": b"MZ"})
        with self.assertRaisesRegex(ReleaseError, "unexpected"):
            self.verify(flat)
        extra = self.fx.windows_zip(self.tmp / "extra.zip", extra={"evil.exe": b"MZ"})
        with self.assertRaisesRegex(ReleaseError, "unexpected"):
            self.verify(extra)
        for name in ("hane-updater.exe", "Hane.ShellIntegration.msix", "Hane.ShellIntegration.cer",
                     "Trust-HaneShellCertificate.ps1"):
            if name == "hane-updater.exe":
                with self.assertRaises(ReleaseError):  # the generator itself refuses
                    self.fx.windows_zip(self.tmp / "m.zip", drop=(name,))
                continue
            path = self.fx.windows_zip(self.tmp / f"drop-{name}.zip", drop=(name,))
            with self.assertRaisesRegex(ReleaseError, "missing", msg=name):
                self.verify(path)

    def test_helper_identity_is_read_from_the_binary(self):
        for kwargs in (dict(helper=pe("0.26.0", SHA)), dict(helper=pe(VERSION, OTHER_SHA)),
                       dict(helper=pe(VERSION, SHA, rc.MACOS_TARGET)), dict(helper=pe(VERSION, "unknown".ljust(40))),
                       dict(helper=b"MZ" + b"\0" * 64)):
            with self.assertRaises(ReleaseError):
                self.fx.windows_zip(self.tmp / "h.zip", **kwargs)


class MacosZipTests(TempCase):
    def verify(self, path, **kwargs):
        args = dict(version=VERSION, source_sha=SHA, trust=self.fx.trust)
        args.update(kwargs)
        return rc.verify_macos_zip(path, **args)

    def test_sealed_bundle_and_external_sidecar_verify(self):
        sealed = self.fx.macos_zip(self.tmp / "sealed.zip")
        with zipfile.ZipFile(sealed) as archive:
            names = archive.namelist()
        self.assertIn(f"{rc.MACOS_APP}/{rc.MACOS_CODE_RESOURCES_REL}", names)
        inventory_path, signature_path = rc.macos_sidecar_paths(VERSION, SHA)
        self.assertEqual(inventory_path, f"{rc.MACOS_ROOT}/Hane.app.update/{VERSION}-{SHA}/hane-install.json")
        self.assertIn(inventory_path, names)
        self.assertIn(signature_path, names)
        self.assertFalse([n for n in names if n.startswith(f"{rc.MACOS_APP}/") and n.endswith("hane-install.json")])
        self.verify(sealed)
        with zipfile.ZipFile(sealed) as archive:
            document = json.loads(archive.read(inventory_path))
        # The seal is inventoried like every other file of the app.
        self.assertIn(rc.MACOS_CODE_RESOURCES_REL, [f["path"] for f in document["files"]])
        self.assertEqual(document["layout_revision"], 2)

    def test_the_validation_identity_is_accepted_only_by_the_validation_verifier(self):
        validation = self.fx.macos_zip(self.tmp / "validation.zip", bundle_id=rc.VALIDATION_MACOS_BUNDLE_ID)
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            self.verify(validation)
        facts = rc.verify_validation_macos_zip(validation, version=VERSION, source_sha=SHA, trust=self.fx.trust)
        self.assertEqual(facts["bundle_id"], rc.VALIDATION_MACOS_BUNDLE_ID)
        product = self.fx.macos_zip(self.tmp / "product.zip")
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            rc.verify_validation_macos_zip(product, version=VERSION, source_sha=SHA, trust=self.fx.trust)
        other = self.fx.macos_zip(self.tmp / "other-id.zip", bundle_id="com.example.fake")
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            rc.verify_validation_macos_zip(other, version=VERSION, source_sha=SHA, trust=self.fx.trust)

    def test_catalog_build_and_release_verify_share_the_explicit_validation_selection(self):
        validation = self.fx.macos_zip(self.tmp / "validation.zip", bundle_id=rc.VALIDATION_MACOS_BUNDLE_ID)
        product = self.fx.macos_zip(self.tmp / "product.zip")
        windows = self.fx.windows_zip(self.tmp / "w.zip")

        def assets(name: str, macos: Path) -> Path:
            directory = self.tmp / name
            directory.mkdir()
            (directory / rc.WINDOWS_ASSET).write_bytes(windows.read_bytes())
            (directory / rc.MACOS_ASSET).write_bytes(macos.read_bytes())
            return directory

        def seal(directory: Path, **kwargs) -> None:
            raw = rc.build_catalog(directory, tag=TAG, source_sha=SHA, sequence=1, trust=self.fx.trust,
                                   published=NOW, **kwargs)
            (directory / rc.CATALOG_NAME).write_bytes(raw)
            (directory / rc.SIGNATURE_NAME).write_bytes(self.fx.sign(rc.CATALOG_DOMAIN, raw))

        # The standard path accepts production and refuses the validation identity.
        production_dir = assets("production", product)
        seal(production_dir)
        self.fx.verify(production_dir)
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            self.fx.verify(production_dir, validation_feed=True)
        validation_dir = assets("validation", validation)
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            seal(validation_dir)

        # The explicit validation path accepts it in both build and verify, and only there.
        seal(validation_dir, validation_feed=True)
        catalog = self.fx.verify(validation_dir, validation_feed=True)
        self.assertEqual(catalog["targets"][rc.MACOS_TARGET]["bundle_id"], rc.VALIDATION_MACOS_BUNDLE_ID)
        with self.assertRaisesRegex(ReleaseError, "CFBundleIdentifier"):
            self.fx.verify(validation_dir)

    def test_an_unsealed_bundle_is_rejected(self):
        with self.assertRaisesRegex(ReleaseError, "CodeResources"):
            self.verify(self.fx.macos_zip(self.tmp / "unsealed.zip", code_resources=False))

    def test_the_v0_27_0_inventory_inside_the_app_is_not_a_layout_2_bundle(self):
        with self.assertRaisesRegex(ReleaseError, "inside Hane.app"):
            self.verify(self.fx.macos_zip(self.tmp / "legacy.zip", legacy_inventory=True))

    def test_sidecar_must_name_this_release_and_nothing_else_may_sit_beside_the_app(self):
        with self.assertRaisesRegex(ReleaseError, "unexpected entry"):
            self.verify(self.fx.macos_zip(self.tmp / "other-version.zip", sidecar_version="0.26.0"))
        with self.assertRaisesRegex(ReleaseError, "unexpected entry"):
            self.verify(self.fx.macos_zip(self.tmp / "other-source.zip", sidecar_source=OTHER_SHA))
        for name in ("extra.json", "run.sh"):
            extra = [(f"{rc.MACOS_ROOT}/Hane.app.update/{VERSION}-{SHA}/{name}", b"x", 0o644)]
            with self.assertRaisesRegex(ReleaseError, "unexpected entry", msg=name):
                self.verify(self.fx.macos_zip(self.tmp / f"{name}.zip", extra=extra))
        executable = [(f"{rc.MACOS_ROOT}/Hane.app.update/{VERSION}-{SHA}/hane-install.json", b"x", 0o755)]
        with self.assertRaises(ReleaseError):
            self.verify(self.fx.macos_zip(self.tmp / "exec.zip", extra=executable))
        for escape in (f"{rc.MACOS_ROOT}/Hane.app.update/../evil", f"{rc.MACOS_ROOT}/Hane.app.update2/x"):
            with self.assertRaises(ReleaseError, msg=escape):
                self.verify(self.fx.macos_zip(self.tmp / "escape.zip", extra=[(escape, b"x", 0o644)]))
            (self.tmp / "escape.zip").unlink(missing_ok=True)

    def test_real_generator_inventory_matches_the_verifier(self):
        staging = self.tmp / "Hane.app"
        for name, data in (("Contents/Info.plist", b"x"), ("Contents/MacOS/hane", macho()),
                           ("Contents/Helpers/hane-updater", macho()), ("Contents/Resources/Hane.icns", b"i"),
                           (rc.MACOS_CODE_RESOURCES_REL, b"<plist/>")):
            (staging / name).parent.mkdir(parents=True, exist_ok=True)
            (staging / name).write_bytes(data)
        raw = rc.build_macos_inventory(staging, VERSION, SHA)
        document = json.loads(raw)
        self.assertEqual((document["target"], document["layout_revision"], document["version"]),
                         (rc.MACOS_TARGET, rc.MACOS_LAYOUT, VERSION))
        self.assertEqual([f["path"] for f in document["files"]],
                         sorted(["Contents/Info.plist", "Contents/MacOS/hane", "Contents/Helpers/hane-updater",
                                 "Contents/Resources/Hane.icns", rc.MACOS_CODE_RESOURCES_REL]))
        with self.assertRaises(ReleaseError):
            rc.build_macos_inventory(staging, VERSION, OTHER_SHA)
        # An inventory inside the app would break the seal and is refused.
        (staging / rc.MACOS_LEGACY_INVENTORY_REL).write_bytes(raw)
        with self.assertRaisesRegex(ReleaseError, "sidecar"):
            rc.build_macos_inventory(staging, VERSION, SHA)
        (staging / rc.MACOS_LEGACY_INVENTORY_REL).unlink()
        # So is an app that was never sealed.
        (staging / rc.MACOS_CODE_RESOURCES_REL).unlink()
        with self.assertRaisesRegex(ReleaseError, "not sealed"):
            rc.build_macos_inventory(staging, VERSION, SHA)

    def test_codesign_is_verified_with_apples_tool_and_must_be_ad_hoc_without_a_team(self):
        calls = []

        def runner(output):
            def run(args):
                calls.append(args)
                code = 0 if "--verify" in args else output[0]
                return mock.Mock(returncode=code, stdout=b"", stderr=output[1])
            return run

        good = (0, b"Signature=adhoc\nTeamIdentifier=not set\n")
        rc.verify_macos_codesign(self.tmp / "Hane.app", runner(good))
        self.assertEqual(calls[0][:5], ["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2"])
        with self.assertRaisesRegex(ReleaseError, "ad hoc"):
            rc.verify_macos_codesign(self.tmp / "Hane.app", runner((0, b"Signature size=9000\nTeamIdentifier=ABCDE12345")))
        with self.assertRaisesRegex(ReleaseError, "Team Identifier"):
            rc.verify_macos_codesign(self.tmp / "Hane.app", runner((0, b"Signature=adhoc\nTeamIdentifier=ABCDE12345")))

        def failing(args):
            return mock.Mock(returncode=1, stdout=b"", stderr=b"invalid signature")

        with self.assertRaisesRegex(ReleaseError, "invalid signature"):
            rc.verify_macos_codesign(self.tmp / "Hane.app", failing)

    def test_inventory_is_required_signed_by_a_trusted_key_and_exact(self):
        with self.assertRaisesRegex(ReleaseError, "missing"):
            self.verify(self.fx.macos_zip(self.tmp / "none.zip", inventory=False))
        other = self.other_fixture()
        with self.assertRaisesRegex(ReleaseError, "no valid signature"):
            self.verify(self.fx.macos_zip(self.tmp / "foreign.zip", signer=other))
        with self.assertRaisesRegex(ReleaseError, "differs from the ZIP"):
            self.verify(self.fx.macos_zip(self.tmp / "tampered.zip",
                                          tamper={"Contents/Resources/Hane.icns": b"evil"}))
        with self.assertRaises(ReleaseError):
            self.verify(self.fx.macos_zip(self.tmp / "unlisted.zip",
                                          extra=[(f"{rc.MACOS_APP}/Contents/Resources/new.png", b"x", 0o644)]))

        def omit_icon(doc):
            doc["files"] = [f for f in doc["files"] if not f["path"].endswith("Hane.icns")]

        def alter(doc):
            doc["files"][0]["sha256"] = "0" * 64

        for tweak in (omit_icon, alter):
            with self.assertRaises(ReleaseError, msg=tweak.__name__):
                self.verify(self.fx.macos_zip(self.tmp / f"{tweak.__name__}.zip", tweak=tweak))

    def test_inventory_identity_fields_are_checked(self):
        for field, value in (("version", "0.27.1"), ("target", rc.WINDOWS_TARGET), ("source_sha", OTHER_SHA),
                             ("updater_protocol", 2), ("layout_revision", 1)):
            path = self.fx.macos_zip(self.tmp / f"{field}.zip", tweak=lambda d, f=field, v=value: d.update({f: v}))
            with self.assertRaisesRegex(ReleaseError, field, msg=field):
                self.verify(path)

    def test_layout_and_info_plist_identity(self):
        path = self.fx.macos_zip(self.tmp / "m.zip")
        facts = self.verify(path)
        self.assertEqual(facts["bundle_id"], "io.github.hide212131.hane")
        with zipfile.ZipFile(path) as archive:
            names = set(archive.namelist())
            info = plistlib.loads(archive.read(f"{rc.MACOS_APP}/Contents/Info.plist"))
        self.assertIn(f"{rc.MACOS_APP}/Contents/Helpers/hane-updater", names)
        self.assertEqual((info["CFBundleShortVersionString"], info["CFBundleVersion"], info["HaneSourceCommit"],
                          info["HaneUpdaterProtocol"]), (VERSION, VERSION, SHA, 1))

    def test_info_plist_mismatches_are_rejected(self):
        cases = {
            "bundle id": dict(bundle_id="com.example.other"),
            "version": dict(plist_updates={"CFBundleShortVersionString": "0.26.0"}),
            "build version": dict(plist_updates={"CFBundleVersion": "1"}),
            "source": dict(plist_updates={"HaneSourceCommit": OTHER_SHA}),
            "protocol missing": dict(plist_updates={"HaneUpdaterProtocol": None}),
            "protocol 2": dict(plist_updates={"HaneUpdaterProtocol": 2}),
            "protocol string": dict(plist_updates={"HaneUpdaterProtocol": "1"}),
            "protocol bool": dict(plist_updates={"HaneUpdaterProtocol": True}),
        }
        for name, kwargs in cases.items():
            path = self.fx.macos_zip(self.tmp / f"{name}.zip", **kwargs)
            with self.assertRaises(ReleaseError, msg=name):
                self.verify(path)

    def test_helper_must_exist_and_report_matching_identity(self):
        path = self.fx.macos_zip(self.tmp / "nohelper.zip", drop=("Contents/Helpers/hane-updater",))
        with self.assertRaisesRegex(ReleaseError, "missing"):
            self.verify(path)
        for name, helper in (("old", macho("0.26.0")), ("sha", macho(VERSION, OTHER_SHA)),
                             ("target", pe()), ("none", b"\xcf\xfa\xed\xfe" + b"\0" * 40)):
            path = self.fx.macos_zip(self.tmp / f"{name}.zip", helper=helper)
            with self.assertRaises(ReleaseError, msg=name):
                self.verify(path)

    def test_executables_without_a_code_signature_are_rejected(self):
        # Header: magic, cputype, cpusubtype, filetype, ncmds=0, sizeofcmds=0, flags, reserved.
        unsigned = macho(signed=False)
        for name, kwargs in (("helper", {"helper": unsigned}), ("app", {"app_binary": unsigned})):
            path = self.fx.macos_zip(self.tmp / f"{name}.zip", **kwargs)
            with self.assertRaisesRegex(ReleaseError, "no code signature", msg=name):
                self.verify(path)
        with self.assertRaisesRegex(ReleaseError, "no code signature"):
            rc.check_macho_signed(unsigned, "hane")
        rc.check_macho_signed(macho(), "hane")
        header = macho()[:32]
        with self.assertRaises(ReleaseError):  # a load command that runs past the declared size
            rc.check_macho_signed(header[:20] + (4096).to_bytes(4, "little") + header[24:] + b"\0" * 16, "hane")
        with self.assertRaises(ReleaseError):
            rc.check_macho_signed(b"MZ" + b"\0" * 64, "hane")

    def test_finder_metadata_and_stray_files_are_rejected(self):
        for extra in ([("__MACOSX/._x", b"x", 0o644)], [(f"{rc.MACOS_ROOT}/.DS_Store", b"x", 0o644)],
                      [(f"{rc.MACOS_ROOT}/evil.sh", b"x", 0o755)],
                      [(f"{rc.MACOS_APP}/Contents/Resources/run.sh", b"x", 0o755)]):
            path = self.fx.macos_zip(self.tmp / "x.zip", extra=extra)
            with self.assertRaises(ReleaseError, msg=extra[0][0]):
                self.verify(path)
            path.unlink()


class ZipSafetyTests(TempCase):
    def inspect(self, entries):
        path = self.tmp / "z.zip"
        write_zip(path, entries)
        with rc.Archive(path):
            pass

    def test_unsafe_names_are_rejected(self):
        for name in ("../evil", "/abs", "a\\b", "C:/x", "a//b", "a/./b", "x.", "CON", "dir/NUL.txt", "a:b"):
            with self.assertRaises(ReleaseError, msg=name):
                self.inspect([(name, b"x", 0o644)])

    def test_collisions_symlinks_and_setuid_are_rejected(self):
        with self.assertRaisesRegex(ReleaseError, "colliding"):
            self.inspect([("a/File", b"1", 0o644), ("A/file", b"2", 0o644)])
        with self.assertRaisesRegex(ReleaseError, "colliding"):
            self.inspect([("caf\u00e9", b"1", 0o644), ("cafe\u0301", b"2", 0o644)])
        with self.assertRaisesRegex(ReleaseError, "link"):
            self.inspect([("link", b"target", 0o120777)])
        with self.assertRaisesRegex(ReleaseError, "setuid"):
            self.inspect([("tool", b"x", 0o104755)])

    def test_decompression_bomb_is_rejected(self):
        with self.assertRaisesRegex(ReleaseError, "bomb"):
            self.inspect([("big", b"\0" * (8 << 20), 0o644)])
    def test_extract_zip_round_trips_and_refuses_non_empty_destination(self):
        path = self.fx.macos_zip(self.tmp / "m.zip")
        dest = self.tmp / "out"
        rc.extract_zip(path, dest)
        helper = dest / rc.MACOS_APP / "Contents/Helpers/hane-updater"
        self.assertTrue(os.access(helper, os.X_OK))
        with self.assertRaisesRegex(ReleaseError, "empty"):
            rc.extract_zip(path, dest)

    def test_make_zip_never_overwrites(self):
        root = self.tmp / "root"
        root.mkdir()
        (root / "a").write_text("a")
        target = self.tmp / "o.zip"
        rc.make_zip(root, target)
        with self.assertRaisesRegex(ReleaseError, "already exists"):
            rc.make_zip(root, target)


class BinaryIdentityTests(unittest.TestCase):
    def test_matching_record_passes(self):
        rc.check_identity(pe(), version=VERSION, target=rc.WINDOWS_TARGET, source_sha=SHA, what="x")

    def test_missing_unknown_conflicting_or_foreign_records_fail(self):
        for data in (b"MZ", pe() + identity(source=OTHER_SHA), b"MZ" + identity(source="unknown")):
            with self.assertRaises(ReleaseError):
                rc.check_identity(data, version=VERSION, target=rc.WINDOWS_TARGET, source_sha=SHA, what="x")
        with self.assertRaises(ReleaseError):
            rc.check_identity(pe(), version=VERSION, target=rc.WINDOWS_TARGET, source_sha=SHA.upper(), what="x")
        with self.assertRaises(ReleaseError):
            rc.check_identity(identity(protocol=2), version=VERSION, target=rc.WINDOWS_TARGET, source_sha=SHA,
                              what="x")


class FakeGh(rc.Gh):
    """Real Gh paged/json/bytes behaviour on top of an in-memory `gh api` runner."""

    def __init__(self, releases=(), tags=(), assets=None, fail=None):
        super().__init__(self._run)
        self.releases = list(releases)
        self.tags = [{"name": t} for t in tags]
        self.assets = dict(assets or {})
        self.fail = fail
        self.calls: list[str] = []

    def _run(self, args):
        path = args[-1]
        self.calls.append(path)
        if self.fail and self.fail in path:
            raise ReleaseError("gh failed")
        base = f"repos/{REPO}/"
        if path.startswith(base + "releases/assets/"):
            return self.assets[int(path.rsplit("/", 1)[1])]
        match = re.match(re.escape(base) + r"releases/(\d+)$", path)
        if match:
            return json.dumps(next(r for r in self.releases if r["id"] == int(match.group(1)))).encode()
        for kind, items in (("releases", self.releases), ("tags", self.tags)):
            if path.startswith(f"{base}{kind}?"):
                page = int(re.search(r"[?&]page=(\d+)", path).group(1))
                return json.dumps(items if page == 1 else []).encode()
        raise AssertionError(f"unexpected gh path {path}")


class PublishedStateTests(TempCase):
    next_id = 100

    def catalog_release(self, tag, sequence, *, draft=False, repository=REPO, channel="stable", sign=True,
                        with_signature=True, signer=None):
        document = {
            "schema": 1, "channel": channel, "repository": repository, "version": tag[1:], "tag": tag,
            "source_sha": SHA, "catalog_sequence": sequence, "published_at": "2026-10-01T00:00:00Z",
            "expires_at": "2027-01-01T00:00:00Z", "updater_protocol": 1, "min_updater_protocol": 1,
            "state_compatibility": rc.STATE_COMPATIBILITY, "targets": {},
        }
        raw = rc.canonical_json(document)
        signature = (signer or self.fx).sign(rc.CATALOG_DOMAIN, raw)
        if not sign:
            signature = self.fx.sign(rc.CATALOG_DOMAIN, raw + b" ")
        PublishedStateTests.next_id += 2
        cid, sid = self.next_id, self.next_id + 1
        assets = [{"name": rc.CATALOG_NAME, "id": cid}]
        blobs = {cid: raw, sid: signature}
        if with_signature:
            assets.append({"name": rc.SIGNATURE_NAME, "id": sid})
        return {"id": cid, "tag_name": tag, "draft": draft, "assets": assets}, blobs

    def gh_with(self, specs, tags=()):
        releases, blobs = [], {}
        for spec in specs:
            release, assets = spec
            releases.append(release)
            blobs.update(assets)
        return FakeGh(releases, tags, blobs)

    def test_first_catalog_only_when_explicitly_initial(self):
        gh = FakeGh([{"id": 1, "tag_name": "v0.26.0", "draft": False, "assets": []}], ["v0.26.0"])
        with self.assertRaisesRegex(ReleaseError, "initial_catalog"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)
        plan = rc.plan_release(gh, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)
        self.assertEqual(plan, {"sequence": 1, "make_latest": "true"})

    def test_sequence_is_highest_published_plus_one(self):
        gh = self.gh_with([self.catalog_release("v0.25.0", 4), self.catalog_release("v0.26.0", 7)],
                          ["v0.25.0", "v0.26.0"])
        plan = rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)
        self.assertEqual(plan["sequence"], 8)
        with self.assertRaisesRegex(ReleaseError, "already exist"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)

    def test_drafts_are_not_published_state_but_block_the_same_tag(self):
        draft = self.catalog_release("v0.26.0", 99, draft=True)
        published = self.catalog_release("v0.25.0", 2)
        gh = self.gh_with([draft, published], ["v0.25.0"])
        plan = rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)
        self.assertEqual(plan["sequence"], 3)
        leftover = self.catalog_release(TAG, 5, draft=True)
        with self.assertRaisesRegex(ReleaseError, "already exists"):
            rc.plan_release(self.gh_with([leftover, published], ["v0.25.0"]), REPO, TAG, tag_push=False,
                            initial=False, trust=self.fx.trust)

    def test_overflow_and_duplicates_are_rejected(self):
        gh = self.gh_with([self.catalog_release("v0.26.0", rc.U64_MAX)], ["v0.26.0"])
        with self.assertRaisesRegex(ReleaseError, "overflow"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)
        gh = self.gh_with([self.catalog_release("v0.25.0", 3), self.catalog_release("v0.26.0", 3)],
                          ["v0.25.0", "v0.26.0"])
        with self.assertRaisesRegex(ReleaseError, "reuse"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)

    def test_fetch_permission_parse_and_verification_failures_are_never_a_first_release(self):
        base = FakeGh([], [])
        for failing in ("releases", "tags"):
            gh = FakeGh([], [], fail=failing)
            with self.assertRaises(ReleaseError):
                rc.plan_release(gh, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)
        self.assertEqual(rc.plan_release(base, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)["sequence"], 1)
        other = self.other_fixture()
        bad_cases = {
            "download": (self.gh_with([self.catalog_release("v0.26.0", 2)], ["v0.26.0"]), "releases/assets"),
            "signature": (self.gh_with([self.catalog_release("v0.26.0", 2, sign=False)], ["v0.26.0"]), None),
            "signer": (self.gh_with([self.catalog_release("v0.26.0", 2, signer=other)], ["v0.26.0"]), None),
            "repository": (self.gh_with([self.catalog_release("v0.26.0", 2, repository="a/b")], ["v0.26.0"]), None),
            "channel": (self.gh_with([self.catalog_release("v0.26.0", 2, channel="beta")], ["v0.26.0"]), None),
            "half": (self.gh_with([self.catalog_release("v0.26.0", 2, with_signature=False)], ["v0.26.0"]), None),
        }
        for name, (gh, fail) in bad_cases.items():
            gh.fail = fail
            with self.assertRaises(ReleaseError, msg=name):
                rc.plan_release(gh, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)

    def test_catalog_tag_must_match_its_release(self):
        release, blobs = self.catalog_release("v0.26.0", 2)
        release["tag_name"] = "v0.25.0"
        gh = FakeGh([release], ["v0.25.0"], blobs)
        with self.assertRaisesRegex(ReleaseError, "names tag"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=False, trust=self.fx.trust)

    def test_existing_tag_is_never_published_over_except_for_the_tag_push_that_created_it(self):
        gh = FakeGh([], [TAG])
        with self.assertRaisesRegex(ReleaseError, "already exists"):
            rc.plan_release(gh, REPO, TAG, tag_push=False, initial=True, trust=self.fx.trust)
        self.assertEqual(rc.plan_release(gh, REPO, TAG, tag_push=True, initial=True, trust=self.fx.trust)["sequence"], 1)
        with self.assertRaisesRegex(ReleaseError, "not visible"):
            rc.plan_release(FakeGh([], []), REPO, TAG, tag_push=True, initial=True, trust=self.fx.trust)

    def test_a_version_older_than_the_latest_is_refused_on_every_path(self):
        # without a tag push the target tag does not exist yet; with one, it has already been pushed
        for tag_push, tags in ((False, ["v0.28.0"]), (True, ["v0.28.0", TAG])):
            gh = self.gh_with([self.catalog_release("v0.28.0", 2)], tags)
            with self.assertRaisesRegex(ReleaseError, "not newer"):
                rc.plan_release(gh, REPO, TAG, tag_push=tag_push, initial=False, trust=self.fx.trust)
        # semantic, not lexicographic, ordering
        gh = self.gh_with([self.catalog_release("v0.9.0", 2)], ["v0.9.0"])
        self.assertEqual(rc.plan_release(gh, REPO, "v0.10.0", tag_push=False, initial=False,
                                         trust=self.fx.trust)["make_latest"], "true")
        with self.assertRaisesRegex(ReleaseError, "not newer"):
            rc.plan_release(self.gh_with([self.catalog_release("v0.10.0", 2)], ["v0.10.0"]), REPO, "v0.9.0",
                            tag_push=False, initial=False, trust=self.fx.trust)

    def test_fetch_assets_requires_exactly_the_four_uploaded_assets(self):
        directory = self.fx.release()
        assets, blobs = [], {}
        for index, name in enumerate(rc.RELEASE_ASSETS, start=1):
            data = (directory / name).read_bytes()
            assets.append({"name": name, "id": index, "state": "uploaded", "size": len(data)})
            blobs[index] = data
        release = {"id": 9, "tag_name": TAG, "draft": True, "assets": assets}
        gh = FakeGh([release], [], blobs)
        dest = self.tmp / "draft"
        rc.fetch_assets(gh, REPO, dest, release_id=9)
        self.fx.verify(dest)

        for name, mutate in (
            ("missing", lambda r: r["assets"].pop()),
            ("extra", lambda r: r["assets"].append({"name": "x", "id": 77, "state": "uploaded", "size": 0})),
            ("state", lambda r: r["assets"][0].update(state="starter")),
            ("size", lambda r: r["assets"][0].update(size=1)),
        ):
            broken = json.loads(json.dumps(release))
            mutate(broken)
            with self.assertRaises(ReleaseError, msg=name):
                rc.fetch_assets(FakeGh([broken], [], blobs), REPO, self.tmp / f"d-{name}", release_id=9)
        with self.assertRaisesRegex(ReleaseError, "exactly one"):
            rc.fetch_assets(gh, REPO, self.tmp / "none")

    def test_fetch_by_tag_ignores_drafts(self):
        draft = {"id": 9, "tag_name": TAG, "draft": True, "assets": []}
        with self.assertRaisesRegex(ReleaseError, "exactly one published"):
            rc.fetch_assets(FakeGh([draft], []), REPO, self.tmp / "t", tag=TAG)


class CommandLineTests(TempCase):
    def env(self, **extra):
        values = {
            rc.ENV_KEY_ID: KEY_ID,
            rc.ENV_PUBLIC_KEY: base64.b64encode(self.fx.public).decode(),
            rc.ENV_SIGNING_KEY: base64.b64encode(self.fx.seed).decode(),
        }
        values.update(extra)
        return mock.patch.dict(os.environ, values, clear=False)

    def test_sign_refuses_a_key_that_does_not_match_the_configured_public_key(self):
        payload = self.tmp / "p.json"
        payload.write_bytes(b"{}")
        other = os.urandom(32)
        with self.env(**{rc.ENV_SIGNING_KEY: base64.b64encode(other).decode()}):
            self.assertEqual(rc.main(["sign", "--domain", "catalog", "--input", str(payload),
                                      "--output", str(self.tmp / "p.sig")]), 1)
        self.assertFalse((self.tmp / "p.sig").exists())

    def test_sign_is_fail_closed_without_configuration_and_never_overwrites(self):
        payload = self.tmp / "p.json"
        payload.write_bytes(b"{}")
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(rc.main(["sign", "--domain", "catalog", "--input", str(payload),
                                      "--output", str(self.tmp / "a.sig")]), 1)
        with self.env():
            self.assertEqual(rc.main(["sign", "--domain", "catalog", "--input", str(payload),
                                      "--output", str(self.tmp / "b.sig")]), 0)
            signature = (self.tmp / "b.sig").read_bytes()
            rc.verify_signature(rc.CATALOG_DOMAIN, b"{}", signature, self.fx.trust)
            self.assertEqual(rc.main(["sign", "--domain", "catalog", "--input", str(payload),
                                      "--output", str(self.tmp / "b.sig")]), 1)

    def test_require_env_reports_names_but_never_values(self):
        with mock.patch.dict(os.environ, {"HANE_SECRET_X": "super-secret"}, clear=False):
            with mock.patch("sys.stderr", new=io.StringIO()) as err:
                self.assertEqual(rc.main(["require-env", "HANE_SECRET_X", "HANE_MISSING_Y"]), 1)
        self.assertIn("HANE_MISSING_Y", err.getvalue())
        self.assertNotIn("super-secret", err.getvalue())

    def test_build_catalog_and_verify_release_commands(self):
        directory = self.fx.release("cli")
        (directory / rc.CATALOG_NAME).unlink()
        (directory / rc.SIGNATURE_NAME).unlink()
        with self.env():
            catalog = directory / rc.CATALOG_NAME
            self.assertEqual(rc.main(["build-catalog", "--dir", str(directory), "--tag", TAG, "--source-sha", SHA,
                                      "--sequence", "9", "--output", str(catalog)]), 0)
            self.assertEqual(rc.main(["sign", "--domain", "catalog", "--input", str(catalog),
                                      "--output", str(directory / rc.SIGNATURE_NAME)]), 0)
            self.assertEqual(rc.main(["verify-release", "--dir", str(directory), "--tag", TAG,
                                      "--source-sha", SHA, "--sequence", "9"]), 0)
            self.assertEqual(rc.main(["verify-release", "--dir", str(directory), "--tag", TAG,
                                      "--source-sha", OTHER_SHA]), 1)

    def test_binary_identity_command(self):
        binary = self.tmp / "hane-updater.exe"
        binary.write_bytes(pe())
        args = ["binary-identity", "--binary", str(binary), "--version", VERSION,
                "--target", rc.WINDOWS_TARGET, "--source-sha", SHA]
        self.assertEqual(rc.main(args), 0)
        binary.write_bytes(pe(source=OTHER_SHA))
        self.assertEqual(rc.main(args), 1)


class PinnedDependencyTests(unittest.TestCase):
    def test_requirements_pin_cryptography_exactly(self):
        text = (ROOT / "scripts/release-requirements.txt").read_text(encoding="utf-8")
        lines = [l.strip() for l in text.splitlines() if l.strip() and not l.startswith("#")]
        self.assertEqual(lines, ["cryptography==50.0.2"])


if __name__ == "__main__":
    unittest.main()
