#!/usr/bin/env python3
"""Bounded verification data transport. Never choose an action or mutate GitHub."""
from __future__ import annotations

import argparse
import base64
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import uuid
import zipfile

from aadw_action_contract import parse_request

LIMIT = 65536
EXCERPT = 12288
WORKFLOW = ".github/workflows/claude-fix.yml"
VERIFY_JOB = "Verify exact worker result without write credentials"
FEEDBACK_JOB = "Collect verification feedback from completed verifier logs"
COMMANDS = {
    "tests": ["cargo", "test", "--workspace", "--all-features", "--locked"],
    "lint": ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings"],
}
RESULT_KEYS = {"argv", "outcome", "exit_code", "started_at", "completed_at", "diagnostic_excerpt", "truncated"}
REPORT_KEYS = {"schema_version", "repository", "pr_number", "run_id", "run_attempt", "verify_job_id",
               "workflow_path", "workflow_sha", "source_comment_id", "source_request_sha256", "actor",
               "source_target_sha", "verified_sha", "base_ref", "base_sha", "root_cause", "tests", "lint", "capture_status"}
ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
SECRET = re.compile(r"(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-ant-[A-Za-z0-9_-]{20,}|sk-proj-[A-Za-z0-9_-]{20,})")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def positive(value):
    return type(value) is int and value > 0


def sha(value, length=40):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{%d}" % length, value) is not None


def digest(data):
    return hashlib.sha256(data).hexdigest()


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def decode(data, limit=LIMIT):
    require(len(data) <= limit, "oversized JSON")
    return json.loads(data.decode("utf-8"), object_pairs_hook=no_duplicates,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError("non-finite JSON")))


def read(path, limit=LIMIT):
    path = Path(path)
    require(not path.is_symlink() and path.is_file() and path.stat().st_size <= limit, "missing or unsafe bounded file")
    return path.read_bytes()


