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


def normalize_path(path: str) -> str:
    if path.startswith("./"):
        return path[2:]
    return path


def requires_rust_ci(path: str) -> bool:
    path = normalize_path(path)
    return path in RUST_CI_PATHS or path.startswith(RUST_CI_PREFIXES)


def changed_paths_require_rust_ci(paths: Iterable[str]) -> bool:
    return any(requires_rust_ci(path) for path in paths)


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
    args = parser.parse_args()

    paths = parse_paths(sys.stdin.buffer.read(), nul_delimited=args.null)
    matches = [path for path in paths if requires_rust_ci(path)]

    if matches:
        for path in matches:
            print(f"Rust CI input changed: {path}", file=sys.stderr)
        print("true")
    else:
        print("No Rust CI inputs changed.", file=sys.stderr)
        print("false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
