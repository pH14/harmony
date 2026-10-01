#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Complete-state Asyncify feasibility runner; intentionally isolated from Session."""
import argparse
import base64
import hashlib
import importlib.metadata
import json
from pathlib import Path
import platform
import struct
import time

import wasmtime as w
from wasmtime import _ffi

RUNTIME = "36.0.0"
NOVA = "9107be62a08a0ae51a01f900bd18a95a52fe043f53e9cae4a64d1e8e73114b08"

class Runtime:
    def __init__(self, path):
        if importlib.metadata.version("wasmtime") != RUNTIME:
            raise ValueError("Wasmtime 36.0.0 required")
        self.path = Path(path)
        self.module_bytes = self.path.read_bytes()
        config = w.Config()
        _ffi.wasmtime_config_cranelift_nan_canonicalization_set(config.ptr(), True)
        config.consume_fuel = True
        self.engine = w.Engine(config)
        self.module = w.Module(self.engine, self.module_bytes)
        self.store = w.Store(self.engine)
        self.store.set_fuel(10**12)
        self.pending = None
        self.effects = []
        self.stop_at = None
        self.boundary = 0
        self.answer = None
        self.root = None
        self.args = []
        self.suspended = False
        self.stack_data = None
        self.stack_start = None
        self.initialized = False
        self.exports = None
        linker = w.Linker(self.engine)
        for imp in self.module.imports:
            key = (imp.module, imp.name)
            if key == ("harmony_v1", "decision"):
                callback = self.decision
            elif key == ("harmony_v1", "yield"):
                callback = self.checkpoint
            elif key == ("wasi_snapshot_preview1", "fd_write"):
                callback = self.fd_write
            elif key in (("wasi_snapshot_preview1", "fd_close"), ("wasi_snapshot_preview1", "fd_seek")):
                callback = lambda *args: 8  # BADF: no open descriptors are provided.
            else:
                raise ValueError(f"unsupported import {key}")
            linker.define_func(imp.module, imp.name, imp.type, callback)
        self.instance = linker.instantiate(self.store, self.module)
        self.exports = self.instance.exports(self.store)
        self.memory = self.exports["memory"]
        self.functions = {name: self.exports[name] for name in self.exports if name.startswith("__harmony_func_")}
        self.function_names = {self.func_key(func): name for name, func in self.functions.items()}

    @staticmethod
    def func_key(func):
        return (func._func.store_id, getattr(func._func, "__private"))

    def fd_write(self, fd, iovs, count, written):
        if fd not in (1, 2):
            return 8
        if count > 1024:
            return 28
        size = self.memory.data_len(self.store)
        if min(iovs, count, written) < 0 or iovs + count * 8 > size or written + 4 > size:
            return 21
        chunks = []
        for index in range(count):
            pointer, length = struct.unpack("<II", self.memory.read(self.store, iovs + 8 * index, iovs + 8 * index + 8))
            if pointer + length > size:
                return 21
            chunks.append(bytes(self.memory.read(self.store, pointer, pointer + length)))
        data = b"".join(chunks)
        self.effects.append(["console", fd, base64.b64encode(data).decode()])
        self.memory.write(self.store, struct.pack("<I", len(data)), written)
        return 0

    def event(self, kind, suggestion=None):
        if self.exports["asyncify_get_state"](self.store) == 2:
            if self.pending != [kind, suggestion]:
                raise ValueError("rewound import differs from pending request")
            self.exports["asyncify_stop_rewind"](self.store)
            self.exports["harmony_rewinding"].set_value(self.store, 0)
            self.pending = None
            self.suspended = False
            answer = self.answer if self.answer is not None else suggestion
            self.answer = None
            if kind == "decision":
                self.effects.append(["decision", suggestion, answer])
            return answer
        self.boundary += 1
        if self.boundary == self.stop_at:
            self.pending = [kind, suggestion]
            self.exports["asyncify_start_unwind"](self.store, self.stack_data)
            self.suspended = True
            return 0 if kind == "decision" else None
        if kind == "decision":
            self.effects.append(["decision", suggestion, suggestion])
        return suggestion

    def decision(self, suggestion):
        return self.event("decision", suggestion)

    def checkpoint(self):
        self.event("yield")

    def initialize(self, rom=None):
        if "_initialize" in self.exports:
            self.exports["_initialize"](self.store)
        if rom is not None:
            data = Path(rom).read_bytes()
            if hashlib.sha256(data).hexdigest() != NOVA:
                raise ValueError("Nova artifact mismatch")
            address = self.exports["rom_address"](self.store)
            self.memory.write(self.store, data, address)
            if self.exports["initialize"](self.store, len(data)) != 0:
                raise ValueError("QuickNES initialization failed")
        self.initialized = True
        # Reserve the end of fixed memory for this experiment; the production
        # transform must reserve its own stack in the module's memory layout.
        self.stack_data = self.memory.data_len(self.store) - 65536
        self.stack_start = self.stack_data + 16
        self.memory.write(self.store, struct.pack("<II", self.stack_start, self.memory.data_len(self.store)), self.stack_data)
        self.boundary = 0
        self.effects = []

    def run(self, root="run", args=(11,), stop_at=None):
        self.root, self.args, self.stop_at = root, list(args), stop_at
        result = self.exports[root](self.store, *args)
        if self.suspended:
            self.exports["asyncify_stop_unwind"](self.store)
        return result

    def resume(self, answer=None, stop_at=None):
        if not self.suspended:
            raise ValueError("not suspended")
        self.answer, self.stop_at = answer, stop_at
        self.exports["harmony_rewinding"].set_value(self.store, 1)
        self.exports["asyncify_start_rewind"](self.store, self.stack_data)
        result = self.exports[self.root](self.store, *self.args)
        if self.suspended:
            self.exports["asyncify_stop_unwind"](self.store)
        return result

    def capture(self):
        globals_ = {}
        tables = {}
        for name in self.exports:
            if name.startswith("__harmony_global_"):
                obj = self.exports[name]
                ty = obj.type(self.store)
                if ty.mutable:
                    kind = str(ty.content)
                    value = obj.value(self.store)
                    globals_[name] = [kind, struct.pack("<f" if kind == "f32" else "<d", value).hex() if kind in ("f32", "f64") else value]
            if name.startswith("__harmony_table_"):
                table = self.exports[name]
                entries = []
                for index in range(table.size(self.store)):
                    func = table.get(self.store, index)
                    entries.append(self.function_names[self.func_key(func)] if isinstance(func, w.Func) else None)
                tables[name] = entries
        return {"format": 1, "runtime": RUNTIME, "module": hashlib.sha256(self.module_bytes).hexdigest(),
            "memory": base64.b64encode(self.memory.read(self.store)).decode(), "globals": globals_, "tables": tables,
            "fuel": self.store.get_fuel(), "pending": self.pending, "effects": self.effects.copy(), "boundary": self.boundary,
            "root": self.root, "args": self.args.copy(), "suspended": self.suspended, "stack_data": self.stack_data,
            "stack_start": self.stack_start, "initialized": self.initialized}

    def restore(self, snapshot):
        if snapshot["format"] != 1 or snapshot["runtime"] != RUNTIME or snapshot["module"] != hashlib.sha256(self.module_bytes).hexdigest():
            raise ValueError("snapshot identity mismatch")
        memory = base64.b64decode(snapshot["memory"], validate=True)
        if len(memory) != self.memory.data_len(self.store):
            raise ValueError("memory capacity mismatch")
        self.memory.write(self.store, memory)
        for name, (kind, value) in snapshot["globals"].items():
            if kind in ("f32", "f64"):
                value = struct.unpack("<f" if kind == "f32" else "<d", bytes.fromhex(value))[0]
            self.exports[name].set_value(self.store, value)
        for name, entries in snapshot["tables"].items():
            table = self.exports[name]
            if len(entries) != table.size(self.store):
                raise ValueError("table capacity mismatch")
            for index, entry in enumerate(entries):
                table.set(self.store, index, None if entry is None else self.functions[entry])
        self.store.set_fuel(snapshot["fuel"])
        for key in ("pending", "effects", "boundary", "root", "args", "suspended", "stack_data", "stack_start", "initialized"):
            setattr(self, key, snapshot[key].copy() if isinstance(snapshot[key], list) else snapshot[key])

    def observation(self):
        addresses = [self.exports[key](self.store) for key in ("ram_address", "save_ram_address")]
        return hashlib.sha256(bytes(self.memory.read(self.store, addresses[0], addresses[0] + 2048)) +
            bytes(self.memory.read(self.store, addresses[1], addresses[1] + 8192))).hexdigest()

