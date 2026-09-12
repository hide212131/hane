"""Allowlist public test evidence; never upload session state or symlinks."""
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
