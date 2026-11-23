import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from scripts import perf_check


class PerformanceReportTests(unittest.TestCase):
    def test_equal_or_over_budget_is_reported_as_over_budget(self):
        report = perf_check.render_report(150.0, 40.0, 25.0, 5)

        self.assertIn("| Cold start (median of 5) | 150.00 ms | < 150 ms | OVER BUDGET |", report)
        self.assertIn("| Idle RSS approximation (median peak of 5) | 40.00 MB | < 40 MB | OVER BUDGET |", report)
        self.assertIn("| Release binary size | 25.00 MB | < 25 MB | OVER BUDGET |", report)

    def test_unavailable_measurements_are_not_marked_as_passing(self):
        report = perf_check.render_report(100.0, None, 10.0, 5)

        self.assertIn("portal does not exist yet", report)
        self.assertIn("server/API does not exist yet", report)
        self.assertIn("Idle RSS approximation | Not available on this platform", report)
        self.assertEqual(report.count("| NOT MEASURED |"), 3)

    def test_main_succeeds_when_metrics_exceed_their_budgets(self):
        with mock.patch.object(perf_check, "release_binary", return_value=Path("genaimage")), \
            mock.patch.object(Path, "is_file", return_value=True), \
            mock.patch.object(Path, "stat", return_value=SimpleNamespace(st_size=26_000_000)), \
            mock.patch.object(perf_check, "launch_sample", return_value=(151.0, 41.0)), \
            mock.patch("builtins.print"), \
            mock.patch.dict(perf_check.os.environ, {"GITHUB_STEP_SUMMARY": ""}):
            self.assertEqual(perf_check.main(["--no-build", "--quick"]), 0)



if __name__ == "__main__":
    unittest.main()
