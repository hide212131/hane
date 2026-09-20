from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = (ROOT / "workflows/codex-usage-limit-fallback.yml").read_text()
CLAUDE_WORKFLOW = (ROOT / "workflows/claude-fix.yml").read_text()


def test_fallback_is_completed_claude_run_only_and_uses_exact_checkpoint():
    assert 'workflow_run:' in WORKFLOW
    assert 'workflows: ["AADW Claude Fix"]' in WORKFLOW
    assert "github.event.workflow_run.conclusion == 'failure'" in WORKFLOW
    assert "github.event.workflow_run.event == 'issue_comment'" in WORKFLOW
    assert "github.event.workflow_run.head_repository.full_name == github.repository" in WORKFLOW
    assert "aadw-worker-checkpoint-${{ github.event.workflow_run.id }}-${{ github.event.workflow_run.run_attempt }}" in WORKFLOW
    assert "run-id: ${{ github.event.workflow_run.id }}" in WORKFLOW
    assert '.failure_category == "usage_or_rate_limit"' in WORKFLOW
    assert ".run_attempt == $run_attempt" in WORKFLOW
    assert ".workflow_sha == $workflow_sha" in WORKFLOW
    assert "AADW_COMMANDER_HANDOFF_V2" in WORKFLOW
    assert "Download the exact Claude Commander handoff" in WORKFLOW


def test_non_usage_categories_do_not_start_the_codex_job():
    assert "Codex fallback is not eligible." in WORKFLOW
    assert "echo 'ready=false'" in WORKFLOW
    assert "needs.prepare.outputs.ready == 'true'" in WORKFLOW
    assert 'refs/heads/${DEFAULT_BRANCH}' in WORKFLOW
    assert "authentication|max_turns|model_or_provider" not in WORKFLOW
    assert "usage_or_rate_limit" in CLAUDE_WORKFLOW


def test_self_hosted_codex_execution_is_pinned_and_uncredentialed():
    assert "runs-on: [self-hosted, macOS, hane-codex]" in WORKFLOW
    assert "Initialize isolated runner paths" in WORKFLOW
    assert 'printf \'WORKTREE=%s\\n\'' in WORKFLOW
    assert "actions: read" in WORKFLOW
    assert "--model gpt-5.6-luna" in WORKFLOW
    assert "--config model_reasoning_effort=xhigh" in WORKFLOW
    assert "--profile hane-codex-fallback" in WORKFLOW
    assert "expected_profile_sha256=" in WORKFLOW
    assert "--ignore-user-config" in WORKFLOW
    assert "GH_TOKEN: ''" in WORKFLOW
    assert "GITHUB_TOKEN: ''" in WORKFLOW
    assert "ACTIONS_RUNTIME_TOKEN: ''" in WORKFLOW
    assert "GH_CONFIG_DIR:" in WORKFLOW
    assert "GIT_CONFIG_GLOBAL:" in WORKFLOW
    assert "unset GH_TOKEN GITHUB_TOKEN ACTIONS_RUNTIME_TOKEN" in WORKFLOW
    assert "--ignore-rules" in WORKFLOW
    assert "--add-dir \"$HANDOFF_DIR\"" in WORKFLOW
    assert "--add-dir \"$CONTEXT_DIR\"" in WORKFLOW
    assert "Do not commit or push." in WORKFLOW
    assert 'checkpoint_patch="$HANDOFF_DIR/checkpoint.patch"' in WORKFLOW
    assert 'git -C "$WORKTREE" apply --index' in WORKFLOW


def test_mutation_guards_cover_start_scope_and_push():
    assert "Revalidate trusted actor and exact target head before Codex" in WORKFLOW
    assert "Target Pull Request changed before Codex execution" in WORKFLOW
    assert ".github/*|AGENTS.md" in WORKFLOW
    assert "Codex attempted to modify trusted authority or workflow path" in WORKFLOW
    assert "Codex created an untrusted commit" in WORKFLOW
    assert "Clean push checkout does not match the exact target head" in WORKFLOW
    assert 'git -C "$PUSH_WORKTREE" apply --index' in WORKFLOW
    assert "Transferred Codex patch contains a trusted authority or workflow path" in WORKFLOW
    assert 'git -C "$PUSH_WORKTREE" -c core.hooksPath=/dev/null push' in WORKFLOW
    assert "Target Pull Request moved before Codex push" in WORKFLOW


if __name__ == "__main__":
    test_fallback_is_completed_claude_run_only_and_uses_exact_checkpoint()
    test_non_usage_categories_do_not_start_the_codex_job()
    test_self_hosted_codex_execution_is_pinned_and_uncredentialed()
    test_mutation_guards_cover_start_scope_and_push()
