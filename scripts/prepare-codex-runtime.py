#!/usr/bin/env python3
"""Fetch, verify, and repackage the pinned upstream Codex App Server release.

This is a *build-time* tool: it runs on a maintainer's or CI machine with
network access, never on an end user's machine, and the Hane binary itself
never requires Python or this script at runtime.

Given `--target <triple>`, it:

1. Reads the fixed upstream archive URL/size/SHA-256 for that target from
   `--lock` (see `assets/ai/codex-runtime-lock.json`). Only an exact size and
   SHA-256 match is accepted; nothing is trusted from the network beyond
   what the lock already pins.
2. Downloads the archive over HTTPS from an explicit host allowlist only
   (`ALLOWED_HOSTS`), including every redirect hop, without ever sending the
   original request's credentials to a redirected host (none are sent in
   the first place) and without logging signed/query parameters.
3. Extracts it into a private staging directory, rejecting any entry that is
   an absolute path, contains a `..` component, a drive prefix, a colon
   (alternate-data-stream-style), a Windows-reserved device name component,
   a trailing dot/space component, a duplicate or case-folded-colliding
   path, a symlink/hardlink/device/fifo entry, or exceeds a fixed per-entry
   or total size cap.
4. Builds a full file-level inventory (path/type/size/sha256/executable)
   from the actual extracted bytes and compares it, entry for entry,
   against a pinned inventory file this repository must already contain
   for that exact target+version (`inventory` field in the lock). A
   missing or mismatched pinned inventory is a fail-closed error: this tool
   never invents or silently accepts inventory data it cannot confirm.
5. Copies only the license/notice documents the lock explicitly lists (by
   archive-relative path) into the bundle's `licenses/`. An empty or
   missing license list is fail-closed: no bundle is produced, because the
   rights/attribution for the upstream helper binaries have not been
   confirmed yet.
6. Writes `manifest.json` (the `schema_version = 1` contract
   `crates/ai/src/runtime_distribution.rs` already resolves against),
   `licenses/`, and `payload/` under `--output`, as one atomic directory
   replace. Any failure at any step leaves `--output` untouched -- this
   tool never leaves a partially-built bundle at that path.

`manifest.json`'s `files[].path`/`entrypoint` are recorded relative to
`payload/` (i.e. as if `payload/` were the manifest's own directory), matching
the existing resolver contract; a later release-packaging step -- out of
this script's scope -- is expected to flatten `payload/`'s contents up to sit
directly beside `manifest.json` before shipping the runtime bundle.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Optional

DEFAULT_LOCK_PATH = Path(__file__).resolve().parent.parent / "assets" / "ai" / "codex-runtime-lock.json"

# Only these hosts may ever serve the archive bytes, on the initial request
# or any redirect hop: the GitHub release-asset page and the storage host it
# redirects downloads to.
ALLOWED_HOSTS = frozenset({"github.com", "objects.githubusercontent.com"})

CHUNK_SIZE = 1024 * 1024
MAX_ENTRY_SIZE = 1 * 1024 ** 3  # generous per-file cap; the whole archive is ~100MB compressed
MAX_TOTAL_SIZE = 4 * 1024 ** 3  # generous total-extracted-size cap
USER_AGENT = "hane-prepare-codex-runtime/1"

_RESERVED_WINDOWS_STEMS = (
    {"CON", "PRN", "AUX", "NUL"}
    | {f"COM{i}" for i in range(1, 10)}
    | {f"LPT{i}" for i in range(1, 10)}
)

MANIFEST_SCHEMA_VERSION = 1
MANIFEST_FILE_NAME = "manifest.json"
LICENSES_DIR_NAME = "licenses"
PAYLOAD_DIR_NAME = "payload"


class PipelineError(Exception):
    """A fail-closed pipeline error. The message is diagnostic text, safe to
    print verbatim (it never contains a full signed URL, a secret, or raw
    archive bytes)."""


def redact_url(url: str) -> str:
    """`url` with any query string/fragment stripped, so a signed GitHub
    release-asset redirect URL is never logged in full."""
    parts = urllib.parse.urlsplit(url)
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path, "", ""))


def load_lock(lock_path: Path) -> dict:
    try:
        data = json.loads(lock_path.read_text(encoding="utf-8"))
    except OSError as error:
        raise PipelineError(f"lockファイルを読み込めません: {lock_path} ({error})") from error
    except json.JSONDecodeError as error:
        raise PipelineError(f"lockファイルがJSONとして不正です: {lock_path} ({error})") from error
    if data.get("schema_version") != 1:
        raise PipelineError(f"lockのschema_versionが不正です: {data.get('schema_version')!r}")
    if not isinstance(data.get("targets"), dict):
        raise PipelineError("lockにtargets定義がありません")
    if not isinstance(data.get("codex_version"), str) or not data["codex_version"].strip():
        raise PipelineError("lockにcodex_versionがありません")
    if not isinstance(data.get("source_commit"), str) or not data["source_commit"].strip():
        raise PipelineError("lockにsource_commitがありません")
    return data


def select_target_entry(lock: dict, target: str) -> dict:
    entry = lock["targets"].get(target)
    if not isinstance(entry, dict):
        raise PipelineError(f"lockに対象targetの定義がありません: {target!r}")
    required = (
        "url",
        "archive_size",
        "archive_sha256",
        "entrypoint_name",
        "inventory",
        "licenses",
        "runtime_profile",
        "config_profile",
        "state_profile",
    )
    for field in required:
        if field not in entry:
            raise PipelineError(f"lockのtarget {target!r} に {field!r} がありません")
    return entry


def validate_relative_member_path(raw_name: str) -> tuple:
    """Validates one archive entry's `name` and returns it split into path
    components, rejecting anything that could escape the staging directory
    or collide/confuse a case-insensitive or short-name-aware filesystem."""
    if not raw_name or not raw_name.strip():
        raise PipelineError("archiveに空のentry pathがあります")
    if "\\" in raw_name:
        raise PipelineError(f"archive entryのpathにbackslashは許可しません: {raw_name!r}")
    if ":" in raw_name:
        raise PipelineError(f"archive entryのpathにcolon(drive/ADS疑い)は許可しません: {raw_name!r}")
    if raw_name.startswith("/"):
        raise PipelineError(f"archive entryのpathは絶対pathを許可しません: {raw_name!r}")
    components = tuple(raw_name.split("/"))
    for component in components:
        if component in ("", ".", ".."):
            raise PipelineError(f"archive entryのpathにtraversal/空要素があります: {raw_name!r}")
        if component != component.rstrip(" ."):
            raise PipelineError(f"archive entryのpath要素が末尾dot/spaceです: {raw_name!r}")
        stem = component.split(".", 1)[0].upper()
        if stem in _RESERVED_WINDOWS_STEMS:
            raise PipelineError(f"archive entryのpathがWindows予約名です: {raw_name!r}")
    return components


def extract_archive(archive_path: Path, staging_dir: Path) -> list:
    """Safely extracts `archive_path` under `staging_dir`, returning a
    `path`-sorted inventory of every regular file actually written:
    `{"path", "type": "file", "size", "sha256", "executable"}`."""
    inventory = []
    seen_exact = set()
    seen_casefold = set()
    total_size = 0

    with tarfile.open(archive_path, mode="r:gz") as archive:
        for member in archive:
            components = validate_relative_member_path(member.name)
            posix_path = "/".join(components)
            casefold_key = posix_path.lower()
            if posix_path in seen_exact:
                raise PipelineError(f"archiveに重複entryがあります: {posix_path!r}")
            if casefold_key in seen_casefold:
                raise PipelineError(f"archiveにcase違いの衝突entryがあります: {posix_path!r}")
            seen_exact.add(posix_path)
            seen_casefold.add(casefold_key)

            if member.isdir():
                staging_dir.joinpath(*components).mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                raise PipelineError(
                    "archiveに通常ファイル以外のentryがあります"
                    f"(symlink/hardlink/device/fifo等): {posix_path!r}"
                )
            if member.size < 0 or member.size > MAX_ENTRY_SIZE:
                raise PipelineError(f"archive entryのsizeが上限を超えています: {posix_path!r}")
            total_size += member.size
            if total_size > MAX_TOTAL_SIZE:
                raise PipelineError("archiveの展開後合計sizeが上限を超えています")

            dest_path = staging_dir.joinpath(*components)
            dest_path.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                raise PipelineError(f"archive entryを読み込めません: {posix_path!r}")

            hasher = hashlib.sha256()
            actual_size = 0
            with open(dest_path, "wb") as out:
                while True:
                    chunk = source.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    actual_size += len(chunk)
                    if actual_size > MAX_ENTRY_SIZE:
                        raise PipelineError(f"archive entryの実sizeが上限を超えています: {posix_path!r}")
                    hasher.update(chunk)
                    out.write(chunk)
            if actual_size != member.size:
                raise PipelineError(f"archive entryの宣言sizeと実sizeが一致しません: {posix_path!r}")

            executable = bool(member.mode & 0o111)
            os.chmod(dest_path, 0o755 if executable else 0o644)

            inventory.append(
                {
                    "path": posix_path,
                    "type": "file",
                    "size": actual_size,
                    "sha256": hasher.hexdigest(),
                    "executable": executable,
                }
            )

    inventory.sort(key=lambda entry: entry["path"])
    return inventory


def load_pinned_inventory(path: Path) -> list:
    if not path.is_file():
        raise PipelineError(
            f"pinned inventoryが見つかりません: {path}. "
            "実archiveの内容を確認してreviewed inventoryとしてcommitしてから再実行してください。"
        )
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise PipelineError(f"pinned inventoryがJSONとして不正です: {path} ({error})") from error
    if not isinstance(data, list) or not data:
        raise PipelineError(f"pinned inventoryが空または配列ではありません: {path}")
    required_fields = ("path", "type", "size", "sha256", "executable")
    for entry in data:
        if not isinstance(entry, dict) or any(field not in entry for field in required_fields):
            raise PipelineError(f"pinned inventoryのentryが不正です: {path}")
    return sorted(data, key=lambda entry: entry["path"])


def diff_inventory(expected: list, actual: list) -> list:
    expected_by_path = {entry["path"]: entry for entry in expected}
    actual_by_path = {entry["path"]: entry for entry in actual}
    problems = []
    for path in sorted(set(expected_by_path) - set(actual_by_path)):
        problems.append(f"pinned inventoryにあるがarchiveにない: {path}")
    for path in sorted(set(actual_by_path) - set(expected_by_path)):
        problems.append(f"archiveにあるがpinned inventoryにない: {path}")
    for path in sorted(set(expected_by_path) & set(actual_by_path)):
        expected_entry = expected_by_path[path]
        actual_entry = actual_by_path[path]
        same = all(expected_entry[field] == actual_entry[field] for field in ("type", "size", "sha256", "executable"))
        if not same:
            problems.append(f"内容が一致しません: {path}")
    return problems


def resolve_entrypoint(inventory: list, entrypoint_name: str) -> str:
    matches = [
        entry["path"]
        for entry in inventory
        if entry["path"].rsplit("/", 1)[-1] == entrypoint_name and entry["executable"]
    ]
    if len(matches) != 1:
        raise PipelineError(
            f"entrypoint候補 {entrypoint_name!r} の一意な実行可能entryが見つかりません"
            f"(候補数={len(matches)})"
        )
    return matches[0]


def copy_licenses(payload_dir: Path, license_relative_paths: list, licenses_dir: Path) -> list:
    if not license_relative_paths:
        raise PipelineError(
            "lockに実在確認済みのlicense文書が列挙されていません。"
            "ライセンス確認が未完了のためbundleを作成できません(fail-closed)。"
        )
    licenses_dir.mkdir(parents=True, exist_ok=True)
    copied_names = []
    seen_names = set()
    for relative in license_relative_paths:
        components = validate_relative_member_path(relative)
        source = payload_dir.joinpath(*components)
        if not source.is_file():
            raise PipelineError(f"lockが指すlicense文書がarchive内に見つかりません: {relative!r}")
        name = components[-1]
        if name in seen_names:
            raise PipelineError(f"license文書のファイル名が衝突しています: {name!r}")
        seen_names.add(name)
        shutil.copyfile(source, licenses_dir / name)
        copied_names.append(name)
    return copied_names


class _AllowlistRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Rejects any redirect hop outside `ALLOWED_HOSTS`/HTTPS, and never
    forwards the original request's headers to the redirected host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        parsed = urllib.parse.urlsplit(newurl)
        if parsed.scheme != "https" or parsed.hostname not in ALLOWED_HOSTS:
            raise urllib.error.URLError(
                f"承認されていないredirect先を拒否しました: {redact_url(newurl)}"
            )
        return urllib.request.Request(newurl, method=req.get_method(), headers={"User-Agent": USER_AGENT})


def download_archive(url: str, expected_size: int, expected_sha256: str, dest_path: Path) -> None:
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https" or parsed.hostname not in ALLOWED_HOSTS:
        raise PipelineError(f"承認されていないURLを拒否しました: {redact_url(url)}")

    opener = urllib.request.build_opener(_AllowlistRedirectHandler)
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    hasher = hashlib.sha256()
    total = 0
    try:
        with opener.open(request, timeout=300) as response, open(dest_path, "wb") as out:
            while True:
                chunk = response.read(CHUNK_SIZE)
                if not chunk:
                    break
                total += len(chunk)
                if total > expected_size:
                    raise PipelineError(f"archiveの受信sizeが期待値を超えました: {redact_url(url)}")
                hasher.update(chunk)
                out.write(chunk)
    except urllib.error.URLError as error:
        raise PipelineError(f"archiveの取得に失敗しました: {redact_url(url)} ({error})") from error

    if total != expected_size:
        raise PipelineError(
            f"archiveのsizeが一致しません: {redact_url(url)} expected={expected_size} actual={total}"
        )
    digest = hasher.hexdigest()
    if digest.lower() != expected_sha256.strip().lower():
        raise PipelineError(f"archiveのSHA-256が一致しません: {redact_url(url)}")


def verify_local_archive(path: Path, entry: dict) -> None:
    """Verifies an already-local archive file (used only by tests, via
    `archive_override`) against the lock's size/SHA-256, the same way
    `download_archive` verifies a freshly downloaded one."""
    actual_size = path.stat().st_size
    if actual_size != entry["archive_size"]:
        raise PipelineError("archiveのsizeがlockと一致しません")
    hasher = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK_SIZE), b""):
            hasher.update(chunk)
    if hasher.hexdigest().lower() != entry["archive_sha256"].strip().lower():
        raise PipelineError("archiveのSHA-256がlockと一致しません")


def build_bundle(
    *,
    target: str,
    lock_path: Path,
    output_dir: Path,
    archive_override: Optional[Path] = None,
) -> Path:
    """Runs the full pipeline for `target` and atomically replaces
    `output_dir` with the resulting bundle. Raises `PipelineError` (and
    leaves `output_dir` untouched) on any failure.

    `archive_override`, when set, skips the network download and verifies a
    local archive file instead; it exists only so tests can exercise the
    full extraction/inventory/license/manifest pipeline against a small
    local fixture archive without any network access.
    """
    lock = load_lock(lock_path)
    entry = select_target_entry(lock, target)
    inventory_path = (lock_path.parent / entry["inventory"]).resolve()
    pinned_inventory = load_pinned_inventory(inventory_path)

    output_dir = output_dir.resolve()
    parent = output_dir.parent
    parent.mkdir(parents=True, exist_ok=True)

    build_tmp = Path(tempfile.mkdtemp(prefix=".prepare-codex-runtime-build-", dir=parent))
    downloaded_tmp: Optional[Path] = None
    try:
        if archive_override is not None:
            archive_path = archive_override
            verify_local_archive(archive_path, entry)
        else:
            fd, raw_name = tempfile.mkstemp(prefix=".codex-archive-", dir=parent)
            os.close(fd)
            downloaded_tmp = Path(raw_name)
            download_archive(entry["url"], entry["archive_size"], entry["archive_sha256"], downloaded_tmp)
            archive_path = downloaded_tmp

        payload_dir = build_tmp / PAYLOAD_DIR_NAME
        payload_dir.mkdir()
        inventory = extract_archive(archive_path, payload_dir)

        problems = diff_inventory(pinned_inventory, inventory)
        if problems:
            raise PipelineError("archiveの内容がpinned inventoryと一致しません: " + "; ".join(problems))

        entrypoint = resolve_entrypoint(inventory, entry["entrypoint_name"])

        licenses_dir = build_tmp / LICENSES_DIR_NAME
        license_names = copy_licenses(payload_dir, entry["licenses"], licenses_dir)

        manifest = {
            "schema_version": MANIFEST_SCHEMA_VERSION,
            "bundle_id": f"codex-app-server-{target}-{lock['codex_version']}",
            "target": target,
            "codex_version": lock["codex_version"],
            "entrypoint": entrypoint,
            "source_commit": lock["source_commit"],
            "archive_sha256": entry["archive_sha256"],
            "files": inventory,
            "licenses": license_names,
            "runtime_profile": entry["runtime_profile"],
            "config_profile": entry["config_profile"],
            "state_profile": entry["state_profile"],
        }
        (build_tmp / MANIFEST_FILE_NAME).write_text(
            json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )

        if output_dir.exists():
            shutil.rmtree(output_dir)
        os.replace(build_tmp, output_dir)
        return output_dir
    finally:
        if downloaded_tmp is not None:
            downloaded_tmp.unlink(missing_ok=True)
        if build_tmp.exists():
            shutil.rmtree(build_tmp, ignore_errors=True)


def main(argv: Optional[list] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--target", required=True, help="e.g. x86_64-pc-windows-msvc or aarch64-apple-darwin")
    parser.add_argument("--output", required=True, type=Path, help="bundle root to atomically (re)create")
    parser.add_argument("--lock", type=Path, default=DEFAULT_LOCK_PATH)
    args = parser.parse_args(argv)

    try:
        build_bundle(target=args.target, lock_path=args.lock, output_dir=args.output)
    except PipelineError as error:
        print(f"prepare-codex-runtime: {error}", file=sys.stderr)
        return 1
    print(f"prepare-codex-runtime: bundle written to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
