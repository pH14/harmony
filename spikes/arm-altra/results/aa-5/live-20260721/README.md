<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-5(c) F12 re-cert — Ampere Altra N1, 2026-07-21 (hm-of6t / hm-9r1)

The AA-5(c) same-seed identity re-cert, **consolidated onto a single pinned image** and
re-run on-silicon after the 2026-07-20 box was account-wiped. Host `6.18.35-aa3preempt`
(stock 6.18.35 + patch 0001 `KVM_EXIT_PREEMPT`), kexec-booted: the exact-work clock cadence
rides that patched force-exit, and the harness refuses to fall back to stock (a stock kernel
cannot run this path). Pinned guest `Image` sha256 `d0161a7d…`, initramfs `604733be…`
(bit-reproduces the 2026-07-20 initramfs). **Every run below uses this one pinned image** —
closing the multi-build provenance smell F12 flagged: register-identity is now measured on
the pinned image itself, not borrowed from a separate diag build.

## The ruled claim (entropy-closure ruling, Paul-adopted 2026-07-20)

AA-5(c) claims **console + clock + input-closure determinism** — *not* full-RAM or full-
register identity. The work clock makes **retired-branch execution and its console** bit-
identical; the kernel-CRNG entropy and its downstream (userspace stack placement, RAM) are
the **disclosed residual**, a subsystem distinct from the work-clock. The register/RAM
divergence below is expected and named, not a gate failure — full-state identity was never
AA-5(c)'s claim.

## Verdicts

| Gate | Result |
|------|--------|
| Boot to userspace + steady state (`HARMONY_AA5_CLOCKSOURCE_OK`/`…READY`, no RCU stall) | **PASS** (`run-a/b`, `smoke`) |
| Same-seed **console** bit-identical (the observable determinism) | **PASS** (`f2cbd019…` both; `identity-ab.json`) |
| Clock + input-closure (work-derived clock, counter page-routed) | **PASS** |
| EL0 `CNTVCT` closure | **PASS** (`el0probe/`: `EL0_CNTVCT_PAGE_OK`) |
| Counter-opcode closure (0 raw `cntvct` in `vmlinux`, section-aware scan) | **PASS** (build gate) |
| Same-seed **register** digests (`regs_digest`, `core_regs_digest`) | **RESIDUAL** — attributed below |
| Same-seed **full-RAM** `state_digest` | **RESIDUAL** — kernel-CRNG entropy |

## Register / RAM residual — attributed precisely on the pinned image (hm-of6t F12)

`linux-boot` now emits, **on the pinned image itself**, a `regs_digest` (registers + vGIC)
and a `core_regs_digest` (registers only, vGIC excluded) — so register-identity is no longer
borrowed from a diag build. Both diverge same-seed. A per-register dump (`regs-dump-a/b.txt`,
`regs-divergence.diff`) attributes it exactly — **4 of 260** registers differ:

- **`x29` (FP) and `SP`** hold a userspace stack pointer (`0x0000ffff_f2…`) — i.e.
  entropy-derived **stack placement**. These are core registers *in* the digest, so they
  are precisely why `core_regs_digest` diverges. The console is unaffected (a stack address
  is never printed), which is why console identity holds.
- **`CNTPCT` and `KVM_REG_ARM_TIMER_CNT`** are generic-timer **counters** (host-time). They
  advance with wall-clock but are **excluded from the determinism digest** by
  `is_host_time_register`, so they move only in the raw dump, never in the compared digests.

So the register divergence is the **kernel-CRNG / stack-placement residual**, not the
work-clock; `regs_digest` (with vGIC) additionally folds in the host-IRQ-timing vGIC
injection state (the diverging `exits`). Full-RAM `state_digest` carries the same entropy
residual. Closing it — disabling the userspace stack randomization these FP/SP reveal, plus
freezing post-seed CRNG — is the standing entropy-closure follow-up
(`docs/PARAVIRT-CLOCK.md` §4.3), not an AA-5(c) gate.

**nokaslr note (tribunal F1-REG):** `RANDOMIZE_BASE=off`, so *kernel* VAs are stable; the
FP/SP residual is a distinct **userspace** stack-placement entropy path, not KASLR.

## Evidence integrity

`console.bin` for every run is committed (see the repo `.gitignore` exception), so
`aa5-identity-check.py` recomputes each console sha256 from a fresh checkout — no reliance on
box-only artifacts. `regs_digest` / `core_regs_digest` are in each run's `stdout.txt`;
`identity-ab.json` is the checker verdict (overall FAIL is expected — the console-identity
row is the PASS that carries the ruled claim). Register-identity is now attributed to the
actual pinned image `d0161a7d…`, closing F12.

Directory map: `run-a`/`run-b` the same-seed pair · `smoke` first boot · `el0probe` EL0
closure · `identity-ab.json` verdict · `regs-dump-a/b.txt` + `regs-divergence.diff`/`.txt`
the register attribution · `BUILD-MANIFEST.txt` build content-pins · `mislabel-evasion.*`
the AA-4 anti-weakening proof (shares this window).
