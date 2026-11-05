"""Real process -> report -> GitHub source fixtures -> bounded worker staging."""
import copy
import base64
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid
import warnings
import zipfile

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("feedback", SCRIPTS / "aadw_verification_feedback.py")
feedback = importlib.util.module_from_spec(spec)
spec.loader.exec_module(feedback)
H0, H1, MAIN = (c * 40 for c in "abc")
REPO = "o/r"
PREFIX = "repos/o/r"
NOW = "2026-11-05T01:00:00Z"


def handoff(head=H0, category="initial", evidence="https://github.com/o/r/issues/1"):
    return ("@claude\nAADW_COMMANDER_HANDOFF_V3\nAADW_ACTION: implement\n"
            f"AADW_TARGET_HEAD: {head}\nAADW_FAILURE_CLASS: {category}\n"
            f"AADW_ROOT_CAUSE: fixture-initial\nAADW_EVIDENCE: {evidence}\n"
            "固定fixtureの範囲だけを編集してください。\n")


def metadata():
    return dict(repository=REPO, pr_number=1, run_id=12, run_attempt=2, verify_job_id=14,
                workflow_sha=MAIN, source_comment_id=10, actor="owner", source_target_sha=H0,
                verified_sha=H1, base_ref="main", base_sha=MAIN, root_cause="fixture-initial")


def result(kind, code):
    return dict(argv=feedback.COMMANDS[kind], outcome="failure" if code else "success", exit_code=code,
                started_at=NOW, completed_at=NOW, diagnostic_excerpt="fixture assertion" if code else "ok", truncated=False)


def report():
    return feedback.make_report(metadata(), handoff().encode(), {"tests": result("tests", 101), "lint": result("lint", 0)})


def archive(r=None, source=None, extra=None):
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w") as z:
        z.writestr("report.json", json.dumps(r or report(), ensure_ascii=False).encode())
        z.writestr("source-request.txt", source if source is not None else handoff().encode())
        if extra:
            with warnings.catch_warnings():
                warnings.simplefilter('ignore', UserWarning)
                z.writestr(*extra)
    return data.getvalue()


def request():
    return dict(repository=REPO, pr_number=1, target_sha=H1, action="implement", failure_class="test",
                root_cause="newly-observed-test-cause", evidence="https://github.com/o/r/actions/runs/12/job/14",
                feedback={"run_id": 12, "run_attempt": 2})


def capture_log(kind, code):
    token = uuid.uuid4().hex
    data = json.dumps(result(kind, code), separators=(",", ":"), ensure_ascii=False).encode()
    encoded = base64.b64encode(data).decode()
    return (f"::stop-commands::{token}\nfixture diagnostic\n::{token}::\n"
            f"AADW_CAPTURE_RESULT:{token}:{encoded}\n").encode()


