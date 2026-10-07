# SPDX-License-Identifier: AGPL-3.0-or-later
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import docs_examples as docs


class DocumentationContract(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        shutil.copytree(docs.ROOT / 'docs/examples', self.root / 'docs/examples')
        shutil.copytree(docs.ROOT / 'docs/user', self.root / 'docs/user')

    def test_code_and_example_changes_select_guest_execution(self):
        from ci_scope import selected
        for path in ('cli/src/main.rs', 'dissonance/searcher/src/lib.rs',
                     'consonance/process-proto/src/lib.rs',
                     'workloads/fault-policy/src/lib.rs',
                     'workloads/bugs/category/lost-update/lost_update.c',
                     'docs/examples/walkthrough.sh', 'docs/hooks.py',
                     'scripts/docs_examples.py'):
            with self.subTest(path=path):
                self.assertTrue(selected([path])['harmony_languages'])

    def test_published_examples_have_executable_owners(self):
        docs.lint(self.root)

    def test_edit_is_rendered_from_the_executed_source(self):
        path = self.root / 'docs/examples/walkthrough.sh'
        path.write_text(path.read_text().replace('harmony prepare', 'harmony prepare --planted-invalid-option'))
        source = docs.snippets(self.root)['prepare']
        self.assertIn(source, docs.render('{{ example "prepare" }}', self.root))
        self.assertIn('--planted-invalid-option', source)

    def test_copied_code_cannot_escape_execution(self):
        path = self.root / 'docs/user/untracked.md'
        path.write_text('```sh\nharmony obsolete\n```\n')
        with self.assertRaisesRegex(ValueError, 'tested example'):
            docs.lint(self.root)

    def test_missing_execution_fails_even_when_page_renders(self):
        path = self.root / 'docs/examples/catalog.json'
        plan = json.loads(path.read_text())
        plan['application'] = [s for s in plan['application'] if s['id'] != 'prepare']
        path.write_text(json.dumps(plan))
        with self.assertRaisesRegex(ValueError, 'untested'):
            docs.lint(self.root)

    def test_unknown_include_and_unclosed_region_fail(self):
        path = self.root / 'docs/user/broken.md'
        path.write_text('{{ example "typo" }}')
        with self.assertRaisesRegex(ValueError, 'unknown example'):
            docs.lint(self.root)
        path.unlink()
        path = self.root / 'docs/examples/broken.sh'
        path.write_text('# example: broken\necho unfinished\n')
        with self.assertRaisesRegex(ValueError, 'unclosed'):
            docs.snippets(self.root)

    def test_removed_inline_option_is_rejected(self):
        root = self.root / 'minimal'
        (root / 'docs/user').mkdir(parents=True)
        (root / 'docs/user/page.md').write_text('Use `harmony show` with `--removed`.')
        with self.assertRaisesRegex(ValueError, 'removed CLI option'):
            docs.check_interface('## show\n--json', root)

    def test_removed_inline_command_is_rejected(self):
        root = self.root / 'minimal'
        (root / 'docs/user').mkdir(parents=True)
        (root / 'docs/user/page.md').write_text('Use `harmony obsolete`.')
        with self.assertRaisesRegex(ValueError, 'CLI command obsolete'):
            docs.check_interface('## show\n--json', root)

    def test_bad_command_status_fails_the_runner(self):
        evidence = self.root / 'failed-execution'
        with patch.object(docs, 'lint', return_value={'broken': 'exit 2'}), \
             patch.object(docs, 'catalog', return_value={'host': [{'id': 'broken', 'exit': [0]}]}):
            with self.assertRaisesRegex(AssertionError, 'got 2'):
                docs.run('host', evidence, Path('/usr/bin/false'), self.root)
        record = json.loads((evidence / 'examples.json').read_text())
        self.assertEqual(record[0]['exit'], 2)
        self.assertFalse(record[0]['verified'])

    def test_missing_expected_output_fails_the_runner(self):
        evidence = self.root / 'missing-output'
        with patch.object(docs, 'lint', return_value={'broken': 'echo wrong'}), \
             patch.object(docs, 'catalog', return_value={
                 'host': [{'id': 'broken', 'exit': [0], 'contains': 'required evidence'}]}):
            with self.assertRaisesRegex(AssertionError, 'missing expected output'):
                docs.run('host', evidence, Path('/usr/bin/false'), self.root)
        self.assertFalse(json.loads((evidence / 'examples.json').read_text())[0]['verified'])

    def test_false_finding_is_rejected(self):
        with patch.object(docs, 'load', return_value={'bug_found': False}):
            with self.assertRaisesRegex(AssertionError, 'real finding'):
                docs.verify('finding', self.root, Path('/unused'), {})

    def test_status_one_without_confirmed_assertion_is_rejected(self):
        with patch.object(docs, 'load', return_value={
            'bug_found': True, 'bugs': [{'confirmed': False, 'violations': [1]}]
        }):
            with self.assertRaisesRegex(AssertionError, 'confirmed assertion'):
                docs.verify('finding', self.root, Path('/unused'), {})

    def test_unrelated_assertion_does_not_count_as_counter_finding(self):
        with patch.object(docs, 'load', return_value={
            'bug_found': True, 'bugs': [{'confirmed': True, 'violations': [
                'workload node ends only by a fault the search injected']}]
        }):
            with self.assertRaisesRegex(AssertionError, 'counter assertion'):
                docs.verify('finding', self.root, Path('/unused'), {})

    def test_searches_must_retain_changed_state(self):
        def load(project, name, filename='report.json'):
            if filename == 'manifest.json':
                return {'status': 'complete'}
            return {'executions': 8 if name == 'extended' else 4}
        for kind in ('followup', 'resume', 'shell-search'):
            with self.subTest(kind=kind), patch.object(docs, 'load', side_effect=load), \
                 patch.object(docs.subprocess, 'run') as command:
                command.return_value.returncode = 0
                command.return_value.stdout = ''
                command.return_value.stderr = ''
                with self.assertRaisesRegex(AssertionError, 'saved state lost'):
                    docs.verify(kind, self.root, Path('/unused'), {})

    def test_resume_must_add_work(self):
        def load(project, name, filename='report.json'):
            return {'status': 'complete'} if filename == 'manifest.json' else {'executions': 4}
        with patch.object(docs, 'load', side_effect=load):
            with self.assertRaises(AssertionError):
                docs.verify('resume', self.root, Path('/unused'), {})


if __name__ == '__main__':
    unittest.main()
