#!/usr/bin/env python3
"""Prepare and validate version-only Hane release pull requests."""

from __future__ import annotations

import argparse
import copy
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
VERSION_PATTERN = re.compile(r"^\d+\.\d+\.\d+$")
PACKAGE_TABLE_PATTERN = re.compile(r"(?m)^\[\[package\]\]\s*$")


def command_output(args: list[str], *, cwd: Path = ROOT) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout


def parse_version(value: str) -> tuple[int, int, int]:
    if not VERSION_PATTERN.fullmatch(value):
        raise ValueError(f"不正なバージョンです: {value}")
    return tuple(int(part) for part in value.split("."))  # type: ignore[return-value]


def manifest_version(text: str) -> str:
    document = tomllib.loads(text)
    return str(document["workspace"]["package"]["version"])


def workspace_packages(root: Path = ROOT) -> dict[str, dict[str, Any]]:
    metadata = json.loads(
        command_output(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            cwd=root,
        )
    )
    member_ids = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in member_ids
    }
    if not packages or len(packages) != len(member_ids):
        raise ValueError("workspace package 名が重複しているか、一覧を取得できません")
    return packages


def replace_workspace_version(text: str, current: str, target: str) -> str:
    document = tomllib.loads(text)
    if manifest_version(text) != current:
        raise ValueError("Cargo.toml の現在版が想定と一致しません")

    section = re.search(
        r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", text
    )
    if section is None:
        raise ValueError("Cargo.toml に [workspace.package] がありません")
    updated, count = re.subn(
        rf'(?m)^([ \t]*version[ \t]*=[ \t]*)"{re.escape(current)}"([ \t]*\r?)$',
        lambda match: f'{match.group(1)}"{target}"{match.group(2)}',
        section.group(1),
    )
    if count != 1:
        raise ValueError("[workspace.package].version を一意に更新できません")
    result = text[: section.start(1)] + updated + text[section.end(1) :]
    if manifest_version(result) != target or document["workspace"]["package"]["version"] != current:
        raise ValueError("Cargo.toml の版更新を確認できません")
    return result


def update_lock_versions(text: str, package_names: set[str], current: str, target: str) -> str:
    document = tomllib.loads(text)
    found: set[str] = set()
    matches = list(PACKAGE_TABLE_PATTERN.finditer(text))
    if not matches:
        raise ValueError("Cargo.lock に package 一覧がありません")

    output: list[str] = []
    cursor = 0
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        block = text[match.start() : end]
        table = tomllib.loads(block)["package"]
        if len(table) != 1:
            raise ValueError("Cargo.lock の package table を一意に読めません")
        package = table[0]
        if package.get("source") is None and package["name"] in package_names:
            name = package["name"]
            if name in found:
                raise ValueError(f"Cargo.lock に workspace package が重複しています: {name}")
            if package["version"] != current:
                raise ValueError(f"Cargo.lock の {name} が現在版と一致しません")
            block, count = re.subn(
                rf'(?m)^version = "{re.escape(current)}"$',
                f'version = "{target}"',
                block,
                count=1,
            )
            if count != 1:
                raise ValueError(f"Cargo.lock の {name} の版を更新できません")
            found.add(name)
        output.append(text[cursor : match.start()])
        output.append(block)
        cursor = end
    output.append(text[cursor:])
    if found != package_names:
        missing = sorted(package_names - found)
        extra = sorted(found - package_names)
        raise ValueError(f"Cargo.lock の workspace package が不一致です: missing={missing}, extra={extra}")
    result = "".join(output)
    updated_document = tomllib.loads(result)
    if document.keys() != updated_document.keys():
        raise ValueError("Cargo.lock の構造が予期せず変わりました")
    return result


