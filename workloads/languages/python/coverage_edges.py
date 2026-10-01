#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Generate the AST-span table consumed by the unchanged Antithesis runtime."""

import argparse
import ast
import csv
import re
import tokenize
from pathlib import Path

COLUMNS = ("file", "class", "function", "edge_kind", "address", "begin_line",
           "begin_column", "end_line", "end_column")
MARKER = "# antithesis-module: "


class Edges(ast.NodeVisitor):
    def __init__(self, path):
        self.path = path
        self.rows = []
        self.qual = ""
        self.cls = ""
        self.in_function = False

    def visit_Module(self, node):
        span = ast.Pass(lineno=1, col_offset=0,
                        end_lineno=node.body[-1].end_lineno if node.body else 1,
                        end_col_offset=node.body[-1].end_col_offset if node.body else 0)
        self.row("entry", span, "<module>")
        self.generic_visit(node)

    def row(self, kind, node, qual=None):
        qual = self.qual if qual is None else qual
        cls = self.cls
        func = qual[len(cls) + 1:] if cls and qual.startswith(cls + ".") else qual
        self.rows.append((self.path, cls, func or "<module>", kind,
                          node.lineno, node.col_offset + 1,
                          node.end_lineno, node.end_col_offset + 1))

    def scope(self, node, name, is_function):
        saved = self.qual, self.cls, self.in_function
        prefix = self.qual + (".<locals>." if self.in_function else ".") if self.qual else ""
        self.qual = prefix + name
        if not is_function:
            self.cls = self.qual
        self.in_function = is_function
        entry = ast.copy_location(ast.Pass(), node)
        if getattr(node, "decorator_list", ()):
            entry.lineno = min(item.lineno for item in node.decorator_list)
        if is_function:
            self.row("entry", entry)
        self.generic_visit(node)
        self.qual, self.cls, self.in_function = saved

    def visit_FunctionDef(self, node):
        self.scope(node, node.name, True)

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node):
        self.scope(node, node.name, False)

    def visit_Lambda(self, node):
        self.scope(node, "<lambda>", True)

    def branch(self, node, span):
        self.row("fall-through", span)
        self.row("jump", span)
        self.generic_visit(node)

    def visit_If(self, node):
        self.branch(node, node.test)

    visit_While = visit_If
    visit_IfExp = visit_If
    visit_Assert = visit_If

    def visit_For(self, node):
        self.branch(node, node)

    visit_AsyncFor = visit_For

    def visit_BoolOp(self, node):
        for value in node.values[:-1]:
            self.row("fall-through", value)
            self.row("jump", value)
        self.generic_visit(node)

    def visit_Compare(self, node):
        if len(node.ops) > 1:
            self.branch(node, node)
        else:
            self.generic_visit(node)

    def visit_ListComp(self, node):
        self.branch(node, node)

    visit_SetComp = visit_ListComp
    visit_DictComp = visit_ListComp
    visit_GeneratorExp = visit_ListComp

    def visit_Match(self, node):
        self.branch(node, node)

    visit_ExceptHandler = visit_Match
    visit_With = visit_Match
    visit_AsyncWith = visit_Match

    def visit_comprehension(self, node):
        for condition in node.ifs:
            self.row("fall-through", condition)
            self.row("jump", condition)
        self.generic_visit(node)


def write_table(root, module, output):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", module):
        raise ValueError("module must be an ASCII catalog name")
    rows = []
    for path in sorted(root.rglob("*.py")):
        if any(character in path.relative_to(root).as_posix() for character in "\t\r\n"):
            raise ValueError("the SDK table cannot represent control characters in filenames")
        with tokenize.open(path) as source:
            encoding = source.encoding
            text = source.read()
        text = "\n".join(line for line in text.splitlines()
                         if not line.strip().startswith(MARKER)) + "\n"
        tree = ast.parse(text, filename=str(path))
        edges = Edges(path.relative_to(root).as_posix())
        edges.visit(tree)
        rows.extend(edges.rows)
        path.write_text(text + MARKER + module + "\n", encoding=encoding)
    rows = list(dict.fromkeys(rows))
    if not rows:
        raise ValueError("source tree has no function or branch edges")
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", newline="", encoding="utf-8") as destination:
        writer = csv.writer(destination, delimiter="\t", lineterminator="\n")
        writer.writerow(COLUMNS)
        for address, row in enumerate(rows, 1):
            writer.writerow((*row[:4], address, *row[4:]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("module")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    write_table(args.root, args.module, args.output)
