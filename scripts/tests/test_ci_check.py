import contextlib
import io
import unittest
from pathlib import Path
from unittest import mock

from scripts import ci_check


class RunnerTests(unittest.TestCase):
    def run_main(self, portal_present, command_results=None, failed_check=None):
        root = Path('/project')
        with mock.patch.object(ci_check, 'ROOT', root), \
             mock.patch.object(Path, 'is_file', return_value=portal_present), \
             mock.patch.object(ci_check, 'check_relative_links', return_value=failed_check or []), \
             mock.patch.object(ci_check, 'check_skill_mirrors', return_value=[]) as mirrors, \
             mock.patch.object(ci_check, 'check_skill_lock', return_value=[]), \
             mock.patch.object(ci_check, 'check_runtime_tooling', return_value=[]) as tooling, \
             mock.patch.object(ci_check, 'run_command', side_effect=command_results or (lambda *args: True)) as run, \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            result = ci_check.main()
        return result, run.call_args_list, tooling, mirrors

    def test_portal_commands_are_skipped_when_package_is_absent(self):
        result, calls, tooling, _ = self.run_main(False)
        self.assertEqual(result, 0)
        tooling.assert_called_once_with(False)
        self.assertFalse(any(call.args[1][0] == 'npm' for call in calls))

    def test_portal_commands_run_in_order_in_portal_directory(self):
        result, calls, tooling, _ = self.run_main(True)
        self.assertEqual(result, 0)
        tooling.assert_called_once_with(True)
        portal_calls = [(call.args[1], call.args[2]) for call in calls if call.args[1][0] == 'npm']
        self.assertEqual(portal_calls, [
            (['npm', 'ci'], Path('/project/apps/portal')),
            (['npm', 'run', 'lint'], Path('/project/apps/portal')),
            (['npm', 'test'], Path('/project/apps/portal')),
            (['npm', 'run', 'build'], Path('/project/apps/portal')),
        ])

    def test_command_failure_stops_subsequent_commands(self):
        result, calls, _, _ = self.run_main(True, [True, False])
        self.assertEqual(result, 1)
        self.assertEqual(len(calls), 2)

    def test_check_failure_stops_subsequent_checks_and_commands(self):
        result, calls, tooling, mirrors = self.run_main(True, failed_check=['broken link'])
        self.assertEqual(result, 1)
        self.assertEqual(calls, [])
        mirrors.assert_not_called()
        tooling.assert_not_called()

    def test_missing_node_and_npm_report_portal_prerequisites(self):
        with mock.patch.object(ci_check.shutil, 'which', side_effect=lambda command: '/bin/tool' if command in ('cargo', 'rustfmt') else None), \
             mock.patch.object(Path, 'is_file', return_value=True):
            self.assertEqual(ci_check.check_runtime_tooling(False), [])
            errors = ci_check.check_runtime_tooling(True)
        self.assertEqual(len(errors), 2)
        self.assertTrue(any('node' in error and 'apps/portal/package.json' in error for error in errors))
        self.assertTrue(any('npm' in error and 'apps/portal/package.json' in error for error in errors))


if __name__ == '__main__':
    unittest.main()