def prepare(root: Path, target: str) -> None:
    parse_version(target)
    manifest_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    manifest_text = manifest_path.read_text(encoding="utf-8")
    lock_text = lock_path.read_text(encoding="utf-8")
    current = manifest_version(manifest_text)
    if parse_version(target) <= parse_version(current):
        raise ValueError(f"更新先 {target} は現在版 {current} より新しくありません")

    packages = workspace_packages(root)
    if {package["version"] for package in packages.values()} != {current}:
        raise ValueError("workspace package の版が Cargo.toml と一致しません")
    names = set(packages)
    updated_manifest = replace_workspace_version(manifest_text, current, target)
    updated_lock = update_lock_versions(lock_text, names, current, target)
    manifest_path.write_text(updated_manifest, encoding="utf-8")
    lock_path.write_text(updated_lock, encoding="utf-8")

    updated_packages = workspace_packages(root)
    if {package["version"] for package in updated_packages.values()} != {target}:
        raise ValueError("更新後の workspace package の版が一致しません")
    lock_document = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    local_names = {
        package["name"]
        for package in lock_document["package"]
        if package.get("source") is None and package["name"] in names
    }
    if local_names != names:
        raise ValueError("更新後の Cargo.lock に workspace package がすべてありません")


def canonical(items: list[dict[str, Any]]) -> list[str]:
    return sorted(json.dumps(item, sort_keys=True, separators=(",", ":")) for item in items)


def validate_release_diff(
    changed_paths: set[str],
    base_manifest: str,
    head_manifest: str,
    base_lock: str,
    head_lock: str,
    package_names: set[str],
) -> tuple[bool, str, str | None]:
    if changed_paths != {"Cargo.toml", "Cargo.lock"}:
        return False, f"変更ファイルが版更新だけではありません: {sorted(changed_paths)}", None

    old_manifest = tomllib.loads(base_manifest)
    new_manifest = tomllib.loads(head_manifest)
    old_version = str(old_manifest["workspace"]["package"]["version"])
    new_version = str(new_manifest["workspace"]["package"]["version"])
    old_without_version = copy.deepcopy(old_manifest)
    new_without_version = copy.deepcopy(new_manifest)
    old_without_version["workspace"]["package"].pop("version", None)
    new_without_version["workspace"]["package"].pop("version", None)
    if old_without_version != new_without_version:
        return False, "Cargo.toml に版番号以外の変更があります", None
    try:
        if parse_version(new_version) <= parse_version(old_version):
            return False, "workspace version が増加していません", None
    except ValueError as error:
        return False, str(error), None
    try:
        expected_manifest = replace_workspace_version(base_manifest, old_version, new_version)
    except ValueError as error:
        return False, str(error), None
    if expected_manifest != head_manifest:
        return False, "Cargo.toml にworkspace version以外のテキスト変更があります", None

    try:
        old_lock = tomllib.loads(base_lock)
        new_lock = tomllib.loads(head_lock)
    except tomllib.TOMLDecodeError as error:
        return False, f"Cargo.lock を読めません: {error}", None

    old_top = {key: value for key, value in old_lock.items() if key != "package"}
    new_top = {key: value for key, value in new_lock.items() if key != "package"}
    if old_top != new_top:
        return False, "Cargo.lock の package 以外の内容が変わっています", None

    def split_packages(lock: dict[str, Any]) -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
        local: dict[str, dict[str, Any]] = {}
        external: list[dict[str, Any]] = []
        for package in lock.get("package", []):
            if package.get("source") is None and package["name"] in package_names:
                if package["name"] in local:
                    raise ValueError(f"Cargo.lock に workspace package が重複しています: {package['name']}")
                local[package["name"]] = package
            else:
                external.append(package)
        if set(local) != package_names:
            raise ValueError("Cargo.lock に current workspace package がそろっていません")
        return local, external

    try:
        old_local, old_external = split_packages(old_lock)
        new_local, new_external = split_packages(new_lock)
    except (KeyError, ValueError) as error:
        return False, str(error), None
    if canonical(old_external) != canonical(new_external):
        return False, "外部 dependency、checksum、または dependency 一覧が変わっています", None

    for name in sorted(package_names):
        old_package = copy.deepcopy(old_local[name])
        new_package = copy.deepcopy(new_local[name])
        if old_package.pop("version", None) != old_version:
            return False, f"base 側 Cargo.lock の {name} が旧版と一致しません", None
        if new_package.pop("version", None) != new_version:
            return False, f"head 側 Cargo.lock の {name} が新しい版と一致しません", None
        if old_package != new_package:
            return False, f"Cargo.lock の {name} に版番号以外の変更があります", None
    try:
        expected_lock = update_lock_versions(base_lock, package_names, old_version, new_version)
    except ValueError as error:
        return False, str(error), None
    if expected_lock != head_lock:
        return False, "Cargo.lock に workspace package version 以外のテキスト変更があります", None
    return True, "Cargo.toml と workspace package の版だけが更新されています", new_version


