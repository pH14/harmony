#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""One source for published examples and executable documentation contracts."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
DIRECTIVE = re.compile(r'{{ (example|file) "([^"\n]+)" }}')


def snippets(root=ROOT):
    result = {}
    for path in sorted((root / 'docs/examples').glob('*.sh')):
        name, lines = None, []
        for line in path.read_text().splitlines(keepends=True):
            if line.startswith('# example: '):
                if name is not None:
                    raise ValueError(f'{path}: nested example')
                name, lines = line.removeprefix('# example: ').strip(), []
                if name in result:
                    raise ValueError(f'duplicate example: {name}')
            elif line.strip() == '# endexample':
                if name is None or not ''.join(lines).strip():
                    raise ValueError(f'{path}: empty or unmatched example')
                result[name] = ''.join(lines).rstrip()
                name = None
            elif name is not None:
                lines.append(line)
        if name is not None:
            raise ValueError(f'{path}: unclosed example {name}')
    return result


def catalog(root=ROOT):
    return json.loads((root / 'docs/examples/catalog.json').read_text())


def lint(root=ROOT):
    examples, plan = snippets(root), catalog(root)
    covered, referenced, assets = set(), set(), set()
    for scenario, steps in plan.items():
        for step in steps:
            if not step.get('exit') or not all(isinstance(n, int) for n in step['exit']):
                raise ValueError(f'{scenario}: each step needs expected exit statuses')
            for key in ('id', 'stdin'):
                if key in step:
                    name = step[key]
                    if name not in examples:
                        raise ValueError(f'unknown executable example: {name}')
                    covered.add(name)
    for path in (root / 'docs/user').rglob('*.md'):
        text = path.read_text()
        # Every displayed code sample must have an executable/source owner.
        if re.search(r'^\s*(```|~~~)', text, re.M):
            raise ValueError(f'{path}: use a tested example or source-file include, not a copied code fence')
        for kind, name in DIRECTIVE.findall(text):
            if kind == 'example':
                if name not in examples:
                    raise ValueError(f'{path}: unknown example {name}')
                referenced.add(name)
            else:
                asset = (root / name).resolve()
                if not asset.is_relative_to(root.resolve()) or not asset.is_file():
                    raise ValueError(f'{path}: invalid source file {name}')
                assets.add(name)
        if '{{' in DIRECTIVE.sub('', text).replace('{{ cli }}', ''):
            raise ValueError(f'{path}: unrecognized documentation directive')
    if set(examples) != covered or set(examples) != referenced:
        raise ValueError(f'example coverage mismatch: untested={set(examples)-covered}, '
                         f'unpublished={set(examples)-referenced}, unknown={covered-set(examples)}')
    # These files are copied unchanged into the executable walkthrough below.
    expected_assets = {'docs/examples/counter.toml', 'docs/examples/inspect.sh'}
    if assets != expected_assets:
        raise ValueError(f'source-file coverage mismatch: {assets ^ expected_assets}')
    for name, source in examples.items():
        subprocess.run(['bash', '-n'], input=source, text=True, check=True)
    return examples


def cli_reference(binary):
    def help_for(*args):
        return subprocess.check_output([str(binary), *args, '--help'], text=True, timeout=20).rstrip()
    top = help_for()
    commands = []
    in_commands = False
    for line in top.splitlines():
        if line == 'Commands:':
            in_commands = True
        elif in_commands and line and not line.startswith(' '):
            in_commands = False
        elif in_commands:
            match = re.match(r'  ([a-z][a-z-]*)\s', line)
            if match and match[1] != 'help':
                commands.append(match[1])
    if not commands:
        raise ValueError('could not read command list from built CLI')
    sections = ['```text\n' + top + '\n```']
    for command in commands:
        sections.append(f'## {command}\n\n```text\n{help_for(command)}\n```')
    return '\n\n'.join(sections)



def check_interface(reference, root=ROOT):
    commands = set(re.findall(r'^## ([a-z][a-z-]*)$', reference, re.M))
    flags = set(re.findall(r'--[a-z][a-z-]*', reference))
    for path in (root / 'docs/user').rglob('*.md'):
        for code in re.findall(r'`([^`\n]+)`', path.read_text()):
            command = re.match(r'harmony ([a-z][a-z-]*)', code)
            if command and command[1] not in commands:
                raise ValueError(f'{path}: undocumented CLI command {command[1]}')
            for flag in re.findall(r'--[a-z][a-z-]*', code):
                if flag not in flags:
                    raise ValueError(f'{path}: removed CLI option {flag}')


def render(text, root=ROOT, binary=None):
    examples = snippets(root)
    def replace(match):
        kind, name = match.groups()
        if kind == 'example':
            source, language = examples[name], 'sh'
        else:
            source = (root / name).read_text().rstrip()
            language = {'.sh': 'sh', '.toml': 'toml', '.c': 'c'}[Path(name).suffix]
        return f'```{language}\n{source}\n```'
    text = DIRECTIVE.sub(replace, text)
    if '{{ cli }}' in text:
        if binary is None:
            raise ValueError('build the CLI and set HARMONY_BINARY before rendering the command reference')
        text = text.replace('{{ cli }}', cli_reference(binary))
    return text


def load(project, name, filename='report.json'):
    return json.loads((project / '.harmony/runs' / name / filename).read_text())


