# Task 167 — hm-xkh5: the task-78 draw probe vs the entropy-stream timeline (P1)

**Bead:** `hm-xkh5` (P1). Read it FIRST — `bd show hm-xkh5` from the main workspace —
it carries the full disagreement, the controls already done, the named experiment, the
repro commands, and the candidate fix path. This spec is the lane discipline and the
deliverable contract, not a restatement.

**Why this is P1:** two load-bearing instruments disagree about whether the Postgres
guests draw entropy in the task-78 hop/tail windows. Branch (a) means `REQUIRE_DRAWS`
is vacuous on these guests and the task-78 "bit-identical even when entropy is drawn
inside a collapsed interval" box evidence does not rest on a drawing window. Branch (b)
means a restored+reseeded branch draws where the live boot does not — a live-vs-branched
execution difference, i.e. a replay-fidelity question. Which branch is true changes what
the project may claim.

## Lane discipline

- **You are the only box-touching worker.** `ssh hetzner`. The coordinator
  `/root/box-window.sh` is the NEW time+pid version (deployed + hand-verified
  2026-07-27): `acquire <name> --ttl <seconds>` prints your leased core; `renew <name>
  <seconds>` from a fresh ssh extends it — renew BEFORE the deadline during long runs;
  `release <name>` when done (last lease out reverts to stock and verifies). Pin every
  workload with `taskset -c <leased-core>` per `docs/BOX-PINNING.md`.
- **Smoke-fire-once:** the bead's two REPRODUCE commands (~3 min each, release build)
  are your smoke — run both and confirm you observe the disagreement yourself before
  any deeper spend. If you cannot reproduce it, STOP and report; do not proceed to fixes
  against a phenomenon you have not seen.
- Build cache: /root/harmony-ibl2 has a warm target tree from the tasks/157 lane.

## The experiment (named in the bead — run it, don't redesign it)

Diff `Vmm::state_components()` between hop 3's plain leg and its probe leg and identify
WHICH hashed chunk differs — `vtim:entropy`, `vtim:eff-vns`, or a RAM region. That
distinguishes (a) probe false-positive from (b) branch-really-draws immediately.

## Branch-dependent deliverables

- **Determination first, in writing** — (a) or (b), with the chunk-diff evidence, in
  the lane record (`docs/history/IMPLEMENTATION-task167.md`).
- **If (a) — probe false positive:** find the mechanism (why the trailing-reseed probe
  reports draws on an unmoved stream), fix the probe, and prove the fix with a
  positive/negative pair: the bridge guest (`initramfs-bridge.cpio.gz`, the first
  workload that genuinely draws seeded entropy on demand) as the real drawing baseline
  (probe MUST fire), and the Postgres guest as the draw-free baseline (fixed probe MUST
  NOT fire). Then update `hm-xkh5` and file one bead for the task-78 evidence-claim
  relabel if the record's wording needs it (docs-match-evidence species) — do NOT
  rewrite historical evidence claims yourself in this lane.
- **If (b) — a restored branch draws where the live boot doesn't:** STOP after
  characterizing the mechanism (where in restore/reseed the extra draw comes from).
  Do NOT change live-path or restore-path behavior — the fix direction is a
  determinism-core design call. Write the mechanism + at least one candidate fix
  direction into the lane record and the bead, and report. This is the hm-ej5-style
  stop condition: an evidence question, not a code fix.
- Either way: `hm-xkh5` gets the determination as a comment; the epic `hm-i2et` gets a
  one-line status.

## Gates

Portable: `cargo nextest run -p campaign-runner`, clippy `-D warnings`, fmt. Any probe
change ships its regression in the same commit, with the fail-before direction shown
(W1 doctrine: a grader never seen to fail is not a gate). Box: the two repro commands
green/red as the determination demands, plus the (a)-branch positive/negative pair if
taken. Leave the box at stock (1396736, zero leases) — verify by hand, not via `status`
alone.

Do not weaken any existing check. `REQUIRE_DRAWS=0` escapes are for diagnosis only —
never commit a default that relaxes the precondition.
