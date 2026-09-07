"""Tests for gui_validate.py that never touch a real screen or a real build.

Every OS-facing call (git, cargo, swift, screencapture, the launched process)
is replaced by a FakeEnvironment/FakeProcess so the state machine — timeouts,
pass/fail/blocked classification, cleanup scoped to the one launched process,
evidence retention on partial failure — can run in CI on any platform.
"""

from __future__ import annotations

import os
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import gui_validate as gv


class FakeClock:
    def __init__(self) -> None:
        self._t = 0.0

    def now_iso(self) -> str:
        return "2026-09-07T00:00:00Z"

    def monotonic(self) -> float:
        return self._t

    def sleep(self, seconds: float) -> None:
        self._t += seconds


class FakeProcess:
    def __init__(self, pid: int, exits_after_polls: int | None = None, exit_code: int = 0, unkillable: bool = False):
        self.pid = pid
        self._polls = 0
        self._exits_after_polls = exits_after_polls
        self._exit_code = exit_code
        self._terminated = False
        self._killed = False
        self._unkillable = unkillable

    def poll(self):
        if self._terminated or self._killed:
            return self._exit_code
        if self._exits_after_polls is not None:
            self._polls += 1
            if self._polls >= self._exits_after_polls:
                return self._exit_code
        return None

    def terminate(self):
        self._terminated = True
        if self._unkillable:
            self._terminated = False

    def kill(self):
        self._killed = True
        if self._unkillable:
            self._killed = False

    def wait(self, timeout=None):
        if self._terminated or self._killed:
            return self._exit_code
        raise subprocess.TimeoutExpired(cmd="fake", timeout=timeout)


class FakeEnvironment(gv.Environment):
    def __init__(
        self,
        *,
        head_sha="abc123",
        dirty_paths=None,
        missing=None,
        build_result=("target/debug/hane", {"profile": "debug", "features": []}),
        process=None,
        ready_after=1,
        window_after=1,
        window_ids=None,
        capture_result=True,
        abort_after_launch_polls=None,
    ):
        self.clock = FakeClock()
        self.head_sha = head_sha
        self.dirty_paths = dirty_paths or []
        self.missing = missing or []
        self.build_result = build_result
        self.process = process
        self.ready_after = ready_after
        self.window_after = window_after
        self.window_ids = window_ids
        self._ready_polls = 0
        self._window_polls = 0
        self.capture_result = capture_result
        self.build_calls = 0
        self.capture_calls = 0
        self.abort_after_launch_polls = abort_after_launch_polls

    def git_head(self, workspace_dir):
        return self.head_sha

    def git_dirty_paths(self, workspace_dir):
        return self.dirty_paths

    def missing_tools(self, config):
        return self.missing

    def build(self, workspace_dir, features):
        self.build_calls += 1
        if isinstance(self.build_result, Exception):
            raise self.build_result
        path, info = self.build_result
        return Path(path), info

    def launch(self, binary_path, config):
        if isinstance(self.process, Exception):
            raise self.process
        return self.process

    def log_contains_ready(self, log_path):
        self._ready_polls += 1
        if self.abort_after_launch_polls and self._ready_polls >= self.abort_after_launch_polls:
            raise gv.Aborted("test signal")
        if self.ready_after is None:
            return False
        return self._ready_polls >= self.ready_after

    def find_window_once(self, pid, config, timeout_seconds=None):
        self._window_polls += 1
        if self.window_ids is not None:
            idx = min(self._window_polls - 1, len(self.window_ids) - 1)
            return self.window_ids[idx]
        if self.window_after is None:
            return None
        return "42" if self._window_polls >= self.window_after else None

    def capture(self, window_id, image_path, config):
        self.capture_calls += 1
        if isinstance(self.capture_result, Exception):
            raise self.capture_result
        return self.capture_result


def make_config(tmp_path: Path, **overrides) -> gv.Config:
    run_dir = tmp_path / "run"
    run_dir.mkdir(exist_ok=True)
    state_dir = run_dir / "state"
    state_dir.mkdir(exist_ok=True)
    defaults = dict(
        workspace_dir=tmp_path,
        scenario="editor",
        expected_sha=None,
        request_id="req-1",
        generation="1",
        run_dir=run_dir,
        state_dir=state_dir,
        log_path=run_dir / "hane.log",
        image_path=run_dir / "editor.png",
        fixture_path=None,
        features=[],
        extra_env={},
        startup_timeout_seconds=5.0,
        window_timeout_seconds=5.0,
        poll_interval_seconds=0.1,
    )
    defaults.update(overrides)
    return gv.Config(**defaults)


