import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import aadw_notification_reconcile as subject


class Tests(unittest.TestCase):
    def test_old_fallback_source_does_not_label_later_codex_generation(self):
        statuses = [
            {
                'id': 10,
                'context': 'hane/review-source',
                'state': 'success',
                'description': 'Review source: Copilot fallback for aaaaaaaaaaaa',
                'target_url': 'https://github.com/example/reviews/fallback',
            },
            {
                'id': 20,
                'context': 'hane/codex-review',
                'state': 'pending',
                'description': 'Codex review pending for aaaaaaaaaaaa',
                'target_url': 'https://github.com/example/actions/runs/200',
            },
            {
                'id': 21,
                'context': 'hane/codex-review',
                'state': 'success',
                'description': 'Codex review clean for aaaaaaaaaaaa',
                'target_url': 'https://github.com/example/actions/runs/200',
            },
        ]
        generations = subject.build_generations(subject.codex_lifecycle_rows(statuses))
        self.assertEqual(len(generations), 1)
        self.assertIsNone(subject.review_source_for_generation(statuses, generations[0]))

    def test_matching_review_url_is_scoped_to_that_generation(self):
        statuses = [{
            'id': 10,
            'context': 'hane/review-source',
            'state': 'success',
            'description': 'Review source: Copilot fallback for aaaaaaaaaaaa',
            'target_url': 'https://github.com/example/reviews/fallback',
        }]
        generation = {
            'latest': {
                'id': 11,
                'target_url': 'https://github.com/example/reviews/fallback',
            },
        }
        self.assertEqual(
            subject.review_source_for_generation(statuses, generation),
            'GitHub Copilot fallback',
        )

    def test_fallback_routing_terminal_is_not_attached_to_parallel_normal_run(self):
        normal_run = 'https://github.com/example/actions/runs/100'
        fallback_run = 'https://github.com/example/actions/runs/200/attempts/2'
        rows = [
            {
                'id': 30,
                'context': 'hane/copilot-routing',
                'state': 'pending',
                'description': 'Copilot routing pending for aaaaaaaaaaaa',
                'target_url': normal_run,
            },
            {
                'id': 31,
                'context': 'hane/copilot-routing',
                'state': 'pending',
                'description': 'Copilot routing pending for aaaaaaaaaaaa',
                'target_url': fallback_run,
            },
            {
                'id': 32,
                'context': 'hane/copilot-routing',
                'state': 'failure',
                'description': 'Copilot routing: fix for aaaaaaaaaaaa',
                'target_url': fallback_run,
            },
            {
                'id': 33,
                'context': 'hane/copilot-routing',
                'state': 'success',
                'description': 'Copilot routing: continue-validation for aaaaaaaaaaaa',
                'target_url': normal_run,
            },
        ]
        generations = subject.build_generations(rows)
        fallback = [generation for generation in generations if generation['run_id'] == '200']
        self.assertEqual(len(fallback), 1)
        self.assertEqual(fallback[0]['attempt'], '2')
        self.assertEqual(fallback[0]['latest']['id'], 32)
        self.assertFalse(any(
            generation['run_id'] == '100' and generation['latest']['id'] == 32
            for generation in generations
        ))

    def test_fallback_routing_workflow_records_actual_run_attempt(self):
        workflow = (
            Path(__file__).resolve().parents[1]
            / 'workflows'
            / 'codex-limit-copilot-fallback.yml'
        ).read_text(encoding='utf-8')
        self.assertIn(
            'routing_run_url="https://github.com/${REPOSITORY}/actions/runs/${GITHUB_RUN_ID}/attempts/${GITHUB_RUN_ATTEMPT}"',
            workflow,
        )
        self.assertIn("-f state=pending -f context='hane/copilot-routing'", workflow)
        self.assertGreaterEqual(workflow.count('-f target_url="$routing_run_url"'), 2)

    def test_fallback_routing_publishes_immediate_lifecycle_comments(self):
        workflow = (
            Path(__file__).resolve().parents[1]
            / 'workflows'
            / 'codex-limit-copilot-fallback.yml'
        ).read_text(encoding='utf-8')
        pending = workflow.index("-f state=pending -f context='hane/copilot-routing'")
        start = workflow.index('aadw_notify.py start', pending)
        terminal = workflow.index("-f state=failure -f context='hane/copilot-routing'", start)
        success = workflow.index('aadw_notify.py success', terminal)
        self.assertLess(pending, start)
        self.assertLess(start, terminal)
        self.assertLess(terminal, success)
        self.assertIn('--process copilot-pre-gui-routing', workflow[start:success + 500])
        self.assertIn('sparse-checkout: .github/scripts/aadw_notify.py', workflow)


if __name__ == '__main__':
    unittest.main()
