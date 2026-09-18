import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "hosted_sidebar_gui", ROOT.parent / "scripts" / "hosted_sidebar_gui.py"
)
assert SPEC is not None and SPEC.loader is not None
sidebar = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sidebar)


class SidebarFocusedDecisionTests(unittest.TestCase):
    def test_idle_accepts_thin_divider(self):
        result = sidebar.evaluate_idle(
            {
                "boundary_score": 32.0,
                "divider_columns": 2,
                "boundary_x": 410,
                "boundary_normalized_x": 0.25,
            }
        )
        self.assertEqual(result["result"], "pass")

    def test_idle_rejects_thick_divider(self):
        result = sidebar.evaluate_idle(
            {
                "boundary_score": 32.0,
                "divider_columns": 8,
                "boundary_x": 410,
                "boundary_normalized_x": 0.25,
            }
        )
        self.assertEqual(result["result"], "fail")

    def test_scrolling_requires_narrow_long_lived_change(self):
        self.assertEqual(
            sidebar.evaluate_scrolling(
                {"max_changed_run": 35, "max_changed_width": 6, "changed_rows": 35}
            )["result"],
            "pass",
        )
        self.assertEqual(
            sidebar.evaluate_scrolling(
                {"max_changed_run": 35, "max_changed_width": 15, "changed_rows": 35}
            )["result"],
            "fail",
        )
        self.assertEqual(
            sidebar.evaluate_scrolling(
                {"max_changed_run": 4, "max_changed_width": 5, "changed_rows": 4}
            )["result"],
            "fail",
        )

    def test_post_scroll_requires_thumb_to_disappear(self):
        self.assertEqual(
            sidebar.evaluate_hidden(
                {"max_changed_run": 3, "max_changed_width": 1, "changed_rows": 3}
            )["result"],
            "pass",
        )
        self.assertEqual(
            sidebar.evaluate_hidden(
                {"max_changed_run": 30, "max_changed_width": 5, "changed_rows": 30}
            )["result"],
            "fail",
        )

    def test_resize_requires_boundary_shift_and_thin_post_state(self):
        before = {"boundary_x": 400, "divider_columns": 2}
        self.assertEqual(
            sidebar.evaluate_resize(before, {"boundary_x": 455, "divider_columns": 2})["result"],
            "pass",
        )
        self.assertEqual(
            sidebar.evaluate_resize(before, {"boundary_x": 408, "divider_columns": 2})["result"],
            "fail",
        )
        self.assertEqual(
            sidebar.evaluate_resize(before, {"boundary_x": 455, "divider_columns": 9})["result"],
            "fail",
        )


class SidebarProcedureContractTests(unittest.TestCase):
    def test_procedure_version_is_focused_sidebar(self):
        self.assertEqual(sidebar.PROCEDURE_VERSION, "hosted-sidebar/1")
        self.assertEqual(sidebar.VERIFICATION_KIND, "sidebar_focused")


if __name__ == "__main__":
    unittest.main()
