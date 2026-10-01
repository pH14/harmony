#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the exact portable play-agent and pinned emulator package."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from qualification.build import QUICKNES, build_core, command


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--quicknes", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    root = source.parents[1]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    sdk, quicknes = args.sdk.resolve(), args.quicknes.resolve()
    revision = subprocess.check_output(["git", "-C", str(quicknes), "rev-parse", "HEAD"], text=True).strip()
    if revision != QUICKNES or subprocess.check_output(["git", "-C", str(quicknes), "diff", "--name-only"], text=True).strip():
        raise SystemExit("Pinned, unmodified QuickNES source required")
    version = subprocess.check_output([str(sdk / "bin/clang"), "--version"], text=True).splitlines()[0]
    if "20.1.8" not in version:
        raise SystemExit("wasi-sdk 27 / LLVM 20.1.8 required")
    rustc = subprocess.check_output(["rustc", "+1.97.0", "-V"], text=True).strip()
    if rustc != "rustc 1.97.0 (2d8144b78 2026-07-07)":
        raise SystemExit("Rust 1.97.0 required")
    build_core(sdk, quicknes)
    command(["cargo", "+1.97.0", "build", "--locked", "--release", "--target", "wasm32-wasip1", "--manifest-path", source / "guest/Cargo.toml"])
    command([sdk / "bin/clang", "-O2", "-g", "-I", quicknes / "libretro/libretro-common/include", "-c", source / "guest/core.c", "-o", output / "core.o"])
    command([sdk / "bin/clang", "-O2", "-g", "-c", source / "qualification/guest/fixed-heap.c", "-o", output / "heap.o"])
    command([sdk / "bin/clang++", "-O2", "-g", "-fno-exceptions", "-fno-rtti", "-c", root / "scripts/quicknes-static-runtime.cpp", "-o", output / "runtime.o"])
    command([sdk / "bin/clang", "-mexec-model=reactor", "-Wl,--initial-memory=16777216", "-Wl,--max-memory=16777216", "-Wl,-z,stack-size=1048576", "-Wl,--wrap=sbrk", "-Wl,--export=play", "-Wl,--export-table", output / "heap.o", output / "core.o", source / "guest/target/wasm32-wasip1/release/libnes_wasm_play_agent.a", quicknes / "libquicknes_wasm.a", output / "runtime.o", "-lm", "-o", output / "play-agent.wasm"])
    module = (output / "play-agent.wasm").read_bytes()
    command(["cargo", "+1.97.0", "run", "--locked", "--release", "--manifest-path", root / "consonance/wasm/Cargo.toml", "--bin", "debug-map", "--", output / "play-agent.wasm", output / "debug-map.json"])
    sources = [source / "guest/core.c", source / "guest/src/lib.rs", source / "guest/Cargo.lock", source / "build.py", source / "qualification/build.py", root / "scripts/quicknes-static-runtime.cpp", root / "workloads/nes-agent/src/lib.rs", root / "workloads/nes-protocol/src/lib.rs", root / "consonance/wasm/guest/src/lib.rs", root / "consonance/harmony-linux/sdk/src/lib.rs", root / "consonance/hypercall-proto/src/lib.rs"]
    manifest = {"version": 1, "quicknes_revision": revision, "wasi_sdk": "27.0", "llvm": "20.1.8", "rustc": rustc, "entry": "play", "memory_pages": 256,
                "module_sha256": hashlib.sha256(module).hexdigest(), "module_bytes": len(module), "debug_map_sha256": hashlib.sha256((output / "debug-map.json").read_bytes()).hexdigest(), "sources": {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
