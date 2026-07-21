# Task 33 — reach GUEST_READY: fix the userspace serial-TX path (diagnose → wire serial IRQ 4)

> **TOP PRIORITY · the final step to the headline goal.** Task 30 boots Linux to userspace; task 32
> (#59) delivers the working interrupt-injection seam. The remaining blocker — discovered + root-caused
> in task 32 — is **not** interrupt delivery: the guest's **userspace interrupt-driven 8250 TX write
> fails** (`echo GUEST_READY` → exit 13, immediately), while the kernel's *polled* printk console works.
> Integrator decision (2026-06-25): **diagnose the exact mechanism, then wire serial IRQ 4** through the
> task-32 LAPIC-injection seam so the interrupt-driven tty write completes and `/init` prints
> `GUEST_READY` + powers off cleanly. Depends on **#59 merged** — branch from a main that has the inject
> seam + the Linux boot path. (After merging #59, **fast-forward local `main` before spawning** —
> agent-spawn branches from local HEAD.)

Read `tasks/00-CONVENTIONS.md`, `tasks/30-linux-boot.md`, `tasks/32-linux-interrupt-injection.md`,
`consonance/vmm-core/IMPLEMENTATION.md` (Task 30 + 32 sections), `consonance/vmm-core/src/devices.rs`
(the 8250 model), and `consonance/lapic/` first. The full box diagnostic logs are on the box at
`<box-logs>/gate3{,c,d}.log`.

## What task 32 already proved (don't redo)

- The V-time LAPIC timer fires and its vector is delivered (`KVM_INTERRUPT`/interrupt-window seam,
  `KvmBackend::inject`). Linux boots to userspace, `mount proc`/`sysfs` succeed, 24,947 steps, no VMM
  contract violation.
- `GUEST_READY` fails because `echo` → the **userspace 8250 TX write** returns an error (exit 13),
  *immediately* — a spin-loop inserted after the `echo` never executed, so it is **not** a
  drain/needs-more-ticks problem. `echo > /dev/ttyS0` fails identically (not `/dev/console` routing).
  The kernel's **polled** printk path works (whole boot log captured). So the bug is in the 8250
  **interrupt-driven** userspace write path / serial model.

## Phase 1 — DIAGNOSE (confirm the exact mechanism before fixing)

The "needs serial IRQ 4" conclusion is a strong hypothesis, **not yet proven**. First pin the exact
cause of the exit-13 write failure on the box, e.g.:
- What does the kernel's 8250 driver do on a userspace `write()` that the polled printk path doesn't —
  does it enable the THRE interrupt (`IER` bit 1) and **block waiting for IRQ 4** that never arrives?
  Does it read a register (`IIR`/`LSR`/`MSR`/scratch) the model returns wrong, and bail? Does
  `serial8250` probe it as a `16450` (no FIFO) and take a path the model mishandles?
- Confirm whether the write **blocks** (hangs until budget) or **errors out** (returns quickly) — task
  32 says it errors immediately, so it is likely a register-state/return-value the driver rejects, OR
  the tty layer giving up waiting for a TX-ready interrupt. Trace the exact 8250 port accesses around
  the failing `write()` and compare to what a real 8250 + IRQ would present.

Write the diagnosis into `IMPLEMENTATION.md` (what register/state/IRQ the userspace write needs that
the model doesn't provide). The fix in Phase 2 follows from it — if diagnosis shows the cause is *not*
IRQ 4 (e.g. a wrong register read), fix that instead and note it.

## Phase 2 — FIX (most likely: raise serial IRQ 4 through the LAPIC seam)

Implement what Phase 1 identifies. The expected shape (now **in scope** per the integrator decision):
- The 8250 model raises **IRQ 4** (the COM1 line) when THR empties / per the `IER` THRE-enable, and
  reflects it in `IIR`. Route IRQ 4 → a LAPIC vector → `inject` (task 32's seam) so the kernel's
  interrupt-driven `write()` completes and userspace TX drains.
- A **minimal legacy IRQ route** is enough: we run `KVM_IRQCHIP_NONE` + the userspace xAPIC, so the
  LAPIC (not a full IO-APIC) delivers it. Wire the 8250 IRQ 4 to the LAPIC the way the kernel expects
  for a legacy COM1 (LVT / the existing xAPIC path). EOI/IRR/ISR flow through the `lapic` register file.
- Keep it **Linux-path-gated** (like task 30/32's device wiring): no-op when the xAPIC is unwired, so
  M1/M2/corpus port-I/O default-deny + `state_hash` stay byte-identical.

## Phase 3 — GUEST_READY + determinism

- `/init` prints **`GUEST_READY`** and `poweroff` cleanly — `gate3_linux_guest_ready_and_clean_poweroff`
  (task 30 gate 3) goes **green** on the box (stock KVM).
- **Deterministic-twice (task 30 gate 4 / task 32 Phase B.2):** two same-seed boots on the **patched**
  backend produce bit-identical serial (incl. `GUEST_READY`) + `state_hash`. The serial IRQ timing
  must be a deterministic function of V-time (the injection points already are).

## Acceptance gates

1. **Diagnosis documented** — the exact mechanism of the exit-13 userspace-write failure, in
   `IMPLEMENTATION.md`, with the box evidence.
2. **`GUEST_READY` (box-only, stock, the milestone gate):** `gate3_linux_guest_ready_and_clean_poweroff`
   passes — serial contains `GUEST_READY`, clean poweroff, bounded budget, no contract violation. **This
   is the headline.** Quote the serial line in the PR.
3. **Deterministic-twice (box-only, patched):** identical serial + `state_hash` across two same-seed
   boots (gate 4 / Phase B.2).
4. **No regression:** M1/M2/P6 + acceptance-suite goldens byte-identical (serial IRQ is Linux-path-gated);
   standard gates green incl. **mutants** (pin the 8250 IRQ-raise / `IIR`/`IER` logic with exact-value
   tests) + **Miri** (any new `unsafe`) + **public-api** (refresh on the box for `cfg(linux)`).

## Non-goals

Full IO-APIC, MSI/MSI-X, multiple legacy IRQ lines beyond COM1/IRQ 4 (and the timer IRQ from task 32);
SMP/IPIs; the R3 fault model; networking; virtio. No CPU/MSR contract or hash change. Build on task
32's seam + task 30's loader/device model — don't re-architect them. If diagnosis reveals the real
cause is unrelated to IRQ 4, fix that and update this spec's framing rather than forcing IRQ wiring.