def api_data(raw=None, logs=None, test_conclusion="success", lint_conclusion="success"):
    def step(name, conclusion="success"):
        return dict(name=name, status="completed", conclusion=conclusion)
    jobs = [dict(id=i, name=name, status="completed", conclusion="success", steps=[])
            for i, name in enumerate(("prepare", "worker", "finalize"), 20)]
    jobs[1]["steps"] = [step("Confirm Claude used Sonnet 5.5")]
    jobs[2]["steps"] = [step("Revalidate exact remote head and push through trusted finalizer")]
    jobs.append(dict(id=14, name=feedback.VERIFY_JOB, status="completed", conclusion="failure", steps=[
        step("Verify current result context"), step("Check out exact worker result"),
        step("Run workspace regression tests", test_conclusion),
        step("Run lint even when regression tests fail", lint_conclusion),
        step("Require both tests and lint", "failure")]))
    jobs.append(dict(id=15, name=feedback.FEEDBACK_JOB, status="completed", conclusion="success", steps=[
        step("Check out trusted feedback collector"), step("Collect verification feedback from GitHub job logs"),
        step("Upload verification feedback")]))
    return {
        PREFIX + "/pulls/1": dict(number=1, state="open", body="", labels=[],
                                  head=dict(sha=H1, repo=dict(full_name=REPO)), base=dict(ref="main")),
        PREFIX + "/actions/runs/12/attempts/2": dict(id=12, run_attempt=2, status="completed", conclusion="failure",
                    event="issue_comment", path=feedback.WORKFLOW, repository=dict(full_name=REPO),
                    head_repository=dict(full_name=REPO), head_sha=MAIN, actor=dict(login="owner")),
        PREFIX + "/actions/runs/12/attempts/2/jobs?per_page=100": [{"jobs": jobs[:2]}, {"jobs": jobs[2:]}],
        PREFIX + "/actions/jobs/14/logs": logs or capture_log("tests", 101) + capture_log("lint", 0),
        PREFIX + "/actions/runs/12/artifacts?per_page=100": [{"artifacts": []}, {"artifacts": [
            dict(id=15, name="aadw-verification-feedback-12-2", expired=False, workflow_run=dict(id=12))]}],
        PREFIX + "/actions/artifacts/15/zip": raw or archive(),
        PREFIX + "/issues/comments/10": dict(id=10, issue_url="https://api.github.com/repos/o/r/issues/1",
                                              user=dict(login="owner"), body=handoff()),
        PREFIX + "/collaborators/owner/permission": dict(permission="write"),
        PREFIX: dict(default_branch="main"),
        PREFIX + f"/compare/{MAIN}...main": dict(status="identical"),
    }


