from pathlib import Path


WORKFLOW = (Path(__file__).resolve().parents[1] / "workflows" / "implement.yml").read_text()


def test_implement_success_requires_remote_branch_with_commits():
    step_name = "      - name: Ensure Claude produced implementation changes\n"
    next_step = "      - name: Open or normalize Pull Request\n"
    assert step_name in WORKFLOW
    step = WORKFLOW.split(step_name, 1)[1].split(next_step, 1)[0]

    assert "GH_TOKEN: ${{ github.token }}" in step
    assert "REPOSITORY: ${{ github.repository }}" in step
    assert "BRANCH_NAME: ${{ steps.claude.outputs.branch_name }}" in step
    assert "branches/${encoded_branch}" in step
    assert "compare/main...${encoded_branch}" in step
    assert "ahead_by" in step
    assert "Claude Code returned success but did not push the implementation branch" in step
    assert "Claude Code returned success but the implementation branch has no commits ahead of main" in step


def test_result_validation_runs_before_pr_creation():
    validation = WORKFLOW.index("      - name: Ensure Claude produced implementation changes\n")
    pull_request = WORKFLOW.index("      - name: Open or normalize Pull Request\n")
    assert validation < pull_request
