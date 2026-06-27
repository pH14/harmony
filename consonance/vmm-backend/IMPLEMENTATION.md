# `vmm-backend` — implementation notes

The trap apparatus decoupled behind the `Backend` trait (ruling **R-Backend**). The lower half of
the `docs/BRINGUP.md` crate split: vmm-core (task 15) compiles against `Backend` alone and is
`KVM_RUN`-unaware. This crate delivers **the trait + the first implementation** (stock
`KvmBackend`), plus a portable `MockBackend` for vmm-core's unit tests.

## Task 21 — `PatchedKvmBackend` (the determinism-complete backend)

Task 21 productionizes the task-16 spike (GO) into `PatchedKvmBackend` — the first
determinism-**complete** backend (R-Backend's ratified baseline), surfacing the four exits
stock KVM swallows.

- **`KVM_EXIT_DETERMINISM` decode/complete lives in the pure `kvm` module** (covered by
  synthetic-`kvm_run` unit tests, scrutinized by Miri), validated against the spike's
  `patches/0001-*.patch`: exit reason `41`, the `kvm_run.determinism` payload (read/written by
  bounded raw offset via `RunBuf` + `offset_of!(kvm_run, __bindgen_anon_1)`, since it is not in
  `kvm-bindings`), cap `245`. `decode_determinism` maps `insn ∈ {RDTSC,RDTSCP,RDRAND,RDSEED}` →
  `Exit::Rdtsc/Rdtscp/Rdrand{width}/Rdseed{width}`; `apply_complete_determinism` writes
  `determinism.value` (→ dest / EDX:EAX), `aux` (RDTSCP's `IA32_TSC_AUX` → ECX), and the CF
  success flag (RNG).
- **`PatchedKvmBackend` (`src/patched_kvm.rs`, box-only) is a thin wrapper over `KvmBackend`**:
  it opts into `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` **before** vCPU creation
  (`KvmBackend::build(true)`; `new()` = `build(false)` is byte-identical stock behavior, which is
  why they are distinct backends, not a runtime mode), and overrides only `capabilities()`
  (`deterministic_tsc`/`deterministic_rng = true`). Everything else — CPUID/MSR install, memory
  map, run loop, save/restore, exit counting — is the inner `KvmBackend` verbatim (the patch
  surfaces four exits and nothing more; TSC offset/scaling + the TSC-deadline path are untouched,
  per spike patch 0003).
- **The backend stays a thin wrapper (R-Backend hard rule):** it surfaces + completes the exits
  and computes **no deterministic value** — the V-time TSC and the seeded RNG bytes are computed
  in vmm-core, above the trait. The one completion detail it owns, RDTSCP's `ECX = IA32_TSC_AUX`,
  reflects guest architectural state (read via `KVM_GET_MSRS` in `complete_read`, the contract's
  `allow-stateful` `TSC_AUX`), not contract policy — so the trait shape is unchanged and vmm-core
  supplies only the one `complete_read(value)`.
- **`impl<B: Backend + ?Sized> Backend for Box<B>`** (in `backend.rs`) lets the composition root
  inject `KvmBackend` vs `PatchedKvmBackend` as a `Box<dyn Backend>` (task-21 P5) and run a `Vmm`
  over it — additive, the trait shape and `Exit` enum are unchanged (per the task's hard rule).
- The live patched-module run is verified by vmm-core's box-only `tests/live_determinism.rs`
  (P6); see that crate's IMPLEMENTATION.md for the box evidence + the canonical-patch
  build/`git am` gate.

## What's portable vs. Linux-only

| Surface | Where | Built/tested on |
|---|---|---|
| `Backend` trait, `Exit`/`Event`/`VcpuState`/`Capabilities`/`ExitCounts`/`BackendError`, `CpuidModel`/`MsrFilter`, `Gpa`/`Vtime` | no `cfg` | macOS + Linux |
| `MockBackend` (+ `Completion`) | `#[cfg(feature = "mock")]` (non-default) | macOS + Linux |
| `region` / `run_buf` pointer seams | no `cfg` (the unsafe pointer logic) | macOS + Linux, **under Miri** |
| `KvmBackend` + every `kvm-*`/`libc`/raw-syscall item | `#[cfg(target_os = "linux")]` | **box only** (bare-metal Linux/KVM, `/dev/kvm`) |

A macOS `cargo build -p vmm-backend` compiles the trait + value types (+ `MockBackend` under
`--features mock`) and nothing else, with zero warnings. The `mock` feature is **not**
`#[cfg(test)]` — a test-only mock is invisible to task 15, which turns `mock` on under its
`[dev-dependencies]`.

## Refinements vs. R-Backend's trait sketch (`[refinement]`, for review)

R-Backend §"Follow-ups" anticipated refinement "when the real `KVM_RUN` loop is wired." The
changes from the sketch:

- **`Gpa`/`Vtime` are `#[repr(transparent)]` newtypes**, not bare `u64`s — an address can't be
  confused with a host pointer or a length at a call site.
- **`Vtime`'s unit is a retired-conditional-branch work count**, not nanoseconds — the same axis
  `vtime`'s `work` and task 07's PMU measure. vmm-core converts vns↔work; the backend counts
  hardware events.
- **`save() -> Result<VcpuState>`** is fallible (the `KVM_GET_*` ioctls can fail; library code must
  not `unwrap`, rule #4). `map_memory`/`restore`/everything else likewise return the crate `Result`.
- **`map_memory` is an `unsafe fn`** — a caller-precondition marker: the backend retains `host`'s
  pointer past the `&mut [u8]` borrow (KVM writes through it on every `run`), an invariant the borrow
  checker can't express. See its `# Safety`. (Declaring/implementing an `unsafe fn` needs no `unsafe`
  *block*, so the portable surface still has none.)
- **Exit completion is explicit** (`complete_read`/`complete_fault`/`complete_ok`/
  `complete_hypercall`/`complete_cpuid`). R-Backend modeled reads as `write: Option<_>` but left the
  return path implicit; KVM completes a read by writing the shared `kvm_run` before the next entry,
  so these make it explicit and mockable. Exactly one completion is valid per pending exit.
- **`Wrmsr` is a completion-needing exit** (not no-completion): resuming a filtered `KVM_EXIT_X86_WRMSR`
  without a completion is taken by KVM as `msr.error == 0` (a silent *allow*), so it stays pending
  until `complete_ok` (allow/drop) or `complete_fault` (deny-gp) — fail-closed like the reads.
- **`complete_read` also services the instruction-reads** (`Rdtsc`/`Rdtscp`/`Rdrand`/`Rdseed`) for
  `PatchedKvmBackend`/`DirectVmxBackend`; **`complete_cpuid`** carries CPUID's four registers (one
  `u64` can't). Stock `KvmBackend` never surfaces those exits (below), so it never calls them.
- **`Exit` is closed** (no `_` arm compiles — gate 5) and **`BackendError` is an impl-agnostic closed
  set** with `NotConfigured`/`PendingCompletion`/`NoPendingRead`/`BadCompletion`/`Unsupported`/… so
  vmm-core never branches on which backend produced an error.
- **`CpuidModel`/`MsrFilter` are defined here as portable POD** (the sketch used config types it
  never defined); the backend translates them to `KVM_SET_CPUID2` / `KVM_X86_SET_MSR_FILTER`.

## The KVM exit mapping (stock KVM + `KVM_IRQCHIP_NONE`)

`run()` issues `KVM_RUN`, maps `kvm_run.exit_reason`, then bumps exactly one per-reason counter:

| Raw `kvm_run` exit | `Exit` | Notes |
|---|---|---|
| `KVM_EXIT_IO` | `Io` | OUT value read from the PIO data buffer (via `run_buf`); IN completed by writing it back |
| `KVM_EXIT_MMIO` | `Mmio` | the userspace-xAPIC page falls through here (R1) |
| `KVM_EXIT_X86_RDMSR` | `Rdmsr` | needs the `set_msr_filter` userspace-MSR mask |
| `KVM_EXIT_X86_WRMSR` | `Wrmsr` | ditto |
| `KVM_EXIT_HLT` | `Hlt` | idle-skip / terminal (vmm-core) |
| `KVM_EXIT_SHUTDOWN` | `Shutdown` | triple fault / shutdown — terminal |
| `KVM_EXIT_INTERNAL_ERROR`, `KVM_EXIT_FAIL_ENTRY` | — | `BackendError::Internal` (fail closed) |
| `KVM_EXIT_IRQ_WINDOW_OPEN` (+ `EINTR`) | — | run-loop control: re-enter; **never** surfaced |
| any other reason | — | `BackendError::Internal` — default-deny, never a silent continue |
| **CPUID** | (none) | serviced in-kernel from the `set_cpuid` table; stock KVM emits no CPUID exit |
| **VMCALL** | (none) | serviced in-kernel; no general userspace VMCALL exit without a patch (see Open question) |
| **RDTSC/RDTSCP/RDRAND/RDSEED** | (none) | execute in-guest, **no `KVM_EXIT`** — the declared holes |

`KVM_EXIT_IO` with `io.count != 1` (string/REP PIO carrying `count*size` bytes) **fails closed**
(`BackendError::Unsupported`) rather than truncating to one scalar — M1/M2 use only single-byte UART
access.

## Snapshot/restore state (the full `allow-stateful` contract)

`save`/`restore` go over `KVM_GET/SET_{REGS, SREGS2, XCRS, DEBUGREGS, VCPU_EVENTS, MP_STATE}`,
`KVM_GET_XSAVE2`/`KVM_SET_XSAVE`, and `KVM_GET/SET_MSRS`, with these correctness invariants (all
fail-closed, never a silent partial):

- **MSRs** — `KVM_GET_MSRS`/`KVM_SET_MSRS` return the count they actually serviced and stop at the
  first index they reject. `save` requires `got == requested` and `restore` requires `set ==
  requested`, else `BackendError::Internal` — an incomplete `allow-stateful` blob never round-trips
  as success. The index list is exactly `MsrFilter::allow_inkernel` (retained from `set_msr_filter`).
- **SREGS2** — `KVM_GET/SET_SREGS2` (issued as direct ioctls; kvm-ioctls 0.25 exposes no SREGS2
  wrapper), preserving the `flags` (`KVM_SREGS2_FLAGS_PDPTRS_VALID`) and the four `pdptrs` so
  `restore(save())` round-trips PAE paging state. (Plain `KVM_SET_SREGS` would drop the PDPTRs.)
- **XSAVE2** — `KVM_GET_XSAVE2` with the `KVM_CAP_XSAVE2`-reported size (queried once in `new`, ≥ 4
  KiB), so a host with dynamically-enabled XSTATE (e.g. AMX) is not truncated to the fixed 4 KiB
  `kvm_xsave`. `VcpuState.xsave` is exactly that host-sized image; restore validates the length and
  writes it with `KVM_SET_XSAVE`. Falls back to the 4 KiB legacy path only if the cap is absent
  (pre-5.17 kernels; the determinism box is 6.12).

## The non-determinism posture (corrected per `docs/BRINGUP.md` step 2)

Stock `KvmBackend` **cannot intercept** RDTSC/RDTSCP/RDRAND/RDSEED — they execute in-guest with no
`KVM_EXIT`, so there is nothing to "fail closed" on per-instruction. The honest posture is
**non-determinism-claiming**, not per-instruction trapping:

- `capabilities()` reports `deterministic_tsc = false`, `deterministic_rng = false`,
  `enforces_tsc_deadline_msr = false` (gate 9). The unison report reads this to **refuse to
  claim determinism** for any payload that executes those instructions. `KvmBackend` determinism
  holds only for the audited RDTSC/RNG-free subset (M2's `hello`/`compute`).
- `save()` never launders a host-derived value (a host TSC, a host RNG draw) into `VcpuState`.
- The forcing function is the determinism gate (same seed twice ⇒ identical hash): any such value
  reaching hashed state diverges the two runs and fails loudly. Payloads that need these
  deterministic require `PatchedKvmBackend` (not this task).

## Miri: the seam, and the single excluded syscall class

This crate grants `unsafe` for two Linux-only purposes (rule #7): (1) `KVM_SET_USER_MEMORY_REGION`
registration in `map_memory`, and (2) `mmap`-ing `kvm_run` in `new`. Per the `unsafe ⇒ Miri` rule,
the pointer/region bookkeeping around those syscalls must stay under the UB gate. Following the
`vmcall-transport` precedent:

- **The pointer-unsafe logic lives in portable seams**, each driven under Miri by an
  `alloc_zeroed`/synthetic buffer with **no syscall** (`#[cfg(test)] mod tests`):
  - `region::MemRegions` — the memslot table: alignment/overlap/zero-length validation, slot
    tracking, and **bounds-checked GPA→host copies** (`read`/`write`). The bound check precedes
    every pointer arithmetic/`copy_nonoverlapping`.
  - `run_buf::RunBuf` — bounds-checked **offset math into the mapped `kvm_run`** (the PIO data
    buffer). The bound check precedes every copy.
  - `kvm::RunPage` + the pure `decode_*`/`apply_*` functions — the entire `kvm_run` ⇄
    `Exit`/completion translation (`exit_reason` dispatch, `decode_io`/`decode_mmio`, the `io.count`
    fail-closed, the union reads, completion routing). Driven by **synthetic `kvm_run` structs** in
    `kvm`'s non-`#[ignore]` `mod tests` — so this logic is covered by `cargo llvm-cov` /
    `cargo mutants` on the Linux runner **and** scrutinized for UB by Miri, with no `/dev/kvm`.
  On the box `KvmBackend` uses all of these in production (map_memory + `write_guest`/`read_guest`
  → `region`; the run loop + completions → `RunPage`/`run_buf`), so none is test-only scaffolding.
- **`#[cfg(not(miri))]`-excluded — the only un-Miri-able lines:** the raw `mmap`/`munmap` and the
  `KVM_RUN` / `KVM_X86_SET_MSR_FILTER` / `KVM_GET/SET_SREGS2` / `KVM_GET_XSAVE2` / `KVM_SET_XSAVE`
  ioctls (the `mmap_kvm_run` / `raw_*` seams), each with a `#[cfg(miri)]` error-returning stub. Miri
  can't execute a syscall; **nothing else is excluded** — the `kvm_run` decode is now Miri-covered
  via the synthetic-struct tests (this is the analogue of `vmcall-transport`'s real `vmcall` asm
  being the one un-Miri-able primitive).
- The CI `miri` job runs on the self-hosted Linux box, where `KvmBackend` *compiles* under Miri (its
  syscalls stubbed) and its **pure `decode_*`/`apply_*` tests run** (the live `kvm_smoke` tests stay
  `#[ignore]`). `vmm-backend --all-features` is added to `.github/workflows/quality.yml`'s `miri` job
  and to `.githooks/pre-push`'s `MIRI_CRATES`.

Miri runs clean (`MIRIFLAGS=-Zmiri-permissive-provenance`, kept for parity with the existing job;
this crate needs no int→ptr round-trips so it is also clean under default provenance) over the
trait / `Exit` / `VcpuState` / `MockBackend` and both seams on macOS (`nightly-2026-06-16`) and the
Linux box.

## Coverage & mutation gating: the `cfg(linux)` KVM path is box-/Miri-verified, not gate-counted

The `coverage` (region floor 93%) and `mutants` jobs run on the Linux runner **without `--ignored`**
(and across every crate's matrix). So the box-only `#[ignore]` KVM integration tests
(`tests/kvm_smoke.rs`) — which exercise the `KvmBackend` FFI orchestration end-to-end against
`/dev/kvm` — never run in those jobs, and spinning up live KVM there would be slow and
KVM-flaky. The whole `#[cfg(target_os = "linux")]` KVM path is therefore **excluded from the
coverage and mutation gates** and verified by other means instead:

- **Excluded from both gates:** `src/kvm.rs` (the `kvm_run`⇄`Exit` / state-conversion logic),
  `src/kvm/` (its unit tests), and `src/kvm_sys.rs` (the `KvmBackend` `Backend` impl + raw
  `mmap`/`ioctl` wrappers). Mechanisms:
  - coverage: `--ignore-filename-regex 'consonance/vmm-backend/src/kvm(\.rs|_sys\.rs|/)'` on the
    `coverage` job;
  - mutants: `**/kvm.rs`, `**/kvm/**`, `**/kvm_sys.rs` in `.cargo/mutants.toml` `exclude_globs` (the
    same mechanism the repo uses for `clock_proofs.rs`).
- **Verified instead by:** the box-only `#[ignore]` integration tests (the live FFI, end-to-end);
  **Miri**, which still *executes* `kvm.rs`'s synthetic-`kvm_run` unit tests on every run; and the
  normal CI `nextest` run, which also still *runs* those unit tests — so a logic regression still
  fails the test suite, it is simply not coverage/mutation-**counted**. (The `kvm.rs` / `kvm_sys.rs`
  split is retained: it keeps the FFI orchestration isolated from the pure mapping logic and lets the
  synthetic-`kvm_run` tests drive that logic under Miri without `/dev/kvm`.)
- **Still coverage- and mutation-gated** is the portable, platform-agnostic logic the split factored
  out — which carries the determinism-critical invariants: the `region`/`run_buf` pointer seams, the
  `Backend` value types (`exit`/`state`/`config`), and the `MockBackend` run-loop contract
  (`mock`/`run_loop`). With the KVM path excluded, the workspace region coverage is **95.4%** and
  `cargo mutants` reports **0 surviving mutants** over the gated set.

## Edge-case correctness (P2 review fixes)

- **`map_memory` atomicity.** The region is recorded in `MemRegions` first (it must, to get the
  slot index), but if `KVM_SET_USER_MEMORY_REGION` then fails the record is rolled back
  (`MemRegions::rollback_last`) — a failed map never leaves a stale host pointer behind for
  `read_guest`/`write_guest` to dereference.
- **`restore` atomicity.** `validate_restore_shape` runs **before any `SET_*` ioctl** (MSR key set
  and XSAVE length), so a malformed snapshot is rejected without half-mutating the live vCPU.
- **`ExitCounts::total` saturating.** The per-reason counters are public and individually saturate,
  so `total()` folds with `saturating_add` rather than `sum()` (no overflow panic/wrap).

## Dependencies — reviewed rule-5 whitelist exception (recorded here + the PR description)

`kvm-ioctls 0.25`, `kvm-bindings 0.14` (`fam-wrappers`), and `libc` enter under
`[target.'cfg(target_os = "linux")'.dependencies]` as the **reviewed rule-5 whitelist exception**
sanctioned by the task spec and `docs/BRINGUP.md` §"Dependency note" — **not** a `deny.toml` edit
(`deny.toml` gates licenses/advisories/sources only; there is no crate allowlist). `libc` is already
whitelisted. `cargo deny check` passes (all three are permissively licensed: Apache-2.0 / MIT /
BSD-3-Clause). **`vm-memory` is named in the spec's allowance but is *not* used** — memory is managed
via the caller's `&mut [u8]` + `KVM_SET_USER_MEMORY_REGION`, so adding it would be an unused dep.

## Snapshot completeness extras

- **`VcpuEvents` carries the full guest-visible event state** of `kvm_vcpu_events`:
  exception (`injected`/`nr`/`has_error_code`/**`pending`**/`error_code` + **`exception_has_payload`**
  / **`exception_payload`** for `#PF` CR2 and `#DB` DR6), interrupt + shadow, NMI, SMI, `sipi_vector`,
  **`triple_fault_pending`**, and `flags`. `flags` carries the `VALID_PAYLOAD`/`VALID_TRIPLE_FAULT`
  bits, so the payload and triple-fault fields round-trip through `restore(save())`. (Only the
  KVM-`reserved` padding is not modeled.)
- **`restore` validates the MSR key set against the configured filter, fail-closed.** Before touching
  the vCPU, `restore` requires `state.msrs.keys()` to exactly equal `MsrFilter::allow_indices()` —
  a missing key would leave that MSR at a stale value (restore wouldn't fully determine state), an
  extra key names an MSR outside the filter. Either is `InvalidState`.

## Deviations considered and rejected

- **Direct `KVM_GET/SET_SREGS2` and `KVM_GET_XSAVE2` ioctls** (rather than kvm-ioctls wrappers) — the
  crate uses `libc::ioctl` for these the same way it does for `KVM_X86_SET_MSR_FILTER`, because
  kvm-ioctls 0.25 exposes no SREGS2 wrapper and its `Xsave` FAM-wrapper would need byte (de)serializing
  anyway. The raw ioctls are `#[cfg(not(miri))]`-excluded with `#[cfg(miri)]` stubs, like the other
  syscall seams.
- **Hand-rolled the `KVM_RUN` loop over a self-`mmap`-ed `kvm_run` instead of `VcpuFd::run()`** — so
  the run loop owns the `kvm_run` and can route the PIO data buffer through the Miri-driven `run_buf`
  seam (granted purpose 2), and surface `Rdmsr`/`Wrmsr` directly. `kvm-ioctls` is still used for
  fd/VM/vCPU creation, `set_user_memory_region`, CPUID, and the remaining `save`/`restore` ioctls.
- **`MockBackend` *implements* `run_until`/`inject`** (a scripted `Deadline`; recorded injections),
  unlike `KvmBackend` (which returns `Unsupported` — Phase 2), so vmm-core's deadline/injection
  planning is testable. Its `restore` accepts any well-typed `VcpuState` (the malformed→`InvalidState`
  path is a `KvmBackend` concern).
- **`#[non_exhaustive]` on `BackendError`/`Exit`** — rejected: the spec defines closed sets, and gate
  5 asserts `Exit` is exhaustively matchable (a wildcard-free `match` compiles).

## Known limitations / integrator notes

- **`inject` is implemented (task 32); `run_until` is still Phase 2.** `inject` runs the
  `KVM_INTERRUPT` / interrupt-window handshake (see the "Task 32" section below). `run_until` still
  returns `BackendError::Unsupported { what: "run_until" }` — the live PMU-single-step deadline needs
  task 07; the task-32 LAPIC-timer drive does not depend on it (the timer is checked at exits, not via
  a precise `run_until` deadline).
- **Stock `KvmBackend` never surfaces `Exit::Hypercall` or `Exit::Cpuid`** (serviced in-kernel), so
  `complete_hypercall`/`complete_cpuid` there always error; they exist for the patched/direct
  backends. M1/M2 use no hypercalls.
- **Single-vCPU, single in-flight exit, no multi-count string I/O.** `Exit::Io` carries one `u32`; a
  `rep outs`/`ins` (`io.count > 1`) **fails closed** with `BackendError::Unsupported` rather than
  truncating. The bring-up stubs do single ops; full string-I/O support is a follow-up if a payload
  needs it.
- **`VcpuState` mirrors task 09's records but does not depend on `vm-state`** (rule #2). vmm-core
  marshals between them; the field sets are kept consistent by review.

## Open question (integrator ruling — does not block this task)

The spec's §"Open questions" flags that `docs/CPU-MSR-CONTRACT.md` §1 lists VMCALL among the
*stock-serviceable* dispositions, while this crate (and the spec) hold that stock KVM offers no
*general* userspace VMCALL exit (it services VMCALL in-kernel via `kvm_emulate_hypercall`, `-ENOSYS`
in RAX for the transport's unknown nr). Likely reconciliation: VMCALL is **backend-dependent** (needs
`PatchedKvmBackend`/`DirectVmxBackend`), so the contract's "stock-serviceable" label is the imprecise
side. This touches a merged contract doc, so it is an integrator ruling, not a unilateral edit. M1/M2
use no hypercalls, so `KvmBackend`'s scope is unaffected.

## Gate status

All standard gates pass on **macOS** (build/nextest/clippy `-D warnings`/fmt/deny) for the portable
surface, and Miri is clean on macOS (`nightly-2026-06-16`) and the Linux box.

The **box-only** live tests (`tests/kvm_smoke.rs`, `#[cfg(target_os = "linux")]` + `#[ignore]`) were
run on the determinism box (`ssh <det-box>`, Linux **6.12.90**, Intel, `/dev/kvm`), **CPU-pinned to the
spare core 1** per `docs/BOX-PINNING.md`:

```sh
ssh <det-box> 'taskset -c 1 cargo test -p vmm-backend --all-features --test kvm_smoke -- --ignored --test-threads=1'
# => test result: ok. 4 passed; 0 failed
```

- gate 6 `bringup_smoke_out_then_hlt` — `OUT 0x3f8` then `HLT`; `io == 1`, `hlt == 1`.
- gate 7 `save_restore_round_trips_on_real_kvm` — GPRs set via `restore`; `restore→save` is a fixpoint
  spanning SREGS2 (flags/PDPTRs), the full host-sized XSAVE2 image (`>= 4 KiB`), and the complete
  `allow-stateful` MSR set (all 3 captured — the `got == requested` path).
- gate 8 `msr_filter_is_loud` — a denied `RDMSR` surfaces as `Exit::Rdmsr` (loud, not a silent
  in-kernel value); `complete_fault()` delivers `#GP` (vectored through the real-mode IVT to a `HLT`
  handler — observably the fault path, not the silent-value path).
- gate 9 `capabilities_are_honest` — `deterministic_tsc`/`deterministic_rng`/`enforces_tsc_deadline_msr`
  all `false`.

The pure KVM mapping logic also has **non-`#[ignore]` unit tests** (`src/kvm/tests.rs`, driven by
synthetic `kvm_run` structs) that run on the Linux runner under `nextest` and under Miri — so a
`kvm.rs` translation-logic regression still fails the test suite without `/dev/kvm` (the
`cfg(linux)` KVM path is excluded from the *coverage/mutation counting* per the section above, not
from being executed).

## Public-API snapshot (`tests/public_api.rs`)

`vmm-backend`'s public API is a frozen contract (rule 3): `tests/public_api.rs` regenerates the
surface with `cargo public-api -p vmm-backend --all-features` on the pinned `nightly-2026-06-16` and
diffs it against `tests/public-api.txt`. **The frozen surface is the Linux one** (it includes
`KvmBackend`), so the snapshot was generated and is checked on the Linux box; the test skips loudly
on macOS (where the surface is a strict subset) and when the tooling is absent. Refresh after a
reviewed API change, on Linux:
`UPDATE_PUBLIC_API=1 cargo test -p vmm-backend --all-features --test public_api -- --ignored`.
(Generated with `cargo-public-api 0.52.0` + `nightly-2026-06-16` on the box.)

## Files touched outside `consonance/vmm-backend/` (all task-/review-sanctioned CI wiring)

- `.github/workflows/quality.yml`: `vmm-backend --all-features` added to the `miri` job; `-p
  vmm-backend` added to the `public-api` job; `--ignore-filename-regex
  'consonance/vmm-backend/src/kvm(\.rs|_sys\.rs|/)'` added to the `coverage` job (the `cfg(linux)` KVM
  path). **Task 21** extended that coverage regex to also exclude `…/src/patched_kvm.rs` (and
  vmm-core's `…/src/work_perf.rs`) — the box-only determinism/perf orchestration, same rationale as
  `kvm_sys.rs`.
- `.githooks/pre-push`: `vmm-backend` added to `MIRI_CRATES`.
- `.cargo/mutants.toml`: `**/kvm.rs`, `**/kvm/**`, `**/kvm_sys.rs` added to `exclude_globs`.
  **Task 21** added `**/patched_kvm.rs` and `**/work_perf.rs` (box-only, same rationale).

No product code outside the crate is modified.

### Task 21 — public-API note

`PatchedKvmBackend` is a **new public item** in the Linux surface, so the `public-api` snapshot
(`tests/public-api.txt`, generated on the box) must be refreshed:
`UPDATE_PUBLIC_API=1 cargo test -p vmm-backend --all-features --test public_api -- --ignored`.
The blanket `impl Backend for Box<B>` is also new surface. (Both are additive; the trait shape and
`Exit` enum are unchanged.) The refresh runs at review on the box, where the frozen Linux surface
is generated.

## Task 32 — interrupt injection (`KvmBackend::inject`)

`KvmBackend::inject` is now implemented (it was `Unsupported`). Under the userspace
irqchip (`KVM_IRQCHIP_NONE`) this is the standard `KVM_INTERRUPT` / interrupt-window
handshake, split — like the rest of this crate — into a **pure, Miri/CI-tested
decision** and a **box-only syscall**:

- **`kvm.rs` (pure, gate 1).** `RunPage` gained typed accessors for the two top-level
  `kvm_run` injection fields — `ready_for_interrupt_injection` (kernel→user, the single
  authoritative injectability gate: KVM folds `RFLAGS.IF` + the STI/MOV-SS shadow +
  in-flight-event into it) and `request_interrupt_window` (user→kernel). The pure
  `plan_irq_entry(page, pending_irq) -> IrqEntry` decides, from the post-exit readiness:
  queue the vector now (`IrqEntry::Queue`, window cleared) or arm the window and run
  (`IrqEntry::Run`) so KVM exits `KVM_EXIT_IRQ_WINDOW_OPEN` the moment the guest is
  injectable. Synthetic-`kvm_run` unit tests run it under `nextest` and **Miri** (0 UB):
  the ready / not-ready / nothing-pending branches and the window arm/clear, plus a
  focused `to_kvm_events`/`from_kvm_events` round-trip of the interrupt fields
  (`interrupt_injected`/`nr`/`shadow`) snapshot/replay relies on.
- **`kvm_sys.rs` (box-only syscall).** A `pending_irq: Option<u8>` holds the vector
  queued by `inject` for the next safe entry. `enter_guest` runs `plan_irq_entry` before
  every `KVM_RUN`; on `IrqEntry::Queue` it issues the raw `KVM_INTERRUPT` ioctl
  (kvm-ioctls 0.25 has no safe wrapper, so it is a direct `libc::ioctl` behind the
  `#[cfg(not(miri))]` seam with a `#[cfg(miri)]` stub, like the MSR-filter ioctl) and
  clears `pending_irq`. The pre-existing `decode_exit` → `None` for
  `KVM_EXIT_IRQ_WINDOW_OPEN` consumes that control exit inside the loop, so the next
  iteration re-runs `plan_irq_entry`, now injectable — the retry is entirely inside the
  backend. `inject(Event::Nmi)` queues an NMI via the safe `vcpu.nmi()` (`KVM_NMI`); not
  needed by the boot (timer IRQ only) but honoured for trait completeness.

**Layering / determinism.** The backend computes no schedule: vmm-core decides *when* to
inject (the V-time LAPIC-timer expiry) and calls `inject`; the backend only writes the
entry-interruption / window state at the next entry. Nothing above the trait branches on
backend identity. The in-flight interrupt + shadow round-trip through the existing
`to_kvm_events`/`from_kvm_events` (`KVM_GET/SET_VCPU_EVENTS`), so once `KVM_INTERRUPT`
has run a snapshot carries the injected vector.

**Gates.** Mutation + coverage already exclude the whole `cfg(linux)` KVM path
(`kvm.rs`/`kvm/`/`kvm_sys.rs`); the new code lives there and is verified by the Miri +
`nextest` synthetic-`kvm_run` tests and the box live boot, not mutation-counted (the
established precedent). **No public-API change**: `inject` is an existing trait method;
`plan_irq_entry`/`IrqEntry`/`pending_irq`/`raw_interrupt`/`KVM_INTERRUPT` are all
`pub(crate)`/private, so `tests/public-api.txt` is unchanged (no box re-bless needed for
this task).

**Known limitation.** `pending_irq` is a transient in-backend queue (between an `inject`
and the entry that delivers it); it is **not** part of `Backend::save`. A snapshot taken
in that narrow window — only reachable mid-run, an explicit task non-goal
("snapshot/restore of a *running* Linux" is a later milestone) — would not carry the
not-yet-delivered vector. Once delivered, KVM's `interrupt.injected` carries it and
round-trips. Moot for the determinism gate: on the patched backend the inject/deliver
sequence is a deterministic function of V-time, so two same-seed runs match.

### Task 32 — review fixes (PR #59)

The injection model is **re-arbitrated every entry** (round-2 review): the VMM owns
the userspace LAPIC, whose IRR *is* the multi-IRQ queue, so the backend holds a
**single** pending slot the VMM overwrites each entry with the freshly re-peeked
highest deliverable vector — never a stale queued one.

- **`set_pending_irq(Option<u8>)`** (trait method): overwrite the one pending
  maskable vector (`None` clears + disarms the window). `enter_guest` runs
  `plan_irq_entry` on that slot; on `KVM_INTERRUPT` it clears the slot and pushes the
  vector to `accepted_irq`. `inject(Event)` keeps the NMI path (`KVM_NMI`) and, for
  `Interrupt`, sets the same slot.
- **`take_accepted_interrupt()`** (trait method) drains `accepted_irq` — reporting
  vectors for which `KVM_INTERRUPT` was actually issued, so vmm-core completes its
  userspace-LAPIC IRR→ISR transition only on *confirmed acceptance* (the blocking
  determinism fix; the transition logic lives above the trait).
- The `MockBackend` mirrors this (single slot + accepted report), exposes
  `pending_irq()` (observe the re-arbitrated slot) and a test-only `set_defer_accept`
  (model the interrupt-window wait), and forwards through the `Box<dyn Backend>`
  blanket impl and `PatchedKvmBackend`.
- **Why a single slot, not a queue:** re-arbitrating from the LAPIC every entry makes
  a backend-side queue both unnecessary (the IRR retains every pending vector) and
  *wrong* (a queued vector goes stale if TPR rises / a higher IRQ arrives in the
  window gap — codex P2). The VMM re-peeks and overwrites, so the injected vector is
  always current and a lower/second IRQ is never dropped.
- Tests: `set_pending_irq_overwrites_single_slot` (overwrite + `None`-retract),
  `deferred_accept_holds_irq_pending`, and `injection_forwards_through_box` (kills the
  `Box<dyn Backend>` `set_pending_irq`/`take_accepted_interrupt` forward mutants — both
  are trait-observable, unlike the effect-only `inject`). The `plan_irq_entry`
  synthetic-`kvm_run` tests are unchanged (Miri 35/0). `cargo mutants --in-diff` over
  the full PR diff: 0 missed. Public-api snapshot regenerated on the box.

## Task 47 — deterministic preemption timer (`run_until` Phase 2)

The §2 inversion seam goes live: `KvmBackend::run_until(deadline)` preempts a guest
that takes **no natural VM-exit** (a busy-spin) at an exact, seed-deterministic
retired-branch count, so the V-time LAPIC timer can fire *mid-spin*. This is the
ROADMAP-D4 machinery (general preemptive-multitasking determinism), not a Go hack.

### What landed

- **`run_until.rs` (portable):** the orchestration above the live primitives —
  `drive_run_until` drives the pure `vtime::InjectionPlanner` over a guest-exit-aware
  `PreemptCpu` and maps `PlanOutcome` → `Exit` (`Deadline` at exactly the target; a
  natural guest exit returned verbatim, short of the deadline; `SkidExceeded` → a loud
  error, never a widened tolerance). No syscalls — fully unit/property-tested on macOS
  against `vtime::sim::SimCpu`.
- **`pmu.rs` (box-only):** `PmuBranchCounter` — the backend-owned
  `BR_INST_RETIRED.CONDITIONAL` counter (same event/flags as vmm-core's `work_perf`)
  in **sampling mode** with async overflow → `SIGIO` → `EINTR` kick.
- **`kvm_sys.rs`:** `KvmBackend::run_until` + the live `vtime::CpuBackend`/`PreemptCpu`
  adapter (`LiveCpu`) over the PMU counter + `KVM_GUESTDBG_SINGLESTEP`; `run_armed`
  (overflow-early free-run) and `single_step_once` (exact landing). `inject` (NMI +
  one-shot maskable) was already present (task 32) and is unchanged.
- **`kvm.rs`:** `classify_step_exit` (pure) distinguishes the single-step debug trap
  and the signal kick from a genuine guest exit (which `decode_exit` rejects as
  "unhandled"); unit-tested with synthetic `kvm_run`s.
- **VMM wiring (`vmm-core/vmm.rs`):** `preemption_deadline` / `on_deadline` — see that
  crate's notes. Strictly additive (gated on the determinism-complete + LAPIC path).

### The central design decision (where the live `CpuBackend` lives)

`run_until` is a `vmm-backend` method, but the V-time work counter historically lives
in `vmm-core` (`work_perf::PerfWorkCounter`), and **`vmm-backend` must not depend
upward on `vmm-core`** (rule 2/3). The two clean options the spec names are "move/share
the counter" or "a backend-owned counter". I chose **a backend-owned counter**:

- Moving/sharing the counter would force the V-time `WorkSource` (a `vmm-core` trait)
  across the `Backend` trait boundary, or a new shared trait method — a larger,
  layering-bending change that touches the composition root and risks the gate-4
  goldens, all unverifiable without the box.
- Instead `KvmBackend` owns a `PmuBranchCounter`, opened at vCPU build with the
  **identical** event (`0x1c4`), flags (`exclude_host=1`, `pinned=1`), and thread
  (`pid=0`) as vmm-core's counter. `vmm-backend` gains only a **downward** dep on
  `vtime` (the pure planner). No upward dep; no trait-signature change.

**The B≡A invariant (the key box-validation item).** The deadline passed to `run_until`
is an absolute count on vmm-core's work axis (counter *A*); the backend lands on its own
counter (*B*). Because both count the same guest event with the same `exclude_host`
baseline on the same thread, and both reset at the same logical points (first guest
entry — backend `ensure_first_run` mirrors vmm-core's `start_run`; and snapshot
restore), on a deterministic single-VM run **B(t) ≡ A(t)** for all t, so a deadline on
A's axis is honoured exactly by B. The counter is opened **non-fatally** (`.ok()`): a
box without `perf` still creates a backend that can `run()`/save/restore; only
`run_until` then returns `BackendError::Capability`. M1/M2/corpus never call `run_until`
and never need it. The extra counter is passive (`exclude_host`, separate fd) and never
hashed, so opening it leaves the gate-4 goldens byte-identical.

### The overflow is a host-side kick, not a guest PMI

The retired-branch overflow programs the `perf_event` **sample period** to
`armed_at − work()` and relies on `O_ASYNC` + `F_SETOWN_EX(TID)` delivering `SIGIO` to
the vCPU thread; a no-op handler installed **without `SA_RESTART`** turns that into a
`KVM_RUN` `EINTR`. Signal-delivery latency is therefore part of the skid, which is why
the margin is task 07's **measured** `skid_margin = 128` (not a guess), and the last
`≤128` branches are covered by exact single-stepping. A skid past the margin is a loud
`SkidExceeded`, never tolerated.

### Count-neutrality (the determinism crux)

`run_until` must retire/count **identically** to `run()`: the single-step trap path and
the `EINTR` exit must not add or drop counted branches, or snapshot/branch hashes and
the M1/M2 goldens would drift. `exclude_host` makes the trap/`EINTR`/host bookkeeping
count-neutral; single-stepping retires exactly the instructions a free-run would. The
**portable** proof is the `run_until_is_count_neutral_and_exact` property test (256
cases): for any seed/density/in-margin skid, `run_until` lands at *exactly* the deadline
— i.e. the preemption instant is a pure function of the seed, independent of where the
skid fell. The **live** count-neutrality (that the real `KVM_GUESTDBG_SINGLESTEP` trap
and `SIGIO`-`EINTR` are count-neutral on the box's PMU) is gate 1/2/4 on the box.

### Verified on this macOS host

- `cargo build` / `clippy -D warnings` / `fmt` — native **and** cross
  (`--target x86_64-unknown-linux-gnu`, which type-checks the box-only KVM/perf path).
- `cargo nextest run -p vmm-backend` — 34/34 (incl. the 6 `run_until` orchestration +
  property tests) + the `classify_step_exit` unit tests (Linux-target, run on CI).
- `cargo deny check` — advisories/bans/licenses/sources ok.
- Miri (`cargo +nightly-2026-06-16 miri test -p vmm-backend`) — the portable seam; the
  Linux-only `pmu`/`run_armed`/`single_step_once` raw syscalls sit behind
  `#[cfg(miri)]` stubs and `PmuBranchCounter` is never constructed under Miri.

### Box-validation frontier (foreman runs these; cannot run on the Mac)

The live PMU + KVM single-step path cannot execute on macOS (no `perf_event`/`/dev/kvm`).
**These are the items only the box confirms; do not relax or fake them.** See "Acceptance
gates" below for exact commands.

1. **Perf overflow mechanics:** that `PERF_EVENT_IOC_PERIOD` re-arms the next overflow
   *immediately* on the box's kernel (≥ 5.17 — true there), that `SIGIO` reliably
   `EINTR`s `KVM_RUN`, and that the ring buffer is drained so a long run (gate 3) never
   stalls on a full buffer. (Mechanism + `data_head`/`data_tail` offsets documented in
   `pmu.rs`; the `KVM_SET_SIGNAL_MASK` race-hardening is the noted alternative if a
   spurious-`EINTR`/missed-kick shows up.)
2. **B ≡ A:** the backend counter and vmm-core's V-time counter read identical work on a
   deterministic run (`run_until` lands at exactly the deadline on A's axis).
3. **Count-neutrality live:** M1/M2/P6/det-corpus/unison goldens byte-identical with the
   `run_until` path present (it is additive; the `run()`/HLT-warp path is untouched).

### Acceptance gates (box; run verbatim, then **revert KVM to stock + verify**)

Box-only (patched KVM + Intel PMU). `ssh hetzner`; pin per `docs/BOX-PINNING.md` — task
41 owns core 4 while PR #12 is open, so use **core 2** (`taskset -c 2`), SMT sibling
idle. rsync is blocked: ship via `git archive | ssh tar` or fetch the branch on the box.

```sh
# On the box, in the worktree, with the patched KVM modules loaded:
# Gate 1 — contract + exact landing (the live CpuBackend == the sim contract):
taskset -c 2 cargo nextest run -p vmm-backend --all-features --run-ignored all -E 'test(run_until_live) or test(cpu_backend_live)'
#   expect: "armed at D−128, single-stepped k branches, landed at D" (work == D exactly);
#   an injected guest exit before D returns that exit short of D.
# Gate 2 — busy-spin preemption, deterministic-twice (same seed ⇒ bit-identical serial
#   + state_hash; different seed ⇒ the preemption branch counts differ):
taskset -c 2 <busy-spin preemption demo>      # streams a marker to ttyS0 from the handler
# Gate 3 — the unlock: runc actually runs the Postgres OCI container (no
#   unshare/chroot/setpriv), task-42 UUID/time workload to ttyS0, GUEST_READY, clean
#   shutdown, deterministic-twice (serial incl. UUIDs/timestamps + state_hash):
taskset -c 2 <runc + postgres demo>
# ALWAYS afterward:
#   <revert to stock KVM>; lsmod | grep kvm   # expect stock module size 1396736
```

**If the Go runtime surfaces a NEW blocker beyond preemption** (something preemption
alone does not resolve), implement what you can, prove gates 1–2 + as much of 3 as the
primitive unlocks, and **document the precise next blocker** — do not fake or relax the
gate. (Gates 2/3 reference demo harnesses that ride this primitive; this task delivers
and box-validates the primitive itself — the `run_until` seam, `Exit::Deadline`, and the
VMM wiring — which gates 1 and the wiring tests already exercise portably.)

### Deviations considered and rejected

- **Move/share the V-time counter across the trait boundary** — rejected: bends the
  R-Backend layering (work source is *above* the trait) and is a larger, box-only-
  verifiable change. Backend-owned counter keeps the layering and the change local.
- **`PERF_EVENT_IOC_REFRESH` to arm the overflow** — rejected: it *disables* the counter
  after the overflow, so the subsequent single-step branches would go uncounted (the
  cumulative read would be wrong). The always-enabled counter + `IOC_PERIOD` keeps
  `work()` and `single_step` accurate.
- **Inject during the single-step (skid-margin) phase** — rejected: it would shift the
  landing and risk a `KVM_SET_GUEST_DEBUG` + `KVM_INTERRUPT` interaction. The free-run
  phase does the normal injection handshake; the skid-margin steps clear the interrupt
  window and land cleanly at the deadline, and a held vector is delivered at the first
  injectable entry ≥ the deadline (still seed-deterministic, per the spec).
- **Widening `Exit::Deadline` to a tolerance band** — rejected outright: landing at
  `deadline ± skid` is a determinism bug; it is reported, not absorbed.

### Known limitations / integrator notes

- `Backend::run_until` exists on **stock** `KvmBackend` too (the PMU + single-step are
  stock features) but the VMM only *invokes* it on the determinism-complete path
  (`deterministic_tsc`), so stock boots are unchanged.
- The two-counter B≡A equality holds for single-VM runs (the demos). A future
  multi-vCPU or concurrent-`run_until` use must revisit counter sharing; `ensure_first_run`
  already mirrors vmm-core's reset so the baselines stay aligned.
- `error.rs`'s `Unsupported` doc still lists `run_until`/`inject` as Phase-2 examples;
  both are now implemented on `KvmBackend`. The variant remains in the closed error set.

### BOX VALIDATION RESULTS (run on `ssh hetzner`, 2026-06-27)

Self-validated on the determinism box (`6.12.90+deb13.1-amd64`, Intel CFL i9-9900K,
`perf_event_paranoid=-1`), CPU-pinned **core 2** (PR #12 owns core 4), each patched
run wrapped in a revert trap that restores stock KVM. **All patched runs reverted to
stock `1396736` + verified.** Gates 1, 2, and 4 PASS on hardware; gate 3 is the
documented frontier.

**Gate 1 — contract (live `CpuBackend`), stock KVM** — `vmm-backend --test live_preemption`, 4/4:
```
[gate1] busy-spin: armed at 10000−128, single-stepped to exact, landed at 10000 (== deadline 10000)
[gate1] busy-spin: armed at 50000−128, single-stepped to exact, landed at 50000 (== deadline 50000)
[gate1] busy-spin: armed at 250000−128, single-stepped to exact, landed at 250000 (== deadline 250000)
[gate1] deterministic-twice: both runs landed at 100000 branches
[gate1] monotone: 20k → 60k → 130k all landed exactly
[gate1] guest exit (OUT 0x42) returned short of the 1e6 deadline ✓
```
The live PMU overflow → `SIGIO`→`EINTR` kick + `KVM_GUESTDBG_SINGLESTEP`-to-exact +
count-neutrality all work: `run_until` lands at **exactly** the armed deadline on a
real busy-spinning guest (infinite conditional-branch loop, zero natural exits), and
a genuine guest exit before the deadline is returned short of it. Needs only stock
KVM (the PMU + single-step are stock features).

**Gate 2 — busy-spin preemption, deterministic-twice (THE headline), patched KVM** —
`vmm-core --test live_preemption` (the deferred `irq-landing` payload), PASS:
```
[gate2] seed A: irq-landing PASS — busy-spin preempted, all 8 timer deadlines landed.
[gate2]   state_hash = 34cc29ebc7fafc670fca54d99f8afcecc7a8ee36f58c59655c0544fcfa6b61c5
[gate2]   serial = "PAYLOAD irq-landing START\nOK irq-landing\nPAYLOAD irq-landing PASS\n"
[gate2] deterministic-twice CONFIRMED at seed 0x5eedd31e2026:
        state_hash 34cc29eb…61c5 == 34cc29eb…61c5
[gate2] seed B 0x0badc0de1234: PASS, state_hash = edfa035192ca…fde58c
```
A guest that takes **no** natural VM-exit (the `pause`-spin waiting on a one-shot
LAPIC timer) is preempted at the V-time deadline, the timer vector is injected, the
ISR runs, and all eight deadlines (bracketing `skid_margin=128`) land → a clean
`DebugExit{0}`, **bit-identical state_hash + serial across two same-seed runs**. This
is the exact payload `box_corpus` *deferred* as "needs LAPIC-timer interrupt
injection … a later vmm-core phase" — now unblocked. **Busy-waiting guest code is
deterministically tolerable.**

**Gate 4 — no regression, patched KVM:**
- `vmm-core --test live_determinism` (P6) 2/2: RDTSC/RDTSCP V-time `[0,2,4,6]`, seeded
  RNG, snapshot/restore — deterministic-twice, unchanged by the additive `run_until`.
- `vmm-core --test live_linux_boot` Phase A (stock): real Linux 6.18 → `Run /init` +
  `GUEST_READY`, `reached_userspace=true`, clean terminal, `exit_counts.deadline=0`
  (stock path takes no preemption — additivity confirmed: the boot's `run()` path is
  byte-for-byte the prior behavior).
- Phase C (patched, deterministic-twice): both boots reach `GUEST_READY`, identical
  serial (6528 B) + `state_hash 4f926e01…c6aa` — a real Linux boot is bit-identical
  twice with preemption wired in.
- Off-box (macOS): vmm-backend 34/34, vmm-core 227/227, det-corpus+unison 92/92,
  cross-clippy/fmt/deny clean, Miri `run_until` 6/6.

**Gate 3 — runc + Postgres OCI, deterministic-twice: the documented frontier.** This
needs the **real** runc/Go-runtime path (no `unshare`/`chroot`/`setpriv` workaround —
task 38 used that workaround *because preemption did not exist*) booting the task-42
Postgres workload to `GUEST_READY` + clean shutdown, deterministic-twice. It is the
full Linux-userspace + container-runtime integration the spec frames as "a later
milestone" that "rides this primitive." The primitive it was blocked on is now proven
(gates 1–2) and integrates with a real Linux boot (gate-4 Phase C). The **precise next
blocker / frontier**: stand up the real-runc Postgres guest image (task 38/42 lineage,
minus the workaround) and boot it via `boot_linux_selected(Patched)` — the Go
runtime's `procyield`/`osyield` busy-spins are now preemptible, so the remaining work
is the runc/containerd/Postgres userspace bring-up + an O1/O2 deterministic-twice
harness over its serial (UUIDs/timestamps) + `state_hash`, not the preemption
mechanism. No new determinism mechanism is required; if the Go runtime surfaces a
blocker *beyond* preemption, that is the next task's frontier to document.

### PR #15 cross-model round-1 fixes (codex/GPT-5.5) + re-validation (2026-06-27)

Three real determinism/robustness bugs found by the cross-model pass, all fixed with
coverage:

- **P1(a) — post-deadline guest exit treated as early.** A genuine IO/MMIO/HLT exit
  arriving after the overflow had already reached the deadline (SIGIO not yet
  delivered) was returned as a *short* guest exit, so the timer was serviced after an
  instruction that ran past its V-time instant. Fix: `run_armed`/`single_step_once`
  read the PMU work **at** the exit and carry it up (`LiveStop::Guest{exit, work}`);
  the portable `drive_run_until` compares it to the deadline — `work < deadline` is a
  true early exit, `work == deadline` is `Deadline` (timer instant reached; pending
  cleared), `work > deadline` is a loud determinism error (exact instant missed).
  Coverage: portable `guest_exit_at_or_past_deadline_is_not_treated_as_early` (all
  three cases) — the decision lives in the testable layer, not the box-only adapter.
- **P1(b) — PMU desync after restore.** Restore reset the counter immediately but left
  `first_run_done = true`, so a coexisting VM on the same pinned thread between restore
  and the restored VM's next entry contaminated `run_until`'s counter. Fix: re-arm
  `first_run_done = false` on restore (mirrors `Vmm::restore_vm_state`) so the reset
  fires at the next entry, excluding foreign branches. Coverage: box test
  `restore_re_arms_pmu_reset_excluding_foreign_branches` (run B1, save, restore, run a
  DIFFERENT VM, then B1.run_until lands at exactly its own count).
- **P2 — silent run_until cleanup failures.** A failed single-step disarm / PMU disarm
  returned the exit as success while the vCPU stayed single-stepping / overflow-armed.
  Fix: `pmu_disarm` is now fallible; `run_until` attempts both cleanups then propagates
  the first failure (fail closed). Structural fix (error propagation); no feasible
  deterministic fault-injection test for the box-only ioctl failure.

**Round-2 box re-validation** (the box was concurrently running task 41's PR #12 gate,
which owned the loaded **patched** KVM; I ran on **core 2** against that module
**without** loading/unloading it, and did **not** revert — task 41 owns it and reverts
when done). All gates re-pass with the fixes:
- Gate 1 + P1(b) (stock-compatible suite, 5/5): exact landing, deterministic, monotone,
  guest-exit-short, and the foreign-branch-contamination test.
- Gate 2 (patched): `state_hash 34cc29eb…61c5 == ` (identical to round-1 — fixes are
  behavior-preserving on the normal path), deterministic-twice.
- Gate 4 (patched): P6 2/2; Linux Phase C deterministic-twice `4f926e01…c6aa == `.
- Off-box: vmm-backend 16/16, vmm-core 227/227, Miri `run_until` 7/7 (incl. P1(a)),
  cross-clippy/fmt/deny clean.

### PR #15 round-2: pin the P1 invariants in portable, gate-covered tests (2026-06-27)

The P1 fixes touched `kvm_sys.rs`, which is **excluded** from the coverage + mutation
gates (box-only FFI). Cross-model review asked that the *determinism decisions* be
factored into the covered + mutation- + property-tested portable layer so a future
regression is caught on CI, not only on the box:

- **P1(a):** the early/at/past-deadline decision is now a pure
  `classify_guest_exit(work, deadline)` in `run_until.rs` (covered + mutation-tested);
  `kvm_sys` is thin FFI that only *reports* the PMU read (`LiveStop::Guest{exit,
  work}`). Property test `drive_run_until_classifies_any_guest_exit` checks the
  classifier + the `Exit`/`Err` mapping for all `(work, deadline)`.
- **P1(b):** the first-entry PMU-reset discipline is extracted into a portable
  `FirstEntryReset` (`new`/`take_reset`/`rearm`); `KvmBackend` holds one instead of
  the `first_run_done` bool. A **`proptest-state-machine` stateful test**
  (`reset_discipline_stateful`) drives random run/restore sequences over N VMs sharing
  one thread and asserts the backend counter **B** equals an *independent* reference
  for vmm-core's V-time counter **A** (a second shared counter with the correct
  discipline) — pinning "B's reset points track A's" across the interleaving the
  contamination bug needs. Verified the stateful test (and the `first_entry_reset`
  unit test) **fail** if `rearm` is broken to a no-op, i.e. they are genuine guards.
  The box test `restore_re_arms_pmu_reset_excluding_foreign_branches` still proves the
  FFI wiring end-to-end.

Behavior-preserving refactor — box gates re-pass **byte-identically**: gate 1 + the
P1(b) box test 5/5 (stock); gate 2 `state_hash 34cc29eb…61c5 ==`; gate 4 P6 2/2 +
Linux Phase C `4f926e01…c6aa ==` (patched, then reverted to stock `1396736`). Off-box:
vmm-backend 19/19 (incl. the new property + stateful tests), vmm-core 265/265, Miri
`run_until` (the stateful test is `#[cfg(not(miri))]` — pure arithmetic, no `unsafe`),
cross-clippy/fmt/deny clean.

### PR #15 round-3: CI-gate fixes (kani lock + pmu mutation coverage) (2026-06-27)

Two red CI gates after round-2, both CI-hygiene / coverage (not determinism bugs):

- **kani (red):** root cause was a `Cargo.lock` inconsistency — the round-2 commit's
  `git add -A consonance/vmm-backend` excluded the root `Cargo.lock`, so the pushed
  HEAD listed the new `proptest-state-machine` dev-dep in `Cargo.toml` but not the
  lock. The kani job builds `--locked` → `cannot update the lock file`. Fix: commit
  the correct `Cargo.lock` (reproduced + verified with `cargo metadata --locked`;
  `cargo kani -p vtime`/`-p lapic` pass locally, both VERIFICATION SUCCESSFUL — the
  proofs were never touched). Also removed an accidentally-committed
  `proptest-regressions/` seed + added a crate `.gitignore`.

- **mutants (red):** survivors were all in `pmu.rs`'s `PerfEventAttr` builder — good
  that it was no longer box-excluded, but it needed pinning. Mirrored the
  `kvm.rs`/`kvm_sys.rs` split: **`pmu.rs`** is now the pure, portable config
  (struct + bit constants + `branch_counter_attr()` + exact-value tests), IN the
  coverage + mutation gates; **`pmu_sys.rs`** (new) is the box-only `PmuBranchCounter`
  syscall orchestration, excluded like `kvm_sys`/`work_perf`. The reported survivors
  are killed by exact-value tests, and the *equivalent* mutants are eliminated rather
  than excluded (dropped the redundant `sample_type: 0`; composed the flag words with
  `+` over disjoint bits so the oracle's operator swaps are all caught/unviable, vs
  the un-killable `|`→`^`). Verified locally: `cargo mutants -f pmu.rs` 18/15 caught/3
  unviable/**0 missed**, and the full `cargo mutants --in-diff` over the whole PR
  (pmu.rs + run_until.rs + vmm.rs) = 45 mutants, 26 caught, 19 unviable, **0 missed**.

Process: the slow gates (`cargo mutants --in-diff`, `cargo kani`) are now run locally
before push, not just nextest/clippy/fmt.

### PR #15 round-4: coverage gate (already fixed by the round-3 split; verified) (2026-06-27)

The coverage red was the same root cause as the mutants red, one step earlier in time:
before the round-3 split, the single cfg(linux) `pmu.rs` (~500 lines of uncoverable
`perf_event_open`/`mmap`/`ioctl` syscall code) was NOT in the coverage
`--ignore-filename-regex`, so the Linux coverage job counted it and it dragged the
region floor below 93%. The round-3 split already fixed this: the syscall orchestration
moved to `pmu_sys.rs` (added to the ignore regex), and the pure `pmu.rs` builder is
100% region-covered by its exact-value tests.

Diagnosed by running the gate locally (not assuming): `cargo llvm-cov` on macOS showed
the workspace total at **95.0%** with pmu.rs 100% and run_until.rs 93.5% — i.e. no
box-only code was leaking in. The only two *testable prod* regions still uncovered in
the new code were covered rather than excluded (per the floor-is-not-a-bar posture):
`drive_run_until`'s unreachable `Err(_)` catch-all (merged into the reachable Backend
arm) and `FirstEntryReset::default()` (asserted in its unit test). The remaining
uncovered run_until.rs lines are `#[cfg(test)]` helper code.

Confirmed GREEN on the box (Linux — the exact CI invocation): `cargo llvm-cov nextest
--all-features … --fail-under-regions 93` → exit 0, 813 tests passed, lcov.info
generated. (Process: the slow gates — `cargo mutants --in-diff`, `cargo kani`, `cargo
llvm-cov` — are now all run locally before push.)

### PR #15 round-2 (cross-model): unify run_until exit handling (2 P1 + 2 P2)

Root cause: inconsistent exit-vs-deadline handling across the KVM exit paths. Fixed
the four and did the unifying audit so every path obeys the same four invariants:
(1) read `pmu_work()` and compare to `armed_at`/`deadline` before deciding; (2) NEVER
drop/absorb a real guest exit; (3) preserve any pending injectable IRQ; (4) fail
closed on any ioctl error.

- **P1(a) — don't absorb real exits before the deadline.** `drive_run_until` no longer
  drops a guest exit by collapsing it into `Exit::Deadline`. A real exit *strictly
  before* the deadline (`work < deadline`) is **delivered** (it carries guest-visible
  PIO/MMIO/read state). _(The `work == deadline` boundary was further refined in
  round-3 below — the timer wins there.)_
- **P1(b) — PMU check on IRQ-window re-entry.** `run_armed` now applies the SAME
  `free_run_decision(work, armed_at)` to the `KVM_EXIT_IRQ_WINDOW_OPEN` control exit as
  to the signal path — if the overflow already crossed `armed_at`, stop instead of
  re-entering (which would overshoot / inject a stale IRQ past the deadline). The
  decision is the portable, tested `free_run_decision`.
- **P2(a) — preserve pending IRQ in the step phase.** `single_step_once` runs the same
  `inject_pending()` handshake as the free-run phase (was: cleared the window with
  `plan_irq_entry(None)`), so a pending injectable serial/LAPIC vector is delivered at
  the next entry per the `set_pending_irq` contract, not delayed past the deadline.
- **P2(b) — propagate PMU reset failures.** `ensure_first_run` now returns the
  `RESET` error (and re-arms so a later entry retries) instead of ignoring it; `run`/
  `run_until` call it with `?`. A failed reset would leave a stale counter (foreign
  branches) → past/late deadlines.

**Unifying audit — every run_until exit path → PMU check → action** (all four
invariants hold; a real exit is never dropped; every ioctl error fails closed):

| Phase | KVM stop | PMU check | Action |
|---|---|---|---|
| free-run | EINTR (signal) | `work ≥ armed_at`? | yes → stop (`Count`); no → re-enter |
| free-run | `KVM_EXIT_INTR` | `work ≥ armed_at`? | yes → stop; no → re-enter |
| free-run | `KVM_EXIT_IRQ_WINDOW_OPEN` | `work ≥ armed_at`? **(P1(b))** | yes → stop; no → re-enter (inject pending) |
| free-run | `KVM_EXIT_DEBUG` | — | fail closed (single-step not armed here) |
| free-run | real guest exit (IO/MMIO/MSR/HLT/…) | read `work@exit` | `<`: deliver · `==`: **fail closed** · `>`: fail closed **(P1 r4)** |
| free-run | `KVM_RUN` errno (non-EINTR) | — | fail closed (`Io`) |
| step | `KVM_EXIT_DEBUG` (trap) | read `work` | `Count(work)` (planner compares to deadline) |
| step | EINTR / `INTR` / `IRQ_WINDOW_OPEN` | — (overflow disarmed) | re-enter, injecting pending **(P2(a))** |
| step | real guest exit | read `work@exit` | `<`: deliver · `==`: **fail closed** · `>`: fail closed **(P1 r4)** |
| step | `KVM_RUN` errno (non-EINTR) | — | fail closed |
| planner | `ReadyToInject` (**no** guest exit) | planner stopped AT `work == deadline` | `Exit::Deadline` (**timer wins** — P1 r4) |
| planner | `TargetInPast` | `work > deadline` at entry (overdue) | `Exit::Deadline{reached: now}` |
| planner | `SkidExceeded` | — | fail closed |
| pre-entry | first-entry / post-restore reset | — | reset PMU; **fail closed on reset error (P2(b))** |

### PR #15 round-3 (cross-model): the boundary tie-break (1 P1)

Round-2 stopped *absorbing* real exits — correct — but then **delivered** an exit at
`work == deadline`, and *that* is host-timing-dependent. A conditional branch reaches
the deadline (`pmu_work() == deadline`) and the very next instruction is a non-counted
exit (PIO/MMIO/HLT — retires no conditional branch, so work stays `== deadline`). Two
same-seed runs diverge on SIGIO latency: if the overflow's SIGIO interrupts `KVM_RUN`
*before* that instruction retires, the planner single-steps to the branch and returns
`Deadline` (timer first); if SIGIO is delayed until *after* it traps, round-2 returned
the IO/MMIO exit (exit first). So timer-vs-exit **ordering at the boundary** depended
on host signal timing — a same-seed determinism leak.

**The precise rule (now airtight):**

- `work < deadline` → **deliver** the real exit (genuinely before the deadline).
- `work == deadline` → **`Exit::Deadline` — the timer wins**, never the coincident
  exit. Boundary ordering is now host-timing-independent.
- `work > deadline` → **impossible**; fail closed loudly (the single-step never
  overshoots — only a gross skid past the margin could reach here).

**Why nothing is dropped (the side-effect guarantee).** The single-step phase stops
*at* the deadline branch, **before** the post-deadline instruction retires — RIP sits
on that instruction, un-executed. So returning `Deadline` does not commit or drop its
side effects: the guest executes it on the **next** entry, *after* the timer ISR.
Because `skid_margin (128) > max_skid` (task-07 measured: margin = max × safety), the
free-run stops *strictly before* the deadline branch and the single-step alone reaches
it — so a *trapped guest exit* at `work == deadline` (the only case that could lose an
already-committed effect) is should-never-happen. The `== deadline` arm is the
**deterministic tie-break** for that edge; `run_until` clears its would-be-dropped
pending so the next entry is clean. This closes round-2's "drop" concern from the
other direction: the IO instruction *never executes before the deadline*, so there is
nothing to drop — it just runs after the ISR.

Tests: portable `guest_exit_boundary_is_a_deterministic_timer_win` (an exit reported
at exactly the deadline count → `Deadline`, not the exit) + the
`drive_run_until_classifies_any_guest_exit` property (all `work`×`deadline`: deliver
below, timer-win at, fail-closed above). The live single-step-stops-before-the-branch
guarantee is exercised by box gates 1/2 (the busy-spin preemption lands at the exact
branch, deterministic-twice).

Cross-cutting: the injection handshake (`inject_pending`) is shared by both phases, so
a pending IRQ is preserved everywhere (invariant 3); every PMU/KVM ioctl returns
`Result` and is `?`-propagated (invariant 4). Tests: P1(a) + `free_run_decision`
(P1(b)) are portable + property/mutation-tested; the box gates 1/2/4 exercise the live
`run_armed`/`single_step_once` paths (the IRQ-window-past-armed and near-deadline
pending-IRQ races are box-only timing and validated there + by the shared, tested
`free_run_decision`/`plan_irq_entry` decisions they call).

### PR #15 round-4 (cross-model): the COMPLETE 3-case boundary rule (1 P1) + Miri ring (1 P2)

**P1 — the other horn.** Round-3's "timer wins at `work == deadline`" was correct only
for *one* sub-case. The flaw: a `pmu_work() == deadline` read does NOT distinguish
"single-step stopped AT the deadline branch" from "a non-counted instruction past the
branch already executed" — an IO/MMIO/HLT/read retires no conditional branch, so it can
run (and **commit a guest-visible side effect** / have its exit reported + consumed)
while the counter still reads `deadline`. Round-3 mapped *that* (a reported exit at
`work == deadline`) to `Exit::Deadline`, which makes `KvmBackend::run_until` drop the
pending completion → the VMM never services the already-executed effect and resumes
*past* it = dropped state + determinism leak. (The skid check can't catch it: a
non-counted instruction advances no branch, so `work` is not `> deadline`.)

The decision therefore turns on **whether a guest exit was reported**, not the count
alone. The complete rule (`drive_run_until`):

| outcome | meaning | action |
|---|---|---|
| guest exit, `work < deadline` | genuinely early | **deliver** the exit |
| **no** guest exit (stopped AT the branch) | nothing ran past the deadline | `Exit::Deadline` (**timer wins**) — runs the next instr after the ISR |
| guest exit, `work == deadline` | a non-counted instr past the branch already ran (side effect committed) | **FAIL CLOSED** (loud) — never absorb/drop |
| guest exit, `work > deadline` | counted instrs ran past the branch (worse overshoot) | **FAIL CLOSED** (loud) |

PRIMARY structural guarantee (unchanged): with `skid_margin (128) > max_skid` the
free-run stops *strictly before* the deadline branch and the single-step lands exactly
ON it (stopping before the next instruction), so a non-counted post-deadline
instruction is **never free-run-executed** — the two fail-closed arms are unreachable
in normal operation; they are the loud backstop if that invariant is ever violated. So
a returned `Deadline` is always the no-exit land → carries **no** pending completion
(the round-3 "clear pending on Deadline" is gone; a stale pending would now trip the
next-entry `PendingCompletion` guard loudly, never a silent drop — plus a
`debug_assert`). Tests: `post_deadline_io_is_not_executed_across_signal_timing` (a
guest exit modeled at `deadline + 1` — the instr right after the branch — is never
reached; `Deadline`, IO un-executed, bit-identical across the full skid range);
`reported_guest_exit_at_or_past_the_deadline_fails_closed` (the constructed overshoot →
loud error); `lands_exactly_at_deadline_with_no_guest_exit` (timer-wins land); and the
`drive_run_until_classifies_any_guest_exit` property (all `work`×`deadline`).

**P2 — make the PMU-ring `unsafe` Miri-exercisable.** The overflow ring-drain's volatile
offset arithmetic (`data_tail := data_head`) was inline in box-only `pmu_sys.rs`,
reachable only via `PmuBranchCounter::open` — which is `cfg(miri)`-stubbed to fail — so
Miri never ran the pointer code (a vacuous unsafe gate; a bad offset/provenance would
pass). Factored into the pure `pmu::drain_ring_at(base: *mut u8)` (with the
`DATA_HEAD_OFF`/`DATA_TAIL_OFF` constants), so `cargo miri test -p vmm-backend` now
exercises it over a **test-owned** u64-aligned page (`drain_ring_at_copies_head_to_tail`)
with real provenance + alignment, and it joins the coverage + mutation gates. `pmu_sys`
keeps only the box-only `mmap` and passes the real control-page base.

### PR #15 round-5 (cross-model): two narrow fail-closed / baselining edges (2 P2)

- **P2(a) — record the pending completion BEFORE the fallible PMU read.**
  `take_guest_exit_stop` decoded a read-style guest exit (IN/RDMSR/…), then read
  `pmu_work()`, then stored `self.pending`. If the PMU read failed in between, it
  returned the error WITHOUT recording the pending — but the KVM run page still held
  the uncompleted exit, so a retry would pass the `PendingCompletion` guard and
  re-enter with **stale completion data** (not fail-closed). Fixed by storing
  `self.pending` *before* the fallible read, so a PMU-read failure leaves the backend
  fail-closed (a retry hits `PendingCompletion`). (Box-only reorder; the happy path is
  exercised by gate 1's `guest_exit_before_deadline_returns_that_exit`, the failure
  path needs PMU fault-injection so it is review-verified.)
- **P2(b) — keep the first-entry reset armed for a zero-step `run_until`.** A first
  `run_until(Vtime(0))` (deadline already at the freshly-reset count) returns
  `Exit::Deadline` with **no `KVM_RUN`** — the guest never enters — yet
  `ensure_first_run` had already consumed the first-entry PMU reset. A coexisting VM on
  the shared pinned thread could then contaminate this backend's baseline before its
  *real* first entry (the baseline no longer resets) — and that coexisting-VM scenario
  IS the branch/multiverse path (task 48/49). Fixed: snapshot `reset_arm.is_armed()`
  before `ensure_first_run`, and after reading `start` re-arm via the pure, gated
  `keep_reset_armed_for_zero_step(was_first, start, deadline)` (`was_first && deadline
  <= start`) when the call is a first-run zero-step. The reset it already performed is
  idempotent; the re-arm makes the real first entry re-baseline. Tests: portable
  `keep_reset_armed_only_for_first_run_zero_step` (the decision across first/non-first ×
  zero/real-step) + the box `zero_step_run_until_keeps_first_entry_reset_armed_excluding_foreign_branches`
  (zero-step → a foreign VM runs ~100k branches on the same thread → the real first
  entry lands at exactly 50 000, no contamination).

### PR #15 round-6 (cross-model): the complete precision invariant — Deadline only via single-step (1 P1)

Rounds 1/3/4 fixed the *exit-handling* boundary cases; this is the **overflow phase**
case they didn't cover. When the PMU overflow skids EXACTLY to the deadline
(`stopped_at == deadline`, e.g. skid == margin), the planner's old `stopped > target`
check accepted it and the no-exit path returned `Exit::Deadline` with **zero
single-steps**. But a perf overflow/SIGIO is not instruction-precise at the boundary —
non-counted guest instructions after the deadline branch may already have retired while
the counter still reads `== deadline` — so that injection point depends on host skid: a
determinism leak in the core feature.

**The complete precision invariant: every `Exit::Deadline` is positioned by the precise
single-step, never by a raw overflow stop.** Enforced in two places:

1. **The planner (`vtime::stop_at`) now requires the overflow to stop STRICTLY before
   the target.** Phase 1's check is `stopped >= target → SkidExceeded` (was `> target`).
   So a Phase-1 (overflow) landing always finishes with ≥ 1 exact single-step to the
   boundary; an overflow that consumes the whole margin is a loud violation, never a raw
   landing. Tests: `overflow_exactly_on_target_is_skid_exceeded`,
   `overflow_one_before_target_single_steps_to_exact`.
2. **`SKID_MARGIN` bumped 128 → 256** (strictly above task-07's measured bound of 128,
   whose acceptance allows `skid ≤ 128`). Arming at `deadline − 256` means even a skid at
   the full task-07 bound leaves ≥ 128 branches for the single-step
   (`stopped ≤ deadline − 128 < deadline`). The result is unchanged (the single-step
   still lands at exactly the deadline — gate-2/4 digests are identical); only the arm
   point moves earlier.

**Audit — every `Exit::Deadline`-producing path goes through precise positioning** (none
from a raw overflow stop): (1) overflow + single-step — precise by the invariant above;
(2) `target == now` / `0 < target − now ≤ margin` — no overflow, the guest is at a clean
exit boundary or single-stepped the whole way; (3) `TargetInPast` — `reached = now` is
the clean entry boundary, not an overflow stop.

Sentinel consequence: a consumer signalling a **non-skid** early stop from
`run_until_overflow` (a genuine guest exit during the free-run) must report a count
`< deadline`, never the deadline itself — otherwise the planner would mistake the
sentinel for an overflow skid and raise `SkidExceeded`. `LiveCpu` (and the `SimPreempt`/
`ExitAtCpu` test mocks) now return `deadline − 1` from the free-run guest-exit path; the
single-step phase short-circuits to the deadline, stopping the planner at ReadyToInject,
and `drive_run_until` recovers the real exit + work via `take_guest_exit` (P1 round-4
still classifies early/at/past from the WORK). Test:
`overflow_landing_exactly_on_deadline_is_skid_exceeded_not_raw_deadline` (an overflow on
the deadline → loud error; one strictly before → single-stepped to the exact boundary),
and `lands_exactly_at_deadline_with_no_guest_exit` exercises skids up to 255 (< 256) →
all land at exactly the deadline, bit-identical across the skid (signal-timing) range.

### PR #15 round-7 (cross-model): kill the zero-step shortcut (1 P1) + independent test oracle (1 P2)

- **P1 — eliminate the zero-step `run_until` shortcut.** When `deadline <= start`,
  `run_until` used to return `Exit::Deadline` WITHOUT any `KVM_RUN`. That skipped the
  entry that COMMITS a completion staged by the prior step (a completed read-style/MSR/
  CPUID exit is committed by the next `KVM_RUN`; `self.pending` is already `None`), so a
  caller could save/restore or re-enter with the staged completion uncommitted = stale
  state / broken run-loop contract. The shortcut also needed special-casing to not spend
  the first-entry reset (round-5 P2b). **Root-cause fix: kill the shortcut.** For
  `deadline <= start`, `run_until` now single-steps ONCE (`commit_step_overdue` →
  `single_step_once`): the first `KVM_RUN` commits any staged completion, the
  (unconditional) `ensure_first_run` resets the baseline, and the overdue `Deadline` is
  delivered at the resulting count. A genuine guest exit on that step is delivered, never
  dropped (round-4). This removes the whole zero-step edge-case class — the round-5 P2b
  machinery (`keep_reset_armed_for_zero_step`, `is_armed`, the snapshot/re-arm dance) is
  GONE; every `run_until` now enters the guest and resets at first entry. The degenerate
  `deadline <= start` case is rare; the planner path (`deadline > start`) is unchanged
  (gate-2/4 digests identical). Box test
  `zero_step_run_until_enters_commits_and_is_deterministic`.
- **P2 — make the foreign-branch stateful test's oracle INDEPENDENT.** The reference
  computed the expected work as the *shared* `total − reset_at`, which for a
  VM0→VM1→VM0 interleaving INCLUDES VM1's branches — exactly the contamination the
  test's name claims impossible. The reference **mirrored the impl** instead of checking
  it, so the test passed with contamination. Fixed: the reference now tracks each VM's
  OWN retired-branch tally (`own − own_baseline`), computed WITHOUT the shared counter,
  so it can never inherit a coexisting VM's branches; the SUT's shared `total − reset_at`
  must equal it. Transitions are constrained to the REAL execution (a VM re-enters only
  when the discipline re-baselines it: active / fresh / just-restored — the VMM never
  time-slices a VM back in after another ran without a snapshot restore). Verified the
  oracle is not vacuous: breaking `FirstEntryReset::rearm` makes the SUT inherit foreign
  branches → it diverges from the independent tally → the test fails.

### PR #15 round-8 (cross-model): the complete run_until contract (1 P1) + V-time-only restore guard (1 P2)

We circled the degenerate `deadline <= current` case three times (round-5 baseline,
round-7 staged-completion, round-8 overstep), so this defines the **complete `run_until`
contract** for ALL deadline-vs-current cases in one pass (a pure `classify_run_until`,
covered + mutation-tested), instead of point-fixing.

**P1 — the contract (deadline vs current work):**

| `deadline` vs `current` | meaning | action |
|---|---|---|
| `> current` | the timer is ahead | **drive the planner**: arm overflow, single-step to EXACTLY the deadline (the converged precision invariant) → `Exit::Deadline` (or a genuine guest exit before it) |
| `== current` | already at the deadline | return `Exit::Deadline` with **ZERO guest steps toward the deadline** — never step a guest instruction past it. The first-entry baseline reset (`ensure_first_run`) still runs |
| `< current` | the deadline is in the past | **invalid → fail closed** — the LAPIC timer deadline is always in the future; we cannot run backward |

The round-7 bug: for `deadline <= current` it single-stepped ONE guest instruction
(`commit_step_overdue`), which at `== current` **oversteps** (a counted branch →
`reached > deadline`, or a side effect/exit → guest-visible work before the timer). The
fix: `== current` takes **zero** guest steps. A completion staged by the prior step
(e.g. a completed `RDMSR`/`IN`) lives in the **run page**, not lost on re-entry — the
NEXT entry's `KVM_RUN` commits it. **Caller contract:** commit an owed completion on a
normal entry (a `> current` deadline, or `run`) — never `save` across an owed completion
at a current deadline (the run page is not part of the saved vCPU state). Tests:
portable `classify_run_until_covers_every_deadline_vs_current_case`; box
`run_until_at_current_deadline_takes_zero_steps` (fresh → `reached == 0`, no overstep),
`run_until_past_deadline_fails_closed`, and
`run_until_at_current_deadline_preserves_owed_completion` (IN read → complete → zero-step
`run_until(0)` → next `run_until` commits the read, guest progresses, no re-trap).

**P2 — guard the V-time-only restore against the run_until path.** `Vmm::restore_vtime`
resets vmm-core's `WorkSource` (the V-time counter `A`) to a new zero point, but the
backend's `run_until` PMU (the SEPARATE counter `B`) is re-armed only by a FULL
`Backend::restore`. A LAPIC-armed VM (the run_until path) would then pass small
post-restore deadlines to a STALE `B` → with the P1 contract that surfaces as a `<
current` fail-closed, but with a confusing message. `restore_vtime` now fails closed
**up front** if `preemption_deadline().is_some()` (a LAPIC timer is armed), directing the
caller to a full `Backend::restore` (`restore_snapshot` / `restore_vm_state`), which
re-arms `B`'s baseline. Documented in `vmm-core`'s notes; test
`restore_vtime_refused_with_a_lapic_timer_armed`.

### PR #15 round-9 (cross-model): realign B on V-time-only restore (P1) + poison no-completion exits (P2) + a real seed assertion (P2)

- **P1 — `rearm_vtime_baseline()` on the `Backend` trait.** Round-8 only *rejected*
  `restore_vtime` while a LAPIC timer was CURRENTLY armed, but a deterministic LAPIC-wired
  VM can V-time-restore BEFORE arming the timer: the backend's separate `run_until` PMU
  counter (`B`) stays stale, and a LATER timer-arm preempts against it. The preferred fix
  (cleaner than blanket-rejecting): a new `Backend::rearm_vtime_baseline()` that re-arms
  the first-entry PMU reset (same mechanism as a full `restore`'s P1(b) re-arm), so the
  NEXT entry re-baselines `B`. `KvmBackend` re-arms `reset_arm`; `PatchedKvmBackend`
  forwards (it IS the preemption path); `MockBackend` records the call; `Box<B>` forwards.
  vmm-core's `restore_vtime` calls it **unconditionally** after resetting its own work
  clock (round-8's guard removed). Tests: box
  `rearm_vtime_baseline_re_zeroes_the_run_until_counter` (advance B → rearm →
  `run_until(small)` lands exactly, not against a stale B) + portable
  `restore_vtime_realigns_the_backend_run_until_baseline` (the mock records the call even
  with NO timer armed — the deferred-arm case).
- **P2 — poison no-completion exits on a PMU-read failure.** Round-5 stored `self.pending`
  before the fallible `pmu_work()` so a read-style exit fails closed on a PMU-read
  failure; but a NO-completion exit (PIO OUT, MMIO write, HLT, shutdown) leaves
  `pending == None`, so a retry would re-enter and SKIP a consumed (guest-visible) exit
  the VMM never observed. `take_guest_exit_stop` now ALSO arms a portable [`ExitPoison`]
  before `pmu_work()` (cleared on success); `run`/`run_until` `check_not_poisoned()` first
  → a retry fails closed instead of skipping. Test: portable
  `exit_poison_fails_closed_until_an_exit_is_delivered` (the state machine: arm without
  `delivered` → poisoned). The live PMU-read fault is review-verified (fault injection,
  as in round-5), with the state machine portably tested.

### PR #15 round-10 (cross-model): revert the round-9 `Backend` trait method — it was a FROZEN-API violation (P2, blocking)

- **The round-9 `Backend::rearm_vtime_baseline()` was a regression my own fix introduced.**
  Task 47's Public API section freezes the `Backend` trait: implement the existing
  `run_until`/`inject`/`save`/`restore` surface, do **not** add trait methods. Round-9 grew
  the trait by a method — drift the public-api gate dutifully recorded (9 new entries). That
  is reverted here: the trait method, its `KvmBackend`/`PatchedKvmBackend`/`MockBackend`
  impls, the `Box<B>` blanket forward, the `MockBackend::rearm_baseline_calls` accessor, and
  the round-9 `dyn_backend` + box tests are all removed; `tests/public-api.txt` is
  regenerated (the 9 entries gone, **no `Backend` trait drift**).
- **B is re-armed through the FROZEN trait instead.** vmm-core's `restore_vtime` re-baselines
  the backend's separate `run_until` PMU counter (`B`) by round-tripping the vCPU through the
  existing `save()` + `restore()`: `restore` already re-arms the first-entry `reset_arm` as a
  side effect (the P1(b) re-arm), and `save`→`restore` is an identity on vCPU state, so the
  hash is unchanged. No new surface, same effect. The backend is type-erased behind
  `Box<dyn Backend>` on the production boot path, so a concrete re-arm method would not even
  be reachable from vmm-core's generic `restore_vtime` — the trait round-trip is the only
  mechanism that works there. Box test renamed/repurposed:
  `save_restore_roundtrip_re_zeroes_the_run_until_counter` (advance `B` → `save`+`restore` →
  `run_until(small)` lands exactly, not against a stale `B`); the broader
  `restore_re_arms_pmu_reset_excluding_foreign_branches` already pins the save+restore re-arm
  against a coexisting VM's foreign branches.

### PR #15 round-11 (cross-model): the first-entry-reset invariant, stated globally + enforced (P1)

- **The recurring bug.** The shared `exclude_host` PMU counter accrues every VM's guest
  branches on the pinned thread, so each VM re-baselines (`FirstEntryReset`) at its first
  entry. Across rounds, individual no-entry paths kept consuming that reset without a real
  `KVM_RUN` — letting a coexisting VM contaminate the baseline. Round-10 left the last one:
  `run_until` called `ensure_first_run()` (which `take_reset()`s + zeroes the PMU)
  **unconditionally before `classify_run_until`**, so the `AlreadyAtDeadline` (zero-step)
  and `DeadlineInPast` branches — which do NO `KVM_RUN` — consumed it anyway.
- **The invariant (now stated on [`FirstEntryReset`] and enforced structurally).** *The
  pending first-entry reset is consumed (counter zeroed, flag disarmed) by an ACTUAL guest
  entry — a real `KVM_RUN` — and by nothing else.* No zero-step / `AlreadyAtDeadline` /
  `DeadlineInPast` / `restore` / `Deadline`-without-entry path may consume or disarm it; it
  stays **pending** until a real entry. `ensure_first_run` is documented as the SOLE
  consumer and is now called only on entering paths: `run` → `enter_guest`, and
  `run_until`'s `Drive` branch → `drive_run_until`. The no-entry branches read the
  **deferred** baseline via a new non-consuming `FirstEntryReset::is_pending()` peek: when
  pending, `run_until` takes `start = 0` (the next real entry will zero `B`) without
  touching the flag — and still reads `pmu_work()` first to prove the PMU is present.
- **Audit (every `reset_arm` consumer gated on a real entry).** `take_reset` is called
  ONLY in `ensure_first_run`; `ensure_first_run` is called ONLY at `run`'s pre-`enter_guest`
  point and `run_until`'s `Drive` branch (both immediately precede a `KVM_RUN`); `rearm` is
  called on a reset-failure retry and on `restore` (deferring to the next entry). Nothing
  consumes it off a no-entry path.
- **Tests.** Portable: `first_entry_reset_fires_once_then_only_after_rearm` extended to
  cover the non-consuming `is_pending` peek (kills both `is_pending` mutants). Box:
  `zero_step_run_until_keeps_first_entry_reset_pending` — a zero-step `run_until(0)`, then a
  foreign VM retires ~100k branches on the same thread, then B1's first REAL entry lands
  `run_until(50_000)` at EXACTLY 50_000 (the still-pending reset excludes the foreign
  branches; with the round-10 bug it would fail closed as a past deadline).
