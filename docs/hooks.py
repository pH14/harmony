# SPDX-License-Identifier: AGPL-3.0-or-later
"""Render exactly the examples CI executes and the current executable's help."""
import importlib.util
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('docs_examples', ROOT / 'scripts/docs_examples.py')
examples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(examples)


def on_pre_build(config):
    examples.lint(ROOT)
    binary = Path(os.environ.get('HARMONY_BINARY', ROOT / 'target/release/harmony')).resolve()
    examples.check_interface(examples.cli_reference(binary), ROOT)


def on_page_markdown(markdown, **kwargs):
    binary = Path(os.environ.get('HARMONY_BINARY', ROOT / 'target/release/harmony')).resolve()
    return examples.render(markdown, ROOT, binary)
