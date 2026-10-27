#!/usr/bin/env python3

import argparse
import os
import sys
from collections.abc import Iterable

RUST_CI_PREFIXES = (
    ".cargo/",
    "assets/icons/work-folder/",
    "crates/",
    "vendor/",
)

RUST_CI_PATHS = {
    ".clippy.toml",
    "assets/app-icon.ico",
    "Cargo.lock",
    "Cargo.toml",
    "clippy.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".github/scripts/rust_ci_path_filter.py",
    ".github/tests/test_rust_ci_path_filter.py",
    ".github/workflows/ci.yml",
}

WINDOWS_GUI_LOCK_CI_PATHS = {
    "scripts/windows_gui_lock_launcher.py",
    "scripts/tests/test_windows_gui_lock_launcher.py",
}

MACOS_GUI_VALIDATOR_LOCK_CI_PATHS = {
    "scripts/gui_validate.py",
    "scripts/tests/test_gui_validate.py",
}


def normalize_path(path: str) -> str:
    if path.startswith("./"):
        return path[2:]
    return path


def requires_rust_ci(path: str) -> bool:
    path = normalize_path(path)
    return path in RUST_CI_PATHS or path.startswith(RUST_CI_PREFIXES)


def changed_paths_require_rust_ci(paths: Iterable[str]) -> bool:
    return any(requires_rust_ci(path) for path in paths)


def requires_windows_gui_lock_test(path: str) -> bool:
    return normalize_path(path) in WINDOWS_GUI_LOCK_CI_PATHS


def changed_paths_require_windows_gui_lock_test(paths: Iterable[str]) -> bool:
    return any(requires_windows_gui_lock_test(path) for path in paths)


def requires_macos_gui_validator_lock_test(path: str) -> bool:
    return normalize_path(path) in MACOS_GUI_VALIDATOR_LOCK_CI_PATHS


def changed_paths_require_macos_gui_validator_lock_test(paths: Iterable[str]) -> bool:
    return any(requires_macos_gui_validator_lock_test(path) for path in paths)


def parse_paths(data: bytes, *, nul_delimited: bool) -> list[str]:
    separator = b"\0" if nul_delimited else b"\n"
    return [
        os.fsdecode(item)
        for item in data.split(separator)
        if item
    ]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Report whether changed repository paths require the Rust CI jobs."
    )
    parser.add_argument(
        "--null",
        action="store_true",
        help="Read NUL-delimited paths, as emitted by git diff --name-only -z.",
    )
    selector = parser.add_mutually_exclusive_group()
    selector.add_argument(
        "--windows-gui-lock",
        action="store_true",
        help="Select the Windows GUI lock lifecycle test paths instead of Rust CI paths.",
    )
    selector.add_argument(
        "--macos-gui-validator-lock",
        action="store_true",
        help="Select the macOS GUI validator lock test paths instead of Rust CI paths.",
    )
    args = parser.parse_args()

    paths = parse_paths(sys.stdin.buffer.read(), nul_delimited=args.null)
    if args.windows_gui_lock:
        requires_ci = requires_windows_gui_lock_test
        label = "Windows GUI lock test input"
    elif args.macos_gui_validator_lock:
        requires_ci = requires_macos_gui_validator_lock_test
        label = "macOS GUI validator lock test input"
    else:
        requires_ci = requires_rust_ci
        label = "Rust CI input"
    matches = [path for path in paths if requires_ci(path)]

    if matches:
        for path in matches:
            print(f"{label} changed: {path}", file=sys.stderr)
        print("true")
    else:
        print(f"No {label.lower()}s changed.", file=sys.stderr)
        print("false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
