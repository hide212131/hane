"""Failure/interleaving regressions from PR #82; no network or paid agent calls."""

from datetime import datetime, timedelta, timezone
import importlib.util
import itertools
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "claude_fix_state.py"
SPEC = importlib.util.spec_from_file_location("claude_fix_state", SCRIPT)
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)
SHA = "a" * 40
SHORT = SHA[:12]
NOW = datetime(2026, 9, 9, tzinfo=timezone.utc)
A = "hane/claude-fix-manual/101-1"
B = "hane/claude-fix-manual/102-1"


def status(id, context, state, description, age=4000):
    return dict(id=id, context=context, state=state, description=description,
                created_at=(NOW - timedelta(seconds=age)).isoformat())


def grant(id=1, context=A):
    return status(id, context, "success", f"Claude manual retry authorized for {SHORT}")


def claim(id=2, context=A):
    return status(id, context, "pending", f"Claude manual retry claimed for {SHORT}")


def consume(id=3, context=A):
    return status(id, context, "error", f"Claude manual retry consumed for {SHORT}")


def route(id=20, kind="fix"):
    return status(id, "hane/copilot-routing", "failure" if kind == "fix" else "error",
                  f"Copilot routing: {kind} for {SHORT}")


def execution(kind, age=4000):
    return status(30, "hane/claude-fix", "error", f"Claude fix {kind} for {SHORT}", age)


class AuthorizationTests(unittest.TestCase):
    def test_old_consumption_never_masks_new_grant_regardless_of_api_order(self):
        for rows in itertools.permutations([grant(1), grant(2, B), consume(3)]):
            self.assertFalse(policy.authorization_valid(rows, SHA, A))
            self.assertTrue(policy.authorization_valid(rows, SHA, B))
            self.assertEqual(policy.recovery([route(), *rows], SHA, NOW)["manual_retry_context"], B)

    def test_claim_is_not_a_reusable_grant(self):
        self.assertFalse(policy.authorization_valid([grant(), claim()], SHA, A))

    def test_unrelated_or_wrong_sha_evidence_cannot_authorize(self):
        for row in [grant(context="hane/claude-fix-manual/not-an-id"),
                    dict(grant(), description="Claude manual retry authorized for other"),
                    dict(grant(), state="pending")]:
            self.assertEqual(policy.authorizations([row], SHA), [])

    def test_authorizations_survive_unrelated_status_traffic(self):
        rows = [grant()] + [status(i, "unrelated", "success", "ok") for i in range(2, 203)]
        self.assertTrue(policy.authorization_valid(rows, SHA, A))


class RecoveryTests(unittest.TestCase):
    def test_new_command_recovers_after_every_old_terminal_outcome(self):
        for kind in ["failed", "produced no changes", "blocked: max iterations",
                     "blocked: workflow patch requires owner", "completed", "stale"]:
            with self.subTest(kind=kind):
                self.assertTrue(policy.recovery([route(), execution(kind), grant(50)], SHA, NOW)["manual_retry"])

    def test_terminal_outcome_without_new_command_never_reexecutes(self):
        for kind in ["failed", "produced no changes", "blocked: max iterations", "completed"]:
            self.assertFalse(policy.recovery([route(), grant(), claim(), consume(), execution(kind)], SHA, NOW)["recover"])

    def test_lost_terminal_post_does_not_repeat_paid_manual_invocation(self):
        for kind in ["running", "controller failed"]:
            self.assertFalse(policy.recovery([route(), grant(), claim(), execution(kind)], SHA, NOW)["recover"])

    def test_delivery_failure_before_claim_recovers_same_authorization(self):
        for kind in ["controller failed", "blocked: max iterations"]:
            decision = policy.recovery([route(), grant(), execution(kind)], SHA, NOW)
            self.assertEqual(decision["manual_retry_context"], A)

    def test_workflow_owner_route_requires_active_manual_authorization(self):
        rows = [route(1), route(2, "workflow changes require owner")]
        self.assertFalse(policy.recovery(rows, SHA, NOW)["recover"])
        self.assertTrue(policy.recovery([*rows, grant(3)], SHA, NOW)["manual_retry"])

    def test_controller_failure_preserves_earlier_fix_but_not_owner_only_route(self):
        failed = status(4, "hane/copilot-routing", "error", f"Copilot routing controller failed for {SHORT}")
        self.assertTrue(policy.recovery([route(1), failed], SHA, NOW)["recover"])
        self.assertFalse(policy.recovery([route(1), route(2, "workflow changes require owner"), failed], SHA, NOW)["recover"])

    def test_active_lease_and_backoff_are_respected(self):
        for kind, age in [("running", 3299), ("pending", 3299), ("controller failed", 59)]:
            self.assertFalse(policy.recovery([route(), grant(), execution(kind, age)], SHA, NOW)["recover"])
        self.assertTrue(policy.recovery([route(), grant(), execution("running", 3300)], SHA, NOW)["recover"])

    def test_unknown_lease_age_does_not_steal_execution(self):
        row = execution("running")
        row["created_at"] = "invalid"
        self.assertFalse(policy.recovery([route(), grant(), row], SHA, NOW)["recover"])

    def test_automatic_recovery_still_works_without_manual_history(self):
        self.assertTrue(policy.recovery([route(), execution("controller failed")], SHA, NOW)["recover"])

    def test_no_fix_evidence_no_dispatch(self):
        self.assertFalse(policy.recovery([grant()], SHA, NOW)["recover"])


