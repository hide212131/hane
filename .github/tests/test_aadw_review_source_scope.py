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


if __name__ == '__main__':
    unittest.main()
