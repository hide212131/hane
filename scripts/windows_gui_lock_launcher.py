#!/usr/bin/env python3
"""Acquire the canonical GUI validation lock on Windows, then launch a candidate.

Usage:
    python scripts/windows_gui_lock_launcher.py -- <candidate-executable> [args...]

This is only a lock-and-launch boundary. It does not build Hane, create
fixtures/state, inspect windows, capture screenshots or send input; the
Windows GUI task does those directly. See docs/gui-test-recovery.md.

The lock is the same canonical path macOS ``scripts/gui_validate.py`` uses
(``<profile>/.cache/hane/gui-validation.lock``). On Windows it is held as an
exclusive-share-mode open file object (``dwShareMode=0``), acquired without
waiting. Only that handle is passed to the candidate (``PROC_THREAD_ATTRIBUTE_HANDLE_LIST``),
so the lock outlives the launcher while the candidate is alive and is released
by the OS when the candidate exits.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Optional, Sequence

EXIT_BLOCKED = 2
EXIT_USAGE = 3

LOCK_RELATIVE_PATH = Path(".cache") / "hane" / "gui-validation.lock"

ERROR_SHARING_VIOLATION = 32
ERROR_LOCK_VIOLATION = 33


class LockBusy(Exception):
    """Another task holds the canonical lock."""


class LockError(Exception):
    """The lock could not be resolved or acquired for a reason other than contention."""


def _log(message: str) -> None:
    print(f"[windows-gui-lock] {message}", file=sys.stderr, flush=True)


def _profile_directory() -> Path:
    """Resolve the account profile through the shell known-folder API.

    Deliberately ignores USERPROFILE/HOME/TEMP environment variables.
    """
    if sys.platform != "win32":
        raise LockError("このランチャーはWindows専用")
    import ctypes
    from ctypes import wintypes

    class GUID(ctypes.Structure):
        _fields_ = [
            ("Data1", wintypes.DWORD),
            ("Data2", wintypes.WORD),
            ("Data3", wintypes.WORD),
            ("Data4", ctypes.c_ubyte * 8),
        ]

    # FOLDERID_Profile {5E6C858F-0E22-4760-9AFE-EA3317B67173}
    folder_id = GUID(
        0x5E6C858F, 0x0E22, 0x4760,
        (ctypes.c_ubyte * 8)(0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73),
    )
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
    ole32 = ctypes.WinDLL("ole32", use_last_error=True)
    shell32.SHGetKnownFolderPath.argtypes = [
        ctypes.POINTER(GUID), wintypes.DWORD, wintypes.HANDLE, ctypes.POINTER(ctypes.c_void_p),
    ]
    shell32.SHGetKnownFolderPath.restype = ctypes.c_long
    ole32.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    ole32.CoTaskMemFree.restype = None

    out = ctypes.c_void_p()
    hr = shell32.SHGetKnownFolderPath(ctypes.byref(folder_id), 0, None, ctypes.byref(out))
    try:
        if hr != 0 or not out.value:
            raise LockError(f"ユーザープロファイルを取得できない (HRESULT={hr & 0xFFFFFFFF:#x})")
        profile = ctypes.wstring_at(out.value)
    finally:
        if out.value:
            ole32.CoTaskMemFree(out)
    if not profile or not os.path.isabs(profile) or not os.path.isdir(profile):
        raise LockError(f"ユーザープロファイルが存在しない: {profile!r}")
    return Path(profile)


def canonical_lock_path() -> Path:
    """Return the canonical lock path, creating only ``.cache/hane`` under the profile."""
    path = _profile_directory() / LOCK_RELATIVE_PATH
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise LockError(f"ロックの親ディレクトリを作成できない: {exc}") from exc
    return path


def _kernel32():
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateFileW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
        wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
    ]
    k32.CreateFileW.restype = wintypes.HANDLE
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.CloseHandle.restype = wintypes.BOOL
    return k32


def acquire_exclusive(lock_path: Path) -> int:
    """Open ``lock_path`` with no sharing, without waiting. Returns a non-inheritable handle.

    The parent directory must already exist; it is never created here.
    """
    if sys.platform != "win32":
        raise LockError("このランチャーはWindows専用")
    import ctypes

    GENERIC_READ = 0x80000000
    GENERIC_WRITE = 0x40000000
    OPEN_ALWAYS = 4
    FILE_ATTRIBUTE_NORMAL = 0x80
    FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000
    invalid = ctypes.c_void_p(-1).value

    k32 = _kernel32()
    # lpSecurityAttributes=NULL: the handle starts non-inheritable.
    handle = k32.CreateFileW(
        str(lock_path), GENERIC_READ | GENERIC_WRITE, 0, None, OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, None,
    )
    if handle is None or handle == invalid:
        err = ctypes.get_last_error()
        if err in (ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION):
            raise LockBusy(f"別の実行が使用中: {lock_path}")
        raise LockError(f"実行ロックを取得できない (Win32 error {err}): {lock_path}")
    return int(handle)


def _close_handle(handle: int) -> None:
    if sys.platform != "win32":
        return
    try:
        _kernel32().CloseHandle(handle)
    except Exception:  # noqa: BLE001 - nothing useful to do while cleaning up
        pass


def launch_with_lock(lock_path: Path, command: Sequence[str]) -> int:
    """Acquire ``lock_path`` then run ``command`` with only that handle inherited.

    Returns the child's exit status, or EXIT_BLOCKED without launching anything
    when the lock cannot be acquired.
    """
    command = list(command)
    if not command:
        _log("起動する候補コマンドが指定されていない")
        return EXIT_USAGE
    _log(f"lock={lock_path} launcher_pid={os.getpid()} command={command!r}")
    try:
        handle = acquire_exclusive(lock_path)
    except LockBusy as exc:
        _log(f"blocked: {exc}。候補は起動しない")
        return EXIT_BLOCKED
    except (LockError, OSError) as exc:
        _log(f"blocked: {exc}。候補は起動しない")
        return EXIT_BLOCKED
    _log("acquired: canonical lock")

    child: Optional[subprocess.Popen] = None
    try:
        try:
            os.set_handle_inheritable(handle, True)
            startupinfo = subprocess.STARTUPINFO(lpAttributeList={"handle_list": [handle]})
            # shell=False (argv list) and close_fds=True: only handle_list is inherited.
            child = subprocess.Popen(command, close_fds=True, startupinfo=startupinfo)
        except (OSError, ValueError) as exc:
            _log(f"blocked: 候補を起動できない: {exc}")
            return EXIT_BLOCKED
        finally:
            try:
                os.set_handle_inheritable(handle, False)
            except OSError:
                pass
        _log(f"launched: child_pid={child.pid}")
        while True:
            try:
                status = child.wait()
                break
            except KeyboardInterrupt:
                # The child shares the console and sees the interrupt itself; keep the lock path alive.
                continue
        _log(f"child exited: status={status}")
        return status
    finally:
        _close_handle(handle)
        _log("released: launcher handle closed")


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] != "--" or len(args) < 2:
        print(
            "usage: windows_gui_lock_launcher.py -- <candidate-executable> [args...]",
            file=sys.stderr,
        )
        return EXIT_USAGE
    command = args[1:]
    try:
        lock_path = canonical_lock_path()
    except (LockError, OSError) as exc:
        _log(f"blocked: {exc}。候補は起動しない")
        return EXIT_BLOCKED
    return launch_with_lock(lock_path, command)


if __name__ == "__main__":
    sys.exit(main())
