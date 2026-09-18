import importlib.util
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "hosted_sidebar_chrome_gui.py"
spec = importlib.util.spec_from_file_location("hosted_sidebar_chrome_gui", SCRIPT)
assert spec is not None and spec.loader is not None
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class SidebarChromeGuiTests(unittest.TestCase):
    def metrics(self, **overrides):
        value = {
            "width": 960,
            "height": 681,
            "divider_x": 222,
            "divider_width": 1,
            "thumb_width": 0,
            "max_non_background_run": 0,
            "sidebar_background": "#f0ede7",
            "main_background": "#faf9f7",
            "active_columns": [],
        }
        value.update(overrides)
        return value

    def test_procedure_identity_is_focused(self):
        self.assertEqual(mod.PROCEDURE_VERSION, "hosted-sidebar-chrome/1")
        self.assertEqual(mod.VERIFICATION_KIND, "sidebar_chrome_focused")

    def test_idle_accepts_thin_boundary_without_track(self):
        self.assertEqual(mod.idle_acceptance(self.metrics()), (True, None))

    def test_idle_rejects_old_thick_track(self):
        ok, reason = mod.idle_acceptance(self.metrics(thumb_width=10, max_non_background_run=620))
        self.assertFalse(ok)
        self.assertIn("idle", reason)

    def test_idle_rejects_thick_divider(self):
        ok, _ = mod.idle_acceptance(self.metrics(divider_width=5))
        self.assertFalse(ok)

    def test_scrolling_accepts_three_pixel_thumb(self):
        self.assertEqual(
            mod.scrolling_acceptance(
                self.metrics(thumb_width=3, max_non_background_run=40)
            ),
            (True, None),
        )

    def test_scrolling_rejects_six_pixel_thumb(self):
        ok, reason = mod.scrolling_acceptance(
            self.metrics(thumb_width=6, max_non_background_run=40)
        )
        self.assertFalse(ok)
        self.assertIn("2〜4px", reason)

    def test_scrolling_rejects_short_noise(self):
        ok, _ = mod.scrolling_acceptance(
            self.metrics(thumb_width=3, max_non_background_run=8)
        )
        self.assertFalse(ok)

    def test_metric_validation_rejects_strings_and_bools(self):
        for key, value in (
            ("width", "960"),
            ("height", True),
            ("divider_x", "222"),
            ("thumb_width", False),
        ):
            with self.subTest(key=key):
                metrics = self.metrics(**{key: value})
                with self.assertRaises(RuntimeError):
                    mod.validate_metrics(metrics)

    def test_fixture_is_scrollable_and_uses_short_names(self):
        with tempfile.TemporaryDirectory() as directory:
            names = mod.make_fixtures(Path(directory) / "work")
        self.assertEqual(len(names), 80)
        self.assertEqual(names[0], "N001.md")
        self.assertEqual(names[-1], "N080.md")
        self.assertTrue(all(len(name) <= 7 for name in names))


if __name__ == "__main__":
    unittest.main()
