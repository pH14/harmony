# tasks/113 — Stale insn-cpuid golden on main (hm-zc2)

**Bead:** `hm-zc2` (P2 bug on main) · **Budget:** small (one box session + a golden PR)

## Problem

The acceptance-suite insn-cpuid golden on main is stale — two independent observations:
1. The nested-x86 spike found it stale (2026-07-10; evidence on `spike/nested-x86`,
   `spikes/nested-x86/` results + README).
2. The task-110 box window (2026-07-15, hm-rk5 notes) hit it again: acceptance-suite **O2
   insn-cpuid FAILS on the box — the digest is DETERMINISTIC run-to-run (identical) but
   != the committed golden**. insn-rdtsc O2 and insn-rng O2 both PASS. The pvclock branch
   touched no golden/CPUID path, so this predates it.

## Task

1. **Reproduce on current main** on the determinism box (`ssh hetzner`; pin per
   `docs/BOX-PINNING.md`; the box is currently free — check `docs/QUEUE.md` first and
   stop if a re-cert or veto-window box run has started).
2. **Diagnose before regenerating**: why does the digest differ? Candidates, in order of
   likelihood: (a) the 2026-07-09 box image rebuild (the `hm-xdp`/`hm-2nt` drift family —
   check whether the golden's provenance predates that rebuild), (b) a microcode update
   changing CPUID-visible bits, (c) a genuinely stale golden never re-captured after a
   legitimate change. Rule out a real regression: confirm the differing digest is stable
   across runs AND across a reboot, and identify WHICH CPUID leaves/bits moved (diff the
   decoded capture, not just the digest).
3. **Refresh through the golden-regeneration discipline** — never hand-edit; use the
   established capture tooling; **document the provenance** (box, kernel, microcode rev,
   image hash, date, and the leaf-level diff vs the old golden) in the commit message and
   the golden's adjacent README/comment if one exists.
4. If the diff shows anything OTHER than explainable drift (e.g. leaves that should be
   contract-frozen moving), STOP and escalate on the PR instead of regenerating.

## Deliverables

- A PR on this task's branch: the regenerated golden + provenance, plus the leaf-level
  diff summary in the PR body. Portable gates green (the golden's consuming tests pass
  locally where runnable); the box acceptance-suite O2 insn-cpuid gate green with the new
  golden (include the run evidence).
- `bd` note on `hm-zc2` linking the PR; if the root cause IS the hm-xdp image family,
  say so on `hm-xdp`/`hm-2nt` too.

## Ground rules

- Only the golden file(s) + their provenance docs change — no harness/crate code. If the
  fix seems to need code, stop and escalate.
- Box discipline: pinned core, release when done, no KVM module changes (stock KVM is
  fine for a CPUID capture — no patched-KVM window needed).
