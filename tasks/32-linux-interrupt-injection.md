# Task 32 — interrupt-injection seam: drive the LAPIC timer to GUEST_READY (Phase B)

> **TOP PRIORITY · the critical path to `GUEST_READY`.** Completes task 30: a real Linux userspace
> app (`/init` printing `GUEST_READY`, then clean poweroff) runs in `consonance`. Depends on task 30
> (#58) being merged — branch from a main that has the Linux boot path + the `vmm-core` xAPIC/timer
> wiring this task delivers vectors into. Integrator-directed (2026-06-25): Linux running is the proof
> consonance exists for.

Read `tasks/00-CONVENTIONS.md`, `tasks/30-linux-boot.md`, `consonance/vmm-core/IMPLEMENTATION.md`
(Task 30 section), `consonance/lapic/`, and `docs/cpu-msr-contract.toml` (xAPIC/timer rows) first.

## Why this exists

Task 30 boots a real Linux kernel to userspace `/init` inside the VMM, but it **cannot print
`GUEST_READY`**: that is a userspace console write, whose 8250 TX buffer drains only on a timer tick,
and **no interrupts are delivered** — `KvmBackend::inject` is `Unsupported`. The `vmm-core` side is
already wired (the `lapic` xAPIC register file is in the run loop; the LAPIC timer is driven from
`Vmm::lapic_now_vns()` off V-time). What's missing is the **backend's ability to deliver a vector**
to the guest vCPU. This task adds that, on stock KVM (to reach `GUEST_READY`) and on the patched
backend (so the delivery is deterministic — Phase C of task 30).

## Goal

Implement userspace interrupt injection in `consonance/vmm-backend` so the V-time-driven LAPIC timer
delivers its vector to the guest, the periodic-tick kernel makes progress, the 8250 TX drains, and
the box live test observes **`GUEST_READY`** followed by a **clean poweroff**. Then make it
**deterministic**: two same-seed boots on the patched backend produce bit-identical serial +
`state_hash` (task 30 Phase C / gate 4).

## Scope (the seam + the KVM mechanics)

1. **`Backend` injection API.** Implement the `inject` path on `KvmBackend` (today `Unsupported`).
   The standard KVM userspace-irqchip mechanism (we run `KVM_IRQCHIP_NONE` + a userspace xAPIC):
   - When the LAPIC has a deliverable vector and the vCPU **can** take it
     (`kvm_run.ready_for_interrupt_injection` && guest `RFLAGS.IF` && not in an interrupt shadow),
     queue it with the **`KVM_INTERRUPT`** ioctl before `KVM_RUN`.
   - When it **cannot** yet, set `kvm_run.request_interrupt_window` (or the equivalent control) so
     KVM exits with **`KVM_EXIT_IRQ_WINDOW_OPEN`** as soon as the guest is injectable; the run loop
     already consumes that exit (`kvm.rs` returns `Ok(None)` for it) — wire it to retry the pending
     vector. Acknowledge/EOI flows back through the `lapic` register file.
   - Honor the existing `vcpu` state save/restore of `interrupt.injected` / interrupt-window fields
     (`to_kvm`/`from_kvm` already round-trip them) so snapshot/replay stays correct.
2. **Drive it from the V-time LAPIC timer.** The timer's expiry (computed from `lapic_now_vns()` /
   the xAPIC `TMCCT`/`TMICT`/divide config) decides *when* a vector is pending; injection is a
   **deterministic function of V-time**, not wall-clock. One-shot and periodic timer modes as the
   kernel programs them. Keep the LPC/8250 poll path working (kernel printk already appears; this
   adds the tick that drains userspace TX).
3. **`unsafe` discipline.** Any new ioctl `unsafe` carries a `// SAFETY:` and must run clean under
   **Miri** behind a seam (the syscall behind a trait, exercised by an in-process/mock test) — the
   unsafe⇒Miri review-bar rule. Determinism: no wall-clock, no host entropy, no `HashMap` reaching
   injected state.

## Phasing

- **Phase B.1 — deliver on stock KVM → `GUEST_READY`.** Inject the timer vector; get the kernel to
  tick, drain the userspace console, print `GUEST_READY`, and `poweroff` cleanly. **This flips task
  30 gate 3 green** (the honest `GUEST_READY` gate, not the userspace-reached intermediate).
- **Phase B.2 — deterministic on the patched backend (task 30 gate 4 / Phase C).** Same boot on
  `PatchedKvmBackend`; the LAPIC timer calibrates (in-guest RDTSC traps → V-time advances) and the
  injection points are deterministic; assert **bit-identical** serial + `state_hash` across two
  same-seed runs.

## Acceptance gates

1. **Injection unit-tested under Miri** (mock/synthetic `kvm_run`): the ready/not-ready branch, the
   `KVM_INTERRUPT` queue, the `request_interrupt_window` + `KVM_EXIT_IRQ_WINDOW_OPEN` retry, and the
   vcpu-state round-trip of the interrupt fields — exercised under the interpreter, 0 UB.
2. **Phase B.1 — `GUEST_READY` (box-only, stock, `#[ignore]`):** the real `guest/linux` bzImage +
   initramfs boots; serial contains **`GUEST_READY`** and the guest powers off cleanly within a
   bounded V-time + wall-clock budget; no contract violation. (This replaces task 30's userspace-
   reached gate as the real milestone gate.)
3. **Phase B.2 — deterministic-twice (box-only, patched):** identical serial + `state_hash` across
   two same-seed boots. (Deferred-OK if B.1 lands first as its own PR, but required to call the
   GUEST_READY milestone *done*.)
4. **No determinism / contract regression:** M1/M2/P6 and the det-corpus goldens unchanged
   (injection is `Option`/Linux-path-gated like task 30's device wiring; `state_hash` for non-Linux
   paths byte-identical). Standard gates green incl. **mutants** (pin the timer-expiry / injectability
   arithmetic with exact-value tests) and **public-api** (refresh on the box for the `cfg(linux)`
   surface).

## Non-goals

I/O-APIC and MSI (LAPIC timer + the legacy path is enough for this guest); multiple vectors beyond
what the boot needs (timer + whatever the kernel's early path requires — discover empirically, don't
gold-plate); SMP/IPIs (single vCPU); the R3 fault model's interrupt faults; networking. Do not change
the CPU/MSR contract or its hash. Keep changes to `vmm-backend` (the seam) + the minimal `vmm-core`
glue to drive it; the loader (task 30) is done.
