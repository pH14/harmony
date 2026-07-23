<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Task 138 — AA-6 masked-register-digest lane (hm-3bwm, pairs with hm-fiqo)

The named condition on upgrading the AA-6 LinuxGuest disposition from PROVISIONAL to full GO:
prove at gate scale that gate-semantics change #4 — the LinuxGuest `console + vGIC` digest
narrowing — is **exactly-and-only** the disclosed AA-5(c) stack-ASLR residual `{x29, SP}`
(hm-of6t F12), and NOT masking an injection-path register divergence.

**Status: apparatus COMPLETE + portably green; the ≥1000-rep on-silicon lane is PENDING** (the
ARM box was spun down 2026-07-22). Delivered as the PR-#139 pattern: the portable half is
finished and gated; the box lane is one turnkey command (`RUNBOOK.md`) for the next ARM window.
**The condition remains OPEN — not claimed met.**

## What was built

Portable logic (`harness/src/sys.rs`):
- `kvm::REG_CORE_X29` / `REG_CORE_SP` — the **full** KVM one-reg ids of the two masked general
  registers, derived from the core prefix + index and pinned to the on-N1 dump ids
  `0x…003A` / `0x…003E`.
- `is_masked_general_register` — the **closed list of two** ({x29, SP}).
- `digest_regs_masked` — the full register file minus exactly {x29, SP}, host-time counters
  inherited-excluded; distinct domain tag `arm-spike-regs-masked-v1` (never collides with the
  registers-only or state digests). Register-file identity only (vGIC passed empty at the call
  sites).
- Two unit tests: the exact-{x29,SP}-mask pin (incl. `sp_el1` as the kept near-miss) and a
  known-register-file-in ⇒ known-digest-out vector.

Plumbing:
- `StepVcpu::masked_regs_digest` on all three implementors (`Machine` + both scripted doubles).
- `run_until_ready_work_clock` (`linux_console.rs`): the injection-Moment witness
  `injected_landed_digest` is now the **masked** digest (hm-fiqo) instead of the full
  `regs_digest` that would diverge same-seed on {x29, SP}.
- `linux-boot` (`arm_spike.rs`) summary line emits `masked_regs_digest`,
  `injected_landed_digest`, and the **enumerated** exclusion set (`masked_excluded_gprs` by
  full id, `masked_excluded_host_time` by name); the witness is no longer discarded. The
  emitted `RunRecord.state_digest` stays `console + vGIC` — change #4 untouched.

Box lane (`host/`): `aa6-masked-digest-lane.sh` (runner, taskset-pinned, `--image-sha256`
safety, `config.json`, smoke-first) + `aa6-masked-digest-check.py` (verdict: bit-identity of
both digests, mask enumerated-and-exactly-{x29,SP}, injection fired, pinned artifacts, rep
floor). Offline-validated against synthetic PASS / register-divergence-FAIL / vacuity /
mask-widening / rep-floor cases.

## Deviations considered and rejected

1. **Masked digest = register-file ONLY, not register+vGIC.** The existing `injected_landed_digest`
   and `regs_digest` fold in the vGIC. I pass the vGIC **empty** to `digest_regs_masked` at both
   call sites. Rationale: the disposition is a biconditional — *masked-digest divergence ⟺ a
   register outside {x29, SP} moved*. Folding in the vGIC would let a vGIC-timing divergence
   trip a **false** P0 STOP (conflating the separately-certified vGIC with a register). The
   vGIC's determinism under injection is already certified (`console_vgic_digest` bit-identical
   ×1000, `vgic-roundtrip` PASS), and the task text is literally "the register file minus {x29,
   SP}". So the digest is register-only; `digest_regs_masked` keeps the `vgic` parameter for
   signature parity + testability (mirrors `core_regs_digest`, which passes `&[]`).
2. **Kept the record's `state_digest` = `console + vGIC`.** The masked lane is the *adversarial
   witness* on change #4, not a replacement of the gate basis. I only **added** emitted evidence;
   I did not disturb the merged PR #139 matrix semantics or the `RunRecord`/schema.
3. **`injected_landed_digest` re-typed to the masked digest** (was the full `regs_digest`, then
   discarded). Acceptance requires the witness bit-identical rep-to-rep; the full digest folds
   {x29, SP} and would diverge. `None`-on-negative-control semantics preserved.
4. **Mask is exactly {x29, SP} — sp_el1 explicitly NOT masked.** Confirmed against the retained
   `results/aa-6/live-20260721/linuxguest-regs-divergence.diff`: exactly 4 ids differ same-seed —
   x29 (`0x…003A`), SP (`0x…003E`), CNTPCT (`0x…DF01`), TIMER_CNT (`0x…DF1A`); the latter two are
   already host-time-excluded, leaving exactly {x29, SP}. `sp_el1` (the kernel SP, index `0x44`)
   is bit-identical same-seed and is a KEPT near-miss the unit test pins.
5. **New method on the `StepVcpu` trait** (not only an inherent `Machine` method): the
   injection-Moment witness is computed through the generic `vcpu` in
   `run_until_ready_work_clock`, so it must be reachable via the trait bound.

## Known limitations / what the foreman must know

- **The ≥1000-rep on-silicon run is NOT done** (ARM box spun down). The named condition is
  **OPEN**. `RUNBOOK.md` is the one-command turnkey lane (smoke-fire ~20 first, then ≥1000);
  same injection config as the merged PR #139 matrix.
- **Disposition on the eventual run:** all-identical ⇒ condition MET → the foreman escalates the
  PROVISIONAL→full-GO upgrade to Paul with this evidence (do **not** self-flip). Any divergence
  in the masked digest ⇒ a register outside {x29, SP} moved ⇒ **P0-class STOP** (commit evidence,
  PARK, escalate). **Never** widen the mask or narrow the digest to reach green — the checker
  enforces this (mask-widening is itself a FAIL) and enumerates every distinct digest on failure.
- **No new unsafe**; the digest logic is pure/safe (Miri-exercisable). CI files are out of the
  surface list, so the `quality.yml` miri `-p` list was not touched — arm-harness's existing
  coverage applies to the new pure functions.
- **Portable gates green:** `cargo build`, `cargo nextest` (164 tests, incl. the 2 new ones),
  `cargo clippy` (host **and** `--target aarch64-unknown-linux-gnu` — the `cfg(linux)` `linux-boot`
  path is where the box-binary edits live, invisible to the macOS build), `cargo fmt`,
  `cargo deny`. No new dependencies.