def canonical(snapshot):
    return hashlib.sha256(json.dumps(snapshot, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("module", type=Path)
    parser.add_argument("--rom", type=Path)
    parser.add_argument("--frames", type=int, default=120)
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--restore", type=Path)
    parser.add_argument("--stop-at", type=int)
    parser.add_argument("--answer", type=int)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    start = time.perf_counter()
    runtime = Runtime(args.module)
    if args.restore:
        runtime.restore(json.loads(args.restore.read_text()))
        restored = time.perf_counter()
        result = runtime.resume(args.answer, args.stop_at)
    else:
        runtime.initialize(args.rom)
        restored = time.perf_counter()
        result = runtime.run(args=(args.frames if args.rom else 11,), stop_at=args.stop_at)
    finished = time.perf_counter()
    snapshot = runtime.capture()
    captured = time.perf_counter()
    if args.capture:
        args.capture.write_text(json.dumps(snapshot, sort_keys=True) + "\n")
    report = {"host": [platform.system(), platform.machine()], "module": snapshot["module"], "result": result,
        "state_sha256": canonical(snapshot), "effects": runtime.effects, "pending": runtime.pending,
        "boundary": runtime.boundary, "fuel": snapshot["fuel"], "memory_bytes": runtime.memory.data_len(runtime.store),
        "setup_or_restore_seconds": restored - start, "run_seconds": finished - restored,
        "capture_seconds": captured - finished, "snapshot_json_bytes": len(json.dumps(snapshot)),
        "observation": runtime.observation() if args.rom else None}
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "effects"}))

if __name__ == "__main__":
    main()
