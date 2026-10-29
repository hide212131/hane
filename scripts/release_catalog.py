#!/usr/bin/env python3
"""Release-side half of Hane's ZIP application update (Issue #510, stage U2).

Builds and verifies what `crates/update` consumes: the Windows inventory, the
macOS inventory (kept outside the ad-hoc sealed `Hane.app`, in the sidecar
`Hane.app.update/<version>-<source_sha>/`), the update catalog and their Ed25519
signatures, over the real ZIP files. The same
verifier runs in the release workflow (on the Draft's re-downloaded assets), in
the release-request workflow (on the published assets) and in the CI fixtures.

Contract mirrored from docs/application-update-implementation-spec.md:

* signatures cover `domain || raw bytes`; the JSON is never re-serialized for
  verification (`hane-update-catalog-v1\\0`, `hane-install-v1\\0`);
* Ed25519 comes from the maintained `cryptography` package only;
* nothing here generates, guesses or stores a production key. Secret values are
  read from the environment by the signing subcommand alone.

Only the standard library is needed except for signing/verifying signatures
(`cryptography`, imported lazily so the identity/layout checks run anywhere).
"""
from __future__ import annotations

import argparse
import base64
import binascii
import datetime as dt
import hashlib
import json
import os
import plistlib
import re
import stat
import subprocess
import sys
import unicodedata
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Callable, Iterable

# ----------------------------------------------------------------- constants

REPOSITORY = "hide212131/hane"
CHANNEL = "stable"
SCHEMA = 1
UPDATER_PROTOCOL = 1
STATE_COMPATIBILITY = "backward-compatible-v1"
CATALOG_DOMAIN = b"hane-update-catalog-v1\0"
INVENTORY_DOMAIN = b"hane-install-v1\0"

CATALOG_NAME = "hane-update.json"
SIGNATURE_NAME = "hane-update.json.sig"
INVENTORY_NAME = "hane-install.json"
INVENTORY_SIGNATURE_NAME = "hane-install.json.sig"

WINDOWS_ASSET = "hane-windows-x64.zip"
MACOS_ASSET = "hane-macos-arm64.zip"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"
MACOS_TARGET = "aarch64-apple-darwin"
WINDOWS_LAYOUT = 2
# Layout 1 (v0.27.0) kept the Ed25519 inventory inside Hane.app and removed the
# Apple signature. Layout 2 seals Hane.app with a valid ad-hoc code signature
# and keeps the inventory outside the app, so the two never invalidate each
# other. The release only ever produces layout 2.
MACOS_LAYOUT = 2
MACOS_BUNDLE_ID = "io.github.hide212131.hane"
# Fixed identity of the isolated `validation-feed` build (Issue #510); only
# verify_validation_macos_zip accepts it.
VALIDATION_MACOS_BUNDLE_ID = "io.github.hide212131.hane.validation"
RELEASE_ASSETS = (WINDOWS_ASSET, MACOS_ASSET, CATALOG_NAME, SIGNATURE_NAME)

MAX_CATALOG_BYTES = 256 << 10
MAX_SIGNATURE_BYTES = 16 << 10
MAX_INVENTORY_BYTES = 1 << 20
MAX_ARCHIVE_BYTES = 256 << 20
MAX_UNPACKED_BYTES = 1 << 30
MAX_ENTRY_BYTES = 512 << 20
MAX_ENTRIES = 10_000
MAX_PATH_BYTES = 240
MAX_COMPONENT_BYTES = 255
MAX_RATIO = 500
RATIO_MIN_BYTES = 1 << 20
MAX_VALIDITY = dt.timedelta(days=180)
MAX_FUTURE_SKEW = dt.timedelta(hours=24)
DEFAULT_VALIDITY = dt.timedelta(days=150)
U64_MAX = (1 << 64) - 1

ENV_SIGNING_KEY = "HANE_UPDATE_SIGNING_KEY_BASE64"
ENV_KEY_ID = "HANE_UPDATE_KEY_ID"
ENV_PUBLIC_KEY = "HANE_UPDATE_PUBLIC_KEY_BASE64"

WINDOWS_ROOT_FILES = (
    "hane.exe",
    "hane-updater.exe",
    INVENTORY_NAME,
    INVENTORY_SIGNATURE_NAME,
    "Hane.ShellIntegration.msix",
    "Hane.ShellIntegration.cer",
    "Trust-HaneShellCertificate.ps1",
    "README.md",
    "LICENSE",
)
WINDOWS_OPTIONAL = ("LICENSE",)
MACOS_ROOT = "hane-macos-arm64"
MACOS_APP = f"{MACOS_ROOT}/Hane.app"
MACOS_HELPER_REL = "Contents/Helpers/hane-updater"
# The ad-hoc seal of the finished bundle. It is an ordinary inventoried file.
MACOS_CODE_RESOURCES_REL = "Contents/_CodeSignature/CodeResources"
# The v0.27.0 location of the inventory. A layout 2 bundle must not contain it.
MACOS_LEGACY_INVENTORY_REL = f"Contents/Resources/{INVENTORY_NAME}"
MACOS_LEGACY_INVENTORY_SIGNATURE_REL = f"Contents/Resources/{INVENTORY_SIGNATURE_NAME}"
# The Ed25519 inventory and its signature live next to Hane.app, never inside.
MACOS_SIDECAR_ROOT = f"{MACOS_ROOT}/Hane.app.update"
MACOS_REQUIRED = (
    f"{MACOS_APP}/Contents/Info.plist",
    f"{MACOS_APP}/Contents/MacOS/hane",
    f"{MACOS_APP}/{MACOS_HELPER_REL}",
    f"{MACOS_APP}/{MACOS_CODE_RESOURCES_REL}",
    f"{MACOS_ROOT}/README.md",
)
MACOS_EXECUTABLE_PREFIXES = (f"{MACOS_APP}/Contents/MacOS/", f"{MACOS_APP}/Contents/Helpers/")

IDENTITY_RE = re.compile(
    rb"hane-updater-identity-v1;version=([0-9A-Za-z.+-]+);target=([A-Za-z0-9_.-]+);"
    rb"source=([0-9a-f]{40}|unknown);protocol=([0-9]+);end"
)
TAG_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
KEY_ID_RE = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
TIME_FORMAT = "%Y-%m-%dT%H:%M:%SZ"
WINDOWS_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}
ZIP_TIMESTAMP = (2026, 1, 1, 0, 0, 0)


class ReleaseError(Exception):
    """Any condition under which the release must not proceed."""


def fail(message: str) -> "ReleaseError":
    return ReleaseError(message)


# ------------------------------------------------------------------- helpers


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def parse_tag(tag: str) -> tuple[int, int, int]:
    match = TAG_RE.match(tag)
    if not match:
        raise fail(f"release tag must be vMAJOR.MINOR.PATCH, got {tag!r}")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def version_of(tag: str) -> str:
    parse_tag(tag)
    return tag[1:]


