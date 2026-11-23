import contextlib
import io
import unittest
from pathlib import Path
from unittest import mock

from scripts import ci_check


class RunnerTests(unittest.TestCase):
    def run_main(self, portal_present, command_results=None, failed_check=None,
                 lockfiles=('package-lock.json',)):
        root = Path('/project')
        is_file = lambda path: portal_present and path.name in ('package.json', *lockfiles)
        with mock.patch.object(ci_check, 'ROOT', root), \
             mock.patch.object(Path, 'is_file', is_file), \
             mock.patch.object(ci_check, 'check_relative_links', return_value=failed_check or []), \
             mock.patch.object(ci_check, 'check_skill_mirrors', return_value=[]) as mirrors, \
             mock.patch.object(ci_check, 'check_skill_lock', return_value=[]), \
             mock.patch.object(ci_check.check_openapi, 'check', return_value=[]), \
             mock.patch.object(ci_check, 'check_runtime_tooling', return_value=[]) as tooling, \
             mock.patch.object(ci_check, 'run_command', side_effect=command_results or (lambda *args: True)) as run, \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            result = ci_check.main()
        return result, run.call_args_list, tooling, mirrors

    def test_portal_commands_are_skipped_when_package_is_absent(self):
        result, calls, tooling, _ = self.run_main(False)
        self.assertEqual(result, 0)
        tooling.assert_called_once_with(False)
        self.assertFalse(any(call.args[1][0] == 'npm' or 'scripts/generate_portal_types.py' in call.args[1] for call in calls))

    def test_portal_commands_run_in_order_in_portal_directory(self):
        result, calls, tooling, _ = self.run_main(True)
        self.assertEqual(result, 0)
        tooling.assert_called_once_with(True)
        portal_calls = [(call.args[1], call.args[2]) for call in calls if call.args[1][0] == 'npm']
        type_check = [ci_check.sys.executable, 'scripts/generate_portal_types.py', '--check']
        commands = [c.args[1] for c in calls]
        self.assertLess(commands.index(['npm', 'ci']), commands.index(type_check))
        self.assertLess(commands.index(type_check), commands.index(['npm', 'run', 'lint']))
        self.assertEqual(calls[commands.index(type_check)].args[2], Path('/project'))
        self.assertEqual(portal_calls, [
            (['npm', 'ci'], Path('/project/apps/portal')),
            (['npm', 'run', 'lint'], Path('/project/apps/portal')),
            (['npm', 'test'], Path('/project/apps/portal')),
            (['npm', 'run', 'build'], Path('/project/apps/portal')),
        ])

    def test_portal_accepts_npm_shrinkwrap_without_package_lock(self):
        result, calls, _, _ = self.run_main(True, lockfiles=('npm-shrinkwrap.json',))
        self.assertEqual(result, 0)
        self.assertTrue(any(call.args[1] == ['npm', 'ci'] for call in calls))

    def test_portal_without_lockfile_fails_before_commands(self):
        result, calls, tooling, _ = self.run_main(True, lockfiles=())
        self.assertEqual(result, 1)
        self.assertEqual(calls, [])
        tooling.assert_not_called()
        with mock.patch.object(Path, 'is_file', return_value=False):
            errors = ci_check.check_portal_lockfile(Path('/project/apps/portal'))
        self.assertEqual(errors, [
            'apps/portal requires package-lock.json or npm-shrinkwrap.json for npm ci'
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

    def test_command_start_failure_is_reported(self):
        output = io.StringIO()
        with mock.patch.object(ci_check.subprocess, 'run', side_effect=FileNotFoundError('missing')), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(output):
            self.assertFalse(ci_check.run_command('Portal tests', ['npm', 'test']))
        self.assertIn('Failed to start Portal tests', output.getvalue())
        self.assertIn('missing', output.getvalue())

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
