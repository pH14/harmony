# Task 34 — deterministic Linux boot (Phase C): same seed ⇒ bit-identical to GUEST_READY

> **TOP PRIORITY — the remaining Linux milestone.** Task 33 (#60) reached `GUEST_READY` on **stock**
> KVM. This task makes the boot **deterministic**: two same-seed boots on the **patched** backend
> produce bit-identical serial (including `GUEST_READY`) + `state_hash` — the project's core thesis
> (same seed ⇒ bit-identical execution) applied to a real Linux guest. Depends on **#60 merged** —
> branch from a main that has the serial-IRQ + GUEST_READY path. (After merging #60, **fast-forward
> local `main` before spawning** — agent-spawn branches from local HEAD; see
> [[agent-spawn-base-local-head]].)

Read `tasks/00-CONVENTIONS.md`, `tasks/30-linux-boot.md`, `tasks/33-guest-ready-serial-irq.md`,
`consonance/vmm-core/IMPLEMENTATION.md` (Task 33 section), and `docs/box-patched-kvm-ops` /
[[box-patched-kvm-ops]] first.

## The blocker (diagnosed in task 33)

The serial IRQ adds **no** nondeterminism (it asserts on the guest `IER.THRI` write, is gated by the
guest PIC-IMR writes, injected at the next injectable entry — all deterministic; both IMRs hashed), so
determinism holds **by construction** *once the patched boot completes*. The problem is the patched
boot **doesn't complete in reasonable time**: on the patched backend every `RDTSC` traps and V-time
advances per retired branch, so the **i8042 keyboard-controller probe** — whose flush/wait loops spin
on a jiffies timeout — burns an enormous number of guest branches; a real patched boot sat **>5 min in
the i8042 probe** without reaching userspace. On stock KVM the same probe clears in 0.33 s. This is a
**pre-existing patched-boot / V-time-vs-jiffies characteristic, orthogonal to the serial work**.

## Phase 1 — make the patched boot complete (the i8042 fix)

Get the patched boot to reach userspace in a bounded V-time + wall-clock budget. Options (pick by
what's cleanest + keeps determinism; document the choice):
- **(A) Disable the i8042 probe via kernel cmdline** — add `i8042.noaux i8042.nokbd i8042.nopnp`
  (and/or `i8042.dumbkbd`) to the boot cmdline so the kernel skips the controller probe entirely. No
  device-model change; the guest has no keyboard/mouse anyway. Simplest if it fully skips the spin.
- **(B) Model the i8042 controller to clear instantly** — answer ports `0x60`/`0x64` so the probe's
  status reads (`OBF`/`IBF`) report ready immediately (the stock-KVM-equivalent fast clear), so the
  probe completes in a few branches. Linux-path-gated like the other device wiring; if it has state,
  fold it into the `state_hash` (e.g. a `KBD` chunk) so determinism is preserved.
- Investigate whether any *other* jiffies-timeout spin (beyond i8042) blows up under patched V-time and
  handle it the same way — the i8042 probe is the one task 33 hit, but confirm nothing downstream does
  the same (calibration loops were already pinned via `lpj=`/`tsc=reliable` in task 33's cmdline).

Keep the **stock** `GUEST_READY` path (task 33 gate3) green — don't regress it.

## Phase 2 — the determinism gate

`c_linux_boot_deterministic_twice_patched` (already written in `live_linux_boot.rs`) passes on the box:
boot the real `harmony-linux/linux` bzImage + initramfs **twice** on the **patched** backend at the same seed
and assert **bit-identical**:
- the full serial capture (including the `GUEST_READY` line and the clean poweroff), and
- the terminal `state_hash`.
This sits on top of the already-box-proven V-time/RNG determinism (P6) and the deterministic serial
IRQ (#60). Run it per [[box-patched-kvm-ops]] (load patched modules, bound with an on-box `timeout`,
**always revert to stock KVM 1396736** after — verify the lsmod size).

## Acceptance gates

1. **Patched boot completes** to `GUEST_READY` within a bounded budget (Phase 1) — the i8042 (and any
   sibling) spin no longer strands the boot.
2. **Deterministic-twice (box-only, patched, the milestone gate):** `c_linux_boot_deterministic_twice_patched`
   passes — identical serial (incl. `GUEST_READY`) + `state_hash` across two same-seed patched boots.
   Quote both digests (equal) in the PR. **This is the headline — same seed ⇒ bit-identical Linux.**
3. **No regression:** the stock `GUEST_READY` gate (task 33 gate3), M1/M2/P6, and the acceptance-suite
   goldens are unchanged (any new device wiring is Linux-path-gated; `state_hash` for non-Linux paths
   byte-identical). Standard gates green incl. **mutants** (pin any new device/state logic with
   exact-value tests) + **Miri** (any new `unsafe`) + **public-api** (refresh **on the box** for the
   `cfg(linux)` surface — vmm-core is box-gated; a Mac regen omits box-only items).
4. **Box hygiene:** every patched-module run reverts to stock KVM (1396736) after; verify.

## Non-goals

A real keyboard/mouse (the guest needs none); IO-APIC/MSI (the LAPIC + 8259 virtual-wire path is
enough); SMP; networking/virtio/disk; the R3 fault model; snapshot/restore of a running Linux (a later
milestone). No CPU/MSR contract or hash change; no kernel-config change (raise to the integrator if a
`CONFIG_*` is implicated). Build on task 30/32/33 — don't re-architect the loader, seam, or serial.