def verify(kind, project, binary, env):
    def manifest(name):
        value = load(project, name, 'manifest.json')
        if value['status'] != 'complete':
            raise AssertionError(f'{name} did not complete: {value}')
        return value
    def retained_file(name, filename, expected):
        # Reconstruct a new branch and read the saved file, rather than trusting
        # a terminal transcript or merely checking that a snapshot exists.
        result = subprocess.run([str(binary), 'branch', name, '--step', '0',
                                 '--exec', f'cat /tmp/{filename}', '--stop',
                                 '--name', f'verify-{name}'], cwd=project, env=env,
                                capture_output=True, text=True, timeout=90)
        if (result.returncode not in (0, 1) or expected not in result.stdout
                or load(project, f'verify-{name}', 'branch.json')['command_exit'] != 0):
            raise AssertionError(f'saved state lost {filename}: {result.stdout}\n{result.stderr}')
    if kind == 'finding':
        report = load(project, 'baseline')
        assert report['bug_found'], 'tutorial must discover a real finding'
        assert report['bugs'] and report['bugs'][0]['confirmed'], 'no confirmed assertion in finding 1'
        assert 'the counter holds every finished increment' in report['bugs'][0]['violations'], 'finding 1 is not the counter assertion'
        assert report['execution_failures'] == 0, 'execution failures are not findings'
        manifest('baseline')
    elif kind == 'state':
        manifest('before-failure')
        assert load(project, 'before-failure', 'branch.json')['command_exit'] == 0
    elif kind == 'branch':
        manifest('debugging')
        retained_file('debugging', 'investigation', 'investigating')
    elif kind == 'followup':
        manifest('followup')
        assert load(project, 'followup')['executions'] == 4
        retained_file('followup', 'investigation', 'investigating')
    elif kind == 'resume':
        manifest('extended')
        assert load(project, 'extended')['executions'] == load(project, 'followup')['executions'] + 4
        retained_file('extended', 'investigation', 'investigating')
    elif kind == 'shell':
        manifest('interactive')
        retained_file('interactive', 'shell-change', 'shell change')
    elif kind == 'shell-search':
        manifest('shell-followup')
        assert load(project, 'shell-followup')['executions'] == 4
        retained_file('shell-followup', 'shell-change', 'shell change')
    elif kind == 'script':
        manifest('inspected')
        retained_file('inspected', 'script-change', 'script change')
    else:
        raise ValueError(f'unknown verification: {kind}')


def run(scenario, evidence, binary, root=ROOT):
    examples = lint(root)
    steps = catalog(root)[scenario]
    evidence.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    env['HARMONY_SDK_DIR'] = str(root)
    if scenario == 'host':
        source = project = root
    else:
        source = evidence / 'workspace'
        source.mkdir()
        inputs = ('workloads/languages', 'workloads/faults/runtime',
                  'workloads/bugs/category/lost-update', 'workloads/bugs/interleaving.h',
                  'consonance/harmony-linux/libvoidstar', 'docs/examples')
        for name in inputs:
            destination = source / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            if (root / name).is_dir():
                shutil.copytree(root / name, destination,
                                ignore=shutil.ignore_patterns('target', 'build', '__pycache__'))
            else:
                shutil.copy2(root / name, destination)
        env['HARMONY_SDK_DIR'] = str(source)
        project = source / 'counter-example'
        # Supply build outputs; the published installation step must install them.
        built = source / 'target/release'
        built.mkdir(parents=True)
        shutil.copy2(binary, built / 'harmony')
        guest = Path(os.environ.get('HARMONY_DOCS_GUEST_DIR',
                                   root / 'consonance/harmony-linux/build' / os.uname().machine)).resolve()
        destination = source / 'consonance/harmony-linux/build' / os.uname().machine
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.symlink_to(guest, target_is_directory=True)
        binary = source / '.harmony-install/bin/harmony'
    env['PATH'] = str(binary.parent) + os.pathsep + env.get('PATH', '')
    results = []
    try:
        for step in steps:
            name = step['id']
            cwd = source if step.get('cwd') == 'source' else project
            if name == 'script':
                shutil.copy2(root / 'docs/examples/inspect.sh', project / 'inspect.sh')
            print(f'documentation: {scenario}/{name}', flush=True)
            command = examples[name]
            # A guest shell receives the exact displayed input, including exit.
            stdin = examples[step['stdin']] + '\n' if 'stdin' in step else None
            result = subprocess.run(['bash', '-euo', 'pipefail', '-c', command],
                                    cwd=cwd, env=env, input=stdin, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    timeout=step.get('timeout', 120))
            (evidence / f'{name}.log').write_text(result.stdout)
            results.append({'id': name, 'sha256': hashlib.sha256(command.encode()).hexdigest(),
                            'exit': result.returncode, 'verified': False})
            if result.returncode not in step['exit']:
                raise AssertionError(f'{name}: expected exit {step["exit"]}, got {result.returncode}\n{result.stdout[-6000:]}')
            if step.get('contains') and step['contains'] not in result.stdout:
                raise AssertionError(f'{name}: missing expected output {step["contains"]!r}')
            if step.get('verify'):
                verify(step['verify'], project, binary, env)
            results[-1]['verified'] = True
    finally:
        (evidence / 'examples.json').write_text(json.dumps(results, indent=2) + '\n')
        saved = project / '.harmony/runs'
        if saved.is_dir():
            for result in saved.iterdir():
                if not result.is_dir():
                    continue
                destination = evidence / 'results' / result.name
                destination.mkdir(parents=True)
                for name in ('manifest.json', 'report.json', 'branch.json', 'terminal.log'):
                    if (result / name).is_file():
                        shutil.copy2(result / name, destination / name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('lint', 'host', 'application'))
    parser.add_argument('--evidence', type=Path)
    parser.add_argument('--binary', type=Path, default=Path(os.environ.get('HARMONY_BINARY', ROOT / 'target/release/harmony')))
    args = parser.parse_args()
    if args.mode == 'lint':
        lint()
    else:
        if args.evidence is None:
            parser.error('--evidence is required for execution')
        run(args.mode, args.evidence.resolve(), args.binary.resolve())


if __name__ == '__main__':
    main()
