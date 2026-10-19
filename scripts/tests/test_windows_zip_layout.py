"""The Windows ZIP layout, the build output and the trust script must agree."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
TRUST = (ROOT / "windows/shell-extension/Trust-HaneShellCertificate.ps1").read_text(encoding="utf-8")
WORKFLOW = (ROOT / ".github/workflows/windows-release.yml").read_text(encoding="utf-8")
BUILD = (ROOT / "windows/shell-extension/build-package.ps1").read_text(encoding="utf-8")


class WindowsZipLayoutTests(unittest.TestCase):
    def test_build_places_dll_in_versioned_directory(self):
        self.assertIn("'shell-integration'", BUILD)
        self.assertIn("$cargoVersion", BUILD)

    def test_workflow_keeps_versioned_directory_not_flat_dll(self):
        self.assertNotIn("'hane_shell_extension.dll', 'Hane.ShellIntegration.msix'", WORKFLOW)
        self.assertIn("target/release/shell-integration", WORKFLOW)

    def test_trust_script_expects_versioned_dll(self):
        self.assertNotIn("Join-Path $directory 'hane_shell_extension.dll'", TRUST)
        self.assertIn("'shell-integration'", TRUST)
        self.assertIn("hane_shell_extension.dll", TRUST)

    def test_trust_script_validates_layout_before_trusting(self):
        layout = TRUST.index("$extensions.Count -eq 0")
        self.assertLess(layout, TRUST.index("Import-Certificate"))


if __name__ == "__main__":
    unittest.main()
