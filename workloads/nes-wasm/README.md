<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Portable NES workload

The production package composes the pinned emulator with the shared `nes-agent`
action loop, `nes-protocol` codecs, guest SDK transport and `WasmSession`. It uses
one fixed 16 MiB memory. ROM length and bounded chunks arrive through declared
payloads; initialization runs exactly once before the setup lifecycle event.
Observation registration publishes the ordinary power-on billboard. Every action
publishes intermediate RAM frames and stops at the shared frame-complete event.
The backend remains unavailable in default product builds until final host and
performance qualification passes.

`build.py` writes the exact `play-agent.wasm`, `manifest.json` and `debug-map.json`.
The manifest binds the module, mappings, pinned compiler/core versions and guest
sources. Original DWARF stays in the module. Mappings connect original WASM bytes,
admitted numerical transforms and compiled interpreter positions; synthetic code
is marked explicitly. The package loader checks both artifact and execution
identities. No ROM, emulator binary or ROM-containing snapshot is checked in.

```sh
python3 workloads/nes-wasm/build.py \
  --sdk "$WASI_SDK" --quicknes "$QUICKNES_SOURCE" --output "$PACKAGE"
cargo +1.97.0 run --release --manifest-path workloads/nes-wasm/Cargo.toml \
  --bin qualify -- compare "$PACKAGE" "$NOVA_ROM" "$RESULT" "$NATIVE_CORE"
cargo +1.97.0 run --release --manifest-path workloads/nes-wasm/Cargo.toml \
  --bin qualify -- restore "$PACKAGE" "$CHECKPOINT" "$RESULT" none
```

The standalone runner uses the ordinary machine adapter, compares every observed
frame and save-RAM endpoint with native execution, exports action checkpoints,
checks fresh reconstruction, and tests identical and different sibling inputs.
The restore mode starts a fresh process directly from the artifact. Native game
agreement is a functional check; complete same-artifact hashes prove replay.
Miri exercises guest allocation and owned/bounded FFI buffers against a Rust
shim; the real emulator executes within the interpreter's checked linear memory.

## Historical feasibility experiment

`qualification/` preserves the first milestone's experiment and frozen budgets.
Its source and digests are historical; later milestones use the production
package and validated shared snapshot/cache implementation.

The selected continuation approach is **Wasmi 0.46.0 with the pinned snapshot
extension**. The Asyncify/Wasmtime implementation remains a comparison fixture,
not a second production backend. `qualification/FEASIBILITY.json` records the
measured costs, exact inputs, checks, and cross-host state digests.

## Workload probe

`qualification/build.py` builds QuickNES revision
`26bb785c9deddb66a17717b21bb4e328f03ade32` with wasi-sdk 27.0 / LLVM 20.1.8,
links the repository's allocation shim, and calls a no-std Rust 1.97.0 action
function from a C libretro harness. The Rust/C/C++ ABI, constructor initialization,
allocator, function table, and scalar floating point are exercised by 120 Nova
frames. ROM bytes are loaded from a declared host input and verified against the
existing Nova digest. No ROM, emulator binary, or snapshot containing a ROM is
checked in here.

The module uses one fixed 16 MiB memory, the linker-defined fixed function table,
and imports `harmony_v1.decision` plus WASI preview-1 `fd_close`, `fd_seek`, and
`fd_write`. The Wasmi probe fails on an unexpected console operation; its closed
and absent descriptors return BADF. The Asyncify comparison captures console
writes with range checks. These probe transports are not the shared SDK service
implementation required by milestone 4.

## Continuation inventory

The Wasmi extension captures every allocated value register and live call frame.
Frame positions are a compiled-function index and instruction offset, with
frame/base offsets and result registers. One eager-translated module per engine
makes compiled-function allocation order stable. Restore resolves positions
against the new engine's immutable instruction arrays. No engine pointer,
store handle, or native stack address enters the artifact.

The host sidecar records linear memory, mutable scalar globals, table entries as
exported function identities, data/element segment lengths (including dropped
segments), available and cumulatively supplied fuel, the pending import argument,
and committed decision effects. Instantiation performs no automatic start;
explicit initialization is called once for a new execution and never during
restore. Initialization state is implicit in these qualification snapshots: every
exported continuation belongs to an initialized run. A product sidecar must make
that flag explicit.