class RoutingTests(unittest.TestCase):
    def test_new_no_fix_survives_controller_failure_for_manual_and_automatic(self):
        for kind in ['continue-validation', 'blocked']:
            rows = [route(1), route(2, kind), status(3, 'hane/copilot-routing', 'error',
                                                   f'Copilot routing controller failed for {SHORT}')]
            for manual in [False, True]:
                with self.subTest(kind=kind, manual=manual):
                    self.assertFalse(policy.routing_allows_fix(rows, SHA, manual))
                    self.assertFalse(policy.recovery(rows + ([grant(4)] if manual else []), SHA, NOW)['recover'])

    def test_owner_override_cannot_cross_a_newer_no_fix(self):
        for kind in ['continue-validation', 'blocked']:
            rows = [route(1), route(2, kind), route(3, 'workflow changes require owner'), grant(4)]
            self.assertFalse(policy.routing_allows_fix(rows, SHA, True))
            self.assertFalse(policy.recovery(rows, SHA, NOW)['recover'])

    def test_pending_unknown_or_wrong_state_never_reuses_old_fix(self):
        for description, state in [(f'Copilot routing pending for {SHORT}', 'pending'),
                                   ('Unknown routing outcome', 'error'),
                                   (f'Copilot routing: fix for {SHORT}', 'success')]:
            rows = [route(1), status(2, 'hane/copilot-routing', state, description),
                    status(3, 'hane/copilot-routing', 'error', f'Copilot routing controller failed for {SHORT}')]
            self.assertFalse(policy.routing_allows_fix(rows, SHA, True))

    def test_newer_fix_restores_permission_after_blocked(self):
        rows = [route(1, 'blocked'), route(2)]
        self.assertTrue(policy.routing_allows_fix(rows, SHA))
        self.assertTrue(policy.recovery(rows, SHA, NOW)['recover'])


def commit(number, parent=None, manual=False, bot=True):
    # Use distinctive prefixes because the workflow records 12-character prefixes.
    sha = f"{number:012x}" + "0" * 28
    message = "Owner change"
    if parent:
        message = f"Automatic Claude fix for Copilot routing decision on {parent[:12]}\n\nManual retry: {str(manual).lower()}"
    return {"sha": sha, "commit": {"message": message},
            "author": {"login": "github-actions[bot]" if bot else "hide212131"},
            "parents": [{"sha": parent}] if parent else []}


class BudgetTests(unittest.TestCase):
    def chain(self, after):
        commits = [commit(1)]
        for number in range(2, 6 + after):
            commits.append(commit(number, commits[-1]["sha"], manual=number == 5))
        return commits

    def test_manual_fix_does_not_consume_any_of_three_new_slots(self):
        for count in range(5):
            commits = self.chain(count)
            result = policy.cycle_budget(commits, {}, commits[-1]["sha"])
            self.assertEqual(result["completed"], count)
            self.assertEqual(result["boundary_sha"], commits[4]["sha"])

    def test_missing_both_completion_and_reset_posts_still_counts_pushes(self):
        commits = self.chain(3)
        self.assertEqual(policy.cycle_budget(commits, {}, commits[-1]["sha"])["completed"], 3)

    def test_delayed_statuses_do_not_change_budget_or_double_count(self):
        commits = self.chain(3)
        rows = {c["sha"]: [status(999 - i, "hane/claude-fix", "success",
                                 f"Claude fix completed for {c['sha'][:12]}")]
                for i, c in enumerate(commits[:-1])}
        rows[commits[4]["sha"]].append(status(1, "hane/claude-fix-cycle", "success",
            f"Claude fix cycle reset for {commits[4]['sha'][:12]} after manual retry"))
        self.assertEqual(policy.cycle_budget(commits, rows, commits[-1]["sha"])["completed"], 3)

    def test_untrusted_trailer_and_wrong_parent_cannot_reset_budget(self):
        for mutation in ("author", "parent"):
            commits = self.chain(1)
            if mutation == "author":
                commits[4]["author"]["login"] = "hide212131"
            else:
                commits[4]["parents"] = [{"sha": "f" * 40}]
            self.assertEqual(policy.cycle_budget(commits, {}, commits[-1]["sha"])["boundary_sha"], "")

    def test_latest_manual_commit_starts_new_cycle(self):
        commits = self.chain(3)
        commits.append(commit(20, commits[-1]["sha"], manual=True))
        self.assertEqual(policy.cycle_budget(commits, {}, commits[-1]["sha"])["completed"], 0)


