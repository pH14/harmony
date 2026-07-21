# Task 21 — deterministic RDTSC/RNG on the box: PatchedKvmBackend + V-time work-counter

Read `tasks/00-CONVENTIONS.md`, `tasks/16-patched-kvm-rdtsc-spike.md` (the GO'd spike this
productionizes), `docs/R-BACKEND.md`, and `docs/INTEGRATION.md §4` (RDTSC/RNG determinism + snapshot
continuity) first. This makes a guest that executes **RDTSC / RDTSCP / RDRAND / RDSEED** run
**deterministically** (bit-identical twice, same `state_hash`) on the box, by productionizing the
task-16 patched KVM into a real `PatchedKvmBackend` **and** wiring V-time's work source (found
**unwired** — `consonance/vmm-core/src/vmm.rs` logs `work=unwired`).

Touch `consonance/vmm-backend/` (new backend + exit mapping), `consonance/vmm-core/` (the
completion logic above the trait + the work-counter wiring + composition root), and the box (build
+ load the patched modules). **Do not** change the `Backend` trait shape or the `Exit` enum — the
`Rdtsc/Rdtscp/Rdrand/Rdseed` variants and `complete_read` already exist for exactly this.

## Layering — the hard rule

The backend is a **thin KVM wrapper**: it *surfaces* the four determinism exits and *completes*
them, nothing more. The **deterministic values are computed above the trait, in vmm-core** — V-time
TSC from the `VClock`, RNG from the seeded entropy service — so **nothing above the trait branches
on which backend is in use** (R-BACKEND). A `VClock::tsc()` call or an entropy draw inside
`vmm-backend` is a layering bug.

## P1 — Box: build + load the patched KVM modules

Per the spike `consonance/vmm-backend/kvm-patches/BUILD.md` (you can `ssh <det-box>` now):
- **Canonical gate** (pinned tag): the 3-patch series (`consonance/vmm-backend/kvm-patches/patches/`) must
  `git am` clean onto `linux-6.18.35` and build `kvm.ko`/`kvm-intel.ko` (BUILD.md Part 1). This is
  the determinism-of-record target — keep it green even though the box can't *load* it (vermagic).
