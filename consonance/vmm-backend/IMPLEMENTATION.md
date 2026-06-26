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
