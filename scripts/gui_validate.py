#!/usr/bin/env python3
"""Launch, capture and tear down Hane for the local GUI validation path.

See docs/local-gui-validation.md (design) and docs/local-gui-validate-cli.md
(operation: usage, env vars, result schema, exit codes) for the contract this
implements. This command only confirms the launch -> ready -> window ->
screenshot -> teardown path; it is not the comprehensive GUI validation from
stage 4 of that design.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

ABORT_SIGNALS = tuple(getattr(signal, name) for name in ("SIGINT", "SIGTERM", "SIGHUP") if hasattr(signal, name))

SCHEMA_VERSION = 1
PROCEDURE_VERSION = "gui-validate/1"
VERIFICATION_KIND = "launch_and_capture_path"
SCOPE_NOTE = (
    "この結果は起動から撮影までの経路確認であり、"
    "描画内容・入力・保存を含む包括的な GUI 検証の合格を意味しない。"
)

EXIT_PASS = 0
EXIT_FAIL = 1
EXIT_BLOCKED = 2
EXIT_USAGE = 3

SCENARIOS = ("editor", "cursor-boundary", "cursor-scroll")
RESULT_PRIORITY = {"fail": 0, "blocked": 1, "pass": 2}


class Aborted(Exception):
    """Raised from a signal handler so an interrupted run still tears down."""


class EnvError(Exception):
    pass


class BuildError(EnvError):
    pass


@contextmanager
def defer_aborts(*, raise_after: bool = True, on_abort=None):
    """Finish a small ownership transaction before delivering an abort.

    Defer Python handlers rather than masking OS signals: a newly spawned Hane
    must not inherit a blocked SIGTERM mask from its parent.
    """
    previous = {}
    received = []
    try:
        if threading.current_thread() is threading.main_thread():
            for sig in ABORT_SIGNALS:
                previous[sig] = signal.signal(sig, lambda signum, _frame: received.append(signum))
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        if received and on_abort is not None:
            on_abort(received)
        if received and raise_after:
            raise Aborted(f"signal {received[0]} (ownership transaction completed)")


def execution_lock_path() -> Path:
    import pwd
    # Account database, not HOME/TMPDIR or a process-specific temp directory.
    return Path(pwd.getpwuid(os.getuid()).pw_dir) / ".cache" / "hane" / "gui-validation.lock"


def make_step(name: str, result: str, reason: Optional[str] = None, **detail) -> dict:
    return {"name": name, "result": result, "reason": reason, **detail}


def skipped_step(name: str, reason: str) -> dict:
    return {"name": name, "result": "skipped", "reason": reason}


@dataclass
class Config:
    workspace_dir: Path
    scenario: str
    expected_sha: Optional[str]
    request_id: str
    generation: str
    run_dir: Path
    state_dir: Path
    log_path: Path
    image_path: Path
    fixture_path: Optional[Path]
    features: list[str]
    extra_env: dict[str, str]
    startup_timeout_seconds: float = 15.0
    window_timeout_seconds: float = 5.0
    poll_interval_seconds: float = 0.1
    window_id_cmd: Optional[list[str]] = None
    capture_cmd: Optional[list[str]] = None
    capture_timeout_seconds: float = 15.0


class Clock:
    def now_iso(self) -> str:
        return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

    def monotonic(self) -> float:
        return time.monotonic()

    def sleep(self, seconds: float) -> None:
        time.sleep(seconds)


class Environment:
    """Everything run_validation needs from the outside world.

    A real implementation shells out to git/cargo/swift/screencapture; tests
    substitute a fake so the state machine runs with no build and no screen.
    """

    clock: Clock

    def acquire_execution(self) -> None:
        """Reserve this user's GUI display for the entire validation."""

    def release_execution(self) -> None:
        """Release the GUI display after cleanup."""

    def cleanup_build(self) -> None:
        """Remove owned build files while retaining execution ownership."""

    def prepare(self, config: Config) -> None:
        """Create owned run artifacts only after the clean-checkout preflight."""

    def git_head(self, workspace_dir: Path) -> str:
        raise NotImplementedError

    def git_dirty_paths(self, workspace_dir: Path) -> list[str]:
        raise NotImplementedError

    def missing_tools(self, config: Config) -> list[str]:
        raise NotImplementedError

    def build(self, workspace_dir: Path, features: list[str], source_sha: Optional[str] = None) -> tuple[Path, dict]:
        raise NotImplementedError

    def launch(self, binary_path: Path, config: Config):
        raise NotImplementedError

    def log_contains_ready(self, log_path: Path) -> bool:
        raise NotImplementedError

    def find_window_once(self, pid: int, config: Config, timeout_seconds: float) -> Optional[str]:
        raise NotImplementedError

    def capture(self, window_id: str, image_path: Path, config: Config) -> bool:
        raise NotImplementedError