class RunValidationTests(unittest.TestCase):
    def test_pass_path(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=111)
        env = FakeEnvironment(process=process)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "pass")
        names = [s["name"] for s in result["steps"]]
        self.assertEqual(names, ["preflight", "build", "launch", "window_discovery", "capture", "cleanup"])
        for step in result["steps"]:
            self.assertEqual(step["result"], "pass", step)
        self.assertEqual(result["evidence"]["image_path"], str(config.image_path))
        self.assertIn("経路確認", result["scope_note"])

    def test_launch_timeout_is_blocked_and_skips_downstream(self):
        config = make_config(Path("/workspace"), startup_timeout_seconds=1.0, poll_interval_seconds=0.2)
        process = FakeProcess(pid=222)
        env = FakeEnvironment(process=process, ready_after=None)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["launch"]["result"], "blocked")
        self.assertIn("タイムアウト", by_name["launch"]["reason"])
        self.assertEqual(by_name["window_discovery"]["result"], "skipped")
        self.assertEqual(by_name["capture"]["result"], "skipped")
        self.assertEqual(by_name["cleanup"]["result"], "pass")
        self.assertTrue(process._terminated)

    def test_launch_crash_before_ready_is_fail(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=333, exits_after_polls=1, exit_code=1)
        env = FakeEnvironment(process=process, ready_after=None)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["launch"]["result"], "fail")
        self.assertIn("exit=1", by_name["launch"]["reason"])
        self.assertEqual(by_name["cleanup"]["result"], "pass")

    def test_launch_env_error_is_fail_not_uncaught(self):
        config = make_config(Path("/workspace"))
        env = FakeEnvironment(process=gv.EnvError("executable could not be started"))
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["launch"]["result"], "fail")
        self.assertIn("executable could not be started", by_name["launch"]["reason"])
        self.assertEqual(by_name["cleanup"]["result"], "pass", "should not crash without a launched process")
        self.assertIn("既に終了", by_name["cleanup"]["reason"])

    def test_window_discovery_timeout_is_blocked(self):
        config = make_config(Path("/workspace"), window_timeout_seconds=1.0, poll_interval_seconds=0.2)
        process = FakeProcess(pid=444)
        env = FakeEnvironment(process=process, window_after=None)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["window_discovery"]["result"], "blocked")
        self.assertEqual(by_name["capture"]["result"], "skipped")

    def test_capture_failure_is_blocked_not_fail(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=555)
        env = FakeEnvironment(process=process, capture_result=False)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["capture"]["result"], "blocked")
        self.assertIsNone(result["evidence"]["image_path"])

    def test_capture_exception_is_blocked(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=556)
        env = FakeEnvironment(process=process, capture_result=gv.EnvError("permission denied"))
        result = gv.run_validation(env, config)
        self.assertEqual(result["overall_result"], "blocked")

    def test_cleanup_failure_forces_overall_blocked(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=666, unkillable=True)
        env = FakeEnvironment(process=process)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["capture"]["result"], "pass")
        self.assertEqual(by_name["cleanup"]["result"], "blocked")

    def test_sha_mismatch_blocks_before_build(self):
        config = make_config(Path("/workspace"), expected_sha="deadbeef")
        env = FakeEnvironment(head_sha="abc123")
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["preflight"]["result"], "blocked")
        self.assertEqual(by_name["build"]["result"], "skipped")
        self.assertEqual(env.build_calls, 0)

    def test_dirty_working_copy_blocks(self):
        config = make_config(Path("/workspace"))
        env = FakeEnvironment(dirty_paths=[" M crates/app/src/main.rs"])
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        self.assertIn("作業コピー", result["steps"][0]["reason"])

    def test_git_dirty_paths_error_blocks_preflight(self):
        class RaisingEnvironment(FakeEnvironment):
            def git_dirty_paths(self, workspace_dir):
                raise gv.EnvError("unreadable index")

        config = make_config(Path("/workspace"))
        env = RaisingEnvironment()
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["preflight"]["result"], "blocked")
        self.assertIn("unreadable index", by_name["preflight"]["reason"])
        self.assertEqual(by_name["build"]["result"], "skipped")

    def test_missing_tools_blocks(self):
        config = make_config(Path("/workspace"))
        env = FakeEnvironment(missing=["swift", "screencapture"])
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        self.assertIn("swift", result["steps"][0]["reason"])

    def test_build_failure_is_fail(self):
        config = make_config(Path("/workspace"))
        env = FakeEnvironment(build_result=gv.BuildError("compile error"))
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["build"]["result"], "fail")
        self.assertEqual(by_name["launch"]["result"], "skipped")

    def test_abort_mid_launch_still_cleans_up_and_is_blocked(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=777)
        env = FakeEnvironment(process=process, ready_after=5, abort_after_launch_polls=2)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertIn("run", by_name)
        self.assertIn("中断", by_name["run"]["reason"])
        self.assertEqual(by_name["cleanup"]["result"], "pass")
        self.assertTrue(process._terminated)

    def test_cleanup_only_touches_the_launched_process(self):
        other = FakeProcess(pid=1)
        launched = FakeProcess(pid=2)
        step = gv.do_cleanup(FakeEnvironment(), launched)
        self.assertEqual(step["result"], "pass")
        self.assertTrue(launched._terminated)
        self.assertFalse(other._terminated)

    def test_no_process_started_cleanup_is_trivially_pass(self):
        step = gv.do_cleanup(FakeEnvironment(), None)
        self.assertEqual(step["result"], "pass")

    def test_result_schema_has_required_fields(self):
        config = make_config(Path("/workspace"))
        process = FakeProcess(pid=888)
        env = FakeEnvironment(process=process)
        result = gv.run_validation(env, config)

        for key in (
            "schema_version",
            "procedure_version",
            "verification_kind",
            "request_id",
            "generation",
            "scenario",
            "overall_result",
            "overall_reason",
            "started_at",
            "finished_at",
            "target",
            "build",
            "steps",
            "evidence",
            "scope_note",
            "summary",
        ):
            self.assertIn(key, result)
        self.assertEqual(result["verification_kind"], "launch_and_capture_path")


