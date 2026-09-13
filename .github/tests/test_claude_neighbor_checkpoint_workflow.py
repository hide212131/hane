from pathlib import Path


WORKFLOW = (
    Path(__file__).resolve().parents[1]
    / "workflows"
    / "claude-fix-focused-neighbor-continuation.yml"
).read_text()


def test_shared_parse_worker_is_one_root_cause_cluster_and_checkpointed():
    assert "github.event.comment.body == '/claude-fix-focused-shared-parse'" in WORKFLOW
    assert "ONE root-cause cluster" in WORKFLOW
    assert "Do NOT fix the byte-size sync/background threshold" in WORKFLOW
    assert "Do NOT fix nested quote marker derivation" in WORKFLOW
    assert "Hane-Checkpoint-Base:" in WORKFLOW
    assert "Hane-Checkpoint-Finding: shared-parse-consistency" in WORKFLOW
    assert "github.run_id }}-${{ github.run_attempt" in WORKFLOW


def test_failed_paid_invocation_still_reaches_checkpoint_and_diagnostics():
    checkpoint_condition = (
        "if: ${{ always() && !cancelled() "
        "&& steps.repair.outcome != 'skipped' }}"
    )
    assert WORKFLOW.count(checkpoint_condition) == 2
    assert 'git diff --binary "$ORIGINAL_SHA"' in WORKFLOW
    assert "actions/upload-artifact@v4" in WORKFLOW
    assert "--output-format stream-json" in WORKFLOW
    assert "--verbose" in WORKFLOW
    assert "claude-shared-parse.jsonl" in WORKFLOW
    assert "claude-result.json" in WORKFLOW
    assert "claude-exit-code.txt" in WORKFLOW


def test_restore_is_bound_to_exact_head_and_root_cause_cluster():
    assert '"$checkpoint_base" != "$ORIGINAL_SHA"' in WORKFLOW
    assert '"$checkpoint_finding" != \'shared-parse-consistency\'' in WORKFLOW
    assert 'git merge-base --is-ancestor "$ORIGINAL_SHA" "$checkpoint_sha"' in WORKFLOW


def test_prompt_pins_the_shared_parse_implementation_seams():
    assert "present_block" in WORKFLOW
    assert "expected_disclosures" in WORKFLOW
    assert "window.joined.is_some()" in WORKFLOW
    assert "layout_around" in WORKFLOW
    assert "joined_parse_cache" in WORKFLOW
    assert "revision + source_range" in WORKFLOW
    assert "Make a code/test change early" in WORKFLOW


def test_final_push_refuses_to_overwrite_a_moved_pr():
    assert 'if [[ "$current_sha" != "$ORIGINAL_SHA" ]]' in WORKFLOW
    assert "refusing to push over newer work" in WORKFLOW