class RealEnvironment(Environment):
    def __init__(self) -> None:
        self.clock = Clock()
        self._reserved_run_dir: Optional[Path] = None
        self._execution_fd: Optional[int] = None
        self._deferred_aborts: list[int] = []
        self._build_snapshot = None

    def record_aborts(self, signals) -> None:
        self._deferred_aborts.extend(signals)

    def acquire_execution(self) -> None:
        if self._execution_fd is not None:
            return
        try:
            import fcntl
            with defer_aborts():
                lock = execution_lock_path()
                lock.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                fd = os.open(lock, os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BaseException:
                    os.close(fd)
                    raise
                self._execution_fd = fd
                self._execution_acquired = True
        except (ImportError, OSError) as exc:
            raise EnvError(f"GUI validator は別の実行が使用中、または実行ロックを取得できない: {exc}") from exc

    def cleanup_build(self) -> None:
        if self._build_snapshot is not None:
            try:
                self._build_snapshot.cleanup()
                self._build_snapshot = None
            except OSError as exc:
                self._snapshot_cleanup_error = str(exc)

    def release_execution(self) -> None:
        try:
            self.cleanup_build()
        finally:
            if self._execution_fd is not None:
                os.close(self._execution_fd)  # Kernel also releases the lock on process exit.
                self._execution_fd = None

    def reserve(self, config: Config) -> None:
        if self._reserved_run_dir == config.run_dir:
            return
        try:
            finalizing = getattr(self, "_finalizing", False)
            with defer_aborts(raise_after=not finalizing,
                              on_abort=self.record_aborts if finalizing else None):
                config.run_dir.mkdir(parents=True, exist_ok=True)
                marker = config.run_dir / ".gui-validate-owner"
                fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(fd, "w") as owner:
                    if any(path != marker for path in config.run_dir.iterdir()):
                        marker.unlink()  # Remove only the marker this call just created.
                        raise EnvError("実行用ディレクトリに既存ファイルがあるため予約できない")
                    self._reserved_run_dir = config.run_dir
                    owner.write(json.dumps({"pid": os.getpid(), "request_id": config.request_id,
                                            "generation": config.generation}))
        except OSError as exc:
            raise EnvError(f"実行用ディレクトリを排他的に予約できない: {exc}") from exc

    def prepare(self, config: Config) -> None:
        try:
            self.reserve(config)
            config.state_dir.mkdir(parents=True, exist_ok=True)
            # Called after do_build and immediately before do_launch: re-read
            # external fixtures too, since they may have changed during Cargo.
            _scenario_setup(config.scenario, config.run_dir)
        except (OSError, ValueError) as exc:
            raise EnvError(f"検証用ファイルを準備できない: {exc}") from exc

    def git_head(self, workspace_dir: Path) -> str:
        try:
            out = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=workspace_dir,
                capture_output=True,
                text=True,
                check=True,
            )
        except (OSError, subprocess.CalledProcessError) as exc:
            raise EnvError(str(exc)) from exc
        return out.stdout.strip()

    def git_dirty_paths(self, workspace_dir: Path) -> list[str]:
        try:
            out = subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=workspace_dir,
                capture_output=True,
                text=True,
                check=True,
            )
        except (OSError, subprocess.CalledProcessError) as exc:
            raise EnvError(str(exc)) from exc
        return [line for line in out.stdout.splitlines() if line.strip()]

    def missing_tools(self, config: Config) -> list[str]:
        missing = []
        if shutil.which("cargo") is None:
            missing.append("cargo")
        if config.window_id_cmd is None and shutil.which("swift") is None:
            missing.append("swift")
        if config.capture_cmd is None and shutil.which("screencapture") is None:
            missing.append("screencapture")
        if shutil.which("sips") is None:
            missing.append("sips")
        return missing

    def snapshot_checkout(self, workspace_dir: Path, source_sha: str) -> Path:
        # Clone immutable Git objects into a private repository; editing or
        # restoring files in the user's working copy cannot change build inputs.
        # No linked worktree registration or hard-linked object files are used.
        workspace_root = workspace_dir.resolve()
        temp_root = Path(tempfile.gettempdir()).resolve()
        if temp_root == workspace_root or workspace_root in temp_root.parents:
            temp_root = workspace_root.parent
        if temp_root == workspace_root:
            raise EnvError("checkout外にビルド用の一時領域を確保できない")
        with defer_aborts():
            self._build_snapshot = tempfile.TemporaryDirectory(prefix="hane-gui-build-", dir=temp_root)
        snapshot = Path(self._build_snapshot.name) / "source"
        try:
            subprocess.run(["git", "clone", "--quiet", "--no-hardlinks", "--no-checkout",
                            "--", str(workspace_dir), str(snapshot)],
                           capture_output=True, text=True, check=True)
            subprocess.run(["git", "checkout", "--quiet", "--detach", source_sha], cwd=snapshot,
                           capture_output=True, text=True, check=True)
            if self.git_head(snapshot) != source_sha or self.git_dirty_paths(snapshot):
                raise EnvError("独立したビルド用checkoutのSHAまたはclean状態が一致しない")
        except (OSError, subprocess.CalledProcessError) as exc:
            raise EnvError(f"ビルド用checkoutを作成できない: {exc}") from exc
        return snapshot

    def build(self, workspace_dir: Path, features: list[str], source_sha: Optional[str] = None) -> tuple[Path, dict]:
        source_sha = source_sha or self.git_head(workspace_dir)
        snapshot = self.snapshot_checkout(workspace_dir, source_sha)
        target_dir = snapshot.parent / "target"
        args = [
            "cargo",
            "build",
            "--locked",
            "--manifest-path",
            str(snapshot / "Cargo.toml"),
            "--target-dir",
            str(target_dir),
            "-p",
            "hane",
            "--bin",
            "hane",
            "--message-format=json-render-diagnostics",
        ]
        if features:
            args += ["--features", ",".join(features)]
        try:
            proc = subprocess.run(args, cwd=snapshot, capture_output=True, text=True)
        except OSError as exc:
            raise BuildError(str(exc)) from exc
        if self.git_head(snapshot) != source_sha or self.git_dirty_paths(snapshot):
            raise EnvError("ビルド用checkoutの入力が変更された")
        if proc.returncode != 0:
            tail = "\n".join(proc.stderr.splitlines()[-40:])
            raise BuildError(f"cargo build exited {proc.returncode}: {tail}")

        binary_path = None
        for line in proc.stdout.splitlines():
            line = line.strip()
            if not line or not line.startswith("{"):
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("reason") != "compiler-artifact":
                continue
            executable = message.get("executable")
            if executable and message.get("target", {}).get("name") == "hane":
                binary_path = Path(executable)
        if binary_path is None:
            raise BuildError("cargo build の出力から実行ファイルのパスを特定できなかった")

        def tool_version(args: list[str]) -> str:
            try:
                out = subprocess.run(args, cwd=snapshot, capture_output=True, text=True, check=True)
                return out.stdout.strip()
            except (OSError, subprocess.CalledProcessError) as exc:
                return f"unknown ({exc})"

        digest = hashlib.sha256(binary_path.read_bytes()).hexdigest()
        build_info = {
            "profile": "debug",
            "features": features,
            "rustc_version": tool_version(["rustc", "--version"]),
            "cargo_version": tool_version(["cargo", "--version"]),
            "binary_path": str(binary_path),
            "binary_sha256": digest,
            "source_snapshot_sha": source_sha,
            "source_snapshot_dir": str(snapshot),
            "source_snapshot_clean": True,
        }
        return binary_path, build_info

    def launch(self, binary_path: Path, config: Config):
        args = [str(binary_path)]
        if config.fixture_path is not None:
            args.append(str(config.fixture_path))
        # Only this scenario's validated Hane settings may affect the child.
        # Ambient measurement flags can change the document/focus or write a
        # metrics file outside this run, even when the scenario overrides pass.
        run_env = {key: value for key, value in os.environ.items() if not key.startswith("HANE_")}
        run_env["HANE_STATE_DIR"] = str(config.state_dir)
        run_env.update(config.extra_env)
        log_file = open(config.log_path, "wb")
        try:
            # The child shares this flock's open-file description. If cleanup
            # fails (or the validator dies), its surviving child keeps the
            # display reserved until that child actually exits.
            inherited_lock = (self._execution_fd,) if self._execution_fd is not None else ()
            process = subprocess.Popen(args, stderr=log_file, stdout=subprocess.DEVNULL,
                                       env=run_env, pass_fds=inherited_lock)
        except OSError as exc:
            raise EnvError(str(exc)) from exc
        finally:
            log_file.close()
        return process

    def log_contains_ready(self, log_path: Path) -> bool:
        try:
            return "hane_ready" in log_path.read_text(errors="replace")
        except OSError:
            return False

    def find_window_once(self, pid: int, config: Config, timeout_seconds: float) -> Optional[str]:
        if config.window_id_cmd is not None:
            args = [*config.window_id_cmd, str(pid)]
        else:
            script = Path(__file__).resolve().parent / "window_id.swift"
            args = ["swift", str(script), str(pid)]
        try:
            out = subprocess.run(args, capture_output=True, text=True, timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            return None
        except OSError:
            return None
        if out.returncode != 0:
            return None
        window_id = out.stdout.strip()
        return window_id or None

    def capture(self, window_id: str, image_path: Path, config: Config) -> bool:
        try:
            image_path.parent.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise EnvError(f"撮影先を準備できない: {exc}") from exc
        if config.capture_cmd is not None:
            args = [*config.capture_cmd, window_id, str(image_path)]
        else:
            # Exclude the macOS window shadow so screenshot coordinates map to
            # kCGWindowBounds used by the hosted OS-input helper.
            args = ["screencapture", "-x", "-o", "-l", window_id, str(image_path)]
        try:
            out = subprocess.run(
                args, capture_output=True, text=True, timeout=config.capture_timeout_seconds
            )
        except subprocess.TimeoutExpired as exc:
            raise EnvError(f"撮影コマンドがタイムアウトした: {exc}") from exc
        except OSError as exc:
            raise EnvError(str(exc)) from exc
        if out.returncode != 0:
            return False
        # Decode the complete image, rather than accepting a file/header left
        # by a custom command that exited successfully without a screenshot.
        try:
            if not image_path.is_file() or image_path.stat().st_size == 0:
                return False
            with tempfile.TemporaryDirectory(prefix='hane-image-check-') as directory:
                decoded = Path(directory) / 'decoded.png'
                checked = subprocess.run(['/usr/bin/sips', '-s', 'format', 'png', str(image_path),
                                          '--out', str(decoded)], capture_output=True, text=True,
                                         timeout=config.capture_timeout_seconds)
                return checked.returncode == 0 and decoded.is_file() and decoded.stat().st_size > 0
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise EnvError(f"撮影画像を検証できない: {exc}") from exc


def do_preflight(env: Environment, config: Config) -> tuple[dict, dict]:
    try:
        actual_sha = env.git_head(config.workspace_dir)
    except EnvError as exc:
        return make_step("preflight", "blocked", reason=f"HEAD の SHA を取得できない: {exc}"), {}

    try:
        dirty_paths = env.git_dirty_paths(config.workspace_dir)
    except EnvError as exc:
        return make_step("preflight", "blocked", reason=f"作業コピーの状態を取得できない: {exc}"), {}

    sha_matches = config.expected_sha is None or actual_sha == config.expected_sha
    target_info = {
        "expected_sha": config.expected_sha,
        "actual_sha": actual_sha,
        "sha_matches": sha_matches,
        "working_copy_clean": not dirty_paths,
        "dirty_paths": dirty_paths,
    }
    if not sha_matches:
        return make_step("preflight", "blocked", reason="要求された SHA と実際の checkout が一致しない", **target_info), target_info
    if dirty_paths:
        return make_step("preflight", "blocked", reason="作業コピーに変更が混入している", **target_info), target_info

    missing_tools = env.missing_tools(config)
    if missing_tools:
        return (
            make_step(
                "preflight",
                "blocked",
                reason=f"必要なツールが見つからない: {', '.join(missing_tools)}",
                missing_tools=missing_tools,
                **target_info,
            ),
            target_info,
        )
    return make_step("preflight", "pass", **target_info), target_info


def do_build(env: Environment, config: Config, baseline_sha: Optional[str] = None) -> tuple[dict, Optional[Path], dict]:
    binary_path, build_info, build_error = None, {}, None
    try:
        initial_sha = env.git_head(config.workspace_dir)
        initial_dirty = env.git_dirty_paths(config.workspace_dir)
        if (initial_dirty or (baseline_sha and initial_sha != baseline_sha)
                or (config.expected_sha and initial_sha != config.expected_sha)):
            return make_step("build", "blocked", reason="ビルド直前に checkout が変更されたため実行しない",
                             actual_sha=initial_sha, dirty_paths=initial_dirty), None, {}
        try:
            binary_path, build_info = env.build(config.workspace_dir, config.features, source_sha=initial_sha)
        except BuildError as exc:
            build_error = exc
        actual_sha = env.git_head(config.workspace_dir)
        dirty_paths = env.git_dirty_paths(config.workspace_dir)
    except EnvError as exc:
        return make_step("build", "blocked", reason=f"ビルド前後の checkout を検証できない: {exc}"), None, {}
    if (actual_sha != initial_sha or (baseline_sha and actual_sha != baseline_sha)
            or (config.expected_sha and actual_sha != config.expected_sha) or dirty_paths):
        return make_step("build", "blocked", reason="ビルド中に checkout が変更されたため起動しない",
                         actual_sha=actual_sha, dirty_paths=dirty_paths), None, build_info
    if build_error is not None:
        return make_step("build", "fail", reason=f"ビルドに失敗した: {build_error}"), None, {}
    build_info.update(validated_sha=actual_sha, working_copy_clean_after_build=True)
    return make_step("build", "pass", **build_info), binary_path, build_info


def do_launch(env: Environment, config: Config, binary_path: Path, process_holder: dict) -> dict:
    try:
        with defer_aborts():
            process = env.launch(binary_path, config)
            process_holder["process"] = process
    except EnvError as exc:
        return make_step("launch", "fail", reason=f"起動に失敗した: {exc}")

    # Recorded before the wait loop so an Aborted raised mid-poll (e.g. a
    # SIGTERM delivered between iterations) still leaves the caller's finally
    # block able to find and stop this exact process.

    deadline = env.clock.monotonic() + config.startup_timeout_seconds
    started = env.clock.monotonic()
    while True:
        exit_code = process.poll()
        if exit_code is not None:
            return make_step(
                "launch",
                "fail",
                reason=f"準備完了 (hane_ready) の前にプロセスが終了した (exit={exit_code})",
                pid=process.pid,
                exit_code=exit_code,
            )
        if env.log_contains_ready(config.log_path):
            elapsed = env.clock.monotonic() - started
            return make_step("launch", "pass", pid=process.pid, elapsed_seconds=round(elapsed, 3))
        if env.clock.monotonic() >= deadline:
            return make_step(
                "launch",
                "blocked",
                reason="起動完了 (hane_ready) を確認できないままタイムアウトした",
                pid=process.pid,
                timeout_seconds=config.startup_timeout_seconds,
            )
        env.clock.sleep(config.poll_interval_seconds)


def do_window_discovery(env: Environment, config: Config, process) -> tuple[dict, Optional[str]]:
    deadline = env.clock.monotonic() + config.window_timeout_seconds
    started = env.clock.monotonic()
    while True:
        exit_code = process.poll()
        if exit_code is not None:
            return (
                make_step(
                    "window_discovery",
                    "fail",
                    reason=f"ウィンドウ確認前にプロセスが終了した (exit={exit_code})",
                    exit_code=exit_code,
                ),
                None,
            )
        remaining = deadline - env.clock.monotonic()
        window_id = env.find_window_once(process.pid, config, remaining) if remaining > 0 else None
        if window_id:
            elapsed = env.clock.monotonic() - started
            return make_step("window_discovery", "pass", window_id=window_id, elapsed_seconds=round(elapsed, 3)), window_id
        if env.clock.monotonic() >= deadline:
            return (
                make_step(
                    "window_discovery",
                    "blocked",
                    reason="対象プロセスのウィンドウを確認できないままタイムアウトした",
                    timeout_seconds=config.window_timeout_seconds,
                ),
                None,
            )
        env.clock.sleep(config.poll_interval_seconds)


def do_capture(env: Environment, config: Config, window_id: str) -> dict:
    try:
        ok = env.capture(window_id, config.image_path, config)
    except EnvError as exc:
        return make_step("capture", "blocked", reason=f"撮影コマンドが失敗した: {exc}")
    if not ok:
        return make_step("capture", "blocked", reason="撮影コマンドが画像を生成しなかった")
    return make_step("capture", "pass", image_path=str(config.image_path))


def _cleanup_process(env: Environment, process) -> dict:
    if process is None:
        return make_step("cleanup", "pass", reason="起動したプロセスはなかった")
    pid = process.pid
    if process.poll() is not None:
        return make_step("cleanup", "pass", terminated_pid=pid, reason="プロセスは既に終了していた")
    process.terminate()
    try:
        process.wait(timeout=5)
        exited = True
    except subprocess.TimeoutExpired:
        exited = False
    if not exited:
        process.kill()
        try:
            process.wait(timeout=5)
            exited = True
        except subprocess.TimeoutExpired:
            exited = False
    if exited:
        return make_step("cleanup", "pass", terminated_pid=pid)
    return make_step("cleanup", "blocked", reason="起動したプロセスを終了できなかった", terminated_pid=pid)


def do_cleanup(env: Environment, process) -> dict:
    # Defer further abort signals while terminating the exact child. Otherwise
    # an exception raised inside the caller's finally would skip its evidence.
    received: list[int] = []
    previous = {}
    if threading.current_thread() is threading.main_thread():
        for sig in ABORT_SIGNALS:
            previous[sig] = signal.signal(sig, lambda signum, _frame: received.append(signum))
    try:
        try:
            result = _cleanup_process(env, process)
        except (Aborted, OSError) as exc:
            result = make_step("cleanup", "blocked", reason=f"終了処理を完了できなかった: {exc}")
        if received:
            result = make_step("cleanup", "blocked", reason="終了処理中に中断要求を受信した",
                               signals=received, process_cleanup=result)
        return result
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def _summary(overall_result: str, overall_reason: str, config: Config) -> str:
    label = {"pass": "PASS", "fail": "FAIL", "blocked": "BLOCKED"}[overall_result]
    return (
        f"[{label}] gui-validate scenario={config.scenario} request={config.request_id} "
        f"gen={config.generation} — {overall_reason} "
        "(起動・撮影の経路確認。描画・入力・保存の包括的検証ではない)"
    )


def finalize(
    steps: list[dict],
    config: Config,
    target_info: dict,
    build_info: dict,
    started_at: str,
    env: Environment,
) -> dict:
    considered = [s["result"] for s in steps if s["result"] in RESULT_PRIORITY]
    overall_result = min(considered, key=lambda r: RESULT_PRIORITY[r]) if considered else "blocked"
    reasons = [s["reason"] for s in steps if s.get("reason") and s["result"] != "pass"]
    overall_reason = "; ".join(reasons) if reasons else "すべての工程が成功した"

    image_step = next((s for s in steps if s["name"] == "capture"), None)
    image_path = str(config.image_path) if image_step and image_step["result"] == "pass" else None

    doc = {
        "schema_version": SCHEMA_VERSION,
        "procedure_version": PROCEDURE_VERSION,
        "verification_kind": VERIFICATION_KIND,
        "request_id": config.request_id,
        "generation": config.generation,
        "scenario": config.scenario,
        "overall_result": overall_result,
        "overall_reason": overall_reason,
        "started_at": started_at,
        "finished_at": env.clock.now_iso(),
        "target": target_info,
        "build": build_info,
        "steps": steps,
        "evidence": {
            "log_path": str(config.log_path),
            "image_path": image_path,
            "state_dir": str(config.state_dir),
            "fixture_path": str(config.fixture_path) if config.fixture_path else None,
        },
        "scope_note": SCOPE_NOTE,
    }
    doc["summary"] = _summary(overall_result, overall_reason, config)
    return doc


def run_validation(env: Environment, config: Config) -> dict:
    """Validate and clean up; the caller releases the lock after publication."""
    env._finalizing = False
    env._execution_acquired = False
    started_at = env.clock.now_iso()
    steps: list[dict] = []
    target_info: dict = {}
    build_info: dict = {}
    process_holder: dict = {"process": None}
    active_stage = None

    try:
        env.acquire_execution()
        env._execution_acquired = True
        active_stage = "preflight"
        preflight_step, target_info = do_preflight(env, config)
        steps.append(preflight_step)
        if preflight_step["result"] != "pass":
            for name in ("build", "launch", "window_discovery", "capture"):
                steps.append(skipped_step(name, "preflight が pass しなかった"))
        else:
            active_stage = "build"
            build_step, binary_path, build_info = do_build(env, config, target_info["actual_sha"])
            steps.append(build_step)
            if build_step["result"] != "pass":
                for name in ("launch", "window_discovery", "capture"):
                    steps.append(skipped_step(name, "build が pass しなかった"))
            else:
                active_stage = "setup"
                env.prepare(config)
                active_stage = "launch"
                launch_step = do_launch(env, config, binary_path, process_holder)
                steps.append(launch_step)
                if launch_step["result"] != "pass":
                    steps.append(skipped_step("window_discovery", "launch が pass しなかった"))
                    steps.append(skipped_step("capture", "launch が pass しなかった"))
                else:
                    active_stage = "window_discovery"
                    window_step, window_id = do_window_discovery(env, config, process_holder["process"])
                    steps.append(window_step)
                    if window_step["result"] != "pass":
                        steps.append(skipped_step("capture", "window_discovery が pass しなかった"))
                    else:
                        active_stage = "capture"
                        steps.append(do_capture(env, config, window_id))
    except Aborted as exc:
        steps.append(make_step("run", "blocked", reason=f"中断された: {exc}"))
    except (EnvError, OSError) as exc:
        steps.append(make_step("setup", "blocked", reason=str(exc)))
    finally:
        env._finalizing = True
        recorded = {step["name"] for step in steps}
        for name in ("preflight", "build", "launch", "window_discovery", "capture"):
            if name not in recorded:
                steps.append(make_step(name, "blocked", reason="工程の途中で検証が中断された")
                             if name == active_stage else skipped_step(name, "前工程が完了しなかった"))
        try:
            steps.append(do_cleanup(env, process_holder["process"]))
        finally:
            env.cleanup_build()

    if getattr(env, "_snapshot_cleanup_error", None):
        steps.append(make_step("snapshot_cleanup", "blocked", reason=env._snapshot_cleanup_error))

    return finalize(steps, config, target_info, build_info, started_at, env)


def _scenario_setup(scenario: str, run_dir: Path, *, prepare: bool = True) -> tuple[Optional[Path], list[str], dict[str, str]]:
    if scenario == "editor":
        fixture = os.environ.get("HANE_CAPTURE_FIXTURE", "")
        fixture_path = run_dir / "editor.md"
        if fixture:
            # This scenario only launches and captures: it never sends editing
            # input. Keep the supplied document's directory so sibling and ../
            # resource references resolve exactly as when opened normally.
            fixture_path = Path(fixture).absolute()
            try:
                if not fixture_path.is_file():
                    raise ValueError("通常ファイルではない")
                # RopeBuffer::from_reader accepts a complete UTF-8 document,
                # not merely a readable first byte. Reject invalid documents
                # before Hane can replace an open error with an error buffer.
                fixture_path.read_text(encoding="utf-8")
            except (OSError, UnicodeError, ValueError) as exc:
                raise ValueError(f"HANE_CAPTURE_FIXTURE を読み込めない: {exc}") from exc
        elif prepare:
            fixture_path.write_text("# Hane GUI validation\n\n起動・撮影の確認用文書です。\n", encoding="utf-8")
        # The normal build does not arm or emit hane_ready. timing-probe
        # enables readiness observation without instrument's synthetic input.
        return fixture_path, ["timing-probe"], {}
    if scenario == "cursor-boundary":
        fixture_path = run_dir / "cursor-boundary.md"
        contents = "first line\nsecond line\n"
        offset = _env_bounded_int("HANE_CAPTURE_CURSOR_OFFSET", 11, len(contents))
        if prepare:
            fixture_path.write_text(contents)
        return fixture_path, ["instrument"], {"HANE_MEASUREMENT_CURSOR_OFFSET": offset}
    if scenario == "cursor-scroll":
        fixture_path = run_dir / "forty-lines.md"
        lines = [f"line {n:02d} — scroll verification\n" for n in range(1, 41)]
        down = _env_bounded_int("HANE_CAPTURE_CURSOR_DOWN", 32, len(lines))
        if prepare:
            fixture_path.write_text("".join(lines))
        return fixture_path, ["instrument"], {"HANE_DEV_CURSOR_DOWN": down}
    raise ValueError(f"unknown scenario: {scenario}")


def _env_bounded_int(name: str, default: int, maximum: int) -> str:
    value = os.environ.get(name, str(default))
    if not value.isascii() or not value.isdecimal():
        raise ValueError(f"{name} は0〜{maximum}の整数である必要がある")
    parsed = int(value)
    if parsed > maximum:
        raise ValueError(f"{name} は0〜{maximum}の整数である必要がある")
    return str(parsed)


def _env_float(name: str, default: float) -> float:
    value = os.environ.get(name)
    if not value:
        return default
    try:
        parsed = float(value)
    except ValueError as exc:
        raise ValueError(f"{name} は数値として解釈できない: {value!r}") from exc
    if not math.isfinite(parsed) or parsed <= 0:
        raise ValueError(f"{name} は有限の正の数である必要がある: {value!r}")
    return parsed


def _env_cmd(name: str) -> Optional[list[str]]:
    value = os.environ.get(name)
    return shlex.split(value) if value else None


def build_config(scenario: str, workspace_dir: Path) -> Config:
    request_id = os.environ.get("HANE_GUI_VALIDATE_REQUEST_ID") or (
        time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{os.getpid()}"
    )
    generation = os.environ.get("HANE_GUI_VALIDATE_GENERATION", "1")
    run_dir = Path(
        os.environ.get("HANE_GUI_VALIDATE_RUN_DIR")
        or (workspace_dir / "target" / "gui-validate" / request_id / generation)
    )
    state_dir = run_dir / "state"
    if (run_dir / ".gui-validate-owner").exists():
        raise EnvError("この実行ディレクトリは別の実行が予約済み（既存の証拠を保護）")
    if run_dir.exists() and (not run_dir.is_dir() or any(run_dir.iterdir())):
        raise ValueError("実行用ディレクトリは未作成または空である必要がある（過去の証拠を上書きしない）")

    workspace_root, output_root = workspace_dir.resolve(), run_dir.resolve()
    if output_root == workspace_root or workspace_root in output_root.parents:
        # Retained artifacts must not make the next generation's checkout
        # dirty. Reject before acquiring/reserving anything, including receipts.
        try:
            ignored = subprocess.run(['git', 'check-ignore', '--quiet', '--', str(output_root) + '/'],
                                     cwd=workspace_root, capture_output=True)
        except OSError as exc:
            raise EnvError(f"保存先のGit無視設定を確認できない: {exc}") from exc
        if ignored.returncode == 1:
            raise ValueError("checkout内の実行用ディレクトリはGitで無視される場所に限る（target/またはcheckout外を指定）")
        if ignored.returncode != 0:
            raise EnvError(f"保存先のGit無視設定を確認できない: git exit {ignored.returncode}")

    fixture_path, features, extra_env = _scenario_setup(scenario, run_dir, prepare=False)

    return Config(
        workspace_dir=workspace_dir,
        scenario=scenario,
        expected_sha=os.environ.get("HANE_GUI_VALIDATE_EXPECTED_SHA") or None,
        request_id=request_id,
        generation=generation,
        run_dir=run_dir,
        state_dir=state_dir,
        log_path=run_dir / "hane.log",
        image_path=run_dir / f"{scenario}.png",
        fixture_path=fixture_path,
        features=features,
        extra_env=extra_env,
        startup_timeout_seconds=_env_float("HANE_GUI_VALIDATE_STARTUP_TIMEOUT_SECS", 15.0),
        window_timeout_seconds=_env_float("HANE_GUI_VALIDATE_WINDOW_TIMEOUT_SECS", 5.0),
        window_id_cmd=_env_cmd("HANE_GUI_VALIDATE_WINDOW_ID_CMD"),
        capture_cmd=_env_cmd("HANE_GUI_VALIDATE_CAPTURE_CMD"),
        capture_timeout_seconds=_env_float("HANE_GUI_VALIDATE_CAPTURE_TIMEOUT_SECS", 15.0),
    )


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        print("expected at most one scenario argument; set the expected SHA with HANE_GUI_VALIDATE_EXPECTED_SHA", file=sys.stderr)
        return EXIT_USAGE
    scenario = argv[1] if len(argv) > 1 else "editor"
    if scenario in ("-h", "--help"):
        print(__doc__)
        print(f"usage: {argv[0]} [{'|'.join(SCENARIOS)}]")
        return EXIT_PASS
    if scenario not in SCENARIOS:
        print(f"unknown scenario: {scenario}", file=sys.stderr)
        print(f"available scenarios: {', '.join(SCENARIOS)}", file=sys.stderr)
        return EXIT_USAGE

    workspace_dir = Path(__file__).resolve().parent.parent
    try:
        config = build_config(scenario, workspace_dir)
    except EnvError as exc:
        print(f"[BLOCKED] {exc}", file=sys.stderr)
        return EXIT_BLOCKED
    except (ValueError, OSError) as exc:
        print(f"invalid configuration: {exc}", file=sys.stderr)
        return EXIT_USAGE

    env = RealEnvironment()

    def _handle_signal(signum, _frame):
        # Cleanup records its own deferred signals. Once cleanup begins, never
        # interrupt final evidence publication with another asynchronous raise.
        if not getattr(env, "_finalizing", False):
            raise Aborted(f"signal {signum}")
        env.record_aborts([signum])

    previous_handlers = {}
    for sig in ABORT_SIGNALS:
        previous_handlers[sig] = signal.signal(sig, _handle_signal)

    try:
        result = run_validation(env, config)
        return _publish_result(env, config, result)
    finally:
        try:
            env.release_execution()
        finally:
            for sig, handler in previous_handlers.items():
                signal.signal(sig, handler)


def _incorporate_publication_aborts(env: RealEnvironment, config: Config, result: dict) -> bool:
    pending = getattr(env, "_deferred_aborts", [])
    steps = result.setdefault("steps", [])
    if not pending or any(step.get("name") == "publication_abort" for step in steps):
        return False
    reason = "終了処理後または結果保存中に中断要求を受信した"
    steps.append(make_step("publication_abort", "blocked", reason=reason, signals=list(pending)))
    if result["overall_result"] == "pass":
        result["overall_result"] = "blocked"
    result["overall_reason"] = "; ".join(filter(None, (result.get("overall_reason"), reason)))
    result["summary"] = _summary(result["overall_result"], result["overall_reason"], config)
    return True


def _publish_result(env: RealEnvironment, config: Config, result: dict) -> int:
    if not getattr(env, "_execution_acquired", False):
        # The display-lock winner may not have reached fixture preparation yet.
        # A loser must not claim its still-empty evidence directory first.
        print(result["summary"], file=sys.stderr)
        return EXIT_BLOCKED
    result_path = config.run_dir / "result.json"
    try:
        env.reserve(config)
    except EnvError as exc:
        # A competing run owns these paths. Do not overwrite its evidence,
        # even with our own blocked result after the failed reservation.
        print(f"[BLOCKED] {exc}", file=sys.stderr)
        return EXIT_BLOCKED
    summary_path = config.run_dir / "summary.md"
    result_temp = config.run_dir / ".result.json.tmp"
    summary_temp = config.run_dir / ".summary.md.tmp"
    while True:
        _incorporate_publication_aborts(env, config, result)
        try:
            result_temp.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")
            summary_temp.write_text(result["summary"] + "\n")
            summary_temp.replace(summary_path)
            result_temp.replace(result_path)  # Publish the canonical receipt last, atomically.
        except OSError as exc:
            # reserve() succeeded for this generation while the execution lock
            # is held. A previous iteration may already have published pass;
            # never leave it canonical after a failed downgrade publication.
            for canonical in (result_path, summary_path):
                try:
                    canonical.unlink(missing_ok=True)
                except OSError as invalidate_error:
                    print(f"[BLOCKED] {canonical.name}の無効化にも失敗した: {invalidate_error}", file=sys.stderr)
            print(f"[BLOCKED] 結果を書き込めなかった: {exc}", file=sys.stderr)
            return EXIT_BLOCKED
        if not _incorporate_publication_aborts(env, config, result):
            break

    print(result["summary"])
    print(f"result: {result_path}")

    if _incorporate_publication_aborts(env, config, result):
        return _publish_result(env, config, result)

    return {"pass": EXIT_PASS, "fail": EXIT_FAIL, "blocked": EXIT_BLOCKED}[result["overall_result"]]


if __name__ == "__main__":
    sys.exit(main(sys.argv))
