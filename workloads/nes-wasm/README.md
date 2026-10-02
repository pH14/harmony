<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Portable NES workload

The package composes pinned QuickNES with the shared `nes-agent` action loop,
`nes-protocol` codecs, guest SDK transport and `WasmSession`. It runs on Linux
and macOS, on x86-64 and Arm64, using one fixed 16 MiB memory. ROM length and
bounded chunks arrive through declared payloads; initialization runs once before
the setup lifecycle event. Observation registration publishes the ordinary
power-on billboard. Each action publishes intermediate RAM frames and stops at
the shared frame-complete event. Setup has a 100-million-fuel deadline; every
product action has an eight-billion-fuel budget relative to its restored virtual
time. A guest that misses the lifecycle event returns a deadline stop. The
session also arms the shared five-second idle watchdog, whose cancellation
irrevocably abandons the execution without entering guest state.

## Package and usage

`build.py` writes `play-agent.wasm`, `manifest.json` and `debug-map.json`.
The manifest binds the module, mappings, pinned compiler/core versions and guest
sources. Original DWARF stays in the module. Mappings connect original WASM bytes,
admitted numerical transforms and compiled interpreter positions; generated code
is marked explicitly. The loader checks artifact and execution identities and
bounds reads to 64 KiB for the manifest, 16 MiB for mappings and 4 MiB for the
module. It decodes the same mapping bytes whose digest it verified.

Build tools are Rust 1.97.0 with `wasm32-wasip1`, wasi-sdk 27.0 / LLVM 20.1.8,
and QuickNES revision `26bb785c9deddb66a17717b21bb4e328f03ade32`.
Python 3 and `patch` prepare the checksummed, vendored interpreter offline.
No ROM, emulator binary or ROM-containing snapshot is checked in.

```sh
python3 workloads/nes-wasm/build.py \
  --sdk "$WASI_SDK" --quicknes "$QUICKNES_SOURCE" --output "$PACKAGE"
cargo +1.97.0 build --locked --release -p harmony-cli
target/release/harmony search --package nes --backend wasm \
  --wasm-package "$PACKAGE" "$ROM" --seed 7 --executions 1 --out run
```

NES still defaults to native execution. Explicit WASM selection starts the
portable in-process session without hardware boot or a hypervisor probe.
`--no-default-features --features wasm` builds a CLI without hardware VMM
dependencies. General worker launch remains deferred; the standalone runner
exercises fresh-process restore through the ordinary machine adapter.

## Qualification

```sh
cargo +1.97.0 run --locked --release --manifest-path workloads/nes-wasm/Cargo.toml \
  --bin qualify -- compare "$PACKAGE" "$NOVA_ROM" "$RESULT" "$NATIVE_CORE"
cargo +1.97.0 run --locked --release --manifest-path workloads/nes-wasm/Cargo.toml \
  --bin qualify -- restore "$PACKAGE" "$CHECKPOINT" "$RESULT" none
```

The runner compares every observed RAM frame and save-RAM endpoint with native
execution, exports action checkpoints, checks fresh reconstruction, and tests
identical and different sibling inputs. Restore starts a fresh process directly
from the artifact. Native game agreement is a functional check; complete
same-artifact state hashes prove replay. Adapter frame counts are monotonic host
work diagnostics; the captured guest frame count supplies replay state.

The host exchange consumes one producer-built module on all four supported host
combinations. Each consumer restores every producer's actual checkpoint and
compares complete future hashes. Generic fixtures also transfer stopped nested
and indirect calls, an uninstrumented source loop and a pending import, with
live scalar values and floating-point edge cases. Independently rebuilding an
emulator and comparing game RAM does not establish portable replay.

Miri exercises guest allocation and owned/bounded FFI buffers against a Rust
shim. The real C emulator executes within the interpreter's checked memory;
this does not claim native C execution under Miri.

## Frozen performance limits

The first milestone fixed these limits for each of eleven 16 MiB / 120-frame
samples. Qualification fails if any sample exceeds a limit.

| Measurement | Ceiling |
| --- | --- |
| Complete 120-frame execution | 500 ms |
| Capture, including exact 4 KiB page comparison | 15 ms |
| Fresh decode, validation, eager compilation and restore | 100 ms |
| Serialized checkpoint | 17 MiB |

`measure` branches independently from sealed setup. It reports branch restore,
execution and its deterministic fuel use, exact comparison and capture,
artifact size, fresh restore, retained
payload bytes and the complete comparison cycle separately. Admission and package
loading are outside fresh restore timing. `qualification/measure.py` isolates the
measurement process and records peak RSS, including allocator and index overhead
outside shared-store payload telemetry. Host times never supply guest decisions
or virtual time.

`qualification/budgets.py` enforces the limits per sample. The memory/history
benchmark also checks all nine combinations of 64 KiB, 1 MiB and 16 MiB memories
with 0, 1,000 and 10,000 events, with eleven samples each. CI workflows and their
bounds are registered in `scripts/ci_contract.py` and `docs/WORKFLOWS.md`.
Raw measurements and complete host state digests belong in CI artifacts and the
pull request, outside the source tree.

## History and upgrades

Wasmi 0.46.0 with the pinned continuation extension is the selected runtime.
`qualification/` retains the first milestone's bounded Wasmi/Asyncify comparison
and frozen budget checker. Its experimental decision import, full-copy snapshots
and digests are historical; reproduce the initial evidence from commit
`9551b318e`. Asyncify's differing complete state did not satisfy the replay
contract, so it is not a production backend.

Current builds use the versioned SDK request ABI, shared services, sparse page
store and validated version-2 artifacts described in the
[engine README](../../consonance/wasm/README.md). The fixed-heap shim returns
`ENOMEM` when libc requests extension; no memory-growth instruction is admitted.

Pin compiler, runtime, patches, core, transform and configuration together when
upgrading. Rerun unsafe-path Miri, upstream conformance, all four actual host
transfers, native functional comparisons and unchanged performance limits.
Regenerate mappings against the new execution identity; old snapshots are
incompatible. Deferred profile extensions are tracked in
[#470](https://github.com/pH14/harmony/issues/470), and worker launch composition in
[#471](https://github.com/pH14/harmony/issues/471).