class FinalizePriorityTests(unittest.TestCase):
    def _finalize(self, results):
        steps = [gv.make_step(f"s{i}", r) for i, r in enumerate(results)]
        config = make_config(Path("/workspace"))
        env = FakeEnvironment()
        return gv.finalize(steps, config, {}, {}, "2026-09-07T00:00:00Z", env)

    def test_all_pass_is_pass(self):
        self.assertEqual(self._finalize(["pass", "pass", "skipped"])["overall_result"], "pass")

    def test_any_blocked_without_fail_is_blocked(self):
        self.assertEqual(self._finalize(["pass", "blocked", "pass"])["overall_result"], "blocked")

    def test_fail_wins_over_blocked(self):
        self.assertEqual(self._finalize(["fail", "blocked", "pass"])["overall_result"], "fail")

    def test_only_skipped_is_blocked(self):
        self.assertEqual(self._finalize(["skipped", "skipped"])["overall_result"], "blocked")


class ScenarioSetupTests(unittest.TestCase):
    def test_editor_defaults_to_no_fixture(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            fixture, features, extra_env = gv._scenario_setup("editor", Path(tmp))
            self.assertIsNone(fixture)
            self.assertEqual(features, [])
            self.assertEqual(extra_env, {})

    def test_cursor_boundary_writes_two_lines_and_instrument_feature(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            fixture, features, extra_env = gv._scenario_setup("cursor-boundary", Path(tmp))
            self.assertEqual(fixture.read_text(), "first line\nsecond line\n")
            self.assertEqual(features, ["instrument"])
            self.assertIn("HANE_MEASUREMENT_CURSOR_OFFSET", extra_env)

    def test_cursor_scroll_writes_forty_lines(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            fixture, features, extra_env = gv._scenario_setup("cursor-scroll", Path(tmp))
            self.assertEqual(len(fixture.read_text().splitlines()), 40)
            self.assertEqual(features, ["instrument"])
            self.assertIn("HANE_DEV_CURSOR_DOWN", extra_env)


class EnvFloatTests(unittest.TestCase):
    def test_missing_env_returns_default(self):
        self.assertEqual(gv._env_float("HANE_GUI_VALIDATE_TEST_UNSET", 15.0), 15.0)

    def test_valid_value_is_parsed(self):
        os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"] = "7.5"
        try:
            self.assertEqual(gv._env_float("HANE_GUI_VALIDATE_TEST_TIMEOUT", 15.0), 7.5)
        finally:
            del os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"]

    def test_nan_is_rejected(self):
        os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"] = "nan"
        try:
            with self.assertRaises(ValueError):
                gv._env_float("HANE_GUI_VALIDATE_TEST_TIMEOUT", 15.0)
        finally:
            del os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"]

    def test_infinite_is_rejected(self):
        os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"] = "inf"
        try:
            with self.assertRaises(ValueError):
                gv._env_float("HANE_GUI_VALIDATE_TEST_TIMEOUT", 15.0)
        finally:
            del os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"]

    def test_non_positive_is_rejected(self):
        os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"] = "0"
        try:
            with self.assertRaises(ValueError):
                gv._env_float("HANE_GUI_VALIDATE_TEST_TIMEOUT", 15.0)
        finally:
            del os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"]

    def test_nonnumeric_is_rejected(self):
        os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"] = "soon"
        try:
            with self.assertRaises(ValueError):
                gv._env_float("HANE_GUI_VALIDATE_TEST_TIMEOUT", 15.0)
        finally:
            del os.environ["HANE_GUI_VALIDATE_TEST_TIMEOUT"]


class RealEnvironmentLaunchTests(unittest.TestCase):
    def test_popen_oserror_is_wrapped_as_env_error(self):
        import tempfile

        env = gv.RealEnvironment()
        with tempfile.TemporaryDirectory() as tmp:
            run_dir = Path(tmp)
            config = make_config(run_dir, run_dir=run_dir, state_dir=run_dir, log_path=run_dir / "hane.log")
            with self.assertRaises(gv.EnvError):
                env.launch(run_dir / "does-not-exist-and-is-not-executable", config)


if __name__ == "__main__":
    unittest.main()
