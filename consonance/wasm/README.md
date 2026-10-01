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
initialization. Artifact restore validates complete state, resource bounds and
compiled continuation structure before reconstructing a runtime.

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
exact signatures. The session driver implements this closed transport. Other preview-1 imports are rejected.

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

## In-process session and transport

`WasmSession` implements the portable `SearchSession` contract. Construction
instantiates a fixed-capacity module and records its initial setup snapshot;
it does not execute an automatic start or repeat initialization. The declared
`Invocation` supplies an exported entry and scalar arguments. Workload setup
runs through that entry and calls `seal_setup` at its lifecycle boundary before
ordinary search begins. Branches and replay reconstruct a fresh instance from
an internal capture without invoking guest setup again.

The versioned request ABI packs service in the high 16 bits and opcode in the
low 16 bits of the first argument. Remaining arguments are request pointer,
request length, output pointer and output capacity. Payloads use the existing
SDK encodings. A nonnegative result is the response length; a negative result
is the negated protocol status. Requests and response capacity are bounded by
`MAX_PAYLOAD`; the full declared output range is validated before a service
handler runs. Invalid guest ranges return OutOfRange; malformed SDK payloads
return BadRequest. An invalid external resolution leaves the request pending.
SDK decisions, coverage, payloads and event classification use
the shared environment helpers. Entropy uses the shared seeded supply. Console
bytes and events are captured, while observation descriptors refer to bounded
linear-memory ranges and support explicit revocation.

Every import first creates a pending request. Preparing a response clones host
state and validates the resolution, all output ranges and accounting. Completion
writes the validated i32 result without executing guest code, commits host state,
memory and fuel accounting, and only then resumes execution. A failed preparation
preserves the pending request and complete state. Lifecycle and assertion stops
occur after that atomic completion and report the actual charged moment.

A payload branch preserves entropy and the extension handler unless the branch
explicitly supplies a different service configuration. `branch_input` replaces
the declared environment as a whole. Verbatim replay restores all inputs and
host state. Scheduled machine effects are rejected before restore. Cancellation
belongs to the live session and cannot be rewound; `run_with_watchdog` connects
the shared progress watchdog to that same cancellation latch.

The descriptor subset opens captured stdout and stderr only. `fd_write` accepts
at most 128 vectors and `MAX_PAYLOAD` bytes, validates every range before capturing
bytes, and writes the exact count. Closed or unknown descriptors return BADF (8),
excess vector counts return INVAL (28), and invalid ranges or excess bytes return
FAULT (21). `fd_close` captures closure and returns BADF on repeat. `fd_seek`
returns SPIPE (70) for an open console descriptor and BADF otherwise; it writes
no offset because console streams are never seekable. Process exit, arguments,
environment, clocks and random preview-1 imports were not required by the build
probe and remain rejected. Declared input, virtual time and seeded entropy are
provided through the shared session and SDK contracts.

Immutable snapshots support hashing, independent branches, replay, drop, shared
caching and validated external artifact imports.

## Portable snapshots and shared cache

The fixed linear memory uses the shared snapshot store's 4 KiB layers and
content interning. Capture compares every page exactly against an immutable
parent and writes only changed pages. It copies the live scalar continuation
and host metadata; completed event and console history retain immutable shared
chunks. Input tapes share their storage across environment clones. No dirty
tracking is used. The full-copy continuation experiment remains the oracle.

Within a live session, restores reuse the compiled module and instantiate a
fresh store and instance. `WasmSession::from_snapshot` decodes and validates an
artifact, compiles eagerly once, and directly creates the restored session.
It executes no initialization or host effects. Import into an existing session
adds an immutable handle after validation and leaves live execution unchanged.
Replay installs a validated replacement instance atomically.

Artifacts use the shared sparse snapshot container with canonical base zero
and the execution identity as image identity. Ordinary frame numbers identify
memory pages. The high 32 bits select four bounded host-state sections: declared
input, recorded environment, events and console. These sections also use 4 KiB
pages in cache deltas, allowing history prefixes to remain shared. Zero pages
are implicit, page order is strict, and partial-page padding must be zero.

The versioned sidecar includes every scalar register, frame, global, table,
passive-segment status, root invocation and execution phase, cumulative fuel,
pending request, observations, coverage thresholds and descriptor state. BLAKE3
protects the complete memory image; SHA-256 protects host sections and the
checksummed sidecar envelope. Digests detect corruption; structural checks
prevent unsafe reconstruction from caller-controlled frame metadata. The
format does not authenticate who created an artifact.

Decoding enforces a 4 MiB sidecar and 16 MiB per host section, verifies identity,
checksums, canonical encoding, page inventory, observations, accounting and
execution phase, and then checks runtime globals, table capacities, root results
and compiled continuation structure. A corrupt or incompatible artifact leaves
live state and retained handles unchanged. Reference bits and host pointers
never appear in the format.

The shared cache stores anchors and page deltas, including changed host-section
pages and reverts. Leases preserve required ancestry; releasing handles and
cache leases allows their pages and history chunks to be collected. Store
telemetry reports resident page/sidecar payloads, unique shared host chunks and
captured register/frame buffers; allocator and index overhead remains outside
that payload count.

`qualification/SNAPSHOT-BUDGETS.json` records release measurements for 64 KiB,
1 MiB and 16 MiB memories with 0, 1,000 and 10,000 prior events. Eleven samples
per case pass the original 15 ms capture/comparison and 100 ms fresh restore
budgets on the measured host. Representative workload cycles and all supported
host transfers still require the final qualification milestone.
