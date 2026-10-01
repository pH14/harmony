#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Bounded feasibility transform. This is not the production admission validator."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

sys.setrecursionlimit(30000)
TOKEN = re.compile(r'\s+|;;[^\n]*|\(;.*?;\)|"(?:[^"\\]|\\.)*"|[()]|[^\s()]+', re.S)

def parse(source):
    roots, stack = [], []
    for match in TOKEN.finditer(source):
        token = match.group()
        if token.isspace() or token.startswith((";;", "(;")):
            continue
        if token == "(":
            node = []
            (stack[-1] if stack else roots).append(node)
            stack.append(node)
        elif token == ")":
            if not stack:
                raise ValueError("unmatched close")
            stack.pop()
        else:
            if not stack:
                raise ValueError("atom outside module")
            stack[-1].append(token)
    if stack or len(roots) != 1 or roots[0][0] != "module":
        raise ValueError("invalid module")
    return roots[0]

def render(node):
    return "(" + " ".join(render(x) if isinstance(x, list) else x for x in node) + ")"

def exported_state(module):
    counters = {"func": 0, "global": 0, "table": 0}
    exports = []
    for node in module[1:]:
        if not isinstance(node, list):
            continue
        target = node[3] if node[0] == "import" else node
        if target[0] in counters:
            kind = target[0]
            index = counters[kind]
            counters[kind] += 1
            exports.append(["export", json.dumps(f"__harmony_{kind}_{index}"), [kind, str(index)]])
    module.extend(exports)

def instrument(module):
    # Symbolic indexes are required so inserting an import cannot relink calls.
    for node in module[1:]:
        if isinstance(node, list) and node[0] == "start":
            raise ValueError("automatic start must not run during restore")
    stats = {"functions": 0, "loops": 0}
    def walk(node):
        for child in list(node):
            if isinstance(child, list):
                walk(child)
        if node and node[0] == "loop":
            offset = 1
            while offset < len(node) and (isinstance(node[offset], str) and node[offset].startswith("$") or
                  isinstance(node[offset], list) and node[offset][0] in ("param", "result", "type")):
                offset += 1
            node.insert(offset, ["call", "$harmony_tick"])
            stats["loops"] += 1
    for node in module[1:]:
        if isinstance(node, list) and node[0] == "func":
            walk(node)
            offset = 1
            while offset < len(node) and (isinstance(node[offset], str) and node[offset].startswith("$") or
                  isinstance(node[offset], list) and node[offset][0] in ("param", "result", "type", "local", "export")):
                offset += 1
            node.insert(offset, ["call", "$harmony_tick"])
            stats["functions"] += 1
    helper = parse('''(module
      (import "harmony_v1" "yield" (func $harmony_yield))
      (global $harmony_countdown (mut i32) (i32.const 4096))
      (global $harmony_rewinding (export "harmony_rewinding") (mut i32) (i32.const 0))
      (func $harmony_tick
        (if (i32.eqz (global.get $harmony_rewinding))
          (then
            (global.set $harmony_countdown (i32.sub (global.get $harmony_countdown) (i32.const 1)))
            (if (i32.eqz (global.get $harmony_countdown))
              (then
                (global.set $harmony_countdown (i32.const 4096))
                (call $harmony_yield)))))))''')
    module.insert(2 if isinstance(module[1], str) else 1, helper[1])
    module.extend(helper[2:])
    return stats

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("module", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--wasm-opt", required=True, type=Path)
    parser.add_argument("--no-ticks", action="store_true")
    parser.add_argument("--wasmi", action="store_true")
    args = parser.parse_args()
    if subprocess.check_output([str(args.wasm_opt), "--version"], text=True).strip() != "wasm-opt version 123 (version_123)":
        raise SystemExit("Binaryen 123 required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    wat = subprocess.check_output(["wasm2wat", "--generate-names", "--fold-exprs", str(args.module)], text=True)
    module = parse(wat)
    if args.wasmi:
        operations = {"add", "sub", "mul", "div", "sqrt", "ceil", "floor", "trunc", "nearest", "min", "max", "promote_f32", "demote_f64"}
        def floats(node):
            for index, child in enumerate(list(node)):
                if isinstance(child, list):
                    floats(child)
                    opcode = child[0] if child else ""
                    if "." in opcode:
                        kind, operation = opcode.split(".", 1)
                        if kind in ("f32", "f64") and operation in operations:
                            if operation in ("demote_f64", "promote_f32"):
                                operand_kind = "f64" if operation == "demote_f64" else "f32"
                                child[1] = ["call", "$harmony_canonical_" + operand_kind, child[1]]
                            node[index] = ["call", "$harmony_canonical_" + kind, child]
        floats(module)
        for kind in ("f32", "f64"):
            helper = parse(f'(module (func $harmony_canonical_{kind} (param {kind}) (result {kind}) (if (result {kind}) ({kind}.ne (local.get 0) (local.get 0)) (then ({kind}.const nan)) (else (local.get 0)))))')
            module.extend(helper[1:])
        exported_state(module)
        (output / "final.wat").write_text(render(module))
        subprocess.run(["wat2wasm", str(output / "final.wat"), "-o", str(output / "final.wasm")], check=True)
        return
    stats = {"functions": 0, "loops": 0} if args.no_ticks else instrument(module)
    (output / "instrumented.wat").write_text(render(module))
    subprocess.run(["wat2wasm", str(output / "instrumented.wat"), "-o", str(output / "instrumented.wasm")], check=True)
    subprocess.run([str(args.wasm_opt), str(output / "instrumented.wasm"), "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--asyncify", "--pass-arg=asyncify-imports@harmony_v1.decision,harmony_v1.yield", "-g", "-o", str(output / "asyncify.wasm")], check=True)
    transformed = parse(subprocess.check_output(["wasm2wat", "--generate-names", "--fold-exprs", str(output / "asyncify.wasm")], text=True))
    exported_state(transformed)
    (output / "final.wat").write_text(render(transformed))
    subprocess.run(["wat2wasm", str(output / "final.wat"), "-o", str(output / "final.wasm")], check=True)
    (output / "transform.json").write_text(json.dumps({"binaryen": 123, "ticks": stats,
        "input_bytes": args.module.stat().st_size, "output_bytes": (output / "final.wasm").stat().st_size,
        "indirect_calls": "conservative full instrumentation"}, indent=2) + "\n")

if __name__ == "__main__":
    main()
