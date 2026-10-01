<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# WebAssembly execution

The backend remains unavailable to ordinary users until admission, sessions,
services, portable snapshots and host qualification pass. The runtime experiment
selects Wasmi 0.46.0 with a complete continuation extension. The continuation
fixtures, pinned patch, source preparation and comparison transforms live in
`qualification/`. Workload builds and measured evidence live in
the workload package under `workloads/`.

The continuation records live scalar registers and call frames, stable code
positions, mutable globals, function-index tables and passive segment status.
A fresh instance replaces the old runtime during restore, without re-running
initialization. The experimental restore API accepts trusted captures only;
production artifact validation remains required before enabling the backend.

## Admission and execution identity

`AdmittedModule::new` validates the binary and every function body before
transformation. The profile admits MVP scalar instructions except memory growth. It also admits
sign extension, saturating float conversions, and bulk memory/table operations.
Reference-valued signatures, locals and globals are rejected. Tables hold only
function indices at fixed capacity; table reference loads, stores and growth are
unsupported. SIMD, threads, components, GC, tail calls, multiple memories,
automatic starts, and unknown imports are rejected. Bodies, signatures, resource
counts and module bytes have explicit bounds.

The closed import ABI is `harmony_v1.request(i32,i32,i32,i32,i32)->i32` and the
three preview-1 descriptor operations `fd_write`, `fd_close` and `fd_seek` with
exact signatures. These are admission contracts; session service transport is
implemented in the next milestone. Other preview-1 imports are rejected.

The binary reencoder adds stable function/global/table exports and a hidden
memory export. It canonicalizes NaN-producing arithmetic and canonicalizes
signalling NaN operands before floating-point widening/narrowing. NaN constants use full-width interpreter registers; compressed f64 NaN
constants are disabled in the runtime translator. Constants,
bitwise reinterpretation, signed zero, infinities and subnormals retain their
specified bits. Original custom sections remain available; transformed code
requires an offset map before debugger positions can refer to original source.

Identity includes source and emitted module digests, input bytes, the limit
profile, runtime archive and patch, compiler identity, transform and accounting
source, eager translation, ABI and fuel model. Native builds require the exact
qualified Rust 1.97.0 compiler. Miri builds use their own compiler identity and
cannot exchange snapshots with native builds. A runtime or transform update
creates a different execution identity.

## Deterministic accounting

The following primitives define the session driver implemented next.
`Meter` grants fixed quanta of 1,024 interpreter fuel units, retaining unused
credit between grants. A run request rounds its deadline upward to that quantum
and checks completion only at the fixed interpreter boundaries. It continues
until actual consumed fuel plus committed service cost reaches the rounded
moment, or an earlier captured outcome. Requests never shrink a quantum or
insert a guest boundary. Stops report the actual consumed moment; partitioned
requests reaching the same boundary yield identical registers and frames.

One consumed fuel unit is one virtual-time unit. The pinned interpreter charges
one unit per translated operation and `floor(bytes/64)` for copying memory;
value copying follows its pinned eight-byte scalar-register rule. The transport
charges 64 plus request and answer byte counts once per completed import.
`Meter::maximum_deadline_overshoot` provides a conservative profile bound covering
the largest bounded bulk operation, translated body, quantum and import. Clock,
service cost and remaining credit are restored together.

Stack register and recursion limits produce deterministic guest traps. Allocation
or translation failures remain infrastructure errors. The host cancellation
latch is checked between bounded quanta and host operations; a canceled execution
is abandoned. The latch and watchdog time do not enter guest state or identity.

## Build checks

The package remains outside the product workspace during qualification. The
pinned interpreter archive and reviewed patch build without network access;
Python 3 and `patch` are source-preparation prerequisites on Linux and macOS.

```sh
cargo test --manifest-path consonance/wasm/Cargo.toml -p consonance-wasm
cargo clippy --manifest-path consonance/wasm/Cargo.toml -p consonance-wasm --all-targets -- -D warnings
```
