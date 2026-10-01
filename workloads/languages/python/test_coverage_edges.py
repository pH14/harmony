# SPDX-License-Identifier: AGPL-3.0-or-later
import csv
import importlib.util
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from coverage_edges import MARKER, write_table


class Library:
    def __init__(self):
        self.calls = []

    def init_coverage_module(self, count, name):
        self.count = count
        return 1000

    def notify_coverage(self, edge):
        self.calls.append(edge - 1000)
        return True


class CoverageEdgesTest(unittest.TestCase):
    def test_sdk_observes_entries_and_repeated_branches_after_source_copy(self):
        from antithesis._internal.coverage import _Resolver
        source = '''def decorator(function):
    return function
class Worker:
    @decorator
    def run(self, value):
        while value > 0:
            value -= 1
        if value == 0:
            value = 1
        return value
def outer():
    def inner():
        return 7
    return inner()
def iterate(values):
    for value in values:
        pass
'''
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "source"
            root.mkdir()
            path = root / "sample.py"
            path.write_text(source)
            table = Path(directory) / "sample.sym.tsv"
            write_table(root, "sample", table)
            first = table.read_bytes()
            write_table(root, "sample", table)
            self.assertEqual(table.read_bytes(), first)
            self.assertEqual(path.read_text().count(MARKER), 1)
            copied = Path(directory) / "copied"
            copied.mkdir()
            target = copied / path.name
            target.write_bytes(path.read_bytes())
            with table.open() as stream:
                rows = list(csv.DictReader(stream, delimiter="\t"))
            library = Library()
            resolver = _Resolver(library, str(table))
            self.assertTrue(resolver.register())
            try:
                spec = importlib.util.spec_from_file_location("sample", target)
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                self.assertEqual(module.Worker().run(4), 1)
                self.assertEqual(module.Worker().run(-1), -1)
                self.assertEqual(module.outer(), 7)
                module.iterate([])
                module.iterate([1, 2, 3])
            finally:
                resolver.unregister()
            observed = Counter(library.calls)
            module_entry = next(row for row in rows if row["function"] == "<module>" and row["edge_kind"] == "entry")
            self.assertGreater(observed[int(module_entry["address"])], 0)
            for row in rows:
                if (row["class"], row["function"]) in (("Worker", "run"), ("", "outer.<locals>.inner"), ("", "iterate")):
                    self.assertGreater(observed[int(row["address"])], 0, row)
            loop = [row for row in rows if row["begin_line"] == "6"]
            self.assertEqual(len(loop), 2)
            self.assertGreater(max(observed[int(row["address"])] for row in loop), 1)

    def test_rejects_an_empty_source_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                write_table(root, "empty", root / "empty.sym.tsv")


if __name__ == "__main__":
    if sys.version_info < (3, 12):
        raise SystemExit("test requires the recipe's monitoring-capable interpreter")
    unittest.main()
