import contextlib
import io
import tempfile
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
        cargo_checks = [call.args[1] for call in calls if call.args[1][:2] in (['cargo', 'clippy'], ['cargo', 'test'])]
        self.assertEqual(len(cargo_checks), 2)
        self.assertTrue(all('--no-default-features' in command for command in cargo_checks))
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
        build_index = commands.index(['npm', 'run', 'build'])
        for command in ('clippy', 'test'):
            index = next(i for i, args in enumerate(commands) if args[:2] == ['cargo', command])
            self.assertLess(build_index, index)
            self.assertNotIn('--no-default-features', commands[index])
        self.assertEqual(calls[commands.index(type_check)].args[2], Path('/project'))
        self.assertEqual(portal_calls, [
            (['npm', 'ci'], Path('/project/apps/portal')),
            (['npm', 'run', 'lint'], Path('/project/apps/portal')),
            (['npm', 'test'], Path('/project/apps/portal')),
            (['npm', 'run', 'build'], Path('/project/apps/portal')),
        ])

    def test_extension_example_runs_after_rust_tests(self):
        result, calls, _, _ = self.run_main(False)
        self.assertEqual(result, 0)
        commands = [c.args[1] for c in calls]
        rust_tests = next(command for command in commands if command[:3] == ['cargo', 'test', '--workspace'])
        example = ['cargo', 'run', '-q', '-p', 'genaimage-core', '--example', 'extending', '--locked']
        self.assertLess(commands.index(rust_tests), commands.index(example))

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


class CliBoundaryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / 'crates/genaimage/src'
        self.source.mkdir(parents=True)
        (self.source / 'serve.rs').write_text(
            'use genaimage_core::HarnessConfig;\n'
            'pub(crate) async fn execute(cli: &Cli, portal_dev_url: Option<&str>) -> ExitCode {}\n'
        )
        (self.source / 'config_local.rs').write_text(
            'use genaimage_core::HarnessConfig;\n'
            'pub(crate) fn execute(cli: &Cli, force: bool) -> ExitCode {}\n'
        )
        (self.source / 'main.rs').write_text(
            'mod serve; mod config_local;\n'
            'fn main() { serve::execute(&cli, None); config_local::execute(&cli, true); }\n'
        )
        self.patch = mock.patch.object(ci_check, 'ROOT', self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def test_denies_local_crates_in_all_client_modules(self):
        nested = self.source / 'client'
        nested.mkdir()
        (nested / 'commands.rs').write_text(
            'use genaimage_core::Harness;\n'
            'let _ = genaimage_comfyui::ComfyUiClient;\n'
            'extern crate genaimage_server;\n'
        )
        errors = ci_check.check_cli_client_boundary()
        self.assertEqual(len(errors), 3)
        self.assertTrue(all('client/commands.rs:' in error for error in errors))
        (nested / 'commands.rs').write_text('use genaimage_api::RunResponse;')
        self.assertEqual(ci_check.check_cli_client_boundary(), [])

    def test_reexport_and_indirect_local_module_reference_are_both_rejected(self):
        serve = self.source / 'serve.rs'
        serve.write_text(serve.read_text() +
            'pub use genaimage_core::HarnessConfig as LocalConfig;\n')
        (self.source / 'server_commands.rs').write_text(
            'use crate::serve::LocalConfig;\n'
        )
        errors = ci_check.check_cli_client_boundary()
        self.assertTrue(any('serve.rs:' in error and 'dispatch function' in error for error in errors))
        self.assertTrue(any('server_commands.rs:' in error and 'only main' in error for error in errors))

    def test_local_public_signatures_and_reexports_are_rejected(self):
        serve = self.source / 'serve.rs'
        original = serve.read_text()
        for extra in [
            'pub(crate) use genaimage_core::HarnessConfig as LocalConfig;',
            'pub type LocalConfig = HarnessConfig;',
            'pub(crate) fn config() -> HarnessConfig { todo!() }',
            '#[macro_export] macro_rules! alias { () => {} }',
        ]:
            with self.subTest(extra=extra):
                serve.write_text(original + extra)
                self.assertTrue(ci_check.check_cli_client_boundary())
        serve.write_text(original.replace('-> ExitCode', '-> HarnessConfig'))
        self.assertTrue(ci_check.check_cli_client_boundary())

    def test_client_references_and_main_aliases_are_rejected(self):
        client = self.source / 'completions.rs'
        for reference in [
            'use crate::{serve::execute};',
            'use crate::{serve as local};',
            'let x = super::serve::execute;',
            'let x = serve::execute;',
            'use crate::{config_local as local};',
            'let x = crate::r#serve::execute;',
        ]:
            with self.subTest(reference=reference):
                client.write_text(reference)
                self.assertTrue(ci_check.check_cli_client_boundary())
        client.unlink()
        main = self.source / 'main.rs'
        original = main.read_text()
        for reference in ['use crate::serve;', 'let x = serve::LocalConfig;',
                          'crate::config_local::execute(&cli, true);']:
            with self.subTest(reference=reference):
                main.write_text(original + reference)
                self.assertTrue(ci_check.check_cli_client_boundary())

    def test_nested_local_names_and_cross_local_dispatch_are_rejected(self):
        nested = self.source / 'client'
        nested.mkdir()
        (nested / 'serve.rs').write_text('use genaimage_core::HarnessConfig;')
        self.assertTrue(any('client/serve.rs' in error for error in ci_check.check_cli_client_boundary()))
        (nested / 'serve.rs').unlink()
        config = self.source / 'config_local.rs'
        config.write_text(config.read_text() + 'fn indirect() { crate::serve::execute(&cli); }')
        self.assertTrue(any('only main' in error for error in ci_check.check_cli_client_boundary()))

    def test_client_source_includes_and_include_aliases_are_rejected(self):
        client = self.source / 'open.rs'
        for source in [
            'include!("serve.rs");',
            'std::include!(concat!("serve", ".rs"));',
            'include_str!(r#"config_local.rs"#);',
            'include_bytes!("serve.rs");',
            'use std::include as alias; alias!("serve.rs");',
            'include_str!("ordinary.txt");',
        ]:
            with self.subTest(source=source):
                client.write_text(source)
                self.assertTrue(any('include' in error for error in ci_check.check_cli_client_boundary()))
        client.unlink()
        main = self.source / 'main.rs'
        main.write_text(main.read_text() + 'include!("serve.rs");')
        self.assertTrue(any('include' in error for error in ci_check.check_cli_client_boundary()))

    def test_client_path_attributes_are_rejected(self):
        client = self.source / 'completions.rs'
        for source in [
            '#[path = "serve.rs"] mod alias;',
            '#[path /* comment */ = r#"config_local.rs"#] mod alias;',
            '#[cfg_attr(feature = "x", path = "serve.rs")] mod alias;',
            '#![path = "serve.rs"]',
        ]:
            with self.subTest(source=source):
                client.write_text(source)
                self.assertTrue(any('path attribute' in error for error in ci_check.check_cli_client_boundary()))
        client.write_text('#[allow(unused)] mod ordinary;')
        self.assertEqual(ci_check.check_cli_client_boundary(), [])
        client.unlink()
        main = self.source / 'main.rs'
        main.write_text(main.read_text() + '#[path = "serve.rs"] mod alias;')
        self.assertTrue(any('path attribute' in error for error in ci_check.check_cli_client_boundary()))

    def test_comments_and_string_literals_are_not_references(self):
        (self.source / 'open.rs').write_text(
            '// crate::serve, include!("serve.rs") and #[path = "serve.rs"] are not code\n'
            '/* outer /* pub use crate::config_local; */ still comment */\n'
            'let a = "genaimage_server and serve";\n'
            'let b = r##"pub use crate::serve; include!(\"serve.rs\"); #[path = \"serve.rs\"]"##;\n'
            "let c = '\\n';\n"
        )
        self.assertEqual(ci_check.check_cli_client_boundary(), [])


if __name__ == '__main__':
    unittest.main()
