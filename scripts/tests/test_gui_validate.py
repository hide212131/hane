"""Tests for gui_validate.py that never touch a real screen or a real build.

Builds and GUI operations use fakes. Temporary Git repositories and a bounded
Python child process verify snapshot isolation and inherited process locks.
The state-machine tests never launch Hane, Cargo, or screen/input tools.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
import json
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

    def build(self, workspace_dir, features, source_sha=None):
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


class TemporaryWorkspaceTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.workspace = Path(temporary.name)


class RunValidationTests(TemporaryWorkspaceTest):
    def test_pass_path(self):
        config = make_config(self.workspace)
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
        config = make_config(self.workspace, startup_timeout_seconds=1.0, poll_interval_seconds=0.2)
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
        config = make_config(self.workspace)
        process = FakeProcess(pid=333, exits_after_polls=1, exit_code=1)
        env = FakeEnvironment(process=process, ready_after=None)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["launch"]["result"], "fail")
        self.assertIn("exit=1", by_name["launch"]["reason"])
        self.assertEqual(by_name["cleanup"]["result"], "pass")

    def test_launch_env_error_is_fail_not_uncaught(self):
        config = make_config(self.workspace)
        env = FakeEnvironment(process=gv.EnvError("executable could not be started"))
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["launch"]["result"], "fail")
        self.assertIn("executable could not be started", by_name["launch"]["reason"])
        self.assertEqual(by_name["cleanup"]["result"], "pass", "should not crash without a launched process")
        self.assertEqual(by_name["cleanup"]["reason"], "起動したプロセスはなかった")

    def test_window_discovery_timeout_is_blocked(self):
        config = make_config(self.workspace, window_timeout_seconds=1.0, poll_interval_seconds=0.2)
        process = FakeProcess(pid=444)
        env = FakeEnvironment(process=process, window_after=None)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["window_discovery"]["result"], "blocked")
        self.assertEqual(by_name["capture"]["result"], "skipped")

    def test_capture_failure_is_blocked_not_fail(self):
        config = make_config(self.workspace)
        process = FakeProcess(pid=555)
        env = FakeEnvironment(process=process, capture_result=False)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["capture"]["result"], "blocked")
        self.assertIsNone(result["evidence"]["image_path"])

    def test_capture_exception_is_blocked(self):
        config = make_config(self.workspace)
        process = FakeProcess(pid=556)
        env = FakeEnvironment(process=process, capture_result=gv.EnvError("permission denied"))
        result = gv.run_validation(env, config)
        self.assertEqual(result["overall_result"], "blocked")

    def test_cleanup_failure_forces_overall_blocked(self):
        config = make_config(self.workspace)
        process = FakeProcess(pid=666, unkillable=True)
        env = FakeEnvironment(process=process)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["capture"]["result"], "pass")
        self.assertEqual(by_name["cleanup"]["result"], "blocked")

    def test_sha_mismatch_blocks_before_build(self):
        config = make_config(self.workspace, expected_sha="deadbeef")
        env = FakeEnvironment(head_sha="abc123")
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["preflight"]["result"], "blocked")
        self.assertEqual(by_name["build"]["result"], "skipped")
        self.assertEqual(env.build_calls, 0)

    def test_dirty_working_copy_blocks(self):
        config = make_config(self.workspace)
        env = FakeEnvironment(dirty_paths=[" M crates/app/src/main.rs"])
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        self.assertIn("作業コピー", result["steps"][0]["reason"])

    def test_git_dirty_paths_error_blocks_preflight(self):
        class RaisingEnvironment(FakeEnvironment):
            def git_dirty_paths(self, workspace_dir):
                raise gv.EnvError("unreadable index")

        config = make_config(self.workspace)
        env = RaisingEnvironment()
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["preflight"]["result"], "blocked")
        self.assertIn("unreadable index", by_name["preflight"]["reason"])
        self.assertEqual(by_name["build"]["result"], "skipped")

    def test_missing_tools_blocks(self):
        config = make_config(self.workspace)
        env = FakeEnvironment(missing=["swift", "screencapture"])
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        self.assertIn("swift", result["steps"][0]["reason"])

    def test_build_failure_is_fail(self):
        config = make_config(self.workspace)
        env = FakeEnvironment(build_result=gv.BuildError("compile error"))
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "fail")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertEqual(by_name["build"]["result"], "fail")
        self.assertEqual(by_name["launch"]["result"], "skipped")

    def test_abort_mid_launch_still_cleans_up_and_is_blocked(self):
        config = make_config(self.workspace)
        process = FakeProcess(pid=777)
        env = FakeEnvironment(process=process, ready_after=5, abort_after_launch_polls=2)
        result = gv.run_validation(env, config)

        self.assertEqual(result["overall_result"], "blocked")
        by_name = {s["name"]: s for s in result["steps"]}
        self.assertIn("run", by_name)
        self.assertIn("中断", by_name["run"]["reason"])
        self.assertEqual(by_name["launch"]["result"], "blocked")
        self.assertEqual(by_name["window_discovery"]["result"], "skipped")
        self.assertEqual(by_name["capture"]["result"], "skipped")
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
        config = make_config(self.workspace)
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


class FinalizePriorityTests(TemporaryWorkspaceTest):
    def _finalize(self, results):
        steps = [gv.make_step(f"s{i}", r) for i, r in enumerate(results)]
        config = make_config(self.workspace)
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
    def test_extra_arguments_are_rejected_before_configuration_or_execution(self):
        import contextlib
        import io
        from unittest.mock import patch
        for arguments in (['editor', 'a' * 40], ['editor', '--unexpected'], ['--help', 'extra']):
            with self.subTest(arguments=arguments), contextlib.redirect_stderr(io.StringIO()) as error, \
                    patch.object(gv, 'build_config') as configure, patch.object(gv, 'RealEnvironment') as environment:
                self.assertEqual(gv.main(['gui_validate.py', *arguments]), gv.EXIT_USAGE)
                self.assertIn('HANE_GUI_VALIDATE_EXPECTED_SHA', error.getvalue())
                configure.assert_not_called()
                environment.assert_not_called()

    def test_editor_uses_isolated_fixture_and_readiness_probe(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            fixture, features, extra_env = gv._scenario_setup("editor", Path(tmp))
            self.assertEqual(fixture, Path(tmp) / "editor.md")
            self.assertIn("# Hane GUI validation", fixture.read_text(encoding="utf-8"))
            self.assertEqual(features, ["timing-probe"])
            self.assertEqual(extra_env, {})

    def test_editor_preserves_supplied_fixture_resource_context(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            documents = root / "documents"
            documents.mkdir()
            source = documents / "source.md"
            contents = "![local](image.png)\n![parent](../shared.png)\n"
            source.write_text(contents, encoding="utf-8")
            (documents / "image.png").write_bytes(b"local resource")
            (root / "shared.png").write_bytes(b"parent resource")
            run = root / "run"
            run.mkdir()
            with patch.dict(os.environ, {"HANE_CAPTURE_FIXTURE": str(source)}):
                fixture, _, _ = gv._scenario_setup("editor", run)
            self.assertEqual(fixture, source)
            self.assertEqual((fixture.parent / "image.png").read_bytes(), b"local resource")
            self.assertEqual((fixture.parent / "../shared.png").read_bytes(), b"parent resource")
            self.assertEqual(source.read_text(encoding="utf-8"), contents)

    def test_missing_or_directory_fixture_returns_controlled_usage_error(self):
        import contextlib
        import io
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            for source in (Path(tmp) / "missing.md", Path(tmp)):
                with self.subTest(source=source), patch.dict(os.environ, {
                    "HANE_CAPTURE_FIXTURE": str(source),
                    "HANE_GUI_VALIDATE_RUN_DIR": str(Path(tmp) / "run"),
                }), contextlib.redirect_stderr(io.StringIO()) as error:
                    self.assertEqual(gv.main(["gui_validate.py", "editor"]), gv.EXIT_USAGE)
                    self.assertIn("invalid configuration", error.getvalue())
                    self.assertNotIn("Traceback", error.getvalue())

    def test_invalid_utf8_fixture_is_rejected_before_launch(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "invalid.md"
            source.write_bytes(b"valid prefix\n\xff")
            with patch.dict(os.environ, {"HANE_CAPTURE_FIXTURE": str(source)}):
                with self.assertRaisesRegex(ValueError, "HANE_CAPTURE_FIXTURE"):
                    gv._scenario_setup("editor", Path(tmp) / "run", prepare=False)

    def test_external_fixture_damaged_during_build_blocks_before_launch(self):
        from unittest.mock import patch
        for damage in ('delete', 'directory', 'invalid_utf8'):
            with self.subTest(damage=damage), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                source = root / 'source.md'
                source.write_text('initially valid document')
                def build(*args, **kwargs):
                    source.unlink()
                    if damage == 'directory':
                        source.mkdir()
                    elif damage == 'invalid_utf8':
                        source.write_bytes(b'invalid document\xff')
                    return root / 'hane', {}
                env = gv.RealEnvironment()
                with patch.dict(os.environ, {'HANE_CAPTURE_FIXTURE': str(source),
                        'HANE_GUI_VALIDATE_RUN_DIR': str(root / 'run')}):
                    config = gv.build_config('editor', root / 'checkout')
                    with patch.object(env, 'acquire_execution'), \
                            patch.object(env, 'git_head', return_value='abc123'), \
                            patch.object(env, 'git_dirty_paths', return_value=[]), \
                            patch.object(env, 'missing_tools', return_value=[]), \
                            patch.object(env, 'build', side_effect=build), \
                            patch.object(env, 'launch') as launch:
                        result = gv.run_validation(env, config)
                self.assertEqual(result['overall_result'], 'blocked')
                self.assertIn('HANE_CAPTURE_FIXTURE', result['overall_reason'])
                stages = {step['name']: step['result'] for step in result['steps']}
                self.assertEqual([stages[name] for name in ('launch', 'window_discovery', 'capture')],
                                 ['skipped', 'skipped', 'skipped'])
                launch.assert_not_called()

    def test_retained_output_must_be_ignored_or_outside_checkout(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "README.md").write_text("fixture repository\n")
            subprocess.run(["git", "-C", str(root), "add", "README.md"], check=True)
            subprocess.run(["git", "-C", str(root), "-c", "user.name=GUI test",
                            "-c", "user.email=gui-test@example.invalid", "-c", "commit.gpgsign=false",
                            "commit", "-qm", "fixture"], check=True)
            run = root / "unignored-run"
            with patch.dict(os.environ, {"HANE_GUI_VALIDATE_RUN_DIR": str(run), "HANE_CAPTURE_FIXTURE": ""}):
                with self.assertRaisesRegex(ValueError, "Gitで無視"):
                    gv.build_config("editor", root)
                self.assertFalse(run.exists())
                (root / '.gitignore').write_text('/unignored-run/\n')
                first = gv.build_config('editor', root)
                gv.RealEnvironment().reserve(first)
                (run / 'result.json').write_text('retained evidence')
            with patch.dict(os.environ, {'HANE_GUI_VALIDATE_RUN_DIR': str(run / 'next')}):
                self.assertEqual(gv.build_config('editor', root).run_dir, run / 'next')
            with patch.dict(os.environ, {'HANE_GUI_VALIDATE_RUN_DIR': str(root.parent / 'external-evidence')}):
                self.assertEqual(gv.build_config('editor', root).run_dir, root.parent / 'external-evidence')

    def test_existing_run_evidence_is_never_overwritten(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            existing = root / "editor.md"
            existing.write_text("preserve me")
            with patch.dict(os.environ, {"HANE_GUI_VALIDATE_RUN_DIR": str(root)}):
                with self.assertRaisesRegex(ValueError, "上書きしない"):
                    gv.build_config("editor", root)
            self.assertEqual(existing.read_text(), "preserve me")

    def test_reserved_generation_main_returns_blocked_without_overwriting(self):
        import contextlib
        import io
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            marker = run / ".gui-validate-owner"
            marker.write_text("existing owner")
            proof = run / "result.json"
            proof.write_text("existing evidence")
            with patch.dict(os.environ, {"HANE_GUI_VALIDATE_RUN_DIR": str(run)}), \
                    patch.object(gv, "run_validation") as execute, \
                    contextlib.redirect_stderr(io.StringIO()) as error:
                self.assertEqual(gv.main(["gui_validate.py", "editor"]), gv.EXIT_BLOCKED)
            execute.assert_not_called()
            self.assertIn("BLOCKED", error.getvalue())
            self.assertEqual(marker.read_text(), "existing owner")
            self.assertEqual(proof.read_text(), "existing evidence")

    def test_duplicate_generation_has_only_one_atomic_directory_owner(self):
        from concurrent.futures import ThreadPoolExecutor
        from threading import Barrier
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.dict(os.environ, {"HANE_GUI_VALIDATE_RUN_DIR": str(root / "run"), "HANE_CAPTURE_FIXTURE": ""}):
                config = gv.build_config("editor", root / 'checkout')
            start = Barrier(2)
            def reserve(_):
                env = gv.RealEnvironment()
                start.wait(timeout=5)
                try:
                    env.reserve(config)
                    env.reserve(config)  # The same owner may publish its result.
                    return "owner"
                except gv.EnvError:
                    return "blocked"
            with ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(reserve, range(2)))
            self.assertCountEqual(results, ["owner", "blocked"])
            marker = json.loads((config.run_dir / ".gui-validate-owner").read_text())
            self.assertEqual(marker["request_id"], config.request_id)

    def test_build_uses_locked_dependencies_and_target_toolchain_directory(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "hane"
            binary.write_bytes(b"test executable")
            calls = []
            def command(args, **kwargs):
                calls.append((args, kwargs))
                output = json.dumps({"reason": "compiler-artifact", "target": {"name": "hane"},
                                     "executable": str(binary)}) if "build" in args else "test version"
                return subprocess.CompletedProcess(args, 0, output, "")
            env = gv.RealEnvironment()
            with patch.object(env, 'snapshot_checkout', return_value=root), \
                    patch.object(env, 'git_head', return_value='a' * 40), \
                    patch.object(env, 'git_dirty_paths', return_value=[]), \
                    patch.object(gv.subprocess, "run", side_effect=command):
                path, build = env.build(root, ["timing-probe"], source_sha='a' * 40)
            self.assertEqual(path, binary)
            self.assertIn("--locked", calls[0][0])
            self.assertIn("--target-dir", calls[0][0])
            self.assertTrue(all(options["cwd"] == root for _, options in calls))
            self.assertEqual(build["features"], ["timing-probe"])

    def test_launch_uses_only_owned_and_validated_hane_environment(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            config = make_config(Path(tmp), extra_env={'HANE_DEV_CURSOR_DOWN': '32'})
            env = gv.RealEnvironment()
            with patch.dict(os.environ, {'HANE_STATE_DIR': '/other/state', 'HANE_MEASUREMENT_EMPTY': '1',
                    'HANE_METRICS_CSV': '/other/metrics.csv', 'HANE_DEV_CURSOR_DOWN': 'invalid',
                    'GUI_TEST_ORDINARY_ENV': 'preserved'}), patch.object(gv.subprocess, 'Popen') as launch:
                env.launch(Path('/test/hane'), config)
            child_env = launch.call_args.kwargs['env']
            self.assertEqual({key: value for key, value in child_env.items() if key.startswith('HANE_')},
                             {'HANE_STATE_DIR': str(config.state_dir), 'HANE_DEV_CURSOR_DOWN': '32'})
            self.assertEqual(child_env['GUI_TEST_ORDINARY_ENV'], 'preserved')

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

    def test_cursor_overrides_reject_invalid_configuration_before_execution(self):
        import contextlib
        import io
        from unittest.mock import patch
        cases = [('cursor-boundary', 'HANE_CAPTURE_CURSOR_OFFSET', 23),
                 ('cursor-scroll', 'HANE_CAPTURE_CURSOR_DOWN', 40)]
        with tempfile.TemporaryDirectory() as tmp:
            for scenario, variable, maximum in cases:
                for value in ('', 'invalid', '1.5', '-1', str(maximum + 1), '999999999999999999999999'):
                    with self.subTest(variable=variable, value=value), patch.dict(os.environ,
                            {variable: value, 'HANE_GUI_VALIDATE_RUN_DIR': str(Path(tmp) / 'run')}), \
                            patch.object(gv, 'RealEnvironment') as environment, \
                            contextlib.redirect_stderr(io.StringIO()):
                        self.assertEqual(gv.main(['gui_validate.py', scenario]), gv.EXIT_USAGE)
                        environment.assert_not_called()
                        self.assertFalse((Path(tmp) / 'run').exists())
                for value in ('0', str(maximum)):
                    with patch.dict(os.environ, {variable: value}):
                        _, _, extra = gv._scenario_setup(scenario, Path(tmp), prepare=False)
                    self.assertEqual(list(extra.values()), [value])


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


class ExecutionIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_build_time_head_or_source_change_prevents_launch(self):
        from unittest.mock import patch
        for kind in ('head', 'dirty'):
            env = FakeEnvironment(process=FakeProcess(42))
            config = make_config(self.root)
            original = env.build
            def changed_build(*args, **kwargs):
                result = original(*args, **kwargs)
                if kind == 'head':
                    env.head_sha = 'different-head'
                else:
                    env.dirty_paths = [' M crates/hane/src/main.rs']
                return result
            with patch.object(env, 'build', side_effect=changed_build), patch.object(env, 'launch') as launch:
                result = gv.run_validation(env, config)
            self.assertEqual(result['overall_result'], 'blocked')
            launch.assert_not_called()

    def test_transient_working_copy_edits_cannot_enter_snapshot_build(self):
        from unittest.mock import patch
        repo = self.root / 'repo'
        repo.mkdir()
        source = repo / 'source.txt'
        source.write_text('recorded commit input')
        (repo / 'Cargo.toml').write_text('[workspace]\n')
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(repo), '-c', 'user.name=GUI test',
                        '-c', 'user.email=gui-test@example.invalid', '-c', 'commit.gpgsign=false',
                        'commit', '-qm', 'snapshot input'], check=True)
        sha = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
        config = make_config(self.root, workspace_dir=repo, expected_sha=sha)
        env = gv.RealEnvironment()
        original_run = subprocess.run
        compiled_inputs = []
        def command(args, **kwargs):
            if args[0] == 'git':
                return original_run(args, **kwargs)
            if args[0] == 'cargo' and args[1] == 'build':
                source.write_text('transient outside-commit input')
                build_dir = Path(kwargs['cwd'])
                compiled_inputs.append((build_dir / 'source.txt').read_text())
                source.write_text('recorded commit input')
                target = Path(args[args.index('--target-dir') + 1])
                target.mkdir()
                binary = target / 'hane'
                binary.write_bytes(b'private snapshot executable')
                output = json.dumps({'reason': 'compiler-artifact', 'target': {'name': 'hane'}, 'executable': str(binary)})
                return subprocess.CompletedProcess(args, 0, output, '')
            return subprocess.CompletedProcess(args, 0, 'test tool version', '')
        try:
            with patch.object(gv.subprocess, 'run', side_effect=command):
                step, binary, info = gv.do_build(env, config, baseline_sha=sha)
            self.assertEqual(step['result'], 'pass')
            self.assertEqual(compiled_inputs, ['recorded commit input'])
            self.assertEqual(info['source_snapshot_sha'], sha)
            self.assertNotEqual(Path(info['source_snapshot_dir']), repo)
            self.assertTrue(binary.is_file())
        finally:
            env.release_execution()
        self.assertFalse(Path(info['source_snapshot_dir']).exists())
        self.assertEqual(source.read_text(), 'recorded commit input')

    def test_checkout_change_before_build_prevents_cargo_execution(self):
        for head, dirty in [('new-head', []), ('abc123', [' M Cargo.toml'])]:
            env = FakeEnvironment(head_sha=head, dirty_paths=dirty)
            step, binary, _ = gv.do_build(env, make_config(self.root), baseline_sha='abc123')
            self.assertEqual(step['result'], 'blocked')
            self.assertIsNone(binary)
            self.assertEqual(env.build_calls, 0)

    def test_temporary_root_inside_checkout_does_not_dirty_source(self):
        from unittest.mock import patch
        repo = self.root / 'repo'
        repo.mkdir()
        (repo / 'source.txt').write_text('committed input')
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(repo), '-c', 'user.name=GUI test',
                        '-c', 'user.email=gui-test@example.invalid', '-c', 'commit.gpgsign=false',
                        'commit', '-qm', 'snapshot input'], check=True)
        temp_root = repo / 'custom-temp'
        temp_root.mkdir()
        env = gv.RealEnvironment()
        self.assertEqual(env.git_dirty_paths(repo), [])
        try:
            with patch.object(gv.tempfile, 'gettempdir', return_value=str(temp_root)):
                snapshot = env.snapshot_checkout(repo, env.git_head(repo))
            self.assertNotIn(repo.resolve(), snapshot.resolve().parents)
            self.assertEqual(env.git_dirty_paths(repo), [])
            self.assertEqual((snapshot / 'source.txt').read_text(), 'committed input')
        finally:
            env.release_execution()

    def test_failed_build_with_checkout_interference_is_environment_blocked(self):
        from unittest.mock import patch
        for kind in ('head', 'dirty'):
            env = FakeEnvironment(process=FakeProcess(42))
            def failed_build(*args, **kwargs):
                if kind == 'head':
                    env.head_sha = 'changed-head'
                else:
                    env.dirty_paths = [' M Cargo.toml']
                raise gv.BuildError('compilation failed after external change')
            with patch.object(env, 'build', side_effect=failed_build), patch.object(env, 'launch') as launch:
                result = gv.run_validation(env, make_config(self.root))
            self.assertEqual(result['overall_result'], 'blocked')
            launch.assert_not_called()

    def test_aborts_after_cleanup_and_during_publication_cannot_publish_pass(self):
        import contextlib
        import io
        import signal
        from unittest.mock import patch
        for phase in ('after_cleanup', 'during_write', 'after_replace', 'retry_write_failure'):
            with tempfile.TemporaryDirectory() as tmp:
                config = make_config(Path(tmp))
                env = gv.RealEnvironment()
                injected = False
                def interrupt_once():
                    nonlocal injected
                    if not injected:
                        injected = True
                        signal.raise_signal(signal.SIGTERM)
                def validated(*args):
                    env._execution_acquired = True
                    env._finalizing = True
                    if phase == 'after_cleanup':
                        interrupt_once()
                    return {'overall_result': 'pass', 'summary': 'passed', 'steps': []}
                original_write, original_replace = Path.write_text, Path.replace
                def write(path, *args, **kwargs):
                    if phase == 'retry_write_failure' and injected:
                        raise OSError('disk full on interrupted receipt retry')
                    value = original_write(path, *args, **kwargs)
                    if phase == 'during_write' and path.name == '.result.json.tmp':
                        interrupt_once()
                    return value
                def replace(path, *args, **kwargs):
                    value = original_replace(path, *args, **kwargs)
                    if phase in ('after_replace', 'retry_write_failure') and path.name == '.result.json.tmp':
                        interrupt_once()
                    return value
                with patch.object(gv, 'build_config', return_value=config), \
                        patch.object(gv, 'RealEnvironment', return_value=env), \
                        patch.object(gv, 'run_validation', side_effect=validated), \
                        patch.object(env, 'reserve'), patch.object(Path, 'write_text', write), \
                        patch.object(Path, 'replace', replace), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(gv.main(['gui_validate.py', 'editor']), gv.EXIT_BLOCKED)
                if phase == 'retry_write_failure':
                    self.assertFalse((config.run_dir / 'result.json').exists())
                    continue
                proof = json.loads((config.run_dir / 'result.json').read_text())
                self.assertEqual(proof['overall_result'], 'blocked')
                self.assertEqual(proof['steps'][-1]['signals'], [signal.SIGTERM])

    @unittest.skipUnless(hasattr(os, 'getuid'), 'Mac/Unix display lock')
    def test_different_run_directories_still_share_execution_lock(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp, patch.object(gv, 'execution_lock_path', return_value=Path(tmp) / 'shared.lock'):
            first, second = gv.RealEnvironment(), gv.RealEnvironment()
            try:
                first.acquire_execution()
                with self.assertRaises(gv.EnvError):
                    second.acquire_execution()
                first.release_execution()
                second.acquire_execution()
            finally:
                first.release_execution()
                second.release_execution()

    @unittest.skipUnless(hasattr(os, 'getuid'), 'Mac/Unix publication ownership')
    def test_early_failure_keeps_lock_until_publication_finishes(self):
        import contextlib
        import io
        from unittest.mock import patch
        for fail_write in (False, True):
            with self.subTest(fail_write=fail_write), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                config = make_config(root, expected_sha='a' * 40)
                config.state_dir.rmdir()
                config.run_dir.rmdir()
                first, second = gv.RealEnvironment(), gv.RealEnvironment()
                original_publish = gv._publish_result
                def publish(*args):
                    with self.assertRaises(gv.EnvError):
                        second.acquire_execution()
                    write = patch.object(Path, 'write_text', side_effect=OSError('test publication failure')) if fail_write else contextlib.nullcontext()
                    with write:
                        outcome = original_publish(*args)
                    with self.assertRaises(gv.EnvError):
                        second.acquire_execution()
                    return outcome
                with patch.object(gv, 'execution_lock_path', return_value=root / 'shared.lock'), \
                        patch.object(gv, 'build_config', return_value=config), \
                        patch.object(gv, 'RealEnvironment', return_value=first), \
                        patch.object(first, 'git_head', return_value='b' * 40), \
                        patch.object(first, 'git_dirty_paths', return_value=[]), \
                        patch.object(first, 'missing_tools', return_value=[]), \
                        patch.object(gv, '_publish_result', side_effect=publish), \
                        contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    try:
                        self.assertEqual(gv.main(['gui_validate.py', 'editor']), gv.EXIT_BLOCKED)
                        self.assertEqual((config.run_dir / 'result.json').exists(), not fail_write)
                        second.acquire_execution()
                    finally:
                        first.release_execution()
                        second.release_execution()

    @unittest.skipUnless(hasattr(os, 'getuid'), 'Mac/Unix inherited display lock')
    def test_surviving_child_keeps_lock_after_failed_cleanup_and_parent_release(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp, patch.object(gv, 'execution_lock_path', return_value=Path(tmp) / 'shared.lock'):
            root = Path(tmp)
            child_script = root / 'child.py'
            child_script.write_text('import time\ntime.sleep(30)\n')
            config = make_config(root, fixture_path=child_script)
            first, second = gv.RealEnvironment(), gv.RealEnvironment()
            process = None
            try:
                first.acquire_execution()
                process = first.launch(Path(sys.executable), config)
                with patch.object(process, 'terminate'), patch.object(process, 'kill'), \
                        patch.object(process, 'wait', side_effect=subprocess.TimeoutExpired('child', 5)):
                    self.assertEqual(gv.do_cleanup(first, process)['result'], 'blocked')
                first.release_execution()
                with self.assertRaises(gv.EnvError):
                    second.acquire_execution()
                process.kill()
                process.wait(timeout=5)
                second.acquire_execution()
            finally:
                if process is not None and process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)
                first.release_execution()
                second.release_execution()

    @unittest.skipUnless(hasattr(os, 'getuid'), 'Mac/Unix account database')
    def test_execution_lock_identity_ignores_temporary_directory_overrides(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        with patch('pwd.getpwuid', return_value=SimpleNamespace(pw_dir=str(self.root))):
            with patch.dict(os.environ, {'TMPDIR': str(self.root / 'terminal')}):
                first = gv.execution_lock_path()
            with patch.dict(os.environ, {'TMPDIR': str(self.root / 'runner')}):
                second = gv.execution_lock_path()
        self.assertEqual(first, second)
        self.assertEqual(first, self.root / '.cache/hane/gui-validation.lock')

    def test_abort_immediately_after_process_creation_still_cleans_child(self):
        import signal
        from unittest.mock import patch
        process = FakeProcess(42)
        env = FakeEnvironment(process=process)
        def launch(*args):
            signal.raise_signal(signal.SIGTERM)
            return process
        with patch.object(env, 'launch', side_effect=launch):
            result = gv.run_validation(env, make_config(self.root))
        self.assertEqual(result['overall_result'], 'blocked')
        self.assertIsNotNone(process.poll())
        cleanup = next(s for s in result['steps'] if s['name'] == 'cleanup')
        self.assertEqual(cleanup['terminated_pid'], 42)

    def test_abort_after_marker_creation_preserves_ownership_and_result(self):
        import contextlib
        import io
        import signal
        from unittest.mock import patch
        config = make_config(self.root)
        config.state_dir.rmdir()
        env = gv.RealEnvironment()
        env._execution_acquired = True
        original_open = os.open
        def interrupted_open(*args, **kwargs):
            fd = original_open(*args, **kwargs)
            signal.raise_signal(signal.SIGTERM)
            return fd
        with patch.object(gv.os, 'open', side_effect=interrupted_open), self.assertRaises(gv.Aborted):
            env.reserve(config)
        self.assertEqual(env._reserved_run_dir, config.run_dir)
        env._finalizing = True
        result = {'summary': 'blocked by test signal', 'overall_result': 'blocked'}
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(gv._publish_result(env, config, result), gv.EXIT_BLOCKED)
        self.assertEqual(json.loads((config.run_dir / 'result.json').read_text())['overall_result'], 'blocked')

    def test_publication_failure_never_exposes_partial_canonical_json(self):
        import contextlib
        import io
        from unittest.mock import patch
        config = make_config(self.root)
        config.state_dir.rmdir()
        env = gv.RealEnvironment()
        env._execution_acquired = True
        env._finalizing = True
        env.reserve(config)
        result = {'summary': 'pass', 'overall_result': 'pass'}
        with patch.object(Path, 'write_text', side_effect=OSError('test write failure')), \
                contextlib.redirect_stderr(io.StringIO()) as error:
            self.assertEqual(gv._publish_result(env, config, result), gv.EXIT_BLOCKED)
        self.assertFalse((config.run_dir / 'result.json').exists())
        self.assertIn('BLOCKED', error.getvalue())

    def test_execution_lock_loser_cannot_reserve_winners_empty_directory(self):
        import contextlib
        import io
        from unittest.mock import patch
        config = make_config(self.root)
        env = gv.RealEnvironment()
        with patch.object(env, 'acquire_execution', side_effect=gv.EnvError('busy')), \
                patch.object(env, 'reserve') as reserve, contextlib.redirect_stderr(io.StringIO()):
            result = gv.run_validation(env, config)
            self.assertEqual(gv._publish_result(env, config, result), gv.EXIT_BLOCKED)
        reserve.assert_not_called()
        self.assertFalse((config.run_dir / '.gui-validate-owner').exists())
        self.assertFalse((config.run_dir / 'result.json').exists())

    def test_signal_during_cleanup_keeps_result_and_finishes_own_child(self):
        import signal
        process = FakeProcess(42)
        original_wait = process.wait
        def interrupted_wait(*args, **kwargs):
            signal.raise_signal(signal.SIGTERM)
            return original_wait(*args, **kwargs)
        process.wait = interrupted_wait
        env = FakeEnvironment(process=process)
        previous = signal.getsignal(signal.SIGTERM)
        result = gv.run_validation(env, make_config(self.root))
        self.assertEqual(result['overall_result'], 'blocked')
        self.assertIsNotNone(process.poll())
        cleanup = next(s for s in result['steps'] if s['name'] == 'cleanup')
        self.assertEqual(cleanup['signals'], [signal.SIGTERM])
        self.assertEqual(cleanup['process_cleanup']['result'], 'pass')
        self.assertEqual(signal.getsignal(signal.SIGTERM), previous)


if __name__ == "__main__":
    unittest.main()
