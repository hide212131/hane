from pathlib import Path


WORKFLOW = (
    Path(__file__).resolve().parents[1]
    / "workflows"
    / "claude-fix-focused-neighbor-continuation.yml"
).read_text()


def test_neighbor_worker_is_single_finding_and_checkpointed():
    assert "github.event.comment.body == '/claude-fix-focused-neighbor'" in WORKFLOW
    assert "FINDING ONLY:" in WORKFLOW
    assert "Repair 2" not in WORKFLOW
    assert "Repair 3" not in WORKFLOW
    assert "Hane-Checkpoint-Base:" in WORKFLOW
    assert "Hane-Checkpoint-Finding: neighbor-row-target" in WORKFLOW
    assert "github.run_id }}-${{ github.run_attempt" in WORKFLOW


def test_failed_paid_invocation_still_reaches_checkpoint_steps():
    save = "if: ${{ always() && !cancelled() && steps.repair.outcome != 'skipped' }}"
    assert WORKFLOW.count(save) == 2
    assert "git diff --binary \"$ORIGINAL_SHA\"" in WORKFLOW
    assert "actions/upload-artifact@v4" in WORKFLOW


def test_restore_is_bound_to_exact_head_and_finding():
    assert '[[ "$checkpoint_base" != "$ORIGINAL_SHA"' in WORKFLOW
    assert '"$checkpoint_finding" != \'neighbor-row-target\'' in WORKFLOW
    assert 'git merge-base --is-ancestor "$ORIGINAL_SHA" "$checkpoint_sha"' in WORKFLOW