class SourceTests(unittest.TestCase):
    def consume(self, data=None, req=None):
        data = data if data is not None else api_data()
        return feedback.fetch_feedback(req or request(), lambda endpoint, **kwargs: data[endpoint])

    def test_source_failure_is_usable_despite_continue_on_error_success(self):
        identity, files = self.consume()
        self.assertEqual(identity["verified_sha"], H1)
        self.assertEqual(feedback.decode(files["report.json"])["tests"]["exit_code"], 101)
        # A newly discovered test cause is allowed; historical root is not authority.
        self.assertNotEqual(request()["root_cause"], report()["root_cause"])

    def test_wrong_pr_workflow_attempt_actor_or_head_is_rejected(self):
        for field, value in (("pr_number", 2), ("run_attempt", 1), ("run_id", 99), ("actor", "outsider"),
                             ("verified_sha", H0), ("workflow_sha", H0), ("verify_job_id", 77), ("base_ref", "other")):
            r = report(); r[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.consume(api_data(archive(r)))

    def test_report_outcome_must_match_github_recorded_step_outcome(self):
        r = report(); r["tests"] = result("tests", 0)
        with self.assertRaisesRegex(ValueError, "captured exit disagrees with immutable GitHub step outcome"):
            self.consume(api_data(archive(r)))

    def test_stale_closed_paused_and_foreign_pr(self):
        for change in ({"state": "closed"}, {"body": "<!-- AADW_PAUSED -->"},
                       {"labels": [{"name": "aadw:paused"}]},
                       {"head": {"sha": H0, "repo": {"full_name": REPO}}},
                       {"head": {"sha": H1, "repo": {"full_name": "fork/r"}}}):
            data = api_data(); data[PREFIX + "/pulls/1"].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.consume(data)

    def test_incomplete_or_foreign_run_and_source_job_failure(self):
        for change in ({"status": "in_progress"}, {"event": "pull_request"}, {"path": ".github/workflows/other.yml"},
                       {"run_attempt": 1}, {"head_repository": {"full_name": "fork/r"}}):
            data = api_data(); data[PREFIX + "/actions/runs/12/attempts/2"].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.consume(data)
        for job_name in ("prepare", "worker", "finalize", feedback.VERIFY_JOB):
            data = api_data()
            for page in data[PREFIX + "/actions/runs/12/attempts/2/jobs?per_page=100"]:
                for job in page["jobs"]:
                    if job["name"] == job_name:
                        job["conclusion"] = "cancelled"
            with self.subTest(job=job_name), self.assertRaises(ValueError):
                self.consume(data)

    def test_missing_model_collector_upload_aggregate_or_lint_cannot_resume(self):
        for name in ("Confirm Claude used Sonnet 5.5", "Revalidate exact remote head and push through trusted finalizer",
                     "Collect verification feedback from GitHub job logs", "Upload verification feedback",
                     "Require both tests and lint", "Run lint even when regression tests fail"):
            data = api_data()
            for page in data[PREFIX + "/actions/runs/12/attempts/2/jobs?per_page=100"]:
                for job in page["jobs"]:
                    job["steps"] = [s for s in job["steps"] if s["name"] != name]
            with self.subTest(step=name), self.assertRaises(ValueError):
                self.consume(data)

    def test_feedback_cannot_turn_unknown_or_diagnosis_into_implementation(self):
        for change in ({'failure_class': 'unknown'}, {'failure_class': 'initial'}, {'action': 'diagnose'}):
            req = request(); req.update(change)
            with self.subTest(change=change), self.assertRaises(ValueError): self.consume(req=req)

    def test_expired_missing_duplicated_or_wrong_attempt_artifact(self):
        for variant in ("expired", "missing", "duplicate", "wrong_attempt", "foreign_run", "bad_digest"):
            data = api_data(); artifacts = data[PREFIX + "/actions/runs/12/artifacts?per_page=100"][1]["artifacts"]
            if variant == "expired": artifacts[0]["expired"] = True
            if variant == "missing": artifacts.clear()
            if variant == "duplicate": artifacts.append(copy.deepcopy(artifacts[0]))
            if variant == "wrong_attempt": artifacts[0]["name"] = "aadw-verification-feedback-12-1"
            if variant == "foreign_run": artifacts[0]["workflow_run"]["id"] = 13
            if variant == "bad_digest": artifacts[0]["digest"] = "sha256:" + "0" * 64
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                self.consume(data)

    def test_source_comment_changed_foreign_or_unauthorized_and_untrusted_workflow(self):
        for variant in ("body", "issue_url", "actor", "permission", "history", "evidence"):
            data = api_data(); req = request(); comment = data[PREFIX + "/issues/comments/10"]
            if variant == "body": comment["body"] += "edited"
            if variant == "issue_url": comment["issue_url"] += "0"
            if variant == "actor": comment["user"]["login"] = "other"
            if variant == "permission": data[PREFIX + "/collaborators/owner/permission"]["permission"] = "read"
            if variant == "history": data[PREFIX + f"/compare/{MAIN}...main"]["status"] = "diverged"
            if variant == "evidence": req["evidence"] = "https://github.com/o/r/actions/runs/12"
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                self.consume(data, req)

    def test_missing_capture_or_all_success_is_not_failure_evidence(self):
        for kind in ("success", "unavailable"):
            r = report()
            if kind == "success": r["tests"] = result("tests", 0)
            else:
                r["tests"].update(outcome="unavailable", exit_code=None)
                r["capture_status"] = "unavailable"
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                self.consume(api_data(archive(r)))

    def test_stage_is_exact_and_never_overwrites(self):
        identity, files = self.consume()
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "feedback"
            feedback.stage(directory, files)
            self.assertEqual(set(p.name for p in directory.iterdir()), set(files))
            self.assertEqual(feedback.digest((directory / "report.json").read_bytes()), identity["report_sha256"])
            with self.assertRaises(ValueError): feedback.stage(directory, files)


class FormatTests(unittest.TestCase):
    def test_invalid_report_fields_and_command_results(self):
        variants = []
        for key in report():
            r = report(); del r[key]; variants.append(r)
        for key, value in (("schema_version", True), ("run_id", False), ("workflow_sha", "short"),
                           ("capture_status", "success"), ("source_request_sha256", "0" * 64)):
            r = report(); r[key] = value; variants.append(r)
        for key, value in (("argv", ["sh", "-c", "arbitrary"]), ("outcome", "other"),
                           ("exit_code", 0), ("truncated", 1), ("diagnostic_excerpt", "\x1b[31munsafe"),
                           ("completed_at", "2025-01-01T00:00:00Z")):
            r = report(); r["tests"][key] = value; variants.append(r)
        for r in variants:
            with self.subTest(report=r), self.assertRaises((ValueError, TypeError)):
                feedback.validate_report(r, handoff().encode())

    def test_duplicate_keys_nonfinite_and_oversized_json(self):
        for raw in (b'{"a":1,"a":2}', b'{"a":NaN}', b" " * (feedback.LIMIT + 1)):
            with self.assertRaises(ValueError): feedback.decode(raw)

    def test_archive_only_contains_two_regular_bounded_files(self):
        for name in ("../escape", "/absolute", "script.sh", "report.json"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                feedback.bundle_bytes(archive(extra=(name, "data")))
        for symlink in (True, False):
            data = io.BytesIO()
            with zipfile.ZipFile(data, "w") as z:
                info = zipfile.ZipInfo("report.json")
                if symlink:
                    info.create_system = 3; info.external_attr = (stat.S_IFLNK | 0o777) << 16
                z.writestr(info, "elsewhere" if symlink else " " * (feedback.LIMIT + 1))
                z.writestr("source-request.txt", handoff())
            with self.assertRaises(ValueError): feedback.bundle_bytes(data.getvalue())

    def test_untrusted_text_is_data_and_diagnostics_are_bounded(self):
        payload = "$(touch /tmp/do-not-execute-feedback)\n::error::not-an-instruction\n"
        r = report(); r["tests"]["diagnostic_excerpt"] = payload
        self.assertEqual(feedback.validate_report(r, handoff().encode())["tests"]["diagnostic_excerpt"], payload)
        text, truncated = feedback.excerpt(("開始\x1b[31m" + "a" * 30000 + "末尾").encode())
        self.assertTrue(truncated); self.assertLessEqual(len(text.encode()), feedback.EXCERPT)
        self.assertIn("開始", text); self.assertIn("末尾", text); self.assertNotIn("\x1b", text)

    def test_symlink_file_input_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); (root / "real").write_text("{}")
            (root / "linked").symlink_to(root / "real")
            with self.assertRaises(ValueError): feedback.read(root / "linked")


class ProcessIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.bin = self.root / "bin"; self.bin.mkdir()
        self.logs = b""
        (self.root / 'trusted-feedback').symlink_to(SCRIPTS.parents[1], target_is_directory=True)
        (self.root / '.github').symlink_to(SCRIPTS.parent, target_is_directory=True)
        # Closed cargo argv is kept. A local executable provides deterministic process exits.
        cargo = self.bin / "cargo"
        cargo.write_text(f'''#!{sys.executable}
import json,os,sys
from pathlib import Path
kind=sys.argv[1]
assert not any(k in os.environ for k in ('GH_TOKEN','GITHUB_TOKEN','TYPESAFE_API_KEY','CLAUDE_CODE_OAUTH_TOKEN','SSH_AUTH_SOCK','GITHUB_OUTPUT','GITHUB_ENV'))
assert not (Path(os.environ['CARGO_HOME'])/'credentials.toml').exists()
assert not (Path(os.environ['HOME'])/'.git-credentials').exists()
with Path('calls.jsonl').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')
print('fixture diagnostic '+kind)
sys.exit(int(Path(kind+'.exit').read_text()))
''')
        cargo.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "GH_TOKEN": "sentinel-github", "GITHUB_TOKEN": "sentinel-github",
                    "TYPESAFE_API_KEY": "sentinel-jev", "CLAUDE_CODE_OAUTH_TOKEN": "sentinel-claude",
                    "SSH_AUTH_SOCK": "sentinel-agent", "GITHUB_OUTPUT": "sentinel-output", "GITHUB_ENV": "sentinel-env"}
        old_home = self.root / 'old-home'
        (old_home / '.cargo').mkdir(parents=True)
        (old_home / '.cargo/credentials.toml').write_text('sentinel-cargo')
        (old_home / '.git-credentials').write_text('sentinel-git')
        self.env.update(HOME=str(old_home), CARGO_HOME=str(old_home / '.cargo'))
        self.env.update(GITHUB_WORKSPACE=str(self.root), RUNNER_TEMP=str(self.root))

    def workflow_run(self, name, env=None):
        text = (SCRIPTS.parent / 'workflows/claude-fix.yml').read_text()
        block = text.split('      - name: ' + name + '\n', 1)[1].split('\n      - name:', 1)[0]
        script = block.split('        run: |\n', 1)[1]
        script = '\n'.join(line[10:] if line.startswith('          ') else line for line in script.splitlines())
        self.assertNotIn('${{', script)
        return subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', script], cwd=self.root,
                              env=env or self.env, capture_output=True, text=True)

    def cli(self, *args, env=None):
        return subprocess.run([sys.executable, str(SCRIPTS / "aadw_verification_feedback.py"), *map(str, args)],
                              cwd=self.root, env=env or self.env, capture_output=True, text=True)

    def capture(self, tests, lint):
        (self.root / "test.exit").write_text(str(tests)); (self.root / "clippy.exit").write_text(str(lint))
        codes, logs = [], b""
        for kind in ("tests", "lint"):
            name = 'Run workspace regression tests' if kind == 'tests' else 'Run lint even when regression tests fail'
            done = self.workflow_run(name)
            self.assertNotIn("sentinel-", done.stdout + done.stderr)
            codes.append(done.returncode)
            logs += done.stdout.encode()
        self.logs = logs
        return codes

    def test_four_real_exit_combinations_lint_runs_after_test_failure(self):
        for tests, lint in ((101, 0), (0, 1), (101, 1), (0, 0)):
            with self.subTest(tests=tests, lint=lint):
                self.assertEqual(self.capture(tests, lint), [tests, lint])
                test_result = feedback.result_from_job_log(self.logs, "tests", "failure" if tests else "success")
                lint_result = feedback.result_from_job_log(self.logs, "lint", "failure" if lint else "success")
                r = feedback.make_report(metadata(), handoff().encode(), {"tests": test_result, "lint": lint_result})
                self.assertEqual(r["tests"]["exit_code"], tests); self.assertEqual(r["lint"]["exit_code"], lint)
                self.assertNotIn("sentinel-", json.dumps(r))
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual([c[0] for c in calls], ["test", "clippy"] * 4)

    def install_gh(self, data):
        directory = self.root / "api"; directory.mkdir(exist_ok=True)
        mapping = {}
        for i, (endpoint, value) in enumerate(data.items()):
            file = directory / str(i)
            file.write_bytes(value if isinstance(value, bytes) else json.dumps(value).encode())
            mapping[endpoint] = str(file)
        mapfile = self.root / "api-map.json"; mapfile.write_text(json.dumps(mapping))
        gh = self.bin / "gh"
        gh.write_text(f'''#!{sys.executable}
import json,sys
from pathlib import Path
mapping=json.loads(Path({str(mapfile)!r}).read_text())
sys.stdout.buffer.write(Path(mapping[sys.argv[-1]]).read_bytes())
'''); gh.chmod(0o755)

    def test_cli_producer_report_source_validation_and_worker_read_staging(self):
        self.assertEqual(self.capture(101, 0), [101, 0])
        data = api_data(logs=self.logs); self.install_gh(data)
        env = {**self.env, "REPOSITORY": REPO, "PR_NUMBER": "1", "RUN_ID": "12", "RUN_ATTEMPT": "2",
               "WORKFLOW_SHA": MAIN, "COMMENT_ID": "10", "ACTOR": "owner", "SOURCE_HANDOFF": handoff(),
               "FINAL_SHA": H1, "BASE_REF": "main", "BASE_SHA": MAIN,
               "TESTS_OUTCOME": "failure", "LINT_OUTCOME": "success"}
        produced = self.root / 'verification-feedback'
        forged = self.root / 'verification-results'; forged.mkdir()
        (forged / 'tests.json').write_text(json.dumps(result("tests", 0)))
        done = self.workflow_run('Collect verification feedback from GitHub job logs', env=env)
        self.assertEqual(done.returncode, 0, done.stderr)
        raw = archive(feedback.decode(feedback.read(produced / "report.json")), (produced / "source-request.txt").read_bytes())
        self.install_gh(api_data(raw, logs=self.logs))
        reqfile = self.root / "request.json"; reqfile.write_text(json.dumps(request()))
        expected = self.root / "identity.json"
        done = self.cli("fetch", "--request", reqfile, "--output", expected)
        self.assertEqual(done.returncode, 0, done.stderr)
        stage = self.root / "feedback"
        c1 = handoff(H1, 'test', request()['evidence']) + 'AADW_FEEDBACK_RUN_ID: 12\nAADW_FEEDBACK_RUN_ATTEMPT: 2\n'
        stage_env = {**self.env, 'COMMENT_BODY': c1, 'REPOSITORY': REPO, 'PR_NUMBER': '1',
                     'FEEDBACK_IDENTITY': expected.read_text()}
        done = self.workflow_run('Stage exact verification feedback as untrusted worker data', env=stage_env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual((stage / "source-request.txt").read_text(), handoff())
        self.assertEqual(feedback.decode(feedback.read(stage / "report.json"))["tests"]["exit_code"], 101)
        bad = feedback.decode(feedback.read(expected)); bad["artifact_id"] = 999; expected.write_text(json.dumps(bad))
        done = self.cli("fetch", "--request", reqfile, "--expected", expected, "--output", self.root / "bad.json", "--stage", self.root / "must-not-stage")
        self.assertNotEqual(done.returncode, 0); self.assertFalse((self.root / "must-not-stage").exists())

    def test_collector_refuses_mutable_result_that_disagrees_with_github_step_outcome(self):
        self.capture(101, 0)
        self.install_gh(api_data(logs=self.logs, test_conclusion="success"))
        env = {**self.env, "REPOSITORY": REPO, "PR_NUMBER": "1", "RUN_ID": "12", "RUN_ATTEMPT": "2",
               "WORKFLOW_SHA": MAIN, "COMMENT_ID": "10", "ACTOR": "owner", "SOURCE_HANDOFF": handoff(),
               "FINAL_SHA": H1, "BASE_REF": "main", "BASE_SHA": MAIN,
               "TESTS_OUTCOME": "success", "LINT_OUTCOME": "success"}
        dest = self.root / 'mismatch-output'
        done = self.workflow_run('Collect verification feedback from GitHub job logs', env=env)
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse((self.root / 'verification-feedback').exists())

    def test_actual_finalizer_scope_rejects_protected_change_even_with_feedback(self):
        text = (SCRIPTS.parent / 'workflows/claude-fix.yml').read_text()
        finalizer = text.split('\n  finalize:', 1)[1].split('\n  verify:', 1)[0]
        start = finalizer.index('          changed=false\n')
        end_text = '          done < <(git -C final-head diff --cached --no-renames --name-only -z)'
        end = finalizer.index(end_text, start) + len(end_text)
        scope = '\n'.join(line[10:] for line in finalizer[start:end].splitlines())
        target = self.root / 'final-head'
        target.mkdir()
        for args in (['init', '-q'], ['config', 'user.name', 'fixture'], ['config', 'user.email', 'fixture@example.invalid']):
            subprocess.run(['git', '-C', str(target), *args], check=True, capture_output=True)
        (target / 'baseline').write_text('fixture')
        subprocess.run(['git', '-C', str(target), 'add', 'baseline'], check=True)
        subprocess.run(['git', '-C', str(target), 'commit', '-qm', 'fixture'], check=True)
        (target / '.github').mkdir(); (target / '.github/forbidden.yml').write_text('untrusted')
        subprocess.run(['git', '-C', str(target), 'add', '.github/forbidden.yml'], check=True)
        done = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', scope], cwd=self.root,
                              env={**self.env, 'FEEDBACK_IDENTITY': '{}', 'U2_RELEASE_SCOPE': 'false'}, capture_output=True, text=True)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn('trusted authority or workflow path', done.stdout)


if __name__ == "__main__":
    unittest.main()
