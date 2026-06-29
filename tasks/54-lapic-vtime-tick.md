# Task 54 — Real Linux's deterministic V-time tick: route the xAPIC MMIO to the model + a deterministic natural-exit fallback for preemption-outrun

> **Determinism-core. The capstone of the V-time effort.** This task productionizes a fix that is
> **already proven end-to-end on the box**: real unmodified Linux → real `runc` 1.3.6 → the real
> Postgres OCI container, running **deterministic-twice** (bit-identical `state_hash` + bit-identical
> "random" UUIDs across two same-seed boots). The reference implementation is the captured diff at
> `~/workspace/harmony-proven-vtime-fix.diff` (foreman, 2026-06-29). Your job is to land that fix
> *properly* — portable seams, full gates, the determinism argument written down — not to re-derive it.

Read `tasks/00-CONVENTIONS.md`, `tasks/47-deterministic-preemption-timer.md` (the `run_until`
mechanism this extends), `tasks/30-*` / `guest/linux/IMPLEMENTATION.md` (the Linux boot + `linux_loader`),
`docs/CPU-MSR-CONTRACT.md` §6 (CPUID 0x15 → the LAPIC-timer V-time rate), and the memory
`lapic-page-hole-unblocks-vtime` first.

## Why (the root cause this fixes)

