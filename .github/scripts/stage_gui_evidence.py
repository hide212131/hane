"""Allowlist public test evidence; never upload session state or symlinks."""
import importlib.util
import json
import os
from pathlib import Path
import shutil

temp = Path(os.environ['RUNNER_TEMP'])
source, dest = temp / 'hane-gui-interaction', temp / 'hane-gui-artifact'
dest.mkdir(exist_ok=True)
names = {'result.json', 'summary.md', 'hane.log', 'ascii-fixture.md', 'ime-fixture.md', 'scroll-fixture.md', 'inline-fixture.md'}
for file in source.rglob('*'):
    relative = file.relative_to(source)
    if 'state' in relative.parts or not file.is_file() or file.is_symlink():
        continue
    if file.name in names or file.suffix == '.png':
        target = dest / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(file, target)

# Run only the checker from the trusted control checkout, never from target
# artifacts. Keep the original observation intact for independent review.
result_path = dest / 'result.json'
if result_path.is_file():
    before = json.loads(result_path.read_text(encoding='utf-8'))
    if before.get('verification_kind') == 'scroll_inertia_focused':
        checker_path = Path(__file__).resolve().parents[2] / 'scripts/aadw_gui_observation.py'
        spec = importlib.util.spec_from_file_location('aadw_gui_observation', checker_path)
        if spec is None or spec.loader is None:
            raise RuntimeError('trusted GUI observation checker is unavailable')
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        shutil.copyfile(result_path, dest / 'raw-result.json')
        after = checker.annotate(result_path)
        (dest / 'observation-quality.json').write_text(
            json.dumps(after['observation_quality'], ensure_ascii=False, indent=2) + '\n',
            encoding='utf-8',
        )
        if before.get('overall_result') == 'pass' and after.get('overall_result') != 'pass':
            (dest / 'summary.md').write_text(after['summary'] + '\n', encoding='utf-8')
            raise SystemExit('GUI observations do not substantiate the reported pass')