def write_json(path, data):
    encoded = (json.dumps(data, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode()
    require(len(encoded) <= LIMIT, "oversized report")
    path = Path(path)
    require(not path.is_symlink(), "unsafe output")
    path.write_bytes(encoded)


def utc():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def timestamp(value):
    require(isinstance(value, str) and value.endswith("Z"), "UTC timestamp required")
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return result


def clean(text):
    text = ANSI.sub("", text)
    return "".join(c for c in text if c in "\n\t" or (ord(c) >= 32 and ord(c) != 127))


def excerpt(data):
    raw = clean(data.decode("utf-8", errors="replace")).encode()
    truncated = len(raw) > EXCERPT
    if truncated:
        raw = raw[:EXCERPT // 2 - 32] + b"\n[diagnostic truncated]\n" + raw[-EXCERPT // 2 + 32:]
    return raw.decode("utf-8", errors="ignore"), truncated


def safe_environment(runtime: Path, environ=None):
    """Keep toolchain data/cache, never credentials or GitHub control files."""
    original = os.environ if environ is None else environ
    runtime = runtime.resolve()
    runtime.mkdir(parents=True, exist_ok=True)
    home, cargo, temp = (runtime / n for n in ("home", "cargo", "tmp"))
    for directory in (home, cargo, temp):
        require(not directory.is_symlink(), "unsafe runtime directory")
        directory.mkdir(exist_ok=True)
    old_home = Path(original.get("HOME", str(Path.home())))
    old_cargo = Path(original.get("CARGO_HOME", str(old_home / ".cargo")))
    for name in ("registry", "git"):
        cached = old_cargo / name
        link = cargo / name
        if cached.is_dir() and not link.exists():
            link.symlink_to(cached.resolve(), target_is_directory=True)
    env = {key: original[key] for key in ("PATH", "DEVELOPER_DIR", "SDKROOT", "MACOSX_DEPLOYMENT_TARGET") if key in original}
    env.update(HOME=str(home), CARGO_HOME=str(cargo), TMPDIR=str(temp), TMP=str(temp), TEMP=str(temp),
               RUSTUP_HOME=original.get("RUSTUP_HOME", str(old_home / ".rustup")),
               LANG="en_US.UTF-8", LC_ALL="en_US.UTF-8", CARGO_TERM_COLOR="never")
    return env


def capture(kind, directory, runtime, *, environ=None):
    """Execute one closed command and preserve its real exit, including failure."""
    require(kind in COMMANDS, "unknown verification command")
    started = utc()
    env = safe_environment(Path(runtime), environ)
    argv = COMMANDS[kind]
    token = uuid.uuid4().hex
    # PR stdout is data, not GitHub workflow commands.
    print(f"::stop-commands::{token}", flush=True)
    first, tail = bytearray(), bytearray()
    total = 0
    code = None
    try:
        proc = subprocess.Popen(argv, cwd=directory, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        while True:
            chunk = proc.stdout.read(4096)
            if not chunk:
                break
            total += len(chunk)
            if len(first) < EXCERPT:
                first.extend(chunk[:EXCERPT - len(first)])
            tail.extend(chunk)
            del tail[:-EXCERPT]
            print(clean(chunk.decode("utf-8", errors="replace")), end="", flush=True)
        code = proc.wait()
    except OSError as exc:
        first.extend(str(exc).encode()[:EXCERPT])
    finally:
        print(f"\n::{token}::", flush=True)
    data = bytes(first) if total <= EXCERPT else bytes(first) + b"\n[log omitted]\n" + bytes(tail)
    diagnostic, truncated = excerpt(data)
    require(not SECRET.search(diagnostic), "credential-like diagnostic cannot be exported")
    result = {"argv": argv, "outcome": "unavailable" if code is None else ("success" if code == 0 else "failure"),
              "exit_code": code, "started_at": started, "completed_at": utc(),
              "diagnostic_excerpt": diagnostic, "truncated": truncated or total > EXCERPT}
    # The later collector reads this from the completed GitHub job log. The
    # nonce is generated by this trusted process and is never passed to Cargo.
    encoded = base64.b64encode((json.dumps(result, separators=(",", ":"), ensure_ascii=False) + "\n").encode()).decode()
    print(f"AADW_CAPTURE_RESULT:{token}:{encoded}", flush=True)
    return code if code is not None and code >= 0 else 1


def result_from_job_log(raw, kind, outcome):
    """Read the trusted post-command capture record from immutable Actions logs."""
    require(kind in COMMANDS and outcome in {"success", "failure"}, "invalid verification step evidence")
    text = raw.decode("utf-8", errors="replace")
    matches = []
    for start in re.finditer(r"::stop-commands::([0-9a-f]{32})", text):
        token = start.group(1)
        end = text.find(f"::{token}::", start.end())
        if end < 0:
            continue
        tail = text[end + len(token) + 4:]
        record = re.search(rf"(?m)^[^\n]*AADW_CAPTURE_RESULT:{token}:([A-Za-z0-9+/=]+)\s*$", tail)
        if record:
            candidate = decode(base64.b64decode(record.group(1), validate=True))
            if isinstance(candidate, dict) and candidate.get("argv") == COMMANDS[kind]:
                matches.append(candidate)
    require(len(matches) == 1, "trusted capture result is missing or ambiguous in Actions logs")
    result = matches[0]
    validate_result(result, kind)
    require(result["outcome"] == outcome, "captured exit disagrees with immutable GitHub step outcome")
    return result


def validate_result(result, kind):
    require(isinstance(result, dict) and set(result) == RESULT_KEYS, "invalid command result schema")
    require(result["argv"] == COMMANDS[kind], "verification command changed")
    code, outcome = result["exit_code"], result["outcome"]
    require(outcome in {"success", "failure", "unavailable"}, "invalid outcome")
    require((outcome == "unavailable" and code is None) or
            (type(code) is int and ((outcome == "success" and code == 0) or (outcome == "failure" and code != 0))),
            "exit/outcome mismatch")
    require(timestamp(result["completed_at"]) >= timestamp(result["started_at"]), "invalid execution interval")
    text = result["diagnostic_excerpt"]
    require(isinstance(text, str) and len(text.encode()) <= EXCERPT and clean(text) == text, "unsafe diagnostic")
    require(not SECRET.search(text) and type(result["truncated"]) is bool, "unsafe diagnostic metadata")


def validate_report(report, source):
    require(isinstance(report, dict) and set(report) == REPORT_KEYS, "invalid report schema")
    require(type(report["schema_version"]) is int and report["schema_version"] == 1, "unknown report schema")
    for key in ("pr_number", "run_id", "run_attempt", "verify_job_id", "source_comment_id"):
        require(positive(report[key]), "invalid report identifier")
    for key in ("workflow_sha", "source_target_sha", "verified_sha", "base_sha"):
        require(sha(report[key]), "invalid report SHA")
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", report["repository"]) is not None, "invalid repository")
    require(report["workflow_path"] == WORKFLOW, "invalid source workflow")
    require(isinstance(report["actor"], str) and re.fullmatch(r"[A-Za-z0-9_-]+", report["actor"]), "invalid actor")
    require(isinstance(report["base_ref"], str) and 0 < len(report["base_ref"]) <= 255, "invalid base ref")
    require(len(source) <= LIMIT and not SECRET.search(source.decode("utf-8")), "unsafe source request")
    require(report["source_request_sha256"] == digest(source), "source request digest mismatch")
    parsed = parse_request(source.decode("utf-8"))
    require(parsed["action"] == "implement" and parsed["target_sha"] == report["source_target_sha"]
            and parsed["root_cause"] == report["root_cause"], "source request identity mismatch")
    for kind in COMMANDS:
        validate_result(report[kind], kind)
    status = "complete" if all(report[k]["outcome"] != "unavailable" for k in COMMANDS) else "unavailable"
    require(report["capture_status"] == status, "invalid capture status")
    return report


def gh_api(endpoint, *, pages=False, binary=False, operation="source API"):
    cmd = ["gh", "api"]
    if pages:
        cmd += ["--paginate", "--slurp"]
    if binary:
        # job logはANSI制御文字を含み得る。ghの既定拒否を外してraw bytesを捕捉し、返却前に除去する。
        cmd.append("--allow-escape-sequences")
    result = subprocess.run(cmd + [endpoint], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace")
        status = re.search(r"\bHTTP(?:/[0-9.]+)?\s+([1-5][0-9]{2})\b", stderr, re.IGNORECASE)
        detail = f"HTTP {status.group(1)}" if status else f"HTTP status unavailable; gh exit {result.returncode}"
        raise ValueError(f"GitHub source could not be read: {operation} ({detail})")
    require(len(result.stdout) <= 16 * 1024 * 1024, "oversized GitHub source")
    return result.stdout if binary else decode(result.stdout, 16 * 1024 * 1024)


def make_report(metadata, source, results):
    report = dict(metadata)
    report.update(schema_version=1, workflow_path=WORKFLOW, source_request_sha256=digest(source))
    for kind in COMMANDS:
        if kind in results:
            report[kind] = results[kind]
        else:
            report[kind] = {"argv": COMMANDS[kind], "outcome": "unavailable", "exit_code": None,
                            "started_at": utc(), "completed_at": utc(), "diagnostic_excerpt": "検証結果を取得できませんでした。", "truncated": False}
    report["capture_status"] = "complete" if all(report[k]["outcome"] != "unavailable" for k in COMMANDS) else "unavailable"
    return validate_report(report, source)


def bundle_bytes(data):
    require(len(data) <= 2 * LIMIT + 8192, "oversized feedback archive")
    result = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        items = archive.infolist()
        require(len(items) == 2 and {i.filename for i in items} == {"report.json", "source-request.txt"}, "unexpected archive members")
        for item in items:
            require(item.file_size <= LIMIT and not stat.S_ISLNK(item.external_attr >> 16)
                    and not item.is_dir() and not item.flag_bits & 1, "unsafe archive member")
            result[item.filename] = archive.read(item)
    report = validate_report(decode(result["report.json"]), result["source-request.txt"])
    return report, result


def one_job(jobs, name):
    matches = [j for j in jobs if j.get("name") == name]
    require(len(matches) == 1, "source job is missing or ambiguous")
    return matches[0]


def step(job, name, conclusion):
    steps = [s for s in job.get("steps", []) if s.get("name") == name]
    require(len(steps) == 1 and steps[0].get("status") == "completed" and steps[0].get("conclusion") == conclusion,
            "source verification step is missing or incomplete")


def fetch_feedback(request, api=gh_api):
    """Read authoritative GitHub facts and validate the selected immutable bundle."""
    require(request.get("action") == "implement" and request.get("failure_class") in {"product", "test"}
            and sha(request.get("target_sha")), "feedback requires a current product/test implementation request")
    repo, pr_number = request["repository"], request["pr_number"]
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) is not None and positive(pr_number), "invalid target")
    ids = request.get("feedback")
    require(isinstance(ids, dict) and set(ids) == {"run_id", "run_attempt"} and all(positive(v) for v in ids.values()), "feedback source required")
    prefix = f"repos/{repo}"
    pr = api(f"{prefix}/pulls/{pr_number}")
    require(pr.get("state") == "open" and pr.get("head", {}).get("repo", {}).get("full_name") == repo
            and pr["head"].get("sha") == request["target_sha"], "stale or foreign target")
    require("<!-- AADW_PAUSED -->" not in (pr.get("body") or "") and
            "aadw:paused" not in [l.get("name") for l in pr.get("labels", [])], "target is paused")
    run_id, attempt = ids["run_id"], ids["run_attempt"]
    run = api(f"{prefix}/actions/runs/{run_id}/attempts/{attempt}")
    require(run.get("id") == run_id and run.get("run_attempt") == attempt and run.get("status") == "completed" and run.get("conclusion") == "failure"
            and run.get("event") == "issue_comment" and run.get("path") == WORKFLOW
            and run.get("repository", {}).get("full_name") == repo
            and run.get("head_repository", {}).get("full_name") == repo, "invalid source run")
    pages = api(f"{prefix}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100", pages=True)
    jobs = [job for page in pages for job in page["jobs"]]
    for name in ("prepare", "worker", "finalize"):
        job = one_job(jobs, name)
        require(job.get("status") == "completed" and job.get("conclusion") == "success", "source implementation did not succeed")
    step(one_job(jobs, "worker"), "Confirm Claude used Sonnet 5.5", "success")
    step(one_job(jobs, "finalize"), "Revalidate exact remote head and push through trusted finalizer", "success")
    verification = one_job(jobs, VERIFY_JOB)
    require(verification.get("status") == "completed" and verification.get("conclusion") == "failure", "source did not finish with a verification failure")
    for name in ("Verify current result context", "Check out exact worker result"):
        step(verification, name, "success")
    step(verification, "Require both tests and lint", "failure")
    for name in ("Run workspace regression tests", "Run lint even when regression tests fail"):
        # continue-on-error normalizes conclusion to success; require execution,
        # then compare the report against the trusted helper record in job logs.
        matches = [s for s in verification.get("steps", []) if s.get("name") == name]
        require(len(matches) == 1 and matches[0].get("status") == "completed"
                and matches[0].get("conclusion") == "success", "source command was not executed")
    collector = one_job(jobs, FEEDBACK_JOB)
    require(collector.get("status") == "completed" and collector.get("conclusion") == "success",
            "trusted feedback collector did not complete")
    for name in ("Collect verification feedback from GitHub job logs", "Upload verification feedback"):
        step(collector, name, "success")
    pages = api(f"{prefix}/actions/runs/{run_id}/artifacts?per_page=100", pages=True)
    name = f"aadw-verification-feedback-{run_id}-{attempt}"
    artifacts = [a for page in pages for a in page["artifacts"] if a.get("name") == name]
    require(len(artifacts) == 1, "source artifact is missing or ambiguous")
    artifact = artifacts[0]
    require(positive(artifact.get("id")) and artifact.get("expired") is False
            and artifact.get("workflow_run", {}).get("id") == run_id, "invalid or expired artifact")
    raw = api(f"{prefix}/actions/artifacts/{artifact['id']}/zip", binary=True)
    if artifact.get("digest"):
        require(artifact["digest"] == "sha256:" + digest(raw), "artifact download digest mismatch")
    report, files = bundle_bytes(raw)
    for key, value in (("repository", repo), ("pr_number", pr_number), ("run_id", run_id), ("run_attempt", attempt),
                       ("verify_job_id", verification["id"]), ("verified_sha", request["target_sha"]),
                       ("workflow_sha", run["head_sha"]), ("actor", run.get("actor", {}).get("login")),
                       ("base_ref", pr.get("base", {}).get("ref"))):
        require(report[key] == value, "source report identity mismatch: " + key)
    source_logs = api(f"{prefix}/actions/jobs/{verification['id']}/logs", binary=True)
    for kind in COMMANDS:
        logged_result = result_from_job_log(source_logs, kind, report[kind]["outcome"])
        require(logged_result == report[kind], "source report disagrees with immutable GitHub job logs")
    require(report["capture_status"] == "complete" and any(report[k]["outcome"] == "failure" for k in COMMANDS), "source has no usable test/lint failure")
    comment = api(f"{prefix}/issues/comments/{report['source_comment_id']}")
    require(comment.get("issue_url") == f"https://api.github.com/{prefix}/issues/{pr_number}"
            and comment.get("user", {}).get("login") == report["actor"]
            and comment.get("body", "").encode() == files["source-request.txt"], "source comment changed or belongs to another PR")
    require(api(f"{prefix}/collaborators/{report['actor']}/permission").get("permission") in {"write", "maintain", "admin"}, "source requester is no longer authorized")
    default = api(prefix)["default_branch"]
    require(re.fullmatch(r"[A-Za-z0-9_./-]+", default) is not None, "invalid default branch")
    compare = api(f"{prefix}/compare/{report['workflow_sha']}...{default}")
    require(compare.get("status") in {"ahead", "identical"}, "source workflow is not in trusted default history")
    require(request["evidence"] == f"https://github.com/{repo}/actions/runs/{run_id}/job/{verification['id']}", "feedback evidence must identify its exact verification job")
    identity = {"artifact_id": artifact["id"], "report_sha256": digest(files["report.json"]),
                "source_request_sha256": digest(files["source-request.txt"]), "verified_sha": report["verified_sha"],
                "run_id": run_id, "run_attempt": attempt}
    return identity, files


def stage(directory, files):
    directory = Path(directory)
    require(not directory.exists() and not directory.is_symlink(), "feedback destination already exists")
    directory.mkdir()
    for name in ("report.json", "source-request.txt"):
        (directory / name).write_bytes(files[name])


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    cap = commands.add_parser("capture")
    cap.add_argument("kind", choices=COMMANDS)
    cap.add_argument("--directory", type=Path, required=True)
    cap.add_argument("--runtime", type=Path, required=True)
    collect = commands.add_parser("collect")
    collect.add_argument("--output", type=Path, required=True)
    fetch = commands.add_parser("fetch")
    fetch.add_argument("--request", type=Path, required=True)
    fetch.add_argument("--output", type=Path, required=True)
    fetch.add_argument("--expected", type=Path)
    fetch.add_argument("--stage", type=Path)
    args = parser.parse_args()
    if args.command == "capture":
        return capture(args.kind, args.directory, args.runtime)
    if args.command == "collect":
        e = os.environ
        source = e["SOURCE_HANDOFF"].encode()
        parsed = parse_request(source.decode())
        repo = e["REPOSITORY"]
        run_id, attempt = int(e["RUN_ID"]), int(e["RUN_ATTEMPT"])
        pages = gh_api(f"repos/{repo}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100",
                       pages=True, operation="attempt jobs list")
        verification = one_job([j for p in pages for j in p["jobs"]], VERIFY_JOB)
        require(verification.get("status") == "completed" and verification.get("conclusion") == "failure",
                "verification job did not finish with a failure")
        step(verification, "Require both tests and lint", "failure")
        metadata = {"repository": repo, "pr_number": int(e["PR_NUMBER"]), "run_id": run_id,
                    "run_attempt": attempt, "verify_job_id": verification["id"], "workflow_sha": e["WORKFLOW_SHA"],
                    "source_comment_id": int(e["COMMENT_ID"]), "actor": e["ACTOR"],
                    "source_target_sha": parsed["target_sha"], "root_cause": parsed["root_cause"],
                    "verified_sha": e["FINAL_SHA"], "base_ref": e["BASE_REF"], "base_sha": e["BASE_SHA"]}
        results = {}
        log_bytes = gh_api(f"repos/{repo}/actions/jobs/{verification['id']}/logs",
                           binary=True, operation="verifier job logs")
        for kind, step_name in (("tests", "Run workspace regression tests"),
                                ("lint", "Run lint even when regression tests fail")):
            matches = [s for s in verification.get("steps", []) if s.get("name") == step_name]
            require(len(matches) == 1 and matches[0].get("status") == "completed"
                    and matches[0].get("conclusion") == "success", "verification step is missing")
            outcome = e.get(kind.upper() + "_OUTCOME", "")
            results[kind] = result_from_job_log(log_bytes, kind, outcome)
        report = make_report(metadata, source, results)
        require(not args.output.exists(), "feedback output already exists")
        args.output.mkdir()
        write_json(args.output / "report.json", report)
        (args.output / "source-request.txt").write_bytes(source)
        print("検証結果を保存しました。合否・追加修正はCommanderが確認します。")
    else:
        request = decode(read(args.request))
        identity, files = fetch_feedback(request)
        if args.expected:
            require(identity == decode(read(args.expected)), "feedback source changed after prepare")
        if args.stage:
            stage(args.stage, files)
        write_json(args.output, identity)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError, zipfile.BadZipFile, json.JSONDecodeError) as exc:
        print("検証結果の生成・受取を停止しました: " + str(exc), file=sys.stderr)
        sys.exit(1)
