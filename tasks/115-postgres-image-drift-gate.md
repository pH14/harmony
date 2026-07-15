# tasks/115 — Postgres-image drift: restore the draw-probe gate on main (hm-xdp + hm-2nt)

**Beads:** `hm-xdp` (P2: the broken gate on main) + `hm-2nt` (its image-side fix) — one
lane, close both or split with reasons. · **Class:** harness + box-artifact work.

## Problem

`conductor live_materialization`'s task-78 **REQUIRE_DRAWS draw-probe precondition fails
on main with default knobs** (hop_draws all false, tail true) — on both the task-95 tree
and a main control, so it is image drift, not a code regression. Root cause (diagnosed at
task 95 M2): the canonical `initramfs-postgres.cpio.gz` on the box was **rebuilt
2026-07-09 02:56** (t81 checkout, md5 9860a065) and differs from the Jul-2 PR-44 build the
gate's default hop windows were tuned against — the first entropy draw now lands past the
default hop windows.

**A pin-by-hash ruling is already recorded** (see the `hm-xdp` bead notes + task-95 M2:
box gates run on content-hash-pinned images; the discipline is in-harness for the
task-95 gates). What remains is THIS gate.

## Task

1. **Read the recorded ruling on `hm-xdp` first** (bd show, notes) — the decision
   framework exists; do not re-litigate it. Confirm which image hash the ruling pins as
   the gate baseline for live_materialization.
2. **Restore the gate green on main**, choosing per the ruling:
   - If the OLD (Jul-2/PR-44) image is the pinned baseline: retrofit
     live_materialization to the in-harness pin-by-hash discipline (refuse to run
     against a mismatched image with a loud, actionable error naming the expected hash;
     re-materialize or locate the pinned image on the box).
   - If the NEW (Jul-9, md5 9860a065) image should become the baseline (`hm-2nt`'s
     path): fix the image content — move `READY_MARKER` into/nearer the uuid workload
     loop (or start the workload earlier) so the first draw lands inside the default hop
     windows — rebuild reproducibly, pin the new content hash, and prove the gate green.
   - Either way: the gate must FAIL CLOSED with the expected-vs-found hash on any future
     image drift, never silently mis-probe.
3. **Box validation**: run live_materialization (and any sibling gate that shares the
   image) green on the box with the pinned image. Pin per `docs/BOX-PINNING.md`; stock
   KVM unless the gate demands otherwise; check `docs/QUEUE.md` for box availability
   first; release when done.
4. Standard portable gates on anything code-touched (nextest/clippy/fmt/deny).

## Deliverables

A PR: the gate restoration + pin enforcement (+ the image rebuild recipe if hm-2nt's
path is taken — reproducible, hash-pinned, provenance documented like
`guest/golden/insn-cpuid.provenance.md`), box run evidence in the PR body, `bd` notes on
both beads. Close-or-split disposition for `hm-xdp`/`hm-2nt` stated explicitly.

## Ground rules

- The recorded ruling governs the baseline choice — apply it; escalate on the PR only
  if the ruling genuinely doesn't cover what you find.
- Never weaken the gate to green it (no widened hop windows without a ruling; the
  gate's power is that drift FAILS it).
- Box discipline as always: pinned core, state-based waits (`bd memories
  box-pkill-argv-landmine`), release + note.