- **Live proxy** (box kernel `6.12.90`): build the ported modules (BUILD.md Part 2 —
  `EXPORT_SYMBOL_FOR_KVM_INTERNAL`→`EXPORT_SYMBOL_GPL`, overlay) and load them
  (`rmmod kvm_intel kvm; insmod` the patched pair). Run the integration test (P6), then **revert to
  stock** (`rmmod` patched; `modprobe kvm_intel`) so the box is left clean. Record the exact recipe
  + the proxy caveat in IMPLEMENTATION.md (the spike already proved this loads — you're
  re-running it as the backend's substrate).

## P2 — `PatchedKvmBackend` (consonance/vmm-backend/src/patched_kvm.rs, new)

- Reuse `KvmBackend`'s KVM/VM/vCPU setup (`KVM_IRQCHIP_NONE`, memslots, CPUID/MSR-filter install).
  Opt in to interception via `KVM_ENABLE_CAP(KVM_CAP_X86_DETERMINISTIC_INTERCEPTS = 245)`
  **before vCPU creation** (`kvm->arch.deterministic_intercepts`); default-off preserves stock
  behavior, so this is a distinct backend, not a mode of `KvmBackend`.
- Exit mapping: `KVM_EXIT_DETERMINISM = 41` → read `kvm_run.determinism` (`insn` ∈ {RDTSC=0,
  RDTSCP=1, RDRAND=2, RDSEED=3}, `width`, `dest_reg`) → emit `Exit::Rdtsc` / `Exit::Rdtscp` /
  `Exit::Rdrand{width}` / `Exit::Rdseed{width}`. Completion: `complete_read(value)` writes
  `kvm_run.determinism.value` (low `width` bytes → dest / EDX:EAX), sets `KVM_DETERMINISM_FLAG_CF`
  for a *successful* RNG draw, carries `aux`→ECX for RDTSCP; resume via the
  `complete_userspace_io` round-trip (mirror the existing `Rdmsr` completion path).
- `cfg(target_os = "linux")`; `capabilities()` returns `deterministic_tsc: true,
  deterministic_rng: true`; increment the per-reason `ExitCounts` for the four exits.
- Validate the ABI structs against `consonance/vmm-backend/kvm-patches/patches/0001-*.patch` (the spike ABI
  is a *proposal* — if you renumber the exit/cap, do it in lockstep with the loaded module and
  document it).

## P3 — V-time work-counter (productionize `CpuBackend::work`)

V-time = f(work); `work` = retired **conditional branches**, read at every VM exit (`vtime`
crate docs). It is defined as a trait (`CpuBackend::work`) but **not wired**. Implement it:
`perf_event_open` a retired-conditional-branch counter on the vCPU thread (pinned, per
`docs/BOX-PINNING.md`), read it (`read(2)` on the perf fd, or `rdpmc`) at exit points, and feed it
into vmm-core's V-time so the clock advances. Replace the `work=unwired` placeholder in `vmm.rs`.
Cross-check task-07 (`tasks/07-pmu-spike.md`) for the counter selection + the
idle-skip/timer-absorption rules (`vtime` docs §). This is host/perf-level — it can live in
`vmm-backend` behind a small `CpuBackend` impl or in the vmm-core run loop; pick per the existing
`vtime`/R-BACKEND seam and raise a `[question]` if the boundary is ambiguous.

## P4 — Completion logic in vmm-core (above the trait)

- `Exit::Rdrand{width}` / `Exit::Rdseed{width}` → draw the next bytes from the **seeded entropy
  service** (`SeededEntropy` / the dispatcher's PRNG; `consonance/hypercall-proto`), mask to `width`,
  `complete_read(value)` (CF set). Same stream the `Entropy` hypercall service uses — RDRAND and the
  hypercall RNG must not diverge. **Fully implementable now** (no work-counter needed).
- `Exit::Rdtsc` / `Exit::Rdtscp` → `complete_read(VClock::tsc(work))` using the active `VClock` and
  the P3 work value; RDTSCP also supplies `IA32_TSC_AUX` (contract value). Snapshot/restore: on
  restore the hardware counter restarts at 0 and `vns_base` is set to the snapshotted V-time so
  `tsc()` continues exactly (INTEGRATION.md §4) — verify a save/restore mid-stream keeps RDTSC
  monotonic and reproducible.

## P5 — Composition root

`fn main` selects `KvmBackend` (stock, default) vs `PatchedKvmBackend` (via a flag/config), injected
as `Box<dyn Backend>`. The one place a concrete backend is named. Nothing else branches on it.

## P6 — Acceptance: box determinism integration test

A bare-metal payload (extend `consonance/acceptance-suite/payloads/`, or a vmm-core integration test) that:
- reads RDTSC/RDTSCP repeatedly → **strictly monotonic**, deltas match the V-time formula, never a
  raw host TSC; RDTSCP `aux` == contract value;
- reads RDRAND/RDSEED → values **== the contract PRNG stream** for the seed, CF set per contract;
runs on the box (patched modules loaded, `taskset` pinned) and is **bit-identical across two runs**
(identical `state_hash`) — and a snapshot/restore mid-run resumes both clocks exactly. This is the
gate the whole task exists for; foreman re-runs it on the box at review.

## Gates

Mac: `build`/`nextest`/`clippy -D warnings`/`fmt` for `vmm-backend` + `vmm-core` — the
`PatchedKvmBackend` is `cfg(linux)` so its *logic* is unit-tested via a mock determinism-exit
(extend `MockBackend` to script `Rdtsc`/`Rdrand` exits) on macOS; the **live** patched-module run +
P6 are **box-only** (evidence in IMPLEMENTATION.md). Exit cost from the spike (~3.4 µs RDTSC,
~3.8 µs RNG) is acceptable — **do not** build the in-kernel V-time fast-path (R-BACKEND defers it;
measure first).

## Deliverables

`PatchedKvmBackend` + the four-exit round-trip; the wired V-time work-counter (no more
`work=unwired`); the vmm-core RDTSC/RNG completion; the composition-root selector; the **box P6
determinism proof** in IMPLEMENTATION.md (deterministic-twice + snapshot continuity); the canonical
patch confirmed `git am`-clean + building vs `linux-6.18.35`. Box left on stock KVM.