The long runc/Postgres deadlock had a single root cause, found by instrumenting `dispatch_mmio`:
**zero of Linux's xAPIC MMIO writes reached the userspace deterministic LAPIC model.** The guest's
`0xFEE00000` LAPIC page was **RAM-backed** — `kvm_sys.rs::map_memory` registers all guest RAM as a
**single memslot** that *covers* the APIC page (~4 GiB) for the 8 GiB Postgres guest, so Linux's LAPIC
accesses were serviced from RAM and never faulted to `KVM_EXIT_MMIO`. The model stayed at reset, the
LAPIC timer never fired, and runc's Go runtime deadlocked on an idle HLT. (The synthetic task-47/52
gates dodged this only because they use < 4 GiB RAM, where the APIC page is naturally unmapped — so
those gates never actually drove real Linux's LAPIC.) **The vPIT (task 53) was a wrong turn; Linux was
always on the LAPIC-timer path** (it calibrates the timer from the contract's CPUID 0x15). Task 53 /
PR #29 is dropped.

## The fix — three coordinated changes (all required; land together)

### 1. Reserve the xAPIC page in the E820 map (`consonance/vmm-core/src/linux_loader.rs`) — portable

`build_boot_params` currently emits high RAM as one `[1 MiB, ram)` usable region, which marks
`0xFEE00000` as RAM so the kernel zeroes the page on init. Split it to mark the 4 KiB LAPIC page
**`E820_RESERVED` (type 2)**: `[1 MiB, 0xFEE00000) RAM`, `[0xFEE00000, +0x1000) RESERVED`,
`[0xFEE01000, ram) RAM` (4 entries). For a guest whose RAM does not reach `0xFEE00000 + 0x1000`, keep
the single high-RAM entry (nothing to split). See the reference diff.

### 2. Punch a matching memslot hole at the xAPIC page (`consonance/vmm-backend/src/kvm_sys.rs`) — box-only FFI + a **portable seam**

`map_memory` must register the guest RAM as **two memslots** that leave the 4 KiB LAPIC page unmapped,
so a guest access faults to `KVM_EXIT_MMIO` → `dispatch_mmio` → the userspace `Lapic`. **Per
`box-only-layer-coverage-blind`: the FFI in `kvm_sys.rs` is coverage/mutation-excluded, so put the
region-splitting *computation* in a portable, pure function** (e.g. `fn split_around_hole(base, len,
hole_base, hole_len) -> impl Iterator<Item=(gpa, size, host_off)>`) with unit + property + Kani
coverage; `kvm_sys.rs` only iterates it and makes the `set_user_memory_region` calls. Remove the
`eprintln!("DIAG-MEMSLOT …")`. The `unsafe` FFI keeps its `// SAFETY:` justification and must pass
Miri on any interpreter-reachable path.

### 3. Deterministic natural-exit fallback for preemption-outrun (`consonance/vmm-backend/src/run_until.rs`) — determinism-core

Currently `drive_run_until` **fail-closes loudly** on `GuestExitDisposition::PastDeadline` (a guest
exit reported at `work > deadline`) and `AtDeadline` (`work == deadline` with an exit). Replace both
with **delivering the guest exit** (`Ok(exit)`) — the missed timer self-heals: on the next
`run_until(deadline)` the planner sees `now > deadline` → `TargetInPast` → delivers the timer at the
next exit boundary. Remove the `eprintln!("DIAG-SKID …")`. Rewrite the (now-stale) "fail closed" doc
comments to state the new behavior and the determinism argument below.

## The determinism argument (the crux — the reviewer MUST verify this is written down and correct)

The preemption mechanism is: a `perf_event` branch-counter overflow → `O_ASYNC` `SIGIO` → `EINTR`s
`KVM_RUN` near `deadline − SKID_MARGIN`, then single-step to the exact deadline. `PastDeadline` occurs
**iff `[deadline − SKID_MARGIN, next-guest-exit]` is exit-free with the next exit past the deadline** —
because inside an exit-free region there is **no VM exit for the queued `SIGIO` to take effect at**, so
the guest *always* runs to the same next natural exit. That exit is a fixed instruction in the
deterministic guest stream, so its retired-branch count (`work`) is **identical across same-seed runs**.

Therefore: whether a given deadline is "single-step-reachable" (an exit/SIGIO-effect at or before it)
or "must be delivered at the next natural exit" (exit-free region spanning it) is a **deterministic
function of the instruction stream**, *not* of the nondeterministic SIGIO latency — which is absorbed
by single-step in the former case and irrelevant in the latter. Delivering the guest exit and the
late timer drops **nothing** (both are delivered; the timer is merely late, deterministically).

**Box evidence (must be reproduced by this task's gates):** over the full runc+Postgres boot there is
**exactly one** such overshoot (a LAPIC deadline in a 28207-branch exit-free region ending in an
RDTSC), and it reproduces **bit-identically** in both r2 boots (same `deadline`, `overshoot`, exit).
`SKID_MARGIN` is unchanged (256); this is **not** a margin change — widening it is determinism-safe but
performance-prohibitive (~28k single-steps/tick). A future, optional robustness enhancement is a
patched-KVM force-exit on overflow (exact-deadline delivery, no outrun) — out of scope here.

## Acceptance gates

Beyond the standard suite (build/clippy/fmt/test on macOS + Linux):

1. **E820 split (portable, exact-value).** Unit + property tests on `build_boot_params`: for an
   8 GiB guest the table is exactly the 4 entries above with the LAPIC page `E820_RESERVED`; for a
   sub-`0xFEE01000` guest it is the single high-RAM entry. Assert exact `addr`/`size`/`type_` and
   `e820_entries` (mutation-killing). Property: the reserved page is never typed RAM for any `ram`.
2. **Memslot-split seam (portable).** Unit + property + **Kani** on the pure splitter: the union of
   emitted regions equals `[base, base+len)` minus exactly `[hole, hole+0x1000)`; regions are
   page-aligned, non-overlapping, non-empty; host offsets are consistent; the no-overlap-with-hole case
   returns the single region. `kvm_sys.rs` is exercised only on the box.
3. **Natural-exit fallback (portable).** `classify_guest_exit` + `drive_run_until` tests (the box
   `LiveCpu` never makes this call — it's the portable seam): `PastDeadline` and `AtDeadline` now return
   the guest exit; the no-exit case still returns `Exit::Deadline`; `Early` unchanged. Add a
   stateful/property test that a deadline inside a scripted exit-free region delivers at the next
   natural exit and that the result is identical across two runs of the same script (the determinism
   property, against an independent reference).
4. **`unsafe`/Miri.** The `kvm_sys.rs` memslot change keeps a justified `// SAFETY:`; Miri clean on any
   reachable path (the splitter is pure → fully Miri-exercised).
5. **Public-API snapshots** updated for any intended surface change (the new portable splitter, if
   `pub(crate)`-or-wider).
6. **Box gates (on `ssh <det-box>`, patched KVM, the built Postgres image; foreman runs these):**
   `r1_runc_postgres_runs_and_streams_patched` reaches GUEST_READY **with ZERO contract violations**
   (the strict xAPIC contract holds — no `BadOffset`, proving the E820 reservation stops the
   page-zeroing); `r2_runc_postgres_deterministic_twice_patched` PASSES (bit-identical serial +
   `state_hash`); `r3_runc_postgres_seed_sensitivity_patched` PASSES. Always revert KVM to stock 1396736.

## Cleanup / coordination (foreman)

- **Drop the vPIT:** close PR #29 (task 53) — the vPIT is unnecessary; Linux uses the LAPIC timer.
- **Re-key HLT-resume:** task 52 / PR #27's idle-HLT-resume targets the LAPIC timer deadline (its
  original keying, before task 53 re-pointed it at the vPIT). Land it against the LAPIC path; it
  composes with this task (47 preempts spins + the fallback; 52 wakes idle HLTs).
- Update `guest/linux/IMPLEMENTATION.md` / the LAPIC-timer docs to state the xAPIC page is reserved +
  MMIO-routed (write positively — what it IS).

## Non-goals

- The patched-KVM force-exit-on-overflow (the robust exact-deadline mechanism) — a future enhancement;
  the natural-exit fallback is determinism-valid and proven for the workload.
- Any `SKID_MARGIN` change. The IOAPIC page (`0xFEC00000`) — Linux is in virtual-wire mode (no MADT),
  doesn't use it; only the LAPIC page is reserved/holed.
