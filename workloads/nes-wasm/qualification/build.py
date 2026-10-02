#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the pinned QuickNES/Rust/C feasibility artifact without Linux guest code."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

QUICKNES = "26bb785c9deddb66a17717b21bb4e328f03ade32"

def command(args, **kwargs):
    subprocess.run([str(x) for x in args], check=True, **kwargs)

def build_core(sdk, quicknes):
    command(["make", "-C", quicknes, "clean"])
    flags = f'-O2 -g -fno-exceptions -fno-rtti -fno-use-cxa-atexit -DGIT_VERSION=\\"{QUICKNES}\\"'
    command(["make", "-C", quicknes, "-j4", "platform=unix", "STATIC_LINKING=1", "TARGET=libquicknes_wasm.a",
             f"CC={sdk / 'bin/clang'}", f"CXX={sdk / 'bin/clang++'}", f"AR={sdk / 'bin/llvm-ar'}", f"CXXFLAGS={flags}"])

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk", required=True, type=Path)
    parser.add_argument("--quicknes", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    root = source.parents[2]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    revision = subprocess.check_output(["git", "-C", str(args.quicknes), "rev-parse", "HEAD"], text=True).strip()
    if revision != QUICKNES:
        raise SystemExit("QuickNES revision mismatch")
    if subprocess.check_output(["git", "-C", str(args.quicknes), "diff", "--name-only"], text=True).strip():
        raise SystemExit("QuickNES source has local modifications")
    sdk = args.sdk.resolve()
    if subprocess.check_output([str(sdk / "bin/clang"), "--version"], text=True).splitlines()[0].find("20.1") < 0:
        raise SystemExit("wasi-sdk 27 / LLVM 20.1 required")
    rust_version = subprocess.check_output(["rustc", "-V"], text=True).strip()
    if rust_version != "rustc 1.97.0 (2d8144b78 2026-07-07)":
        raise SystemExit("Rust 1.97.0 required")
    start = time.perf_counter()
    build_core(sdk, args.quicknes)
    command(["rustc", "--edition=2024", "--target=wasm32-wasip1", "--crate-type=staticlib", "-C", "panic=abort",
             "-C", "opt-level=2", "-g", source / "guest/chord.rs", "-o", output / "chord.a"])
    command([sdk / "bin/clang", "-O2", "-g", "-I", args.quicknes / "libretro/libretro-common/include", "-c", source / "guest/play-agent.c", "-o", output / "agent.o"])
    command([sdk / "bin/clang", "-O2", "-g", "-c", source / "guest/fixed-heap.c", "-o", output / "fixed-heap.o"])
    command([sdk / "bin/clang++", "-O2", "-g", "-fno-exceptions", "-fno-rtti", "-c", root / "scripts/quicknes-static-runtime.cpp", "-o", output / "runtime.o"])
    command([sdk / "bin/clang", "-mexec-model=reactor", "-Wl,--initial-memory=16777216", "-Wl,--max-memory=16777216",
             "-Wl,-z,stack-size=1048576", "-Wl,--wrap=sbrk", output / "fixed-heap.o", "-Wl,--export-table", output / "agent.o", args.quicknes / "libquicknes_wasm.a",
             output / "chord.a", output / "runtime.o", "-lm", "-o", output / "quicknes.wasm"])
    artifact = (output / "quicknes.wasm").read_bytes()
    (output / "build.json").write_text(json.dumps({"quicknes_revision": revision, "sdk": "27.0", "rustc": subprocess.check_output(["rustc", "-V"], text=True).strip(),
        "build_seconds": time.perf_counter() - start, "module_sha256": hashlib.sha256(artifact).hexdigest(), "module_bytes": len(artifact)}, indent=2) + "\n")

if __name__ == "__main__":
    main()
