import json
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
            mock.patch.object(perf_check, "find_gnu_time", return_value=None), \
            mock.patch.object(perf_check, "read_criterion_estimates", return_value=[]), \
            mock.patch("builtins.print"), \
            mock.patch.dict(perf_check.os.environ, {"GITHUB_STEP_SUMMARY": ""}):
            self.assertEqual(perf_check.main(["--no-build", "--quick"]), 0)

    def test_rss_comes_from_gnu_time_child_usage(self):
        result = SimpleNamespace(stderr="5120\n")
        with mock.patch.object(
            perf_check.subprocess,
            "run",
            side_effect=[SimpleNamespace(), result],
        ):
            _, rss_mb = perf_check.launch_sample(Path("genaimage"), Path("/usr/bin/time"))

        self.assertAlmostEqual(rss_mb, 5.24288)

    def test_criterion_median_estimates_are_added_to_benchmarks_table(self):
        estimates_path = Path("criterion/workflow_binding/new/estimates.json")
        with mock.patch.object(Path, "glob", return_value=[estimates_path]), \
            mock.patch.object(
                Path,
                "read_text",
                return_value=json.dumps({"median": {"point_estimate": 1149.25}}),
            ):
            benchmarks = perf_check.read_criterion_estimates(Path("target"))

        report = perf_check.render_report(100.0, 10.0, 10.0, 5, benchmarks)
        self.assertIn("## Benchmarks", report)
        self.assertIn("| workflow_binding | 1.15 us |", report)

    def test_relative_target_dir_is_resolved_from_repository_root(self):
        with mock.patch.dict(perf_check.os.environ, {"CARGO_TARGET_DIR": "../target-rv24"}):
            expected = perf_check.ROOT / "../target-rv24" / "release" / "genaimage"
            self.assertEqual(perf_check.release_binary(), expected)


if __name__ == "__main__":
    unittest.main()
