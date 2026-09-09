from pathlib import Path


WORKFLOW = Path(__file__).parents[1] / "workflows" / "implement.yml"


def test_implement_uses_issue_body_as_task_not_trigger_comment():
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert "track_progress: true" in workflow
    assert "The /implement comment is only the trusted trigger" in workflow
    assert "Treat the GitHub Issue title and body as the implementation requirements" in workflow
