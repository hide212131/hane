"""Execute the actual workflow selectors against review identity variants."""
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]

class FindingAuthorsTests(unittest.TestCase):
    def select(self, workflow, bot):
        text = (ROOT / 'workflows' / workflow).read_text()
        line = next(x for x in text.splitlines() if '| jq "[.[][] | select(' in x and 'pull_request_review_id' in x)
        query = line.split('| jq "', 1)[1].rsplit('"', 1)[0]
        query = query.replace('\\"', '"').replace('${review_bot}', bot).replace('${review_id}', '42')
        rows = [{'user': {'login': user}, 'pull_request_review_id': rid, 'path': user, 'line': 1, 'body': 'finding'}
                for user, rid in [('Copilot', 42), ('copilot-pull-request-reviewer[bot]', 42),
                                  ('chatgpt-codex-connector[bot]', 42), ('human', 42), ('Copilot', 43)]]
        result = subprocess.run(['jq', query], input=json.dumps([rows]), text=True, capture_output=True, check=True)
        return [x['path'] for x in json.loads(result.stdout)]

    def test_copilot_fallback_accepts_both_bot_names_only(self):
        self.assertEqual(self.select('codex-limit-copilot-fallback.yml', ''),
                         ['Copilot', 'copilot-pull-request-reviewer[bot]'])

    def test_worker_copilot_accepts_both_bot_names_only(self):
        self.assertEqual(self.select('claude-fix.yml', 'copilot-pull-request-reviewer[bot]'),
                         ['Copilot', 'copilot-pull-request-reviewer[bot]'])

    def test_worker_codex_does_not_mix_copilot_or_human_replies(self):
        self.assertEqual(self.select('claude-fix.yml', 'chatgpt-codex-connector[bot]'),
                         ['chatgpt-codex-connector[bot]'])