def strict_json(raw: bytes, what: str = "JSON") -> object:
    """Parses JSON rejecting duplicate keys and non-finite numbers."""

    def pairs(items: list[tuple[str, object]]) -> dict:
        result: dict = {}
        for key, value in items:
            if key in result:
                raise fail(f"{what} has a duplicate key {key!r}")
            result[key] = value
        return result

    def constant(name: str) -> object:
        raise fail(f"{what} contains the non-finite number {name}")

    try:
        text = raw.decode("utf-8")
        return json.loads(text, object_pairs_hook=pairs, parse_constant=constant)
    except (UnicodeDecodeError, ValueError) as err:
        if isinstance(err, ReleaseError):
            raise
        raise fail(f"{what} is not valid UTF-8 JSON: {err}") from err


def is_uint(value: object, minimum: int = 0, maximum: int = U64_MAX) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and minimum <= value <= maximum


def utc_text(moment: dt.datetime) -> str:
    return moment.astimezone(dt.timezone.utc).strftime(TIME_FORMAT)


def parse_time(text: object, what: str) -> dt.datetime:
    if not isinstance(text, str):
        raise fail(f"{what} must be a string")
    try:
        return dt.datetime.strptime(text, TIME_FORMAT).replace(tzinfo=dt.timezone.utc)
    except ValueError as err:
        raise fail(f"{what} is not UTC RFC 3339 (YYYY-MM-DDTHH:MM:SSZ)") from err


def now_utc() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode("ascii")


# ------------------------------------------------------------ signing / trust


class Trust:
    """Public keys the verifier accepts (id -> raw 32-byte Ed25519 key)."""

    def __init__(self, keys: dict[str, bytes]):
        if not keys:
            raise fail("no trusted update public key is configured")
        self.keys = keys

    @classmethod
    def from_values(cls, key_id: str | None, public_key_b64: str | None) -> "Trust":
        if not key_id or not public_key_b64:
            raise fail(f"{ENV_KEY_ID} and {ENV_PUBLIC_KEY} must both be set")
        if not KEY_ID_RE.match(key_id):
            raise fail(f"{ENV_KEY_ID} has an invalid form")
        return cls({key_id: decode_key(public_key_b64.strip(), ENV_PUBLIC_KEY)})

    @classmethod
    def from_env(cls, env: dict[str, str] | None = None) -> "Trust":
        env = os.environ if env is None else env
        return cls.from_values(env.get(ENV_KEY_ID), env.get(ENV_PUBLIC_KEY))


def decode_key(text: str, what: str) -> bytes:
    try:
        raw = base64.b64decode(text, validate=True)
    except (binascii.Error, ValueError) as err:
        raise fail(f"{what} is not valid base64") from err
    if len(raw) != 32:
        raise fail(f"{what} must decode to 32 bytes")
    return raw


def _ed25519():
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives.asymmetric import ed25519
        from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
    except ImportError as err:  # pragma: no cover - environment problem
        raise fail("the `cryptography` package from scripts/release-requirements.txt is required") from err
    return ed25519, InvalidSignature, Encoding, PublicFormat


def public_key_of(seed: bytes) -> bytes:
    ed25519, _, encoding, fmt = _ed25519()
    return ed25519.Ed25519PrivateKey.from_private_bytes(seed).public_key().public_bytes(encoding.Raw, fmt.Raw)


def make_signature_file(domain: bytes, payload: bytes, key_id: str, seed: bytes) -> bytes:
    ed25519, _, _, _ = _ed25519()
    signature = ed25519.Ed25519PrivateKey.from_private_bytes(seed).sign(domain + payload)
    document = {
        "schema": SCHEMA,
        "signatures": [{"key_id": key_id, "signature_base64": base64.b64encode(signature).decode("ascii")}],
    }
    return canonical_json(document)


def verify_signature(domain: bytes, payload: bytes, signature_file: bytes, trust: Trust) -> str:
    """Returns the key id of a valid signature over `domain || payload`."""
    ed25519, invalid, _, _ = _ed25519()
    if len(signature_file) > MAX_SIGNATURE_BYTES:
        raise fail("signature file exceeds the size limit")
    document = strict_json(signature_file, "signature file")
    if not isinstance(document, dict) or set(document) != {"schema", "signatures"}:
        raise fail("signature file has unexpected fields")
    if document["schema"] != SCHEMA or isinstance(document["schema"], bool):
        raise fail("unknown signature file schema")
    entries = document["signatures"]
    if not isinstance(entries, list) or not 1 <= len(entries) <= 8:
        raise fail("signature count out of range")
    message = domain + payload
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"key_id", "signature_base64"}:
            raise fail("signature entry has unexpected fields")
        public = trust.keys.get(entry["key_id"]) if isinstance(entry["key_id"], str) else None
        if public is None or not isinstance(entry["signature_base64"], str):
            continue
        try:
            signature = base64.b64decode(entry["signature_base64"], validate=True)
            if len(signature) != 64:
                continue
            ed25519.Ed25519PublicKey.from_public_bytes(public).verify(signature, message)
            return entry["key_id"]
        except (binascii.Error, ValueError, invalid):
            continue
    raise fail("no valid signature from a trusted key")


def signing_seed(env: dict[str, str] | None = None) -> bytes:
    env = os.environ if env is None else env
    value = env.get(ENV_SIGNING_KEY, "")
    if not value:
        raise fail(f"{ENV_SIGNING_KEY} is not configured")
    return decode_key(value.strip(), ENV_SIGNING_KEY)


# ------------------------------------------------------------ binary identity


def read_identity(data: bytes) -> tuple[str, str, str, int]:
    """Reads the record `crates/updater` embeds and consults at startup."""
    matches = set(IDENTITY_RE.findall(data))
    if not matches:
        raise fail("the helper binary carries no updater identity record")
    if len(matches) != 1:
        raise fail("the helper binary carries conflicting updater identity records")
    version, target, source, protocol = next(iter(matches))
    return version.decode(), target.decode(), source.decode(), int(protocol)


def check_identity(data: bytes, *, version: str, target: str, source_sha: str, what: str) -> None:
    found_version, found_target, found_source, protocol = read_identity(data)
    if (found_version, found_target, found_source, protocol) != (version, target, source_sha, UPDATER_PROTOCOL):
        raise fail(
            f"{what} identity is version={found_version} target={found_target} "
            f"source={found_source} protocol={protocol}, expected version={version} "
            f"target={target} source={source_sha} protocol={UPDATER_PROTOCOL}"
        )


LC_CODE_SIGNATURE = 0x1D
MACHO64_HEADER_BYTES = 32


def macho_load_commands(data: bytes, what: str) -> list[int]:
    """Load command numbers of a 64-bit little-endian Mach-O; malformed is an error."""
    if data[:4] != b"\xcf\xfa\xed\xfe" or len(data) < MACHO64_HEADER_BYTES:
        raise fail(f"{what} is not a 64-bit Mach-O executable")
    ncmds = int.from_bytes(data[16:20], "little")
    sizeofcmds = int.from_bytes(data[20:24], "little")
    end = MACHO64_HEADER_BYTES + sizeofcmds
    if end > len(data):
        raise fail(f"{what} has load commands beyond the end of the file")
    offset = MACHO64_HEADER_BYTES
    commands = []
    for _ in range(ncmds):
        if offset + 8 > end:
            raise fail(f"{what} has a truncated Mach-O load command")
        cmd = int.from_bytes(data[offset:offset + 4], "little")
        size = int.from_bytes(data[offset + 4:offset + 8], "little")
        if size < 8 or offset + size > end:
            raise fail(f"{what} has a malformed Mach-O load command")
        commands.append(cmd)
        offset += size
    return commands


