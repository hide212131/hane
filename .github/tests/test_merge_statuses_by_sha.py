"""Regression coverage for the claude-fix.yml jq argv overflow (run 34253902865).

Root cause: "Resolve automatic fix" accumulated every commit's GitHub status
array into a single `statuses_by_sha` value and handed the whole thing to
`jq --argjson` (and `jq -n --argjson commits ... --argjson statuses ...`).
Both flags splice the JSON text into the jq child process's execve() argv.
Linux caps a single argv string at MAX_ARG_STRLEN (128 KiB via
/proc/sys/kernel/... internals -- 32 * PAGE_SIZE), so once a long-lived PR's
combined event log crossed that size the controller step failed closed with
"/usr/bin/jq: Argument list too long" before Claude ever ran.

The fix (.github/scripts/merge_statuses_by_sha.sh, used from
claude-fix.yml) keeps every accumulated payload file-backed: commit and
status arrays are read by jq via a positional filename or --slurpfile,
which streams the file directly instead of encoding it as an argv string.
That has no comparable size bound.
"""
import errno
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "merge_statuses_by_sha.sh"

# Comfortably over the 128 KiB single-argv-string limit that broke run 34253902865.
OVER_ARGV_LIMIT_BYTES = 200 * 1024

# Large enough to overflow total argv+environ on every common platform (not
# just Linux's 128 KiB per-argument cap), so the "old implementation fails"
# test below is portable across dev machines and CI runners.
OVER_TOTAL_ARGV_BYTES = 2 * 1024 * 1024


def status_batch(target_bytes, context="hane/padding-context"):
    """A single commit's status array, padded past target_bytes when serialized."""
    statuses = []
    approx = 0
    i = 0
    while approx < target_bytes:
        statuses.append({
            "id": i,
            "context": f"{context}-{i}",
            "state": "success",
            "description": "x" * 200,
            "created_at": "2026-09-09T00:00:00Z",
        })
        approx += 260
        i += 1
    return statuses


class MergeStatusesByShaTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def write_commits_and_statuses(self, shas_to_statuses):
        commits_file = self.root / "commits.json"
        commits_file.write_text(json.dumps([{"sha": sha} for sha in shas_to_statuses]))
        statuses_dir = self.root / "statuses"
        statuses_dir.mkdir()
        for sha, statuses in shas_to_statuses.items():
            (statuses_dir / f"{sha}.json").write_text(json.dumps(statuses))
        return commits_file, statuses_dir

    def run_merge(self, commits_file, statuses_dir, output_file):
        return subprocess.run(
            ["bash", str(SCRIPT), str(commits_file), str(statuses_dir), str(output_file)],
            capture_output=True, text=True, timeout=30,
        )

    def test_oversized_single_commit_payload_merges_via_file_backed_helper(self):
        big = status_batch(OVER_ARGV_LIMIT_BYTES)
        self.assertGreater(len(json.dumps(big)), 128 * 1024,
                            "fixture must exceed the 128 KiB argv limit to be a meaningful regression check")
        sha = "a" * 40
        commits_file, statuses_dir = self.write_commits_and_statuses({sha: big})
        output_file = self.root / "statuses_by_sha.json"

        result = self.run_merge(commits_file, statuses_dir, output_file)

        self.assertEqual(result.returncode, 0, result.stderr)
        merged = json.loads(output_file.read_text())
        self.assertEqual(merged[sha], big)

    def test_oversized_total_payload_across_many_ordinary_commits_merges(self):
        # A single commit's status log rarely hits 128 KiB on its own; the
        # historical failure came from accumulating many ordinary-sized
        # commits into one growing map across the whole PR's event log.
        shas_to_statuses = {
            f"{n:040x}": status_batch(20000)
            for n in range(8)
        }
        self.assertGreater(len(json.dumps(shas_to_statuses)), 128 * 1024)
        commits_file, statuses_dir = self.write_commits_and_statuses(shas_to_statuses)
        output_file = self.root / "statuses_by_sha.json"

        result = self.run_merge(commits_file, statuses_dir, output_file)

        self.assertEqual(result.returncode, 0, result.stderr)
        merged = json.loads(output_file.read_text())
        self.assertEqual(merged, shas_to_statuses)

    def test_empty_event_log_merges_to_empty_map(self):
        commits_file, statuses_dir = self.write_commits_and_statuses({})
        output_file = self.root / "statuses_by_sha.json"

        result = self.run_merge(commits_file, statuses_dir, output_file)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(output_file.read_text()), {})

    def test_old_argjson_form_fails_closed_on_a_sufficiently_oversized_payload(self):
        """Reproduces the run 34253902865 root cause directly against jq.

        Uses a payload well past 128 KiB (Linux's per-argument cap) so the
        failure also reproduces on platforms with a larger single-argument
        allowance, which only bound total argv+environ instead.
        """
        big_json = json.dumps(status_batch(OVER_TOTAL_ARGV_BYTES))
        self.assertGreater(len(big_json), OVER_TOTAL_ARGV_BYTES)

        try:
            result = subprocess.run(
                ["jq", "--arg", "sha", "a" * 40, "--argjson", "statuses", big_json,
                 '. + {($sha): $statuses}'],
                input="{}", capture_output=True, text=True, timeout=30,
            )
        except OSError as exc:
            # E2BIG ("Argument list too long") raised by execve before jq even started.
            self.assertEqual(exc.errno, errno.E2BIG)
            return
        self.assertNotEqual(
            result.returncode, 0,
            "expected the legacy --argjson invocation to fail once the payload "
            "exceeds the platform's argv size limit, matching run 34253902865",
        )

    def test_large_evidence_reaches_claude_on_stdin_without_an_agent_call(self):
        workflow = (ROOT / "workflows" / "claude-fix.yml").read_text()
        step = workflow.split("      - name: Run Claude Code fix\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0])
        evidence = json.dumps({"review_body": "x" * (3 * 1024 * 1024)})
        (self.root / "hane-claude-fix-evidence.json").write_text(evidence)
        mock = self.root / "claude"
        mock.write_text('#!/bin/sh\ncat > "$RUNNER_TEMP/received-prompt.txt"\n')
        mock.chmod(0o700)
        environment = {**os.environ, "PATH": str(self.root) + os.pathsep + os.environ["PATH"],
                       "RUNNER_TEMP": str(self.root), "GITHUB_OUTPUT": str(self.root / "output"),
                       "PR_NUMBER": "79", "TARGET_SHA": "a" * 40}
        result = subprocess.run(["bash", "-c", script], env=environment,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        received = (self.root / "received-prompt.txt").read_text()
        self.assertIn(evidence, received)
        self.assertIn("exact reviewed commit " + "a" * 40, received)
        self.assertEqual((self.root / "output").read_text(), "exit_code=0\n")


if __name__ == "__main__":
    unittest.main()