def step_script(workflow, name):
    """Extract the production shell, without maintaining a second implementation."""
    text = (ROOT / "workflows" / workflow).read_text()
    step = text.split(f"      - name: {name}\n", 1)[1].split("\n      - name:", 1)[0]
    return textwrap.dedent(step.split("        run: |\n", 1)[1])


class WorkflowShellTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.state = self.root / "api.json"
        self.output = self.root / "output"
        self.output.touch()
        shutil.copyfile(SCRIPT, self.root / "hane-claude-fix-state.py")
        fake = self.root / "gh"
        fake.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''
            import json, os, pathlib, sys
            path = pathlib.Path(os.environ['API_STATE'])
            data = json.loads(path.read_text())
            args = sys.argv[1:]
            if '--method' not in args and any('/pulls/' in a for a in args):
                if os.environ.get('FAIL_PR_READ') == 'true':
                    sys.exit(1)
                print(json.dumps({'state': os.environ.get('PR_STATE', 'open'),
                                  'draft': os.environ.get('PR_DRAFT') == 'true',
                                  'head': {'sha': data.get('head', os.environ.get('CURRENT_HEAD', os.environ['TARGET_SHA']))}}))
                sys.exit(0)
            if '--method' not in args:
                print(json.dumps([data['statuses']]))
                sys.exit(0)
            fields = dict(args[i+1].split('=', 1) for i, a in enumerate(args) if a == '-f')
            if fields:
                data['statuses'].append(dict(fields, id=max([s['id'] for s in data['statuses']], default=0)+1))
            else:
                data['dispatches'] += 1
            if fields.get('description', '').startswith('Claude manual retry claimed') and os.environ.get('MOVE_HEAD_ON_CLAIM'):
                data['head'] = os.environ['MOVE_HEAD_ON_CLAIM']
            if fields.get('description', '').startswith('Claude manual retry claimed') and os.environ.get('ROUTE_AFTER_CLAIM'):
                data['statuses'].append(dict(context='hane/copilot-routing', state='success',
                    description='Copilot routing: continue-validation for ' + os.environ['TARGET_SHA'][:12],
                    id=max(s['id'] for s in data['statuses'])+1))
            path.write_text(json.dumps(data))
            if os.environ.get('FAIL_POST') == 'true':
                sys.exit(1)
            if not fields and os.environ.get('FAIL_DISPATCH') == 'true':
                sys.exit(1)
        '''))
        fake.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH'],
                        API_STATE=str(self.state), RUNNER_TEMP=str(self.root),
                        GITHUB_OUTPUT=str(self.output), REPOSITORY="owner/repo", TARGET_SHA=SHA,
                        MANUAL_RETRY="true", MANUAL_RETRY_CONTEXT=A, PR_NUMBER="82",
                        GITHUB_RUN_ID="101", GITHUB_RUN_ATTEMPT="1", COMMENT_ID="101")

    def run_shell(self, script, statuses=None, fail=False):
        if statuses is not None:
            self.state.write_text(json.dumps({"statuses": statuses, "dispatches": 0}))
        self.output.write_text("")
        return subprocess.run(["bash", "-c", script], env=dict(self.env, FAIL_POST=str(fail).lower()),
                              text=True, capture_output=True, timeout=10)

    def test_duplicate_delivery_claims_only_once(self):
        script = step_script("claude-fix.yml", "Claim one paid invocation for this authorization")
        self.assertEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
        self.assertIn("granted=true", self.output.read_text())
        self.assertEqual(self.run_shell(script).returncode, 0)
        self.assertNotIn("granted=true", self.output.read_text())

    def test_ambiguous_claim_post_does_not_grant_execution_or_retry(self):
        script = step_script("claude-fix.yml", "Claim one paid invocation for this authorization")
        self.assertNotEqual(self.run_shell(script, [route(), grant()], fail=True).returncode, 0)
        self.assertNotIn("granted=true", self.output.read_text())
        self.assertEqual(self.run_shell(script).returncode, 0)
        self.assertNotIn("granted=true", self.output.read_text())

    def test_new_authorization_can_claim_after_old_consumption(self):
        self.env['MANUAL_RETRY_CONTEXT'] = B
        script = step_script("claude-fix.yml", "Claim one paid invocation for this authorization")
        self.assertEqual(self.run_shell(script, [route(), grant(1), grant(2, B), consume(3)]).returncode, 0)
        self.assertIn("granted=true", self.output.read_text())

    def test_prepared_checkout_is_stale_at_invocation_for_manual_and_automatic(self):
        script = step_script('claude-fix.yml', 'Claim one paid invocation for this authorization')
        self.env['CURRENT_HEAD'] = 'b' * 40
        for manual in ('true', 'false'):
            with self.subTest(manual=manual):
                self.env['MANUAL_RETRY'] = manual
                self.assertEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
                self.assertNotIn('granted=true', self.output.read_text())

    def test_head_moved_during_claim_post_never_grants_paid_execution(self):
        script = step_script('claude-fix.yml', 'Claim one paid invocation for this authorization')
        self.env['MOVE_HEAD_ON_CLAIM'] = 'b' * 40
        self.assertEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
        self.assertNotIn('granted=true', self.output.read_text())
        rows = json.loads(self.state.read_text())['statuses']
        self.assertFalse(policy.authorization_valid(rows, SHA, A))

    def test_failed_final_pr_read_is_fail_closed(self):
        script = step_script('claude-fix.yml', 'Claim one paid invocation for this authorization')
        self.env['FAIL_PR_READ'] = 'true'
        self.assertNotEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
        self.assertNotIn('granted=true', self.output.read_text())

    def test_closed_or_draft_pr_cannot_invoke_claude(self):
        script = step_script('claude-fix.yml', 'Claim one paid invocation for this authorization')
        for state, draft in [('closed', 'false'), ('open', 'true')]:
            with self.subTest(state=state, draft=draft):
                self.env.update(PR_STATE=state, PR_DRAFT=draft)
                self.assertEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
                self.assertNotIn('granted=true', self.output.read_text())

    def test_new_no_fix_during_claim_stops_invocation(self):
        script = step_script('claude-fix.yml', 'Claim one paid invocation for this authorization')
        self.env['ROUTE_AFTER_CLAIM'] = 'true'
        self.assertEqual(self.run_shell(script, [route(), grant()]).returncode, 0)
        self.assertNotIn('granted=true', self.output.read_text())

    def test_router_delivery_failure_persists_one_grant_for_reconciliation(self):
        script = step_script("copilot-routing.yml", "Dispatch manual Claude retry")
        self.assertNotEqual(self.run_shell(script, [], fail=True).returncode, 0)
        self.assertEqual(self.run_shell(script).returncode, 0)
        rows = json.loads(self.state.read_text())['statuses']
        self.assertEqual(len(rows), 1)
        self.assertTrue(policy.authorization_valid(rows, SHA, A))

    def test_workflow_rerun_cannot_mint_new_authorization_on_moved_head(self):
        self.env['GITHUB_RUN_ATTEMPT'] = '2'
        script = step_script("copilot-routing.yml", "Dispatch manual Claude retry")
        self.assertEqual(self.run_shell(script, []).returncode, 0)
        self.assertEqual(json.loads(self.state.read_text())['statuses'], [])

    def test_lost_dispatch_is_recoverable_without_minting_a_second_grant(self):
        self.env['FAIL_DISPATCH'] = 'true'
        script = step_script("copilot-routing.yml", "Dispatch manual Claude retry")
        self.assertNotEqual(self.run_shell(script, [route()]).returncode, 0)
        data = json.loads(self.state.read_text())
        self.assertEqual(data['dispatches'], 1)
        self.assertEqual(policy.recovery(data['statuses'], SHA, NOW)['manual_retry_context'], A)
        self.assertEqual(self.run_shell(script).returncode, 0)
        self.assertEqual(json.loads(self.state.read_text())['dispatches'], 1)

    def test_terminal_workflow_steps_consume_only_their_own_claim(self):
        for name in ['Publish Claude execution failure', 'Publish workflow fix handoff']:
            with self.subTest(name=name):
                script = step_script('claude-fix.yml', name)
                result = self.run_shell(script, [grant(1), claim(2), grant(3, B)])
                self.assertEqual(result.returncode, 0, result.stderr)
                rows = json.loads(self.state.read_text())['statuses']
                self.assertFalse(policy.authorization_valid(rows, SHA, A))
                self.assertTrue(policy.authorization_valid(rows, SHA, B))

    def test_no_change_finalization_preserves_newer_authorization(self):
        fake_git = self.root / 'git'
        fake_git.write_text('#!/bin/sh\n[ "$1" = status ]\n')
        fake_git.chmod(0o755)
        script = step_script('claude-fix.yml', 'Finalize automatic fix')
        result = self.run_shell(script, [grant(1), claim(2), grant(3, B)])
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = json.loads(self.state.read_text())['statuses']
        self.assertFalse(policy.authorization_valid(rows, SHA, A))
        self.assertTrue(policy.authorization_valid(rows, SHA, B))


if __name__ == "__main__":
    unittest.main()
