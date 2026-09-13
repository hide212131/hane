import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_notification_reconcile as subject


ROOT = Path(__file__).resolve().parents[1]
RUN_URL = 'https://github.com/${REPOSITORY}/actions/runs/${GITHUB_RUN_ID}/attempts/${GITHUB_RUN_ATTEMPT}'


def block_after(path, marker, width=500):
    text = (ROOT / path).read_text(encoding='utf-8')
    start = text.index(marker)
    return text[start:start + width]


class Tests(unittest.TestCase):
    def test_claude_workflow_change_terminal_keeps_exact_run_identity(self):
        block = block_after(
            'workflows/claude-fix.yml',
            'Claude fix blocked: workflow changes require owner for ${short_sha}',
        )
        self.assertIn(f'-f target_url="{RUN_URL}"', block)

    def test_routing_workflow_change_terminal_keeps_exact_run_identity(self):
        block = block_after(
            'workflows/copilot-routing.yml',
            'Copilot routing: workflow changes require owner for ${short_sha}',
        )
        self.assertIn(f'-f target_url="{RUN_URL}"', block)

    def test_legacy_pr_url_terminal_is_never_given_a_fake_status_id_run(self):
        legacy = {
            'id': 987654,
            'context': 'hane/claude-fix',
            'state': 'error',
            'description': 'Claude fix blocked: workflow changes require owner for aaaaaaaaaaaa',
            'target_url': 'https://github.com/hide212131/hane/pull/131',
        }
        self.assertEqual(subject.build_generations([legacy]), [])

    def test_correlated_terminal_uses_actions_run_and_attempt(self):
        correlated = {
            'id': 987655,
            'context': 'hane/claude-fix',
            'state': 'error',
            'description': 'Claude fix blocked: workflow changes require owner for aaaaaaaaaaaa',
            'target_url': 'https://github.com/hide212131/hane/actions/runs/321/attempts/2',
        }
        generations = subject.build_generations([correlated])
        self.assertEqual(len(generations), 1)
        self.assertEqual(generations[0]['run_id'], '321')
        self.assertEqual(generations[0]['attempt'], '2')


if __name__ == '__main__':
    unittest.main()
