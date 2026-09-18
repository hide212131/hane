"""Stage only public evidence from the focused sidebar-chrome GUI validator."""
from pathlib import Path
import os
import shutil

temp = Path(os.environ["RUNNER_TEMP"])
source = temp / "hane-sidebar-chrome-gui"
dest = temp / "hane-sidebar-chrome-gui-artifact"
dest.mkdir(exist_ok=True)

allowed_names = {"result.json", "summary.md", "hane.log", "aadw-context.json"}
for file in source.rglob("*"):
    relative = file.relative_to(source)
    if "state" in relative.parts or not file.is_file() or file.is_symlink():
        continue
    if file.name in allowed_names or file.suffix == ".png":
        target = dest / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(file, target)