def check_macho_signed(data: bytes, what: str) -> None:
    """Rejects a Mach-O without a code signature load command.

    Apple Silicon refuses to run unsigned code, so every executable of the
    release must carry the (ad-hoc) signature the final bundle seal covers. This
    is a portable structural check; `verify_macos_codesign` asks Apple's tool.
    """
    if LC_CODE_SIGNATURE not in macho_load_commands(data, what):
        raise fail(f"{what} carries no code signature; Apple Silicon would not run it")


def verify_macos_codesign(app: Path, runner: Callable[[list[str]], "subprocess.CompletedProcess"] | None = None) -> None:
    """Asks Apple's `codesign` to verify the sealed bundle (macOS only).

    The bundle must verify as a whole (`--deep --strict`) and must be signed
    ad hoc with no Team Identifier: no Developer ID or notarization is involved.
    """
    run = runner or (lambda args: subprocess.run(args, capture_output=True, check=False))
    if runner is None and sys.platform != "darwin":
        raise fail("the Apple code signature can only be verified on macOS")
    verify = run(["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
    if verify.returncode != 0:
        raise fail(f"codesign --verify failed for {app}: {verify.stderr.decode(errors='replace').strip()}")
    display = run(["/usr/bin/codesign", "--display", "--verbose=2", str(app)])
    text = (display.stdout + display.stderr).decode(errors="replace")
    if display.returncode != 0 or "Signature=adhoc" not in text:
        raise fail(f"{app} is not signed ad hoc")
    if "TeamIdentifier=not set" not in text:
        raise fail(f"{app} carries a Team Identifier; Hane is not signed with an Apple identity")


# ------------------------------------------------------------ ZIP inspection


def macos_sidecar_dir(version: str, source_sha: str) -> str:
    """ZIP directory of this release's external inventory generation."""
    return f"{MACOS_SIDECAR_ROOT}/{version}-{source_sha}"


def macos_sidecar_paths(version: str, source_sha: str) -> tuple[str, str]:
    directory = macos_sidecar_dir(version, source_sha)
    return f"{directory}/{INVENTORY_NAME}", f"{directory}/{INVENTORY_SIGNATURE_NAME}"


def collision_key(path: str) -> str:
    return unicodedata.normalize("NFC", path).casefold()


def validate_member_name(name: str) -> None:
    raw = name[:-1] if name.endswith("/") else name
    if not raw or "\0" in raw or "\\" in raw or raw.startswith("/") or ":" in raw:
        raise fail(f"unsafe ZIP entry name {name!r}")
    if len(raw.encode("utf-8")) > MAX_PATH_BYTES:
        raise fail(f"ZIP entry name too long: {name!r}")
    for part in raw.split("/"):
        if part in ("", ".", "..") or len(part.encode("utf-8")) > MAX_COMPONENT_BYTES:
            raise fail(f"unsafe ZIP entry name {name!r}")
        if part[-1] in ". " or part.split(".")[0].upper() in WINDOWS_RESERVED:
            raise fail(f"ZIP entry name is not portable: {name!r}")


class Archive:
    """A ZIP that passed the structural checks of the spec (section 5.3)."""

    def __init__(self, path: Path):
        self.path = Path(path)
        size = self.path.stat().st_size
        if size == 0 or size > MAX_ARCHIVE_BYTES:
            raise fail(f"{self.path.name} size {size} is out of range")
        try:
            self.zf = zipfile.ZipFile(self.path)
        except zipfile.BadZipFile as err:
            raise fail(f"{self.path.name} is not a valid ZIP") from err
        infos = self.zf.infolist()
        if not infos or len(infos) > MAX_ENTRIES:
            raise fail(f"{self.path.name} entry count out of range")
        seen: set[str] = set()
        total = 0
        for info in infos:
            validate_member_name(info.filename)
            key = collision_key(info.filename.rstrip("/"))
            if key in seen:
                raise fail(f"{self.path.name} has colliding entry {info.filename!r}")
            seen.add(key)
            if info.flag_bits & 0x1:
                raise fail(f"{info.filename!r} is encrypted")
            if info.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                raise fail(f"{info.filename!r} uses an unsupported compression method")
            if info.file_size > MAX_ENTRY_BYTES:
                raise fail(f"{info.filename!r} exceeds the entry size limit")
            if info.file_size > RATIO_MIN_BYTES and info.file_size > max(info.compress_size, 1) * MAX_RATIO:
                raise fail(f"{info.filename!r} looks like a decompression bomb")
            mode = info.external_attr >> 16
            if info.create_system == 3 and mode:
                kind = stat.S_IFMT(mode)
                if kind not in (0, stat.S_IFREG, stat.S_IFDIR):
                    raise fail(f"{info.filename!r} is a link or special file")
                if mode & 0o6000:
                    raise fail(f"{info.filename!r} carries setuid/setgid bits")
            total += info.file_size
        if total > MAX_UNPACKED_BYTES:
            raise fail(f"{self.path.name} unpacks beyond the size limit")
        self.unpacked_size = total
        self.entries = infos
        self.files = {i.filename: i for i in infos if not i.is_dir()}
        self.dirs = {i.filename.rstrip("/") for i in infos if i.is_dir()}

    def executable(self, name: str) -> bool:
        info = self.files[name]
        return info.create_system == 3 and bool((info.external_attr >> 16) & 0o111)

    def read(self, name: str, limit: int = MAX_ENTRY_BYTES) -> bytes:
        info = self.files[name]
        if info.file_size > limit:
            raise fail(f"{name!r} exceeds {limit} bytes")
        try:
            with self.zf.open(info) as handle:
                data = handle.read(info.file_size + 1)
        except (zipfile.BadZipFile, OSError) as err:
            raise fail(f"{name!r} is corrupt: {err}") from err
        if len(data) != info.file_size:
            raise fail(f"{name!r} differs from its declared size")
        return data

    def digest(self, name: str) -> tuple[int, str]:
        info = self.files[name]
        sha = hashlib.sha256()
        total = 0
        try:
            with self.zf.open(info) as handle:
                while chunk := handle.read(1 << 20):
                    total += len(chunk)
                    if total > info.file_size:
                        raise fail(f"{name!r} is larger than declared")
                    sha.update(chunk)
        except (zipfile.BadZipFile, OSError) as err:
            raise fail(f"{name!r} is corrupt: {err}") from err
        if total != info.file_size:
            raise fail(f"{name!r} is smaller than declared")
        return total, sha.hexdigest()

    def close(self) -> None:
        self.zf.close()

    def __enter__(self) -> "Archive":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def file_sha256(path: Path) -> tuple[int, str]:
    sha = hashlib.sha256()
    size = 0
    with open(path, "rb") as handle:
        while chunk := handle.read(1 << 20):
            size += len(chunk)
            sha.update(chunk)
    return size, sha.hexdigest()


# ------------------------------------------------------------ Windows ZIP


def walk_files(root: Path) -> list[str]:
    """Relative posix paths of every file below `root`; links are refused."""
    root = Path(root)
    found: list[str] = []
    for current, dirs, names in os.walk(root, followlinks=False):
        for name in [*dirs, *names]:
            if os.path.islink(os.path.join(current, name)):
                raise fail(f"{os.path.join(current, name)} is a link")
        for name in names:
            found.append(Path(os.path.join(current, name)).relative_to(root).as_posix())
    return sorted(found)


def _inventory_files(root: Path, skip: tuple[str, ...]) -> list[dict]:
    files = []
    seen: set[str] = set()
    for rel in walk_files(root):
        if rel in skip:
            continue
        validate_member_name(rel)
        if collision_key(rel) in seen:
            raise fail(f"colliding inventory path {rel!r}")
        seen.add(collision_key(rel))
        size, digest = file_sha256(Path(root) / rel)
        files.append({"path": rel, "size": size, "sha256": digest})
    return files


def _inventory_document(files: list[dict], *, version: str, target: str, source_sha: str, layout: int) -> bytes:
    return canonical_json({
        "schema": SCHEMA,
        "version": version,
        "target": target,
        "source_sha": source_sha,
        "updater_protocol": UPDATER_PROTOCOL,
        "layout_revision": layout,
        "files": files,
    })


def build_inventory(root: Path, version: str, source_sha: str) -> bytes:
    """Raw `hane-install.json` covering every owned file below `root`."""
    if not SHA1_RE.match(source_sha):
        raise fail("source SHA must be 40 lowercase hex digits")
    files = _inventory_files(root, (INVENTORY_NAME, INVENTORY_SIGNATURE_NAME))
    helper = Path(root) / "hane-updater.exe"
    if not helper.is_file():
        raise fail("hane-updater.exe is missing from the Windows staging directory")
    check_identity(helper.read_bytes(), version=version, target=WINDOWS_TARGET, source_sha=source_sha,
                   what="hane-updater.exe")
    return _inventory_document(files, version=version, target=WINDOWS_TARGET, source_sha=source_sha,
                               layout=WINDOWS_LAYOUT)


def build_macos_inventory(app: Path, version: str, source_sha: str) -> bytes:
    """Raw bundle inventory covering every file below the finished `Hane.app`.

    Paths are relative to the bundle. The bundle must already carry its ad-hoc
    seal: `Contents/_CodeSignature/CodeResources` is listed like any other file,
    so the inventory describes exactly the bytes macOS will verify. The
    inventory itself lives outside the app (the sidecar), so a v0.27.0-style
    inventory inside the bundle is refused.
    """
    if not SHA1_RE.match(source_sha):
        raise fail("source SHA must be 40 lowercase hex digits")
    files = _inventory_files(app, ())
    listed = {entry["path"] for entry in files}
    if listed & {MACOS_LEGACY_INVENTORY_REL, MACOS_LEGACY_INVENTORY_SIGNATURE_REL}:
        raise fail("the bundle must not carry the inventory inside Hane.app; it belongs in the sidecar")
    if MACOS_CODE_RESOURCES_REL not in listed:
        raise fail("the bundle is not sealed (Contents/_CodeSignature/CodeResources is missing); "
                   "sign it before the inventory is made")
    helper = Path(app) / MACOS_HELPER_REL
    if not helper.is_file():
        raise fail("Contents/Helpers/hane-updater is missing from the macOS bundle")
    check_identity(helper.read_bytes(), version=version, target=MACOS_TARGET, source_sha=source_sha,
                   what=MACOS_HELPER_REL)
    return _inventory_document(files, version=version, target=MACOS_TARGET, source_sha=source_sha,
                               layout=MACOS_LAYOUT)


def make_zip(root: Path, output: Path) -> None:
    """Deterministic ZIP of `root`'s files, forward-slash names, no dir entries."""
    output = Path(output)
    if output.exists():
        raise fail(f"{output} already exists; refusing to overwrite")
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for rel in walk_files(root):
            validate_member_name(rel)
            info = zipfile.ZipInfo(rel, date_time=ZIP_TIMESTAMP)
            info.create_system = 3
            info.external_attr = 0o644 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, (Path(root) / rel).read_bytes())


def msix_check(data: bytes, version: str) -> None:
    """Manifest identity and DLL path of the MSIX must match the Cargo version."""
    import io

    try:
        with zipfile.ZipFile(io.BytesIO(data)) as package:
            names = package.namelist()
            if "AppxManifest.xml" not in names:
                raise fail("the MSIX has no AppxManifest.xml")
            if "AppxSignature.p7x" not in names:
                raise fail("the MSIX is not signed (AppxSignature.p7x is missing)")
            manifest = package.read("AppxManifest.xml")
    except zipfile.BadZipFile as err:
        raise fail("Hane.ShellIntegration.msix is not a valid package") from err
    try:
        root = ET.fromstring(manifest)
    except ET.ParseError as err:
        raise fail("AppxManifest.xml is not valid XML") from err

    def local(tag: str) -> str:
        return tag.rsplit("}", 1)[-1]

    identity = next((e for e in root.iter() if local(e.tag) == "Identity"), None)
    if identity is None or identity.get("Version") != f"{version}.0":
        raise fail(f"MSIX manifest version must be {version}.0")
    classes = [e for e in root.iter() if local(e.tag) == "Class"]
    expected = f"shell-integration\\{version}\\hane_shell_extension.dll"
    if len(classes) != 2 or any(c.get("Path") != expected for c in classes):
        raise fail(f"MSIX COM class paths must all be {expected}")
    application = next((e for e in root.iter() if local(e.tag) == "Application"), None)
    if application is None or application.get("Executable") != "hane.exe":
        raise fail("MSIX application executable must stay hane.exe")


def verify_windows_zip(path: Path, *, version: str, source_sha: str, trust: Trust) -> dict:
    with Archive(path) as archive:
        dll = f"shell-integration/{version}/hane_shell_extension.dll"
        allowed_dirs = {"shell-integration", f"shell-integration/{version}"}
        if not archive.dirs <= allowed_dirs:
            raise fail(f"unexpected directory entries {sorted(archive.dirs - allowed_dirs)}")
        allowed = {*WINDOWS_ROOT_FILES, dll}
        extra = set(archive.files) - allowed
        if extra:
            raise fail(f"Windows ZIP has unexpected entries {sorted(extra)}")
        missing = allowed - set(archive.files) - set(WINDOWS_OPTIONAL)
        if missing:
            raise fail(f"Windows ZIP is missing {sorted(missing)}")
        for exe in ("hane.exe", "hane-updater.exe", dll):
            if not archive.read(exe)[:2] == b"MZ":
                raise fail(f"{exe} is not a Windows executable image")
        check_identity(archive.read("hane-updater.exe"), version=version, target=WINDOWS_TARGET,
                       source_sha=source_sha, what="hane-updater.exe")
        msix_check(archive.read("Hane.ShellIntegration.msix"), version)

        inventory_sha256 = verify_archive_inventory(
            archive, prefix="", inventory_path=INVENTORY_NAME, signature_path=INVENTORY_SIGNATURE_NAME,
            version=version, target=WINDOWS_TARGET, source_sha=source_sha, layout=WINDOWS_LAYOUT, trust=trust)
        return {
            "inventory_sha256": inventory_sha256,
            "unpacked_size": archive.unpacked_size,
        }


def verify_archive_inventory(archive: Archive, *, prefix: str, inventory_path: str, signature_path: str,
                             version: str, target: str, source_sha: str, layout: int, trust: Trust) -> str:
    """Checks the Ed25519-signed inventory against the real ZIP content.

    Inventory paths are relative to `prefix`; files outside it are not owned.
    `inventory_path` / `signature_path` are full ZIP names: inside `prefix` for
    Windows (the inventory is then not owned) and in the sidecar for macOS.
    Returns the SHA-256 of the raw inventory bytes (the value the catalog binds).
    """
    for name in (inventory_path, signature_path):
        if name not in archive.files:
            raise fail(f"ZIP is missing {name!r}")
    raw = archive.read(inventory_path, MAX_INVENTORY_BYTES)
    verify_signature(INVENTORY_DOMAIN, raw, archive.read(signature_path, MAX_SIGNATURE_BYTES), trust)
    inventory = strict_json(raw, "inventory")
    fields = {"schema", "version", "target", "source_sha", "updater_protocol", "layout_revision", "files"}
    if not isinstance(inventory, dict) or set(inventory) != fields:
        raise fail("inventory has unexpected fields")
    expected = {
        "schema": SCHEMA,
        "version": version,
        "target": target,
        "source_sha": source_sha,
        "updater_protocol": UPDATER_PROTOCOL,
        "layout_revision": layout,
    }
    for key, value in expected.items():
        if inventory[key] != value or isinstance(inventory[key], bool):
            raise fail(f"inventory {key} is {inventory[key]!r}, expected {value!r}")
    listed = inventory["files"]
    if not isinstance(listed, list) or not listed or len(listed) > MAX_ENTRIES:
        raise fail("inventory file list is out of range")
    owned = {
        name[len(prefix):] for name in archive.files
        if name.startswith(prefix) and name not in (inventory_path, signature_path)
    }
    paths = []
    for entry in listed:
        if not isinstance(entry, dict) or set(entry) != {"path", "size", "sha256"}:
            raise fail("inventory entry has unexpected fields")
        rel = entry["path"]
        if not isinstance(rel, str) or rel not in owned:
            raise fail(f"inventory lists {rel!r}, which is not in the ZIP")
        if not is_uint(entry["size"], 0, MAX_ENTRY_BYTES) or not (
            isinstance(entry["sha256"], str) and SHA256_RE.match(entry["sha256"])
        ):
            raise fail(f"inventory entry {rel!r} has an invalid size or hash")
        size, digest = archive.digest(prefix + rel)
        if (size, digest) != (entry["size"], entry["sha256"]):
            raise fail(f"inventory size/hash for {rel!r} differs from the ZIP content")
        paths.append(rel)
    if len(set(paths)) != len(paths) or set(paths) != owned:
        raise fail("inventory must list every owned file in the ZIP exactly once")
    return sha256_hex(raw)


# ------------------------------------------------------------ macOS ZIP


def verify_macos_zip(path: Path, *, version: str, source_sha: str, trust: Trust) -> dict:
    """Release verifier: accepts the product bundle identifier only."""
    return _verify_macos_zip(path, version=version, source_sha=source_sha, trust=trust,
                             bundle_id=MACOS_BUNDLE_ID)


def verify_validation_macos_zip(path: Path, *, version: str, source_sha: str, trust: Trust) -> dict:
    """Verifier of the isolated `validation-feed` package (Issue #510): the fixed
    validation bundle identifier only. Never used by the Release path."""
    return _verify_macos_zip(path, version=version, source_sha=source_sha, trust=trust,
                             bundle_id=VALIDATION_MACOS_BUNDLE_ID)


def _verify_macos_zip(path: Path, *, version: str, source_sha: str, trust: Trust, bundle_id: str) -> dict:
    inventory_path, signature_path = macos_sidecar_paths(version, source_sha)
    sidecar_dir = macos_sidecar_dir(version, source_sha)
    with Archive(path) as archive:
        allowed_files = {f"{MACOS_ROOT}/README.md", f"{MACOS_ROOT}/LICENSE", inventory_path, signature_path}
        for name in archive.files:
            if not name.startswith(f"{MACOS_APP}/") and name not in allowed_files:
                raise fail(f"macOS ZIP has unexpected entry {name!r}")
            if any(part in ("__MACOSX", ".DS_Store") or part.startswith("._") for part in name.split("/")):
                raise fail(f"macOS ZIP contains Finder/AppleDouble metadata {name!r}")
            if archive.executable(name) != name.startswith(MACOS_EXECUTABLE_PREFIXES):
                raise fail(f"{name!r} has an unexpected executable bit")
        allowed_dirs = {MACOS_ROOT, MACOS_SIDECAR_ROOT, sidecar_dir}
        for directory in archive.dirs:
            if directory not in allowed_dirs and directory != MACOS_APP and not directory.startswith(f"{MACOS_APP}/"):
                raise fail(f"macOS ZIP has unexpected directory {directory!r}")
        for legacy in (MACOS_LEGACY_INVENTORY_REL, MACOS_LEGACY_INVENTORY_SIGNATURE_REL):
            if f"{MACOS_APP}/{legacy}" in archive.files:
                raise fail(f"macOS ZIP carries the inventory inside Hane.app ({legacy}); it belongs in the sidecar")
        missing = [name for name in (*MACOS_REQUIRED, inventory_path, signature_path) if name not in archive.files]
        if missing:
            raise fail(f"macOS ZIP is missing {missing}")
        try:
            info = plistlib.loads(archive.read(f"{MACOS_APP}/Contents/Info.plist", 1 << 20))
        except Exception as err:  # plistlib raises several types
            raise fail(f"Info.plist is not a valid property list: {err}") from err
        expected = {
            "CFBundleIdentifier": bundle_id,
            "CFBundleExecutable": "hane",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": version,
            "CFBundleVersion": version,
            "HaneSourceCommit": source_sha,
            "HaneUpdaterProtocol": UPDATER_PROTOCOL,
        }
        for key, value in expected.items():
            found = info.get(key)
            if found != value or isinstance(found, bool) or type(found) is not type(value):
                raise fail(f"Info.plist {key} is {found!r}, expected {value!r}")
        # Apple Silicon only runs signed code: both executables carry the ad-hoc
        # signature and the finished bundle is sealed by CodeResources. This is
        # the structural half; the release job also runs `verify-macos-codesign`
        # on the extracted app with Apple's own tool.
        for binary in ("Contents/MacOS/hane", "Contents/Helpers/hane-updater"):
            check_macho_signed(archive.read(f"{MACOS_APP}/{binary}"), binary)
        check_identity(archive.read(f"{MACOS_APP}/Contents/Helpers/hane-updater"), version=version,
                       target=MACOS_TARGET, source_sha=source_sha, what="Contents/Helpers/hane-updater")
        inventory_sha256 = verify_archive_inventory(
            archive, prefix=f"{MACOS_APP}/", inventory_path=inventory_path, signature_path=signature_path,
            version=version, target=MACOS_TARGET, source_sha=source_sha, layout=MACOS_LAYOUT, trust=trust)
        return {"bundle_id": info["CFBundleIdentifier"], "bundle_version": info["CFBundleShortVersionString"],
                "inventory_sha256": inventory_sha256, "unpacked_size": archive.unpacked_size}


# ------------------------------------------------------------ catalog


def build_catalog(release_dir: Path, *, tag: str, source_sha: str, sequence: int,
                  trust: Trust, published: dt.datetime | None = None,
                  validity: dt.timedelta = DEFAULT_VALIDITY) -> bytes:
    version = version_of(tag)
    if not SHA1_RE.match(source_sha):
        raise fail("source SHA must be 40 lowercase hex digits")
    if not is_uint(sequence, 1):
        raise fail("catalog_sequence must be an integer from 1 to 2^64-1")
    release_dir = Path(release_dir)
    windows = verify_windows_zip(release_dir / WINDOWS_ASSET, version=version, source_sha=source_sha, trust=trust)
    macos = verify_macos_zip(release_dir / MACOS_ASSET, version=version, source_sha=source_sha, trust=trust)
    w_size, w_sha = file_sha256(release_dir / WINDOWS_ASSET)
    m_size, m_sha = file_sha256(release_dir / MACOS_ASSET)
    published = (published or now_utc()).replace(microsecond=0)
    catalog = {
        "schema": SCHEMA,
        "channel": CHANNEL,
        "repository": REPOSITORY,
        "version": version,
        "tag": tag,
        "source_sha": source_sha,
        "catalog_sequence": sequence,
        "published_at": utc_text(published),
        "expires_at": utc_text(published + validity),
        "updater_protocol": UPDATER_PROTOCOL,
        "min_updater_protocol": UPDATER_PROTOCOL,
        "state_compatibility": STATE_COMPATIBILITY,
        "targets": {
            WINDOWS_TARGET: {
                "asset_name": WINDOWS_ASSET,
                "archive_size": w_size,
                "archive_sha256": w_sha,
                "unpacked_size": windows["unpacked_size"],
                "layout_revision": WINDOWS_LAYOUT,
                "inventory_sha256": windows["inventory_sha256"],
            },
            MACOS_TARGET: {
                "asset_name": MACOS_ASSET,
                "archive_size": m_size,
                "archive_sha256": m_sha,
                "unpacked_size": macos["unpacked_size"],
                "layout_revision": MACOS_LAYOUT,
                "inventory_sha256": macos["inventory_sha256"],
                "bundle_id": macos["bundle_id"],
                "bundle_version": macos["bundle_version"],
            },
        },
    }
    raw = canonical_json(catalog)
    if len(raw) > MAX_CATALOG_BYTES:
        raise fail("catalog exceeds the size limit")
    return raw


CATALOG_FIELDS = {
    "schema", "channel", "repository", "version", "tag", "source_sha", "catalog_sequence", "published_at",
    "expires_at", "updater_protocol", "min_updater_protocol", "state_compatibility", "targets",
}
WINDOWS_ENTRY_FIELDS = {"asset_name", "archive_size", "archive_sha256", "unpacked_size", "layout_revision",
                        "inventory_sha256"}
MACOS_ENTRY_FIELDS = {"asset_name", "archive_size", "archive_sha256", "unpacked_size", "layout_revision",
                      "inventory_sha256", "bundle_id", "bundle_version"}


def parse_catalog(raw: bytes) -> dict:
    """Structural checks shared by publication and the published-catalog scan."""
    if len(raw) > MAX_CATALOG_BYTES:
        raise fail("catalog exceeds the size limit")
    catalog = strict_json(raw, "catalog")
    if not isinstance(catalog, dict) or set(catalog) != CATALOG_FIELDS:
        raise fail("catalog has unexpected fields")
    if catalog["schema"] != SCHEMA or isinstance(catalog["schema"], bool):
        raise fail("unknown catalog schema")
    if catalog["channel"] != CHANNEL or catalog["repository"] != REPOSITORY:
        raise fail("catalog channel or repository is not accepted")
    if not is_uint(catalog["catalog_sequence"], 1):
        raise fail("catalog_sequence must be an integer from 1 to 2^64-1")
    tag = catalog["tag"]
    if not isinstance(tag, str) or catalog["version"] != version_of(tag):
        raise fail("catalog tag does not match its version")
    if not isinstance(catalog["source_sha"], str) or not SHA1_RE.match(catalog["source_sha"]):
        raise fail("catalog source_sha is malformed")
    return catalog


def verify_catalog_bytes(raw: bytes, signature_file: bytes, trust: Trust) -> dict:
    verify_signature(CATALOG_DOMAIN, raw, signature_file, trust)
    return parse_catalog(raw)


def verify_release_dir(release_dir: Path, *, tag: str, source_sha: str, trust: Trust,
                       now: dt.datetime | None = None, sequence: int | None = None) -> dict:
    """The single verifier: signature, catalog fields and both real ZIPs."""
    release_dir = Path(release_dir)
    version = version_of(tag)
    names = sorted(p.name for p in release_dir.iterdir())
    if names != sorted(RELEASE_ASSETS):
        raise fail(f"release assets are {names}, expected exactly {sorted(RELEASE_ASSETS)}")
    for name in RELEASE_ASSETS:
        if (release_dir / name).is_symlink() or not (release_dir / name).is_file():
            raise fail(f"{name} is not a regular file")
    raw = (release_dir / CATALOG_NAME).read_bytes()
    catalog = verify_catalog_bytes(raw, (release_dir / SIGNATURE_NAME).read_bytes(), trust)
    now = now or now_utc()
    if (catalog["tag"], catalog["version"], catalog["source_sha"]) != (tag, version, source_sha):
        raise fail("catalog tag/version/source_sha differ from the release being published")
    if sequence is not None and catalog["catalog_sequence"] != sequence:
        raise fail("catalog_sequence differs from the planned sequence")
    published = parse_time(catalog["published_at"], "published_at")
    expires = parse_time(catalog["expires_at"], "expires_at")
    if expires <= published or expires - published > MAX_VALIDITY:
        raise fail("catalog validity window is out of range")
    if published > now + MAX_FUTURE_SKEW or now >= expires:
        raise fail("catalog is not valid at the current time")
    for key in ("updater_protocol", "min_updater_protocol"):
        if catalog[key] != UPDATER_PROTOCOL or isinstance(catalog[key], bool):
            raise fail(f"catalog {key} must be {UPDATER_PROTOCOL}")
    if catalog["state_compatibility"] != STATE_COMPATIBILITY:
        raise fail("catalog state_compatibility is not accepted")
    targets = catalog["targets"]
    if not isinstance(targets, dict) or set(targets) != {WINDOWS_TARGET, MACOS_TARGET}:
        raise fail("catalog must contain exactly the Windows x64 and macOS arm64 targets")

    windows = verify_windows_zip(release_dir / WINDOWS_ASSET, version=version, source_sha=source_sha, trust=trust)
    macos = verify_macos_zip(release_dir / MACOS_ASSET, version=version, source_sha=source_sha, trust=trust)
    expected = {
        WINDOWS_TARGET: (WINDOWS_ASSET, WINDOWS_LAYOUT, WINDOWS_ENTRY_FIELDS, windows),
        MACOS_TARGET: (MACOS_ASSET, MACOS_LAYOUT, MACOS_ENTRY_FIELDS, macos),
    }
    for target, (asset, layout, fields, facts) in expected.items():
        entry = targets[target]
        if not isinstance(entry, dict) or set(entry) != fields:
            raise fail(f"{target} entry has unexpected fields")
        size, digest = file_sha256(release_dir / asset)
        checks = {
            "asset_name": asset,
            "archive_size": size,
            "archive_sha256": digest,
            "unpacked_size": facts["unpacked_size"],
            "layout_revision": layout,
        }
        for key, value in checks.items():
            if entry[key] != value or isinstance(entry[key], bool):
                raise fail(f"{target} {key} is {entry[key]!r}, expected {value!r}")
        if size > MAX_ARCHIVE_BYTES or facts["unpacked_size"] > MAX_UNPACKED_BYTES:
            raise fail(f"{target} archive exceeds the size limits")
    if targets[WINDOWS_TARGET]["inventory_sha256"] != windows["inventory_sha256"]:
        raise fail("Windows inventory_sha256 differs from the ZIP's hane-install.json")
    mac_entry = targets[MACOS_TARGET]
    for key, value in {"inventory_sha256": macos["inventory_sha256"], "bundle_id": macos["bundle_id"],
                       "bundle_version": macos["bundle_version"]}.items():
        if mac_entry[key] != value:
            raise fail(f"macOS {key} is {mac_entry[key]!r}, expected {value!r}")
    return catalog


# ------------------------------------------------------------ GitHub access


def _run_gh(args: list[str]) -> bytes:
    try:
        result = subprocess.run(["gh", *args], capture_output=True, check=False)
    except OSError as err:
        raise fail(f"could not run gh: {err}") from err
    if result.returncode != 0:
        raise fail(f"gh {' '.join(args[:3])} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


class Gh:
    """Thin `gh api` wrapper. Any failure raises; nothing is treated as 'empty'."""

    def __init__(self, runner: Callable[[list[str]], bytes] | None = None):
        self.run = runner or _run_gh

    def json(self, path: str) -> object:
        try:
            return json.loads(self.run(["api", path]))
        except ValueError as err:
            raise fail(f"GitHub returned invalid JSON for {path}") from err

    def bytes(self, path: str) -> bytes:
        return self.run(["api", "-H", "Accept: application/octet-stream", path])

    def paged(self, path: str) -> list[dict]:
        items: list[dict] = []
        joiner = "&" if "?" in path else "?"
        for page in range(1, 1001):
            batch = self.json(f"{path}{joiner}per_page=100&page={page}")
            if not isinstance(batch, list):
                raise fail(f"GitHub returned a non-list page for {path}")
            if not batch:
                return items
            items.extend(batch)
        raise fail(f"too many pages while reading {path}")


def published_catalogs(gh: Gh, repo: str, trust: Trust) -> list[tuple[int, str]]:
    """(sequence, tag) of every catalog on a non-draft Release, fully verified.

    A Release without any catalog asset is an older pre-update release and is
    skipped. A half-present or unverifiable catalog is an error, never "none".
    """
    found: list[tuple[int, str]] = []
    for release in gh.paged(f"repos/{repo}/releases"):
        if release.get("draft"):
            continue
        assets = {a.get("name"): a for a in release.get("assets", [])}
        has_catalog, has_signature = CATALOG_NAME in assets, SIGNATURE_NAME in assets
        if not has_catalog and not has_signature:
            continue
        tag = release.get("tag_name")
        if has_catalog != has_signature:
            raise fail(f"release {tag} has only one of the catalog and its signature")
        raw = gh.bytes(f"repos/{repo}/releases/assets/{assets[CATALOG_NAME]['id']}")
        signature = gh.bytes(f"repos/{repo}/releases/assets/{assets[SIGNATURE_NAME]['id']}")
        catalog = verify_catalog_bytes(raw, signature, trust)
        if catalog["tag"] != tag:
            raise fail(f"catalog on release {tag} names tag {catalog['tag']}")
        found.append((catalog["catalog_sequence"], tag))
    return found


def next_sequence(published: Iterable[tuple[int, str]], initial: bool) -> int:
    published = list(published)
    sequences = [seq for seq, _ in published]
    if len(set(sequences)) != len(sequences):
        raise fail("published catalogs reuse a catalog_sequence")
    if not published:
        if not initial:
            raise fail("no published catalog was found; the first catalog needs the explicit initial_catalog input")
        return 1
    if initial:
        raise fail("initial_catalog was requested but published catalogs already exist")
    highest = max(sequences)
    if highest >= U64_MAX:
        raise fail("catalog_sequence would overflow u64")
    return highest + 1


def plan_release(gh: Gh, repo: str, tag: str, *, tag_push: bool, initial: bool, trust: Trust) -> dict:
    """Checks that must hold inside the serialized publish section.

    No Release (draft or public) may exist for `tag`; the tag itself may exist
    only when this run was started by pushing it; the version must be newer than
    every other stable tag/Release (so `--latest` is never given to an older
    version); and the next sequence follows the highest published catalog.
    """
    version = parse_tag(tag)
    releases = gh.paged(f"repos/{repo}/releases")
    if any(r.get("tag_name") == tag for r in releases):
        raise fail(f"a Release for {tag} already exists; refusing to modify or replace it")
    tag_names = {t.get("name") for t in gh.paged(f"repos/{repo}/tags")}
    if tag_push and tag not in tag_names:
        raise fail(f"tag {tag} was pushed but is not visible")
    if not tag_push and tag in tag_names:
        raise fail(f"tag {tag} already exists; refusing to publish over it")
    known = {n for n in tag_names if isinstance(n, str)} | {
        r["tag_name"] for r in releases if not r.get("draft") and isinstance(r.get("tag_name"), str)
    }
    others = [parse_tag(n) for n in known if n != tag and TAG_RE.match(n)]
    if others and version <= max(others):
        latest = "v" + ".".join(map(str, max(others)))
        raise fail(f"{tag} is not newer than the latest stable version {latest}")
    sequence = next_sequence(published_catalogs(gh, repo, trust), initial)
    return {"sequence": sequence, "make_latest": "true"}


def fetch_assets(gh: Gh, repo: str, dest: Path, *, release_id: int | None = None, tag: str | None = None) -> dict:
    """Downloads exactly the four release assets of a Draft (by id) or public (by tag) Release."""
    if (release_id is None) == (tag is None):
        raise fail("give exactly one of --release-id and --tag")
    if release_id is not None:
        release = gh.json(f"repos/{repo}/releases/{release_id}")
    else:
        matches = [r for r in gh.paged(f"repos/{repo}/releases") if r.get("tag_name") == tag and not r.get("draft")]
        if len(matches) != 1:
            raise fail(f"expected exactly one published Release for {tag}, found {len(matches)}")
        release = matches[0]
    if not isinstance(release, dict):
        raise fail("GitHub returned an unexpected Release document")
    assets = release.get("assets", [])
    names = sorted(a.get("name") for a in assets)
    if names != sorted(RELEASE_ASSETS):
        raise fail(f"Release assets are {names}, expected exactly {sorted(RELEASE_ASSETS)}")
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    if any(dest.iterdir()):
        raise fail(f"{dest} must be empty")
    for asset in assets:
        if asset.get("state") != "uploaded":
            raise fail(f"asset {asset.get('name')} is not fully uploaded")
        data = gh.bytes(f"repos/{repo}/releases/assets/{asset['id']}")
        if len(data) != asset.get("size"):
            raise fail(f"asset {asset['name']} downloaded {len(data)} bytes, GitHub reports {asset.get('size')}")
        (dest / asset["name"]).write_bytes(data)
    return release


# ------------------------------------------------------------ extraction


def extract_zip(path: Path, dest: Path) -> None:
    dest = Path(dest)
    if dest.exists() and any(dest.iterdir()):
        raise fail(f"{dest} must be empty")
    dest.mkdir(parents=True, exist_ok=True)
    with Archive(path) as archive:
        for info in archive.entries:
            target = dest / info.filename
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(info.filename))
            if archive.executable(info.filename):
                target.chmod(0o755)


# ------------------------------------------------------------ CLI


def _github_output(path: str | None, values: dict) -> None:
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            for key, value in values.items():
                handle.write(f"{key}={value}\n")


def _require_env(names: list[str]) -> None:
    missing = [n for n in names if not os.environ.get(n)]
    if missing:
        raise fail("required configuration is not set: " + ", ".join(missing))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    def add(name: str, *flags: str, **kw: object) -> argparse.ArgumentParser:
        p = sub.add_parser(name)
        for flag in flags:
            p.add_argument(f"--{flag}", required=kw.get("required", True))
        return p

    add("require-env").add_argument("names", nargs="+")
    add("binary-identity", "binary", "version", "target", "source-sha")
    add("macho-signed", "binary")
    add("verify-macos-codesign", "app")
    add("macos-sidecar-dir", "version", "source-sha")
    add("windows-inventory", "root", "version", "source-sha", "output")
    add("macos-inventory", "app", "version", "source-sha", "output")
    sign = add("sign", "input", "output")
    sign.add_argument("--domain", choices=("catalog", "inventory"), required=True)
    add("make-zip", "root", "output")
    add("extract-zip", "zip", "dest")
    add("verify-windows-zip", "zip", "version", "source-sha")
    add("verify-macos-zip", "zip", "version", "source-sha")
    bc = add("build-catalog", "dir", "tag", "source-sha", "output")
    bc.add_argument("--sequence", type=int, required=True)
    vr = add("verify-release", "dir", "tag", "source-sha")
    vr.add_argument("--sequence", type=int)
    pl = add("plan", "repo", "tag")
    pl.add_argument("--tag-push", action="store_true")
    pl.add_argument("--initial", action="store_true")
    pl.add_argument("--github-output")
    fa = add("fetch-assets", "repo", "dest")
    fa.add_argument("--release-id", type=int)
    fa.add_argument("--tag")

    args = parser.parse_args(argv)
    try:
        return _dispatch(args)
    except ReleaseError as err:
        print(f"release_catalog: {err}", file=sys.stderr)
        return 1


def _dispatch(args: argparse.Namespace) -> int:
    cmd = args.command
    if cmd == "require-env":
        _require_env(args.names)
    elif cmd == "binary-identity":
        check_identity(Path(args.binary).read_bytes(), version=args.version, target=args.target,
                       source_sha=args.source_sha, what=args.binary)
    elif cmd == "macho-signed":
        check_macho_signed(Path(args.binary).read_bytes(), args.binary)
    elif cmd == "verify-macos-codesign":
        verify_macos_codesign(Path(args.app))
    elif cmd == "macos-sidecar-dir":
        if not SHA1_RE.match(args.source_sha):
            raise fail("source SHA must be 40 lowercase hex digits")
        # Relative to the directory that holds Hane.app (the ZIP's top folder).
        print(macos_sidecar_dir(args.version, args.source_sha).removeprefix(f"{MACOS_ROOT}/"))
    elif cmd == "windows-inventory":
        Path(args.output).write_bytes(build_inventory(Path(args.root), args.version, args.source_sha))
    elif cmd == "macos-inventory":
        Path(args.output).write_bytes(build_macos_inventory(Path(args.app), args.version, args.source_sha))
    elif cmd == "sign":
        seed = signing_seed()
        trust = Trust.from_env()
        key_id = os.environ[ENV_KEY_ID]
        if trust.keys[key_id] != public_key_of(seed):
            raise fail("the signing key does not match the configured public key")
        payload = Path(args.input).read_bytes()
        domain = CATALOG_DOMAIN if args.domain == "catalog" else INVENTORY_DOMAIN
        out = Path(args.output)
        if out.exists():
            raise fail(f"{out} already exists; refusing to overwrite")
        signature = make_signature_file(domain, payload, key_id, seed)
        verify_signature(domain, payload, signature, trust)
        out.write_bytes(signature)
    elif cmd == "make-zip":
        make_zip(Path(args.root), Path(args.output))
    elif cmd == "extract-zip":
        extract_zip(Path(args.zip), Path(args.dest))
    elif cmd == "verify-windows-zip":
        verify_windows_zip(Path(args.zip), version=args.version, source_sha=args.source_sha, trust=Trust.from_env())
    elif cmd == "verify-macos-zip":
        verify_macos_zip(Path(args.zip), version=args.version, source_sha=args.source_sha, trust=Trust.from_env())
    elif cmd == "build-catalog":
        trust = Trust.from_env()
        out = Path(args.output)
        if out.exists():
            raise fail(f"{out} already exists; refusing to overwrite")
        out.write_bytes(build_catalog(Path(args.dir), tag=args.tag, source_sha=args.source_sha,
                                      sequence=args.sequence, trust=trust))
    elif cmd == "verify-release":
        verify_release_dir(Path(args.dir), tag=args.tag, source_sha=args.source_sha,
                           trust=Trust.from_env(), sequence=args.sequence)
    elif cmd == "plan":
        plan = plan_release(Gh(), args.repo, args.tag, tag_push=args.tag_push, initial=args.initial,
                            trust=Trust.from_env())
        _github_output(args.github_output, plan)
        print(json.dumps(plan))
    elif cmd == "fetch-assets":
        fetch_assets(Gh(), args.repo, Path(args.dest), release_id=args.release_id, tag=args.tag)
    return 0


if __name__ == "__main__":
    sys.exit(main())