def classify(root: Path, base: str, head: str) -> tuple[bool, str, str | None]:
    try:
        changed = command_output(
            ["git", "diff", "--name-only", "--no-renames", "-z", f"{base}...{head}"],
            cwd=root,
        )
        changed_paths = {path for path in changed.split("\0") if path}
        if changed_paths != {"Cargo.toml", "Cargo.lock"}:
            return False, f"版更新以外の変更ファイルがあります: {sorted(changed_paths)}", None
        base_manifest = command_output(["git", "show", f"{base}:Cargo.toml"], cwd=root)
        head_manifest = command_output(["git", "show", f"{head}:Cargo.toml"], cwd=root)
        base_lock = command_output(["git", "show", f"{base}:Cargo.lock"], cwd=root)
        head_lock = command_output(["git", "show", f"{head}:Cargo.lock"], cwd=root)
        packages = workspace_packages(root)
        if {package["version"] for package in packages.values()} != {manifest_version(base_manifest)}:
            return False, "base の Cargo.toml と workspace package の版が一致しません", None
        return validate_release_diff(
            changed_paths,
            base_manifest,
            head_manifest,
            base_lock,
            head_lock,
            set(packages),
        )
    except (OSError, subprocess.CalledProcessError, KeyError, ValueError, tomllib.TOMLDecodeError) as error:
        return False, f"release-only 判定ができません: {error}", None


def validate_release_branch(head_ref: str, release_version: str) -> None:
    expected = f"release/v{release_version}"
    if head_ref != expected:
        raise ValueError(
            f"release branch {head_ref!r} does not match requested version {expected!r}"
        )


def validate(root: Path, base: str, head: str, latest_tag: str, head_ref: str) -> str:
    is_release, reason, version = classify(root, base, head)
    if not is_release or version is None:
        raise ValueError(reason)
    # The branch name binds the validated Cargo diff to workflow_dispatch's requested
    # version. It is only a consistency check; release-only classification still uses
    # the exact base-to-head file contents above.
    validate_release_branch(head_ref, version)
    if latest_tag:
        if not latest_tag.startswith("v"):
            raise ValueError(f"最新 tag の形式が不正です: {latest_tag}")
        if parse_version(version) <= parse_version(latest_tag[1:]):
            raise ValueError(f"更新先 v{version} は最新 tag {latest_tag} より新しくありません")
    return version


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--version", required=True)

    classify_parser = subparsers.add_parser("classify")
    classify_parser.add_argument("--base", required=True)
    classify_parser.add_argument("--head", required=True)
    classify_parser.add_argument("--github-output")

    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("--base", required=True)
    validate_parser.add_argument("--head", required=True)
    validate_parser.add_argument("--head-ref", required=True)
    validate_parser.add_argument("--latest-tag", default="")
    validate_parser.add_argument("--github-output")

    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(ROOT, args.version)
            print(f"workspace package を {args.version} に更新しました")
            return 0
        if args.command == "classify":
            is_release, reason, _ = classify(ROOT, args.base, args.head)
            print(f"release_only={str(is_release).lower()}")
            print(reason, file=sys.stderr)
            if args.github_output:
                with open(args.github_output, "a", encoding="utf-8") as output:
                    output.write(f"release_only={str(is_release).lower()}\n")
            return 0
        version = validate(ROOT, args.base, args.head, args.latest_tag, args.head_ref)
        print(f"release v{version} の軽量チェックに合格しました")
        if args.github_output:
            with open(args.github_output, "a", encoding="utf-8") as output:
                output.write(f"release_version=v{version}\n")
        return 0
    except (OSError, subprocess.CalledProcessError, KeyError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"release PR check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
