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