This is a trusted-artifact experiment. The unsafe continuation restore API
requires a capture from the exact same scalar-only module and eager translation
configuration, with only controlled scalar/function-index test mutations.
Structural offset checks are not a validator for adversarial snapshots. The
production implementation must validate or authenticate all continuation
structure before a safe restore API is exposed. Segments restore into fresh
instances; resurrecting a dropped segment in a reused instance requires staging
a fresh instance. Multiple modules/instances and reference-valued registers are
outside this prototype.

Full-copy captures are the oracle. Exact 4 KiB comparisons report changed pages
without hashing to decide equality. The retained portable state for the Nova
probe is about 16 MiB plus 8 KiB of metadata; memory retention grows linearly
with full-copy checkpoints until milestone 5 introduces shared page layers.
The measured process RSS includes the instance, interpreter, captured state,
and encoded artifact, and is recorded separately from retained artifact bytes.

## Runtime changes and numerical semantics

`../../consonance/wasm/qualification/wasmi-snapshot.patch` changes eight upstream files. In addition
to capture/restore, it initializes every allocated register: reading unused
uninitialized result or scratch registers during capture is undefined behavior.
The new instruction pointer retains provenance for the whole instruction array,
so resuming may advance to later instructions. Both paths are exercised under
Miri, along with independent-engine restore and an uninstrumented source loop.

Wasmi has no NaN-canonicalization configuration in this pinned version. The
feasibility transform wraps NaN-producing scalar arithmetic with canonical-NaN
helpers, and canonicalizes operands before promotion/demotion. The initial
signaling-NaN demotion fixture exposed an x86 result of zero with an unpinned
Rust 1.98 host build. The final experiment pins Rust 1.97.0 on every host and
passes the expanded same-artifact transfer fixture. Negation, absolute value,
copysign, constants, and bit reinterpretation are not canonicalized; signed zero
and subnormal checks are retained. The production admission/translation path
must preserve this numerical policy and test constant folding separately from
runtime arithmetic.

The original Clang module retains DWARF, and `transform.py` retains generated
WAT before and after transformation. Exported function indexes identify the
original module functions; canonicalization helpers append new functions and
introduce no imports. Snapshot instruction offsets identify Wasmi bytecode,
not original WASM byte offsets or C/Rust source lines. A debugger must not use
original DWARF addresses as transformed instruction addresses. Milestone 6 must
package a verified mapping for the final product artifact.

## Comparison and fixed budgets

Asyncify uses Binaryen 123, conservative indirect-call instrumentation, and
Wasmtime 36.0.0 with NaN canonicalization. The transform inserts deterministic
checks at function entries and loop headers, with a yield every 4096 checks.
Unwind stacks occupy a reserved end-of-memory area in this experiment. A
production transform would have to reserve that area in the module layout.
Functions, globals, and tables are exported to capture their state. Import
rewind consumes the pending request once, without replaying committed effects.

The same nested/indirect-call fixture and QuickNES workload restore in a fresh
process under both approaches. Asyncify restores transfer across architectures,
but its uninterrupted and interrupted final states differ in runtime fuel,
stale unwind-stack bytes, and its private data-pointer global. Supporting its
execution contract would require an independent cost model, canonical scratch
state, explicit passive-segment handling, and stronger bounded-coverage
instrumentation. Wasmi's interpreter continuation preserves fuel and complete
state directly, with a smaller maintenance surface, so it is selected even
though the Asyncify execution prototype is somewhat faster.

The Python Asyncify capture timing includes base64 encoding; the Wasmi capture
timing excludes its Postcard encoding. They are recorded as prototype pipeline
costs, not an isolated comparison of memory-copy implementations. Code expansion
compares stripped module sizes so debug information cannot hide expansion.

The frozen initial 16 MiB / 120-frame budgets are:

| Measurement | Ceiling |
| --- | --- |
| Complete 120-frame execution | 500 ms |
| Full capture, including exact page comparison when restoring | 15 ms |
| Fresh restore, including artifact decoding and eager translation | 100 ms |
| Retained serialized checkpoint | 17 MiB |

