import unittest
from pathlib import Path


WORKFLOW = (Path(__file__).resolve().parents[1] / "workflows" / "implement.yml").read_text()


class ImplementResultValidationTests(unittest.TestCase):
    def test_implement_success_requires_remote_branch_with_commits(self):
        step_name = "      - name: Ensure Claude produced implementation changes\n"
        next_step = "      - name: Open or normalize Pull Request\n"
        self.assertIn(step_name, WORKFLOW)
        step = WORKFLOW.split(step_name, 1)[1].split(next_step, 1)[0]

        self.assertIn("GH_TOKEN: ${{ github.token }}", step)
        self.assertIn("REPOSITORY: ${{ github.repository }}", step)
        self.assertIn("BRANCH_NAME: ${{ steps.claude.outputs.branch_name }}", step)
        self.assertIn("branches/${encoded_branch}", step)
        self.assertIn("compare/main...${encoded_branch}", step)
        self.assertIn("ahead_by", step)
        self.assertIn(
            "Claude Code returned success but did not push the implementation branch",
            step,
        )
        self.assertIn(
            "Claude Code returned success but the implementation branch has no commits ahead of main",
            step,
        )

    def test_result_validation_runs_before_pr_creation(self):
        validation = WORKFLOW.index(
            "      - name: Ensure Claude produced implementation changes\n"
        )
        pull_request = WORKFLOW.index("      - name: Open or normalize Pull Request\n")
        self.assertLess(validation, pull_request)


if __name__ == "__main__":
    unittest.main()
