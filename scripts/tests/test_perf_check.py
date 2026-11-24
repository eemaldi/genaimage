import gzip
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

        self.assertIn("portal build not found", report)
        self.assertIn("API latency benchmark not implemented", report)
        self.assertIn("Idle RSS approximation | Not available on this platform", report)
        self.assertEqual(report.count("| NOT MEASURED |"), 3)

    def test_main_succeeds_when_metrics_exceed_their_budgets(self):
        with mock.patch.object(perf_check, "release_binary", return_value=Path("genaimage")), \
            mock.patch.object(Path, "is_file", return_value=True), \
            mock.patch.object(Path, "stat", return_value=SimpleNamespace(st_size=26_000_000)), \
            mock.patch.object(perf_check, "launch_sample", return_value=(151.0, 41.0)), \
            mock.patch.object(perf_check, "find_gnu_time", return_value=None), \
            mock.patch.object(perf_check, "read_criterion_estimates", return_value=[]), \
            mock.patch.object(perf_check, "portal_initial_js_gzip_bytes", return_value=None), \
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


class PortalPerformanceTests(unittest.TestCase):
    def test_sums_entry_and_preloads_once_with_separate_gzip_streams(self):
        dist = Path('/project/dist')
        html = '''<script type="module" src="/assets/entry.js?hash=1"></script>
                  <link rel="modulepreload" href="./assets/shared.js">
                  <link href="/assets/shared.js" rel="modulepreload">
                  <link rel="stylesheet" href="/assets/style.css">'''
        files = {dist / 'assets/entry.js': b'entry' * 100,
                 dist / 'assets/shared.js': b'shared' * 100}
        with mock.patch.object(Path, 'read_text', return_value=html), \
             mock.patch.object(Path, 'read_bytes', lambda path: files[path]):
            size = perf_check.portal_initial_js_gzip_bytes(dist)
        self.assertEqual(size, sum(len(gzip.compress(data, mtime=0)) for data in files.values()))

    def test_missing_build_is_not_measured(self):
        with mock.patch.object(Path, 'read_text', side_effect=FileNotFoundError):
            self.assertIsNone(perf_check.portal_initial_js_gzip_bytes(Path('/project/dist')))

    def test_broken_or_external_assets_do_not_report_success(self):
        for html in ('<script type="module" src="https://example.com/a.js"></script>',
                     '<script type="module" src="/../outside.js"></script>',
                     '<link rel="modulepreload" href="/assets/chunk.js">',
                     '<html></html>'):
            with self.subTest(html=html), mock.patch.object(Path, 'read_text', return_value=html):
                with self.assertRaises(ValueError):
                    perf_check.portal_initial_js_gzip_bytes(Path('/project/dist'))
        with mock.patch.object(Path, 'read_text', return_value='<script type="module" src="/missing.js"></script>'), \
             mock.patch.object(Path, 'read_bytes', side_effect=FileNotFoundError):
            with self.assertRaises(FileNotFoundError):
                perf_check.portal_initial_js_gzip_bytes(Path('/project/dist'))

    def test_reports_portal_budget_boundary(self):
        report = perf_check.render_report(1, 1, 1, 5, portal_js_bytes=150_000)
        self.assertIn('| Portal initial JavaScript | 150.00 KB gzip | < 150 KB gzip | OVER BUDGET |', report)
        report = perf_check.render_report(1, 1, 1, 5, portal_js_bytes=149_999)
        self.assertIn('| WITHIN BUDGET |', report.split('Portal initial JavaScript')[1].splitlines()[0])


if __name__ == "__main__":
    unittest.main()