Eleven local samples must all meet the budgets. Whole run/branch/restore cycles,
sparse-page scaling, and long histories remain milestone 5/7 qualifications.
These wall-clock measurements are host diagnostics; they never supply guest
virtual time or decisions.

## Reproduce

Provide the pinned wasi-sdk, Binaryen, QuickNES checkout, source-built Nova ROM,
Rust 1.97.0 with `wasm32-wasip1`, and WABT 1.0.39. Download the Wasmi 0.46.0
crate archive; `prepare-wasmi.py` verifies its SHA-256 before applying the patch
in a fresh disposable directory.

```sh
python3 workloads/nes-wasm/qualification/build.py \
  --sdk "$WASI_SDK" --quicknes "$QUICKNES_SOURCE" --output "$PROBE_BUILD"
python3 consonance/wasm/qualification/prepare-wasmi.py \
  "$WASMI_ARCHIVE" "$PATCH_OUTPUT"
python3 consonance/wasm/qualification/transform.py \
  "$PROBE_BUILD/quicknes.wasm" "$PROBE_BUILD/wasmi" \
  --wasm-opt "$BINARYEN/bin/wasm-opt" --wasmi
cargo +1.97.0 build --release --locked \
  --manifest-path workloads/nes-wasm/qualification/wasmi/Cargo.toml \
  --config "patch.crates-io.wasmi.path=\"$PATCH_OUTPUT/wasmi-0.46.0\""
```

Compile `guest/continuation.wat` and `guest/floats.wat` with `wat2wasm`, then
apply the same `--wasmi` transform. Pass those modules, the workload module,
the Nova ROM, and the probe executable to `qualification/qualify.py`. Its
local result deliberately requires separate cross-host and Miri evidence.
For the Asyncify comparison, omit `--wasmi` and run `qualification/asyncify.py`
with a disposable environment containing Wasmtime 36.0.0. On this macOS host,
Homebrew Python runs Wasmtime; Apple's command-line-tools Python is terminated
by its guarded Mach exception-port behavior when guest execution begins.

```sh
python3 -m unittest discover -s consonance/wasm/qualification -p 'test_*.py'
cargo +1.97.0 test --locked \
  --manifest-path workloads/nes-wasm/qualification/wasmi/Cargo.toml \
  --config "patch.crates-io.wasmi.path=\"$PATCH_OUTPUT/wasmi-0.46.0\""
cargo +1.97.0 clippy --locked --all-targets \
  --manifest-path workloads/nes-wasm/qualification/wasmi/Cargo.toml \
  --config "patch.crates-io.wasmi.path=\"$PATCH_OUTPUT/wasmi-0.46.0\"" -- -D warnings
```

Run Miri from a disposable directory outside the repository, using an absolute
manifest path and the same patch configuration. The repository's macOS hardware
runner signs native executables and cannot execute Miri's non-native artifact.
Use `MIRIFLAGS=-Zmiri-permissive-provenance`; the interpreter's existing pointer
representation requires it. The three branch/import/segment tests and the fuel
continuation test have passed under Miri in this experiment.

## Upgrade procedure

Pin a new source archive, checksum, lockfile, compiler, transform, and runtime
configuration together. Apply the patch without fuzz, inspect its changed
ownership and register-allocation assumptions, then rerun Miri, the planted
continuation differences, NaN/zero/subnormal fixtures, local budgets, and
same-artifact transfers. Record a new execution identity; do not reuse old
snapshots or infer compatibility from matching game observations.

The admitted build links `guest/fixed-heap.c` through the linker `sbrk` wrapper.
The initial libc heap remains bounded by the declared 16 MiB memory. Querying
its end returns that fixed capacity; extension returns `ENOMEM`. No growth
instruction survives linking. The original feasibility artifact predates this
admission change; its digests remain historical measurements.

The current harness imports the versioned `harmony_v1.request` transport and
encodes opaque SDK service questions with stable request IDs. Nominal answers
retain the portable Rust action suggestion; data answers supply the declared
button mask. The historical milestone-1 probe uses its experimental decision
import. Reproduce those recorded digests from commit `9551b318e`; current builds
use the admitted ABI and the session runner completed in the next milestone.
